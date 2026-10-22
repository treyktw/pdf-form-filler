//! Command-line tool over the PDF engine.
//!
//!   pdfctl inspect <file.pdf>
//!   pdfctl fill --form <file.pdf> --data <values.json> --out <out.pdf>
//!               [--skip-missing] [--skip-unsupported] [--isolate] [--timeout-ms N]
//!   pdfctl verify <file.pdf> [--data <values.json>] [--no-qpdf]
//!   pdfctl render <file.pdf> <page> <out.png> [--dpi N]
//!
//! `values.json` is either an object of field name -> value or the array form
//! `[{"name": ..., "value": ...}]`. Values are strings, booleans, string arrays
//! (list boxes) or null.
//!
//! `--isolate` runs the fill in a child process (this binary, started again
//! with `--process-job <dir>`) that is killed after `--timeout-ms`
//! (default 60000).
//!
//! PDFIUM_DYNAMIC_LIB_PATH points at libpdfium (or its directory); default
//! ./.pdfium/lib. QPDF_PATH overrides the qpdf binary for `verify`.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use pdf_form_filler::job_process::{self, ChildPolicies, ProcessError};
use pdf_form_filler::pdf::{self, Assignment, Engine, FillOptions, Limits, Policy, Value};
use pdf_form_filler::verify::{self, VerifyOptions};

const USAGE: &str = "usage:
  pdfctl inspect <file.pdf>
  pdfctl fill --form <file.pdf> --data <values.json> --out <out.pdf> [--skip-missing] [--skip-unsupported] [--isolate] [--timeout-ms N]
  pdfctl verify <file.pdf> [--data <values.json>] [--no-qpdf]
  pdfctl render <file.pdf> <page> <out.png> [--dpi N]";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // The child process speaks only its JSON outcome on stdout.
    if args.first().map(String::as_str) == Some("--process-job") {
        return match args.get(1) {
            Some(dir) => job_process::serve(Path::new(dir)),
            None => ExitCode::from(2),
        };
    }
    let result = match args.first().map(String::as_str) {
        Some("inspect") => inspect(&args[1..]),
        Some("fill") => fill(&args[1..]),
        Some("verify") => verify_cmd(&args[1..]),
        Some("render") => render(&args[1..]),
        _ => Err(USAGE.to_string()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

/// Splits `--flag value` options from positional arguments.
struct Args {
    positional: Vec<String>,
    options: Vec<(String, Option<String>)>,
}

impl Args {
    fn parse(raw: &[String], with_value: &[&str]) -> Result<Self, String> {
        let mut positional = Vec::new();
        let mut options = Vec::new();
        let mut iter = raw.iter();
        while let Some(arg) = iter.next() {
            if let Some(name) = arg.strip_prefix("--") {
                if with_value.contains(&name) {
                    let value = iter.next().ok_or_else(|| format!("--{name} needs a value\n{USAGE}"))?;
                    options.push((name.to_string(), Some(value.clone())));
                } else {
                    options.push((name.to_string(), None));
                }
            } else {
                positional.push(arg.clone());
            }
        }
        Ok(Self { positional, options })
    }

    fn value(&self, name: &str) -> Option<&str> {
        self.options
            .iter()
            .rev()
            .find(|(n, _)| n == name)
            .and_then(|(_, v)| v.as_deref())
    }

    fn flag(&self, name: &str) -> bool {
        self.options.iter().any(|(n, _)| n == name)
    }

    fn require(&self, name: &str) -> Result<&str, String> {
        self.value(name).ok_or_else(|| format!("--{name} is required\n{USAGE}"))
    }
}

fn engine() -> Result<Engine, String> {
    Engine::load(&pdf::default_library_path())
}

fn read(path: &str) -> Result<Vec<u8>, String> {
    std::fs::read(path).map_err(|e| format!("{path}: {e}"))
}

/// Reads values.json as either `{"field": value, ...}` (in file order) or
/// `[{"name": "field", "value": value}, ...]`.
fn read_assignments(path: &str) -> Result<Vec<Assignment>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    let json: serde_json::Value = serde_json::from_str(&text).map_err(|e| format!("{path}: {e}"))?;
    match json {
        serde_json::Value::Object(map) => map
            .into_iter()
            .filter(|(name, _)| !name.starts_with("$")) // "$comment" and friends
            .map(|(name, value)| {
                let value: Value = serde_json::from_value(value).map_err(|e| format!("{path}: {name}: {e}"))?;
                Ok(Assignment { name, value })
            })
            .collect(),
        array @ serde_json::Value::Array(_) => serde_json::from_value(array).map_err(|e| format!("{path}: {e}")),
        _ => Err(format!("{path}: expected an object or an array of assignments")),
    }
}

fn print_json(value: &impl serde::Serialize) {
    use std::io::Write;
    let text = serde_json::to_string_pretty(value).expect("reports serialise");
    // A closed pipe (`pdfctl inspect f.pdf | head`) is not an error worth a panic.
    let _ = writeln!(std::io::stdout().lock(), "{text}");
}

fn inspect(raw: &[String]) -> Result<(), String> {
    let args = Args::parse(raw, &[])?;
    let [file] = args.positional.as_slice() else {
        return Err(USAGE.into());
    };
    let engine = engine()?;
    let inspection = pdf::inspect(&engine, read(file)?, &Limits::default()).map_err(|e| e.to_string())?;
    print_json(&inspection);
    Ok(())
}

fn fill(raw: &[String]) -> Result<(), String> {
    let args = Args::parse(raw, &["form", "data", "out", "timeout-ms"])?;
    let form = args.require("form")?;
    let data = args.require("data")?;
    let out = args.require("out")?;
    let bytes = read(form)?;
    let assignments = read_assignments(data)?;
    let policy = |flag: &str| if args.flag(flag) { Policy::Skip } else { Policy::Error };
    let (missing, unsupported) = (policy("skip-missing"), policy("skip-unsupported"));

    if args.flag("isolate") || args.value("timeout-ms").is_some() {
        let timeout_ms: u64 = args
            .value("timeout-ms")
            .unwrap_or("60000")
            .parse()
            .map_err(|_| "--timeout-ms must be a whole number of milliseconds".to_string())?;
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let library = std::fs::canonicalize(pdf::default_library_path()).map_err(|e| format!("PDFium: {e}"))?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| e.to_string())?;
        let done = runtime
            .block_on(job_process::process_with(
                &exe,
                &library,
                &bytes,
                &assignments,
                Limits::default(),
                ChildPolicies { missing, unsupported },
                Duration::from_millis(timeout_ms),
            ))
            .map_err(|e| match e {
                ProcessError::Pdf(e) => e.to_string(),
                other => other.to_string(),
            })?;
        std::fs::write(out, &done.output).map_err(|e| format!("{out}: {e}"))?;
        print_json(&serde_json::json!({
            "isolated": true,
            "pageCount": done.page_count,
            "fieldCount": done.field_count,
            "filled": done.filled,
            "skipped": done.skipped,
            "outputBytes": done.output.len(),
            "readBackVerified": true,
            "pagesRendered": done.page_count,
        }));
        return Ok(());
    }

    let engine = engine()?;
    let options = FillOptions {
        missing,
        unsupported,
        ..FillOptions::default()
    };
    let (output, report) = pdf::fill(&engine, bytes, &assignments, &options).map_err(|e| e.to_string())?;
    std::fs::write(out, output).map_err(|e| format!("{out}: {e}"))?;
    print_json(&report);
    Ok(())
}

fn verify_cmd(raw: &[String]) -> Result<(), String> {
    let args = Args::parse(raw, &["data"])?;
    let [file] = args.positional.as_slice() else {
        return Err(USAGE.into());
    };
    let assignments = args.value("data").map(read_assignments).transpose()?;
    let mut options = VerifyOptions::default();
    if args.flag("no-qpdf") {
        options.qpdf = None;
    }
    let engine = engine()?;
    let report = verify::verify(&engine, &read(file)?, assignments.as_deref(), &options).map_err(|e| e.to_string())?;
    print_json(&report);
    Ok(())
}

fn render(raw: &[String]) -> Result<(), String> {
    let args = Args::parse(raw, &["dpi"])?;
    let [file, page, out] = args.positional.as_slice() else {
        return Err(USAGE.into());
    };
    let dpi: f32 = args
        .value("dpi")
        .unwrap_or("110")
        .parse()
        .map_err(|_| "--dpi must be a number".to_string())?;
    let page: usize = page.parse::<usize>().map_err(|e| e.to_string())?;
    let engine = engine()?;
    let doc = engine.open(read(file)?).map_err(|e| e.to_string())?;
    if page == 0 || page > doc.page_count() {
        return Err(format!("page {page} is out of range (1-{})", doc.page_count()));
    }
    let (w, h, rgba) = doc.render_rgba(page - 1, dpi).map_err(|e| e.to_string())?;
    image::save_buffer(PathBuf::from(out), &rgba, w, h, image::ExtendedColorType::Rgba8).map_err(|e| e.to_string())
}
