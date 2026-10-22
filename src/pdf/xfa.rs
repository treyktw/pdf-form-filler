//! Hybrid XFA forms carry both an XFA layer and ordinary AcroForm fields (the
//! FTC buyer's guide is one). Filling only the AcroForm leaves stale XFA data
//! that Adobe Reader would show instead, so, as pdf-lib does, the XFA layer is
//! removed first and the AcroForm becomes the single source of truth. PDFium
//! cannot edit objects, so this one step uses lopdf. Pure XFA forms have no
//! AcroForm to fill and stay unsupported; encrypted hybrids are refused rather
//! than rewritten.

use lopdf::{Document, Object};

use super::error::{PdfError, PdfErrorCode, PdfResult};

pub fn strip_xfa(bytes: &[u8]) -> PdfResult<Vec<u8>> {
    let unsupported = || PdfError::new(PdfErrorCode::XfaUnsupported, "XFA PDFs are not supported");
    let mut doc = Document::load_mem(bytes).map_err(|_| unsupported())?;
    if doc.is_encrypted() {
        return Err(unsupported());
    }
    let acroform = doc
        .catalog()
        .ok()
        .and_then(|c| c.get(b"AcroForm").ok().cloned())
        .ok_or_else(unsupported)?;
    let removed = match acroform {
        Object::Reference(id) => doc
            .get_object_mut(id)
            .ok()
            .and_then(|o| o.as_dict_mut().ok())
            .map(|d| d.remove(b"XFA").is_some()),
        Object::Dictionary(_) => {
            let catalog = doc.catalog_mut().map_err(|_| unsupported())?;
            catalog
                .get_mut(b"AcroForm")
                .ok()
                .and_then(|o| o.as_dict_mut().ok())
                .map(|d| d.remove(b"XFA").is_some())
        }
        _ => None,
    };
    if removed != Some(true) {
        return Err(unsupported());
    }
    let mut out = Vec::new();
    doc.save_to(&mut out).map_err(|_| unsupported())?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::{Dictionary, Stream, dictionary};

    /// A minimal hybrid form: a catalog whose AcroForm holds both /Fields and /XFA.
    fn hybrid(inline_acroform: bool) -> Vec<u8> {
        let mut doc = Document::with_version("1.7");
        let xfa = doc.add_object(Stream::new(Dictionary::new(), b"<xdp:xdp/>".to_vec()));
        let acroform = dictionary! { "Fields" => Vec::<Object>::new(), "XFA" => xfa };
        let pages = doc.add_object(dictionary! { "Type" => "Pages", "Kids" => Vec::<Object>::new(), "Count" => 0 });
        let acroform_entry: Object = if inline_acroform {
            acroform.into()
        } else {
            doc.add_object(acroform).into()
        };
        let catalog =
            doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages, "AcroForm" => acroform_entry });
        doc.trailer.set("Root", catalog);
        let mut out = Vec::new();
        doc.save_to(&mut out).unwrap();
        out
    }

    fn acroform_has_xfa(bytes: &[u8]) -> bool {
        let doc = Document::load_mem(bytes).unwrap();
        let acroform = doc.catalog().unwrap().get(b"AcroForm").unwrap().clone();
        match acroform {
            Object::Reference(id) => doc.get_object(id).unwrap().as_dict().unwrap().has(b"XFA"),
            Object::Dictionary(d) => d.has(b"XFA"),
            _ => unreachable!(),
        }
    }

    #[test]
    fn strips_xfa_from_a_referenced_or_inline_acroform() {
        for inline in [false, true] {
            let input = hybrid(inline);
            assert!(acroform_has_xfa(&input));
            let out = strip_xfa(&input).unwrap();
            assert!(!acroform_has_xfa(&out), "inline={inline}");
        }
    }

    #[test]
    fn refuses_files_with_nothing_to_strip() {
        let once = strip_xfa(&hybrid(false)).unwrap();
        assert_eq!(strip_xfa(&once).unwrap_err().code, PdfErrorCode::XfaUnsupported);
        assert_eq!(strip_xfa(b"not a pdf").unwrap_err().code, PdfErrorCode::XfaUnsupported);
    }
}
