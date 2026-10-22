//! The form corpus (fixtures/forms, blank public government forms): every form
//! with fields is filled with a value for each fillable field, read back,
//! structure-checked with qpdf and rendered page by page. Needs PDFium
//! (PDFIUM_DYNAMIC_LIB_PATH, default .pdfium/lib) and qpdf on PATH. Without
//! PDFium the tests say so and pass, so a machine without the library can still
//! run the unit tests; set REQUIRE_PDFIUM=1 (CI does) to make that a failure.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, MutexGuard, OnceLock};

use pdf_form_filler::pdf::{self, Assignment, Engine, FillOptions, Limits, PdfErrorCode, Value};
use pdf_form_filler::verify::{self, Structure, VerifyOptions};

/// PDFium is process-global and not thread-safe: the tests share one engine
/// and take turns, as jobs in one process must.
fn engine() -> Option<MutexGuard<'static, Engine>> {
    static ENGINE: OnceLock<Option<Mutex<Engine>>> = OnceLock::new();
    let engine = ENGINE.get_or_init(|| match Engine::load(&pdf::default_library_path()) {
        Ok(e) => Some(Mutex::new(e)),
        Err(e) => {
            assert!(
                std::env::var_os("REQUIRE_PDFIUM").is_none(),
                "REQUIRE_PDFIUM is set: {e}"
            );
            eprintln!("skipping: {e}");
            None
        }
    });
    engine.as_ref().map(|m| m.lock().unwrap_or_else(|p| p.into_inner()))
}

fn corpus() -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/forms");
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .expect("fixture corpus")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "pdf"))
        .collect();
    files.sort();
    files
}

/// A value for every fillable field. Text is one character, which fits any
/// maximum length and every comb box.
fn assignments(inspection: &pdf::Inspection) -> Vec<Assignment> {
    use pdf::fill::FieldType::*;
    inspection
        .fields
        .iter()
        .enumerate()
        .filter_map(|(i, f)| {
            let value = match f.kind {
                Text => Value::Text(char::from(b'A' + (i % 26) as u8).to_string()),
                Checkbox => Value::Bool(i % 2 == 0),
                Radio | Dropdown | OptionList => Value::Text(f.options.last()?.clone()),
                _ => return None,
            };
            Some(Assignment {
                name: f.name.clone(),
                value,
            })
        })
        .collect()
}

#[test]
fn every_form_in_the_corpus_fills_and_passes_its_checks() {
    let Some(engine) = engine() else { return };
    let mut filled_forms = 0;
    for path in corpus() {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let bytes = std::fs::read(&path).unwrap();
        let inspection = pdf::inspect(&engine, bytes.clone(), &Limits::default())
            .unwrap_or_else(|e| panic!("{name}: inspect failed: {e}"));
        if inspection.fields.is_empty() {
            continue;
        }
        let assignments = assignments(&inspection);
        let (out, report) = pdf::fill(&engine, bytes, &assignments, &FillOptions::default())
            .unwrap_or_else(|e| panic!("{name}: fill failed: {e}"));
        assert_eq!(report.filled.len(), assignments.len(), "{name}");
        assert!(report.skipped.is_empty(), "{name}");

        let doc = engine.open(out.clone()).unwrap();
        for page in 0..doc.page_count() {
            doc.render_page(page, 72.0)
                .unwrap_or_else(|e| panic!("{name}: page {}: {e}", page + 1));
        }
        drop(doc);

        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), &out).unwrap();
        if let Ok(status) = Command::new("qpdf")
            .arg("--check")
            .arg(tmp.path())
            .stdout(std::process::Stdio::null())
            .status()
        {
            assert!(status.success(), "{name}: qpdf --check failed");
        } else {
            eprintln!("qpdf not installed: structure check skipped for {name}");
        }
        filled_forms += 1;
    }
    // MV-1, T-7, T-8 and the buyer's guide. The GA-25 odometer statement is a
    // flat form with no fields and is skipped above.
    assert!(
        filled_forms >= 4,
        "expected the four fillable forms, filled {filled_forms}"
    );
}

#[test]
fn a_missing_field_fails_closed_and_can_be_skipped_by_policy() {
    let Some(engine) = engine() else { return };
    let path = corpus()
        .into_iter()
        .find(|p| p.to_string_lossy().contains("t7-bill-of-sale"))
        .unwrap();
    let bytes = std::fs::read(path).unwrap();
    let missing = [Assignment {
        name: "no.such.field".into(),
        value: Value::Text("x".into()),
    }];

    let err = pdf::fill(&engine, bytes.clone(), &missing, &FillOptions::default()).unwrap_err();
    assert_eq!(err.code, PdfErrorCode::FieldMissing);
    // Field values never appear in errors.
    assert!(!err.to_string().contains("\"x\""));

    let skip = FillOptions {
        missing: pdf::Policy::Skip,
        ..FillOptions::default()
    };
    let (_, report) = pdf::fill(&engine, bytes, &missing, &skip).unwrap();
    assert_eq!(report.skipped.len(), 1);
}

#[test]
fn values_longer_than_a_field_allows_are_refused_not_truncated() {
    let Some(engine) = engine() else { return };
    let path = corpus()
        .into_iter()
        .find(|p| p.to_string_lossy().contains("t7-bill-of-sale"))
        .unwrap();
    let bytes = std::fs::read(path).unwrap();
    // The T-7's VIN is a comb field with /MaxLen 17.
    let too_long = [Assignment {
        name: "VIN".into(),
        value: Value::Text("1ZZZZ00Z8NZ0000012".into()),
    }];
    let err = pdf::fill(&engine, bytes.clone(), &too_long, &FillOptions::default()).unwrap_err();
    assert_eq!(err.code, PdfErrorCode::ReadbackFailed);
    let fits = [Assignment {
        name: "VIN".into(),
        value: Value::Text("1ZZZZ00Z8NZ000001".into()),
    }];
    assert!(pdf::fill(&engine, bytes, &fits, &FillOptions::default()).is_ok());
}

#[test]
fn limits_are_enforced_before_parsing() {
    let Some(engine) = engine() else { return };
    let path = corpus()
        .into_iter()
        .find(|p| p.to_string_lossy().contains("t7-bill-of-sale"))
        .unwrap();
    let bytes = std::fs::read(path).unwrap();
    let options = FillOptions {
        limits: Limits {
            max_input_bytes: bytes.len() - 1,
            ..Limits::default()
        },
        ..FillOptions::default()
    };
    assert_eq!(
        pdf::fill(&engine, bytes, &[], &options).unwrap_err().code,
        PdfErrorCode::LimitExceeded
    );
    let not_pdf = b"hello".to_vec();
    assert_eq!(
        pdf::fill(&engine, not_pdf, &[], &FillOptions::default())
            .unwrap_err()
            .code,
        PdfErrorCode::InvalidInput
    );
}

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/forms").join(name)).unwrap()
}

fn text(name: &str, value: &str) -> Assignment {
    Assignment {
        name: name.into(),
        value: Value::Text(value.into()),
    }
}

/// Raw bytes contain this marker only while the file still has an XFA layer.
fn has_xfa_key(bytes: &[u8]) -> bool {
    lopdf::Document::load_mem(bytes)
        .ok()
        .and_then(|d| {
            let acroform = d.catalog().ok()?.get(b"AcroForm").ok()?.clone();
            let dict = match acroform {
                lopdf::Object::Reference(id) => d.get_object(id).ok()?.as_dict().ok()?.clone(),
                lopdf::Object::Dictionary(dict) => dict,
                _ => return None,
            };
            Some(dict.has(b"XFA"))
        })
        .unwrap_or(false)
}

#[test]
fn a_hybrid_xfa_form_is_filled_through_its_acroform_with_the_xfa_layer_removed() {
    let Some(engine) = engine() else { return };
    let bytes = fixture("ftc-buyers-guide.pdf");
    assert!(has_xfa_key(&bytes), "the FTC buyer's guide ships as a hybrid XFA form");
    let assignments = [
        text("topmostSubform[0].BG-AsIs[0].VehicleMake[0]", "EXAMPLE"),
        text("topmostSubform[0].BG-AsIs[0].VIN[0]", "1ZZZZ00Z8NZ000001"),
        text("topmostSubform[0].BG-AsIs[0].Warranty[0]", "As Is"),
    ];
    let (out, report) = pdf::fill(&engine, bytes, &assignments, &FillOptions::default()).unwrap();
    assert!(report.read_back_verified);
    assert!(
        !has_xfa_key(&out),
        "stale XFA data would override the AcroForm in Acrobat"
    );
}

#[test]
fn an_owner_password_form_is_filled_and_keeps_its_encryption() {
    let Some(engine) = engine() else { return };
    let bytes = fixture("ga-mv1-title-application.pdf");
    assert!(engine.open(bytes.clone()).unwrap().is_encrypted());
    let assignments = [
        text("VIN", "1ZZZZ00Z8NZ000001"),
        text("Make", "EXAMPLE"),
        Assignment {
            name: "Leased Vehicle".into(),
            value: Value::Text("No".into()),
        },
        Assignment {
            name: "EXEMPT".into(),
            value: Value::Bool(true),
        },
    ];
    let (out, report) = pdf::fill(&engine, bytes, &assignments, &FillOptions::default()).unwrap();
    assert_eq!(report.filled.len(), 4);
    let saved = engine.open(out).unwrap();
    assert!(saved.is_encrypted(), "the issuer's restrictions must survive the fill");
}

#[test]
fn verify_passes_a_fresh_fill_and_reports_what_it_checked() {
    let Some(engine) = engine() else { return };
    let assignments = [
        text("VIN", "1ZZZZ00Z8NZ000001"),
        text("Make", "EXAMPLE"),
        text("Year", "2022"),
    ];
    let (out, _) = pdf::fill(
        &engine,
        fixture("ga-t7-bill-of-sale.pdf"),
        &assignments,
        &FillOptions::default(),
    )
    .unwrap();

    let report = verify::verify(&engine, &out, Some(&assignments), &VerifyOptions::default()).unwrap();
    assert_eq!(report.read_back_verified, Some(3));
    assert_eq!(report.fields_with_values, 3);
    assert_eq!(report.widgets_with_appearance, 3);
    assert_eq!(report.pages_rendered, report.page_count);
    if Command::new("qpdf").arg("--version").output().is_ok() {
        assert_eq!(report.structure, Structure::Passed);
    }

    // The same file checked against values it does not hold fails read-back.
    let wrong = [text("Make", "OTHER")];
    let err = verify::verify(&engine, &out, Some(&wrong), &VerifyOptions::default()).unwrap_err();
    assert_eq!(err.code, PdfErrorCode::ReadbackFailed);
}

#[test]
fn qpdf_passes_a_fresh_save_and_rejects_a_damaged_file() {
    let Some(engine) = engine() else { return };
    if Command::new("qpdf").arg("--version").output().is_err() {
        eprintln!("skipping: qpdf not installed");
        return;
    }
    let qpdf = Path::new("qpdf");
    // The issuers' own files carry stale linearization hints, which qpdf
    // reports as warnings (exit 3). PDFium's full save rewrites them cleanly,
    // which is why only exit 0 is accepted for a filled file.
    assert!(verify::qpdf_check(qpdf, &fixture("ga25-odometer-disclosure.pdf")).is_err());
    let (filled, _) = pdf::fill(
        &engine,
        fixture("ga-t7-bill-of-sale.pdf"),
        &[text("Make", "EXAMPLE")],
        &FillOptions::default(),
    )
    .unwrap();
    assert!(verify::qpdf_check(qpdf, &filled).is_ok());

    // Cut the cross-reference table and trailer off.
    let mut damaged = filled;
    damaged.truncate(damaged.len() * 2 / 3);
    let err = verify::qpdf_check(qpdf, &damaged).unwrap_err();
    assert_eq!(err.code, PdfErrorCode::QaFailed);
}

#[test]
fn a_flat_form_inspects_as_having_no_fields() {
    let Some(engine) = engine() else { return };
    let inspection = pdf::inspect(&engine, fixture("ga25-odometer-disclosure.pdf"), &Limits::default()).unwrap();
    assert_eq!((inspection.page_count, inspection.field_count), (1, 0));
}

#[test]
fn bad_values_are_refused_by_type() {
    let Some(engine) = engine() else { return };
    let bytes = fixture("ga-mv1-title-application.pdf");
    for (assignment, code) in [
        (
            Assignment {
                name: "EXEMPT".into(),
                value: Value::Text("maybe".into()),
            },
            PdfErrorCode::FieldValueInvalid,
        ),
        (
            Assignment {
                name: "Leased Vehicle".into(),
                value: Value::Text("Sometimes".into()),
            },
            PdfErrorCode::FieldValueInvalid,
        ),
        (
            Assignment {
                name: "VIN".into(),
                value: Value::Bool(true),
            },
            PdfErrorCode::FieldValueInvalid,
        ),
        (
            Assignment {
                name: "PRINT".into(),
                value: Value::Bool(true),
            },
            PdfErrorCode::FieldUnsupported,
        ),
    ] {
        let err = pdf::fill(
            &engine,
            bytes.clone(),
            std::slice::from_ref(&assignment),
            &FillOptions::default(),
        )
        .unwrap_err();
        assert_eq!(err.code, code, "{}", assignment.name);
    }
    let duplicate = [text("VIN", "A"), text("VIN", "B")];
    assert_eq!(
        pdf::fill(&engine, bytes, &duplicate, &FillOptions::default())
            .unwrap_err()
            .code,
        PdfErrorCode::InvalidInput
    );
}
