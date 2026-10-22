//! Fill AcroForms with PDFium, verify what was written, and run each job in a
//! child process that is killed if it overruns.
//!
//! - [`pdf`]: the engine (PDFium form-fill environment), fill and inspect with
//!   read-back verification, and hybrid-XFA stripping.
//! - [`job_process`]: the child-process boundary with a kill timeout.
//! - [`verify`]: post-fill checks on a saved file (`qpdf --check`, every page
//!   rendered, every field's appearance).
//! - [`signing`] (feature `signing`): HMAC-SHA256 request signing, for a
//!   service that hands jobs to this library over HTTP.

pub mod job_process;
pub mod pdf;
#[cfg(feature = "signing")]
pub mod signing;
pub mod verify;
