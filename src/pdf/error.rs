//! Failure codes, the same set the original TypeScript (pdf-lib) filler
//! reported, so callers and dashboards see one vocabulary whichever engine ran.
//! Details carry field names and counts only, never field values.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PdfErrorCode {
    InvalidInput,
    LimitExceeded,
    EncryptedUnsupported,
    XfaUnsupported,
    FieldMissing,
    FieldUnsupported,
    FieldValueInvalid,
    ReadbackFailed,
    /// Post-fill checks (structure, render, appearances). Not in the
    /// TypeScript set: the pdf-lib filler reported these as a generic failure.
    QaFailed,
}

impl PdfErrorCode {
    const ALL: [Self; 9] = [
        Self::InvalidInput,
        Self::LimitExceeded,
        Self::EncryptedUnsupported,
        Self::XfaUnsupported,
        Self::FieldMissing,
        Self::FieldUnsupported,
        Self::FieldValueInvalid,
        Self::ReadbackFailed,
        Self::QaFailed,
    ];

    pub fn parse(code: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.as_str() == code)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidInput => "PDF_INVALID_INPUT",
            Self::LimitExceeded => "PDF_LIMIT_EXCEEDED",
            Self::EncryptedUnsupported => "PDF_ENCRYPTED_UNSUPPORTED",
            Self::XfaUnsupported => "PDF_XFA_UNSUPPORTED",
            Self::FieldMissing => "PDF_FIELD_MISSING",
            Self::FieldUnsupported => "PDF_FIELD_UNSUPPORTED",
            Self::FieldValueInvalid => "PDF_FIELD_VALUE_INVALID",
            Self::ReadbackFailed => "PDF_READBACK_FAILED",
            Self::QaFailed => "PDF_QA_FAILED",
        }
    }
}

#[derive(Debug, Clone)]
pub struct PdfError {
    pub code: PdfErrorCode,
    pub message: &'static str,
    /// Safe to log: field names, limits, counts.
    pub detail: Option<String>,
}

impl PdfError {
    pub fn new(code: PdfErrorCode, message: &'static str) -> Self {
        Self {
            code,
            message,
            detail: None,
        }
    }

    pub fn with(code: PdfErrorCode, message: &'static str, detail: impl Into<String>) -> Self {
        Self {
            code,
            message,
            detail: Some(detail.into()),
        }
    }
}

impl fmt::Display for PdfError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.detail {
            Some(d) => write!(f, "{}: {} ({d})", self.code.as_str(), self.message),
            None => write!(f, "{}: {}", self.code.as_str(), self.message),
        }
    }
}

impl std::error::Error for PdfError {}

pub type PdfResult<T> = Result<T, PdfError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_code_round_trips_through_its_wire_name() {
        for code in PdfErrorCode::ALL {
            assert_eq!(PdfErrorCode::parse(code.as_str()), Some(code));
        }
        assert_eq!(PdfErrorCode::parse("PDF_SOMETHING_ELSE"), None);
    }

    #[test]
    fn display_carries_the_detail_but_nothing_else() {
        let e = PdfError::with(
            PdfErrorCode::FieldMissing,
            "Mapped PDF field does not exist",
            "Owner1_0",
        );
        assert_eq!(
            e.to_string(),
            "PDF_FIELD_MISSING: Mapped PDF field does not exist (Owner1_0)"
        );
        let bare = PdfError::new(PdfErrorCode::InvalidInput, "Input is not a PDF document");
        assert_eq!(bare.to_string(), "PDF_INVALID_INPUT: Input is not a PDF document");
    }
}
