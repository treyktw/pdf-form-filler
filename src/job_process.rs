//! Each job's PDF work runs in a child process: `pdfctl` (or any binary that
//! calls [`serve`]) started again with `--process-job <dir>`.
//!
//! PDFium calls cannot be interrupted, and a hostile or broken template can
//! keep one busy (or exhaust memory, or crash it) indefinitely. In a thread,
//! a timeout would only stop waiting while PDFium kept running. In a child,
//! the parent kills the process on timeout, and a crash or an out-of-memory
//! kill ends that job and nothing else. The child also gets a clean
//! environment, so it never holds the parent's secrets.
//!
//! Protocol: the parent writes `input.pdf` and `job.json` into a private
//! temporary directory. The child writes `output.pdf` there and prints one
//! JSON outcome on stdout.

use std::path::Path;
use std::process::{ExitCode, Stdio};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::pdf::{self, Assignment, Engine, FillOptions, Limits, PdfError, PdfErrorCode, Policy};

#[derive(Debug, Serialize, Deserialize)]
pub struct ChildJob {
    pub assignments: Vec<Assignment>,
    pub limits: Limits,
    #[serde(default)]
    pub missing: Policy,
    #[serde(default)]
    pub unsupported: Policy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum ChildOutcome {
    Ok {
        page_count: usize,
        field_count: usize,
        filled: usize,
        skipped: usize,
    },
    Err {
        code: String,
        detail: Option<String>,
    },
}

/// The parent's view of a finished child: the filled PDF and its counts.
pub struct Processed {
    pub output: Vec<u8>,
    pub page_count: usize,
    pub field_count: usize,
    pub filled: usize,
    pub skipped: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum ProcessError {
    #[error("{0}")]
    Pdf(PdfError),
    #[error("PDF processing timed out")]
    Timeout,
    #[error("PDF processing stopped without an answer")]
    Crashed,
    #[error("PDF processing could not start")]
    Spawn,
}

/// What the child does when an assignment names a missing field or a field
/// it cannot fill (both `Policy::Error` by default).
#[derive(Debug, Clone, Copy, Default)]
pub struct ChildPolicies {
    pub missing: Policy,
    pub unsupported: Policy,
}

/// Runs one job's PDF work in a child process, killed if it overruns. `exe`
/// is a binary whose main hands `--process-job <dir>` to [`serve`] (pdfctl
/// does; a service would pass std::env::current_exe).
pub async fn process(
    exe: &Path,
    pdfium_path: &Path,
    input: &[u8],
    assignments: &[Assignment],
    limits: Limits,
    timeout: Duration,
) -> Result<Processed, ProcessError> {
    process_with(
        exe,
        pdfium_path,
        input,
        assignments,
        limits,
        ChildPolicies::default(),
        timeout,
    )
    .await
}

/// [`process`] with explicit missing/unsupported field policies.
pub async fn process_with(
    exe: &Path,
    pdfium_path: &Path,
    input: &[u8],
    assignments: &[Assignment],
    limits: Limits,
    policies: ChildPolicies,
    timeout: Duration,
) -> Result<Processed, ProcessError> {
    let dir = tempfile::Builder::new()
        .prefix("pdf-job-")
        .tempdir()
        .map_err(|_| ProcessError::Spawn)?;
    write_private(&dir.path().join("input.pdf"), input).map_err(|_| ProcessError::Spawn)?;
    let job = serde_json::to_vec(&ChildJob {
        assignments: assignments.to_vec(),
        limits,
        missing: policies.missing,
        unsupported: policies.unsupported,
    })
    .map_err(|_| ProcessError::Spawn)?;
    write_private(&dir.path().join("job.json"), &job).map_err(|_| ProcessError::Spawn)?;

    let child = tokio::process::Command::new(exe)
        .arg("--process-job")
        .arg(dir.path())
        .env_clear()
        .env("PDFIUM_DYNAMIC_LIB_PATH", pdfium_path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| ProcessError::Spawn)?;

    // Dropping the child on timeout kills it (kill_on_drop).
    let out = tokio::time::timeout(timeout, child.wait_with_output())
        .await
        .map_err(|_| ProcessError::Timeout)?
        .map_err(|_| ProcessError::Crashed)?;
    let outcome: ChildOutcome = serde_json::from_slice(&out.stdout).map_err(|_| ProcessError::Crashed)?;
    match outcome {
        ChildOutcome::Ok {
            page_count,
            field_count,
            filled,
            skipped,
        } => {
            let output = std::fs::read(dir.path().join("output.pdf")).map_err(|_| ProcessError::Crashed)?;
            Ok(Processed {
                output,
                page_count,
                field_count,
                filled,
                skipped,
            })
        }
        ChildOutcome::Err { code, detail } => Err(ProcessError::Pdf(PdfError {
            code: PdfErrorCode::parse(&code).unwrap_or(PdfErrorCode::QaFailed),
            message: "PDF processing failed",
            detail,
        })),
    }
}

/// The child: fill, then render every page, then report.
pub fn serve(dir: &Path) -> ExitCode {
    let outcome = match run_child(dir) {
        Ok(o) => o,
        Err(e) => ChildOutcome::Err {
            code: e.code.as_str().to_string(),
            detail: e.detail,
        },
    };
    match serde_json::to_string(&outcome) {
        Ok(line) => {
            println!("{line}");
            ExitCode::SUCCESS
        }
        Err(_) => ExitCode::FAILURE,
    }
}

fn run_child(dir: &Path) -> Result<ChildOutcome, PdfError> {
    let setup = |_| PdfError::new(PdfErrorCode::InvalidInput, "PDF job could not be read");
    let input = std::fs::read(dir.join("input.pdf")).map_err(setup)?;
    let job: ChildJob = serde_json::from_slice(&std::fs::read(dir.join("job.json")).map_err(setup)?)
        .map_err(|_| PdfError::new(PdfErrorCode::InvalidInput, "PDF job could not be read"))?;
    let engine = Engine::load(&pdf::default_library_path())
        .map_err(|_| PdfError::new(PdfErrorCode::QaFailed, "PDFium could not be loaded"))?;

    let options = FillOptions {
        limits: job.limits,
        missing: job.missing,
        unsupported: job.unsupported,
    };
    let (output, report) = pdf::fill(&engine, input, &job.assignments, &options)?;
    {
        let doc = engine.open(output.clone())?;
        for page in 0..doc.page_count() {
            doc.render_page(page, 110.0)?;
        }
    }
    write_private(&dir.join("output.pdf"), &output)
        .map_err(|_| PdfError::new(PdfErrorCode::QaFailed, "PDF output could not be written"))?;
    Ok(ChildOutcome::Ok {
        page_count: report.page_count,
        field_count: report.field_count,
        filled: report.filled.len(),
        skipped: report.skipped.len(),
    })
}

pub fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)?.write_all(bytes)
}
