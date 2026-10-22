//! PDF form filling on PDFium.

pub mod engine;
pub mod error;
pub mod fill;
pub mod xfa;

pub use engine::{Engine, default_library_path};
pub use error::{PdfError, PdfErrorCode, PdfResult};
pub use fill::{
    AppearanceCheck, Assignment, FillOptions, FillReport, Inspection, Limits, Policy, Value, check_appearances, fill,
    inspect, verify_readback,
};
