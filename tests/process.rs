//! The child-process boundary: a job's PDF work runs in `pdfctl` started with
//! --process-job, and a job that overruns is killed. The PDF cases need PDFium
//! like tests/forms.rs and skip without it; the timeout, crash and clean-env
//! cases use stand-in child scripts and always run.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use pdf_form_filler::job_process::{ProcessError, process};
use pdf_form_filler::pdf::{self, Assignment, Limits, PdfErrorCode, Value};

fn pdfium() -> Option<PathBuf> {
    let path = std::fs::canonicalize(pdf::default_library_path())
        .ok()
        .filter(|p| p.exists());
    if path.is_none() {
        assert!(
            std::env::var_os("REQUIRE_PDFIUM").is_none(),
            "REQUIRE_PDFIUM is set but PDFium was not found"
        );
    }
    path
}

fn pdfctl() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_pdfctl"))
}

fn bill_of_sale() -> Vec<u8> {
    std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/forms/ga-t7-bill-of-sale.pdf")).unwrap()
}

/// A stand-in child: a shell script in its own temporary directory.
fn script(body: &str) -> (tempfile::TempDir, PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("child.sh");
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    (dir, path)
}

#[tokio::test]
async fn a_job_runs_in_a_child_and_returns_the_filled_pdf() {
    let Some(lib) = pdfium() else {
        eprintln!("skipping: no PDFium");
        return;
    };
    let assignments = vec![Assignment {
        name: "Make".into(),
        value: Value::Text("EXAMPLE".into()),
    }];
    let done = process(
        pdfctl(),
        &lib,
        &bill_of_sale(),
        &assignments,
        Limits::default(),
        Duration::from_secs(60),
    )
    .await
    .unwrap_or_else(|e| panic!("child failed: {e}"));
    assert!(done.output.starts_with(b"%PDF-"));
    assert_eq!((done.filled, done.skipped, done.page_count), (1, 0, 1));
    assert!(done.field_count > 40);
}

#[tokio::test]
async fn a_failing_job_reports_its_code() {
    let Some(lib) = pdfium() else { return };
    let missing = vec![Assignment {
        name: "no.such.field".into(),
        value: Value::Text("x".into()),
    }];
    let err = process(
        pdfctl(),
        &lib,
        &bill_of_sale(),
        &missing,
        Limits::default(),
        Duration::from_secs(60),
    )
    .await
    .err()
    .unwrap();
    match err {
        ProcessError::Pdf(e) => assert_eq!(e.code, PdfErrorCode::FieldMissing),
        other => panic!("expected a PDF error, got {other}"),
    }
}

#[tokio::test]
async fn a_job_that_overruns_is_killed() {
    let Some(lib) = pdfium() else { return };
    let err = process(
        pdfctl(),
        &lib,
        &bill_of_sale(),
        &[],
        Limits::default(),
        Duration::from_millis(1),
    )
    .await
    .err()
    .unwrap();
    assert!(matches!(err, ProcessError::Timeout));
}

/// A child that never answers (a template that sends PDFium into a loop) is
/// killed at the deadline instead of being waited on.
#[tokio::test]
async fn a_hung_child_is_killed_at_the_deadline() {
    let (_dir, child) = script("sleep 30");
    let started = Instant::now();
    let err = process(
        &child,
        Path::new("/nonexistent"),
        b"%PDF-1.7",
        &[],
        Limits::default(),
        Duration::from_millis(300),
    )
    .await
    .err()
    .unwrap();
    assert!(matches!(err, ProcessError::Timeout));
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the parent waited {:?}",
        started.elapsed()
    );
}

/// A child that dies mid-job (PDFium segfaults, or the kernel's OOM killer)
/// ends that job with Crashed and leaves the parent running.
#[tokio::test]
async fn a_crashing_child_is_reported_as_crashed() {
    let (_dir, child) = script("kill -SEGV $$");
    let err = process(
        &child,
        Path::new("/nonexistent"),
        b"%PDF-1.7",
        &[],
        Limits::default(),
        Duration::from_secs(10),
    )
    .await
    .err()
    .unwrap();
    assert!(matches!(err, ProcessError::Crashed), "got {err}");

    let (_dir, garbage) = script("echo 'not json'");
    let err = process(
        &garbage,
        Path::new("/nonexistent"),
        b"%PDF-1.7",
        &[],
        Limits::default(),
        Duration::from_secs(10),
    )
    .await
    .err()
    .unwrap();
    assert!(matches!(err, ProcessError::Crashed), "got {err}");
}

/// The child starts with an empty environment plus the PDFium path, so it
/// cannot read the parent's secrets.
#[tokio::test]
async fn the_child_gets_a_clean_environment() {
    assert!(std::env::var_os("HOME").is_some() || std::env::var_os("PATH").is_some());
    let (_dir, child) = script(
        r#"if [ -z "$HOME" ] && [ -z "$CARGO" ] && [ -n "$PDFIUM_DYNAMIC_LIB_PATH" ] && [ -f "$2/input.pdf" ] && [ -f "$2/job.json" ]; then
  echo '{"status":"err","code":"PDF_INVALID_INPUT","detail":"clean"}'
else
  echo '{"status":"err","code":"PDF_QA_FAILED","detail":"leaked"}'
fi"#,
    );
    let err = process(
        &child,
        Path::new("/opt/pdfium/lib"),
        b"%PDF-1.7",
        &[],
        Limits::default(),
        Duration::from_secs(10),
    )
    .await
    .err()
    .unwrap();
    match err {
        ProcessError::Pdf(e) => assert_eq!(
            (e.code, e.detail.as_deref()),
            (PdfErrorCode::InvalidInput, Some("clean"))
        ),
        other => panic!("expected the script's answer, got {other}"),
    }
}
