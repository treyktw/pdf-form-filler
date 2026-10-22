//! Post-fill checks on a saved file, the gate a filled PDF passes before it
//! leaves the process:
//!
//! 1. read-back: every assigned field re-read from the saved bytes (when the
//!    assignments are known; `pdf::fill` always does this itself),
//! 2. appearances: every filled text or choice widget has a normal appearance
//!    stream, so viewers that do not regenerate them still show the value,
//! 3. structure: `qpdf --check` on the bytes, bounded by a timeout,
//! 4. render: every page drawn with its form widgets, as a viewer would.
//!
//! In the original service, steps 1, 2 and 4 ran in the job's child process
//! and step 3 in the parent after the child returned.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::job_process::write_private;
use crate::pdf::{self, Assignment, Engine, Limits, PdfError, PdfErrorCode, PdfResult};

/// Resolution for the render check. High enough to exercise real rasterising,
/// low enough to stay fast on 50-page files.
pub const RENDER_DPI: f32 = 110.0;
const QPDF_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone)]
pub struct VerifyOptions {
    pub limits: Limits,
    /// `qpdf` binary; `None` skips the structure check (reported as skipped).
    pub qpdf: Option<PathBuf>,
}

impl Default for VerifyOptions {
    fn default() -> Self {
        Self {
            limits: Limits::default(),
            qpdf: Some(
                std::env::var_os("QPDF_PATH")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| "qpdf".into()),
            ),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Structure {
    Passed,
    Skipped,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifyReport {
    pub page_count: usize,
    pub pages_rendered: usize,
    pub field_count: usize,
    pub fields_with_values: usize,
    pub widgets_with_appearance: usize,
    /// Number of assignments re-read and matched, when assignments were given.
    pub read_back_verified: Option<usize>,
    pub structure: Structure,
}

pub fn verify(
    engine: &Engine,
    bytes: &[u8],
    assignments: Option<&[Assignment]>,
    options: &VerifyOptions,
) -> PdfResult<VerifyReport> {
    let read_back_verified = match assignments {
        Some(a) => {
            pdf::verify_readback(engine, bytes, a, &options.limits)?;
            Some(a.len())
        }
        None => None,
    };
    let appearances = pdf::check_appearances(engine, bytes.to_vec(), &options.limits)?;
    let structure = match &options.qpdf {
        Some(qpdf) => {
            qpdf_check(qpdf, bytes)?;
            Structure::Passed
        }
        None => Structure::Skipped,
    };
    let doc = engine.open(bytes.to_vec())?;
    for page in 0..doc.page_count() {
        doc.render_page(page, RENDER_DPI)?;
    }
    Ok(VerifyReport {
        page_count: appearances.page_count,
        pages_rendered: doc.page_count(),
        field_count: appearances.field_count,
        fields_with_values: appearances.fields_with_values,
        widgets_with_appearance: appearances.widgets_checked,
        read_back_verified,
        structure,
    })
}

/// `qpdf --check` on the bytes, in a private temporary directory, killed if it
/// runs past a minute. Only a clean exit (0) passes: qpdf exits 3 when it had
/// to recover from damage, and a freshly saved fill should have none.
pub fn qpdf_check(qpdf: &Path, bytes: &[u8]) -> PdfResult<()> {
    let failed = |why: &str| {
        PdfError::with(
            PdfErrorCode::QaFailed,
            "PDF failed the structure check",
            why.to_string(),
        )
    };
    let dir = tempfile::Builder::new()
        .prefix("pdf-check-")
        .tempdir()
        .map_err(|_| failed("temporary directory"))?;
    let path = dir.path().join("output.pdf");
    write_private(&path, bytes).map_err(|_| failed("temporary file"))?;
    let mut child = Command::new(qpdf)
        .arg("--check")
        .arg(&path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| failed("qpdf could not be started"))?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return if status.success() {
                    Ok(())
                } else {
                    Err(failed("qpdf --check reported problems"))
                };
            }
            Ok(None) if started.elapsed() > QPDF_TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(failed("qpdf --check timed out"));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(_) => return Err(failed("qpdf --check could not be awaited")),
        }
    }
}
