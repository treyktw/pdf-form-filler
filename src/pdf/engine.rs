//! A small, safe wrapper over PDFium's C API.
//!
//! pdfium-render supplies the bindings. Its high-level form setters write a
//! value onto the widget annotation and leave appearance streams alone, which
//! breaks fields with several widgets and checkboxes whose "on" state is not
//! named /Yes. So values are set the way a person filling the form in Chrome
//! sets them: through PDFium's form-fill environment (focus the widget, type or
//! press space, pick an option). PDFium then stores the value on the field and
//! regenerates every widget's appearance itself.
//!
//! PDFium is not thread-safe. Run one job at a time per process (see
//! job_process.rs) and hold the engine behind a mutex; nothing here is Send
//! across threads.

use std::ffi::c_void;
use std::os::raw::{c_int, c_ulong};
use std::path::Path;

use pdfium_render::prelude::*;

use super::error::{PdfError, PdfErrorCode, PdfResult};

// From PDFium's public headers (fpdf_formfill.h, fpdf_annot.h, fpdf_save.h,
// fpdfview.h); pdfium-render does not re-export these constants.
pub const FIELD_PUSHBUTTON: c_int = 1;
pub const FIELD_CHECKBOX: c_int = 2;
pub const FIELD_RADIOBUTTON: c_int = 3;
pub const FIELD_COMBOBOX: c_int = 4;
pub const FIELD_LISTBOX: c_int = 5;
pub const FIELD_TEXTFIELD: c_int = 6;
pub const FIELD_SIGNATURE: c_int = 7;

pub const FLAG_READONLY: c_int = 1 << 0;
pub const FLAG_REQUIRED: c_int = 1 << 1;
pub const FLAG_COMBO_EDIT: c_int = 1 << 18;

const ANNOT_WIDGET: c_int = 20;
const FORMTYPE_XFA_FULL: c_int = 2;
const FORMTYPE_XFA_FOREGROUND: c_int = 3;
const FPDF_ERR_PASSWORD: c_ulong = 4;
const FPDF_NO_INCREMENTAL: FPDF_DWORD = 1 << 1;
const FPDF_RENDER_ANNOT: c_int = 0x01;
const AP_MODE_NORMAL: c_int = 0;
// Permission bits (PDF 32000-1, table 22): 6 = annotate and fill forms,
// 9 = fill existing form fields.
const PERM_ANNOTATE: c_ulong = 1 << 5;
const PERM_FILL_FORMS: c_ulong = 1 << 8;
const KEY_SPACE: c_int = 0x20;

/// Detail of the XFA error for a hybrid form, whose XFA layer can be removed.
pub const HYBRID_XFA: &str = "hybrid";

/// Where to find libpdfium: `PDFIUM_DYNAMIC_LIB_PATH`, else `PDFIUM_LIB_PATH`
/// (the name the original service used), else `./.pdfium/lib` (where
/// scripts/fetch-pdfium.sh unpacks it). Either a directory or the file.
pub fn default_library_path() -> std::path::PathBuf {
    std::env::var_os("PDFIUM_DYNAMIC_LIB_PATH")
        .or_else(|| std::env::var_os("PDFIUM_LIB_PATH"))
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(".pdfium/lib"))
}

/// The loaded PDFium library. One per process.
pub struct Engine {
    b: Box<dyn PdfiumLibraryBindings>,
}

impl Engine {
    /// Loads PDFium from a directory holding the platform library
    /// (libpdfium.dylib / libpdfium.so) or from the library file itself.
    pub fn load(path: &Path) -> Result<Self, String> {
        let file = if path.is_dir() {
            Pdfium::pdfium_platform_library_name_at_path(path)
        } else {
            path.to_path_buf()
        };
        let b = Pdfium::bind_to_library(&file)
            .map_err(|e| format!("could not load PDFium from {}: {e}", file.display()))?;
        unsafe { b.FPDF_InitLibrary() };
        Ok(Self { b })
    }

    /// Opens a document held in memory. The bytes are owned by the document:
    /// PDFium reads from them for as long as it is open.
    pub fn open(&self, bytes: Vec<u8>) -> PdfResult<Document<'_>> {
        let bytes = bytes.into_boxed_slice();
        let b = &*self.b;
        let doc = unsafe { b.FPDF_LoadMemDocument64(&bytes, None) };
        if doc.is_null() {
            let err = unsafe { b.FPDF_GetLastError() };
            return Err(if err == FPDF_ERR_PASSWORD {
                PdfError::new(PdfErrorCode::EncryptedUnsupported, "Encrypted PDFs are not supported")
            } else {
                PdfError::new(PdfErrorCode::InvalidInput, "PDF could not be parsed")
            });
        }
        // Many state forms (MV-1, T-7, T-8) are encrypted with an owner
        // password only: they open without one and restrict what may change.
        // Those are filled when the issuer allows form filling, and saved with
        // their encryption intact. pdf-lib could not read them at all.
        if unsafe { b.FPDF_GetSecurityHandlerRevision(doc) } != -1 {
            let permissions = unsafe { b.FPDF_GetDocPermissions(doc) };
            if permissions & (PERM_FILL_FORMS | PERM_ANNOTATE) == 0 {
                unsafe { b.FPDF_CloseDocument(doc) };
                return Err(PdfError::new(
                    PdfErrorCode::EncryptedUnsupported,
                    "The PDF's permissions do not allow filling its form",
                ));
            }
        }
        let form_type = unsafe { b.FPDF_GetFormType(doc) };
        if form_type == FORMTYPE_XFA_FULL || form_type == FORMTYPE_XFA_FOREGROUND {
            unsafe { b.FPDF_CloseDocument(doc) };
            // A hybrid (foreground) form also has AcroForm fields; the caller
            // may strip its XFA layer and try again (pdf/xfa.rs).
            let detail = if form_type == FORMTYPE_XFA_FOREGROUND {
                HYBRID_XFA
            } else {
                "full"
            };
            return Err(PdfError::with(
                PdfErrorCode::XfaUnsupported,
                "XFA PDFs are not supported",
                detail,
            ));
        }

        // Version 1 with no callbacks: PDFium checks each one before calling,
        // and a headless fill needs none of them.
        let mut info: Box<FPDF_FORMFILLINFO> = Box::new(unsafe { std::mem::zeroed() });
        info.version = 1;
        let form = unsafe { b.FPDFDOC_InitFormFillEnvironment(doc, &mut *info) };

        let mut document = Document {
            e: self,
            _bytes: bytes,
            doc,
            form,
            _info: info,
            pages: Vec::new(),
        };
        let count = unsafe { b.FPDF_GetPageCount(doc) };
        for i in 0..count.max(0) {
            let page = unsafe { b.FPDF_LoadPage(doc, i) };
            if page.is_null() {
                return Err(PdfError::new(
                    PdfErrorCode::InvalidInput,
                    "PDF page could not be loaded",
                ));
            }
            if !form.is_null() {
                unsafe { b.FORM_OnAfterLoadPage(page, form) };
            }
            document.pages.push(page);
        }
        Ok(document)
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        unsafe { self.b.FPDF_DestroyLibrary() };
    }
}

/// One widget annotation of an interactive form field.
#[derive(Debug, Clone)]
pub struct Widget {
    pub page: usize,
    pub index: c_int,
    pub name: String,
    pub kind: c_int,
    pub flags: c_int,
}

pub struct Document<'e> {
    e: &'e Engine,
    _bytes: Box<[u8]>,
    doc: FPDF_DOCUMENT,
    form: FPDF_FORMHANDLE,
    _info: Box<FPDF_FORMFILLINFO>,
    pages: Vec<FPDF_PAGE>,
}

impl Drop for Document<'_> {
    fn drop(&mut self) {
        let b = &*self.e.b;
        unsafe {
            if !self.form.is_null() {
                b.FORM_ForceToKillFocus(self.form);
            }
            for &page in &self.pages {
                if !self.form.is_null() {
                    b.FORM_OnBeforeClosePage(page, self.form);
                }
                b.FPDF_ClosePage(page);
            }
            if !self.form.is_null() {
                b.FPDFDOC_ExitFormFillEnvironment(self.form);
            }
            b.FPDF_CloseDocument(self.doc);
        }
    }
}

impl Document<'_> {
    fn b(&self) -> &dyn PdfiumLibraryBindings {
        &*self.e.b
    }

    pub fn page_count(&self) -> usize {
        self.pages.len()
    }

    pub fn has_form(&self) -> bool {
        !self.form.is_null()
    }

    /// Whether the document uses a standard security handler (it was opened
    /// without a password, so this means owner-password encryption).
    pub fn is_encrypted(&self) -> bool {
        unsafe { self.b().FPDF_GetSecurityHandlerRevision(self.doc) != -1 }
    }

    /// Runs f with the widget's annotation handle open.
    fn with_annot<R>(&self, w: &Widget, f: impl FnOnce(FPDF_ANNOTATION, FPDF_PAGE) -> R) -> R {
        let page = self.pages[w.page];
        let annot = unsafe { self.b().FPDFPage_GetAnnot(page, w.index) };
        let out = f(annot, page);
        if !annot.is_null() {
            unsafe { self.b().FPDFPage_CloseAnnot(annot) };
        }
        out
    }

    /// Every form widget, in page and annotation order.
    pub fn widgets(&self) -> Vec<Widget> {
        let mut out = Vec::new();
        if self.form.is_null() {
            return out;
        }
        for (pi, &page) in self.pages.iter().enumerate() {
            let count = unsafe { self.b().FPDFPage_GetAnnotCount(page) };
            for index in 0..count.max(0) {
                let annot = unsafe { self.b().FPDFPage_GetAnnot(page, index) };
                if annot.is_null() {
                    continue;
                }
                if unsafe { self.b().FPDFAnnot_GetSubtype(annot) } as c_int == ANNOT_WIDGET {
                    let kind = unsafe { self.b().FPDFAnnot_GetFormFieldType(self.form, annot) };
                    let name = self.annot_string(|buf, len| unsafe {
                        self.b().FPDFAnnot_GetFormFieldName(self.form, annot, buf, len)
                    });
                    let flags = unsafe { self.b().FPDFAnnot_GetFormFieldFlags(self.form, annot) };
                    if kind > 0 && !name.is_empty() {
                        out.push(Widget {
                            page: pi,
                            index,
                            name,
                            kind,
                            flags,
                        });
                    }
                }
                unsafe { self.b().FPDFPage_CloseAnnot(annot) };
            }
        }
        out
    }

    /// Reads one of PDFium's UTF-16LE string getters (call once for the byte
    /// length, again to fill the buffer).
    fn annot_string(&self, get: impl Fn(*mut FPDF_WCHAR, c_ulong) -> c_ulong) -> String {
        let len = get(std::ptr::null_mut(), 0) as usize;
        if len <= 2 {
            return String::new();
        }
        let mut buf = vec![0u16; len / 2];
        get(buf.as_mut_ptr(), len as c_ulong);
        while buf.last() == Some(&0) {
            buf.pop();
        }
        String::from_utf16_lossy(&buf)
    }

    pub fn value(&self, w: &Widget) -> String {
        self.with_annot(w, |annot, _| {
            self.annot_string(|buf, len| unsafe { self.b().FPDFAnnot_GetFormFieldValue(self.form, annot, buf, len) })
        })
    }

    pub fn export_value(&self, w: &Widget) -> String {
        self.with_annot(w, |annot, _| {
            self.annot_string(|buf, len| unsafe {
                self.b().FPDFAnnot_GetFormFieldExportValue(self.form, annot, buf, len)
            })
        })
    }

    pub fn is_checked(&self, w: &Widget) -> bool {
        self.with_annot(
            w,
            |annot, _| unsafe { self.b().FPDFAnnot_IsChecked(self.form, annot) } != 0,
        )
    }

    pub fn options(&self, w: &Widget) -> Vec<(String, bool)> {
        self.with_annot(w, |annot, _| {
            let count = unsafe { self.b().FPDFAnnot_GetOptionCount(self.form, annot) };
            (0..count.max(0))
                .map(|i| {
                    let label = self.annot_string(|buf, len| unsafe {
                        self.b().FPDFAnnot_GetOptionLabel(self.form, annot, i, buf, len)
                    });
                    let selected = unsafe { self.b().FPDFAnnot_IsOptionSelected(self.form, annot, i) } != 0;
                    (label, selected)
                })
                .collect()
        })
    }

    /// Whether the widget carries a normal appearance stream with content.
    pub fn has_normal_appearance(&self, w: &Widget) -> bool {
        self.with_annot(w, |annot, _| {
            let has_ap = unsafe { self.b().FPDFAnnot_HasKey(annot, "AP") } != 0;
            let content = unsafe { self.b().FPDFAnnot_GetAP(annot, AP_MODE_NORMAL, std::ptr::null_mut(), 0) };
            // A checkbox's normal appearance is a dictionary of states, which
            // the getter does not return as content; the /AP key is the test.
            has_ap && (content > 2 || w.kind == FIELD_CHECKBOX || w.kind == FIELD_RADIOBUTTON)
        })
    }

    fn focus(&self, annot: FPDF_ANNOTATION) -> bool {
        unsafe { self.b().FORM_SetFocusedAnnot(self.form, annot) != 0 }
    }

    fn unfocus(&self) {
        unsafe { self.b().FORM_ForceToKillFocus(self.form) };
    }

    /// Replaces a text field's (or editable combo box's) text, as typing would.
    pub fn set_text(&self, w: &Widget, value: &str) -> bool {
        self.with_annot(w, |annot, page| {
            if !self.focus(annot) {
                return false;
            }
            let wide: Vec<u16> = value.encode_utf16().chain(std::iter::once(0)).collect();
            unsafe {
                self.b().FORM_SelectAllText(self.form, page);
                self.b().FORM_ReplaceSelection(self.form, page, wide.as_ptr());
            }
            self.unfocus();
            true
        })
    }

    /// Toggles a checkbox or selects a radio button, as pressing space would.
    pub fn press(&self, w: &Widget) -> bool {
        self.with_annot(w, |annot, page| {
            if !self.focus(annot) {
                return false;
            }
            unsafe { self.b().FORM_OnChar(self.form, page, KEY_SPACE, 0) };
            self.unfocus();
            true
        })
    }

    /// Sets which options of a choice field are selected.
    pub fn select(&self, w: &Widget, indices: &[c_int]) -> bool {
        let current: Vec<c_int> = self
            .options(w)
            .iter()
            .enumerate()
            .filter(|(_, (_, sel))| *sel)
            .map(|(i, _)| i as c_int)
            .collect();
        self.with_annot(w, |annot, page| {
            if !self.focus(annot) {
                return false;
            }
            let mut ok = true;
            unsafe {
                for i in current.iter().filter(|i| !indices.contains(i)) {
                    ok &= self.b().FORM_SetIndexSelected(self.form, page, *i, 0) != 0;
                }
                for i in indices {
                    ok &= self.b().FORM_SetIndexSelected(self.form, page, *i, 1) != 0;
                }
            }
            self.unfocus();
            ok
        })
    }

    /// Saves a complete (non-incremental) copy.
    pub fn save(&self) -> PdfResult<Vec<u8>> {
        #[repr(C)]
        struct Writer {
            base: FPDF_FILEWRITE,
            out: Vec<u8>,
        }
        unsafe extern "C" fn write_block(this: *mut FPDF_FILEWRITE, data: *const c_void, size: c_ulong) -> c_int {
            // `base` is the first field of a #[repr(C)] Writer.
            let writer = unsafe { &mut *(this as *mut Writer) };
            if size > 0 {
                let chunk = unsafe { std::slice::from_raw_parts(data as *const u8, size as usize) };
                writer.out.extend_from_slice(chunk);
            }
            1
        }
        if !self.form.is_null() {
            self.unfocus();
        }
        let mut writer = Box::new(Writer {
            base: FPDF_FILEWRITE {
                version: 1,
                WriteBlock: Some(write_block),
            },
            out: Vec::new(),
        });
        let ok = unsafe {
            self.b()
                .FPDF_SaveAsCopy(self.doc, &mut writer.base, FPDF_NO_INCREMENTAL)
        };
        if ok == 0 {
            return Err(PdfError::new(PdfErrorCode::QaFailed, "PDF could not be saved"));
        }
        Ok(std::mem::take(&mut writer.out))
    }

    /// Draws a page as a viewer would: page content and annotations, then the
    /// form widgets from their appearance streams. FPDF_RenderPageBitmap
    /// leaves widgets out by design; FPDF_FFLDraw paints them.
    unsafe fn draw(&self, bitmap: FPDF_BITMAP, page: FPDF_PAGE, w: c_int, h: c_int) {
        unsafe {
            self.b()
                .FPDF_RenderPageBitmap(bitmap, page, 0, 0, w, h, 0, FPDF_RENDER_ANNOT);
            if !self.form.is_null() {
                self.b()
                    .FPDF_FFLDraw(self.form, bitmap, page, 0, 0, w, h, 0, FPDF_RENDER_ANNOT);
            }
        }
    }

    /// Renders one page to 8-bit RGBA, annotations included.
    pub fn render_rgba(&self, index: usize, dpi: f32) -> PdfResult<(u32, u32, Vec<u8>)> {
        let page = self.pages[index];
        let (wpt, hpt) = unsafe { (self.b().FPDF_GetPageWidthF(page), self.b().FPDF_GetPageHeightF(page)) };
        let w = ((wpt * dpi / 72.0).round() as c_int).clamp(1, 6000);
        let h = ((hpt * dpi / 72.0).round() as c_int).clamp(1, 6000);
        unsafe {
            let bitmap = self.b().FPDFBitmap_Create(w, h, 1);
            if bitmap.is_null() {
                return Err(PdfError::new(PdfErrorCode::QaFailed, "PDF page could not be rendered"));
            }
            self.b().FPDFBitmap_FillRect(bitmap, 0, 0, w, h, 0xFFFF_FFFF);
            self.draw(bitmap, page, w, h);
            let stride = self.b().FPDFBitmap_GetStride(bitmap) as usize;
            let src = self.b().FPDFBitmap_GetBuffer(bitmap) as *const u8;
            let mut rgba = Vec::with_capacity((w * h * 4) as usize);
            for y in 0..h as usize {
                let row = std::slice::from_raw_parts(src.add(y * stride), w as usize * 4);
                for px in row.chunks_exact(4) {
                    rgba.extend_from_slice(&[px[2], px[1], px[0], px[3]]); // BGRA -> RGBA
                }
            }
            self.b().FPDFBitmap_Destroy(bitmap);
            Ok((w as u32, h as u32, rgba))
        }
    }

    /// Renders one page, annotations included, at the given resolution. Fails
    /// if PDFium cannot allocate or the page has no drawable size.
    pub fn render_page(&self, index: usize, dpi: f32) -> PdfResult<()> {
        let page = self.pages[index];
        let (wpt, hpt) = unsafe { (self.b().FPDF_GetPageWidthF(page), self.b().FPDF_GetPageHeightF(page)) };
        let w = ((wpt * dpi / 72.0).round() as c_int).clamp(1, 6000);
        let h = ((hpt * dpi / 72.0).round() as c_int).clamp(1, 6000);
        if !(wpt > 0.0 && hpt > 0.0) {
            return Err(PdfError::with(
                PdfErrorCode::QaFailed,
                "PDF page has no drawable size",
                format!("page {}", index + 1),
            ));
        }
        unsafe {
            let bitmap = self.b().FPDFBitmap_Create(w, h, 0);
            if bitmap.is_null() {
                return Err(PdfError::with(
                    PdfErrorCode::QaFailed,
                    "PDF page could not be rendered",
                    format!("page {}", index + 1),
                ));
            }
            self.b().FPDFBitmap_FillRect(bitmap, 0, 0, w, h, 0xFFFF_FFFF);
            self.draw(bitmap, page, w, h);
            self.b().FPDFBitmap_Destroy(bitmap);
        }
        Ok(())
    }
}
