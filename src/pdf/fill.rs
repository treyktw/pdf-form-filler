//! Filling and inspecting AcroForms, with the rules of the original
//! TypeScript (pdf-lib) filler: limits checked before parsing, unique
//! assignments, missing and unsupported fields fail unless a policy says skip,
//! the same value coercions, and a read-back of every filled field from the
//! saved bytes. On top of that, every widget of a filled text or choice field
//! must carry a normal appearance, so what renders is what was written.
//!
//! Reports carry field names, types and counts. Never values.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::engine::{
    Document, Engine, FIELD_CHECKBOX, FIELD_COMBOBOX, FIELD_LISTBOX, FIELD_PUSHBUTTON, FIELD_RADIOBUTTON,
    FIELD_SIGNATURE, FIELD_TEXTFIELD, FLAG_COMBO_EDIT, FLAG_READONLY, FLAG_REQUIRED, HYBRID_XFA, Widget,
};
use super::error::{PdfError, PdfErrorCode, PdfResult};

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Limits {
    pub max_input_bytes: usize,
    pub max_output_bytes: usize,
    pub max_pages: usize,
    pub max_fields: usize,
    pub max_assignments: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_input_bytes: 25 * 1024 * 1024,
            max_output_bytes: 25 * 1024 * 1024,
            max_pages: 50,
            max_fields: 500,
            max_assignments: 500,
        }
    }
}

/// An assignment value: a string, a boolean, a list of strings, or null.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Value {
    Bool(bool),
    Text(String),
    List(Vec<String>),
    Null,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Assignment {
    pub name: String,
    #[serde(default = "null_value")]
    pub value: Value,
}

fn null_value() -> Value {
    Value::Null
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Policy {
    #[default]
    Error,
    Skip,
}

#[derive(Debug, Clone, Default)]
pub struct FillOptions {
    pub limits: Limits,
    pub missing: Policy,
    pub unsupported: Policy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum FieldType {
    Text,
    Checkbox,
    Dropdown,
    OptionList,
    Radio,
    Signature,
    Button,
    Unknown,
}

impl FieldType {
    fn of(kind: i32) -> Self {
        match kind {
            FIELD_TEXTFIELD => Self::Text,
            FIELD_CHECKBOX => Self::Checkbox,
            FIELD_COMBOBOX => Self::Dropdown,
            FIELD_LISTBOX => Self::OptionList,
            FIELD_RADIOBUTTON => Self::Radio,
            FIELD_SIGNATURE => Self::Signature,
            FIELD_PUSHBUTTON => Self::Button,
            _ => Self::Unknown,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldSummary {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: FieldType,
    pub widget_count: usize,
    pub widgets_with_normal_appearance: usize,
    pub read_only: bool,
    pub required: bool,
    /// Radio export values, or a choice field's option labels.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Inspection {
    pub byte_length: usize,
    pub page_count: usize,
    pub field_count: usize,
    pub fields: Vec<FieldSummary>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Filled {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: FieldType,
    pub widget_count: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Skipped {
    pub name: String,
    pub reason: &'static str,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FillReport {
    pub page_count: usize,
    pub field_count: usize,
    pub filled: Vec<Filled>,
    pub skipped: Vec<Skipped>,
    pub output_bytes: usize,
    pub editable: bool,
    pub read_back_verified: bool,
}

/// A form field: its widgets, grouped by fully qualified name, in the order
/// they first appear.
struct Field {
    name: String,
    widgets: Vec<Widget>,
}

impl Field {
    fn kind(&self) -> i32 {
        self.widgets[0].kind
    }
    fn flags(&self) -> i32 {
        self.widgets[0].flags
    }
}

fn fields_of(doc: &Document<'_>) -> Vec<Field> {
    let mut order: Vec<String> = Vec::new();
    let mut by_name: BTreeMap<String, Vec<Widget>> = BTreeMap::new();
    for w in doc.widgets() {
        if !by_name.contains_key(&w.name) {
            order.push(w.name.clone());
        }
        by_name.entry(w.name.clone()).or_default().push(w);
    }
    order
        .into_iter()
        .map(|name| {
            let widgets = by_name.remove(&name).unwrap_or_default();
            Field { name, widgets }
        })
        .collect()
}

fn check_input(bytes: &[u8], limits: &Limits) -> PdfResult<()> {
    if bytes.len() < 5 || &bytes[..5] != b"%PDF-" {
        return Err(PdfError::new(PdfErrorCode::InvalidInput, "Input is not a PDF document"));
    }
    within("input byte length", bytes.len(), limits.max_input_bytes)
}

fn within(label: &'static str, actual: usize, maximum: usize) -> PdfResult<()> {
    if actual > maximum {
        return Err(PdfError::with(
            PdfErrorCode::LimitExceeded,
            "PDF exceeds a configured limit",
            format!("{label}: {actual} > {maximum}"),
        ));
    }
    Ok(())
}

fn open_checked<'e>(engine: &'e Engine, bytes: Vec<u8>, limits: &Limits) -> PdfResult<(Document<'e>, Vec<Field>)> {
    let doc = match engine.open(bytes.clone()) {
        Err(e) if e.code == PdfErrorCode::XfaUnsupported && e.detail.as_deref() == Some(HYBRID_XFA) => {
            // lopdf rewrites the file; the result is held to the input limit too.
            let stripped = super::xfa::strip_xfa(&bytes)?;
            within("input byte length", stripped.len(), limits.max_input_bytes)?;
            engine.open(stripped)?
        }
        other => other?,
    };
    within("page count", doc.page_count(), limits.max_pages)?;
    let fields = fields_of(&doc);
    within("field count", fields.len(), limits.max_fields)?;
    Ok((doc, fields))
}

pub fn inspect(engine: &Engine, bytes: Vec<u8>, limits: &Limits) -> PdfResult<Inspection> {
    check_input(&bytes, limits)?;
    let byte_length = bytes.len();
    let (doc, fields) = open_checked(engine, bytes, limits)?;
    Ok(Inspection {
        byte_length,
        page_count: doc.page_count(),
        field_count: fields.len(),
        fields: fields
            .iter()
            .map(|f| FieldSummary {
                name: f.name.clone(),
                kind: FieldType::of(f.kind()),
                widget_count: f.widgets.len(),
                widgets_with_normal_appearance: f.widgets.iter().filter(|w| doc.has_normal_appearance(w)).count(),
                read_only: f.flags() & FLAG_READONLY != 0,
                required: f.flags() & FLAG_REQUIRED != 0,
                options: match f.kind() {
                    FIELD_RADIOBUTTON => {
                        let mut v: Vec<String> = Vec::new();
                        for w in &f.widgets {
                            let e = doc.export_value(w);
                            if !v.contains(&e) {
                                v.push(e);
                            }
                        }
                        v
                    }
                    FIELD_COMBOBOX | FIELD_LISTBOX => doc.options(&f.widgets[0]).into_iter().map(|(l, _)| l).collect(),
                    _ => Vec::new(),
                },
            })
            .collect(),
    })
}

/// Fills the form and returns the saved bytes with a report.
pub fn fill(
    engine: &Engine,
    bytes: Vec<u8>,
    assignments: &[Assignment],
    options: &FillOptions,
) -> PdfResult<(Vec<u8>, FillReport)> {
    let limits = &options.limits;
    check_input(&bytes, limits)?;
    within("assignment count", assignments.len(), limits.max_assignments)?;
    unique(assignments)?;

    let (doc, fields) = open_checked(engine, bytes, limits)?;
    let page_count = doc.page_count();
    let field_count = fields.len();
    let mut filled = Vec::new();
    let mut skipped = Vec::new();

    for a in assignments {
        let Some(field) = fields.iter().find(|f| f.name == a.name) else {
            if options.missing == Policy::Skip {
                skipped.push(Skipped {
                    name: a.name.clone(),
                    reason: "missing-field",
                });
                continue;
            }
            return Err(PdfError::with(
                PdfErrorCode::FieldMissing,
                "Mapped PDF field does not exist",
                &a.name,
            ));
        };
        match apply(&doc, field, &a.value) {
            Ok(()) => filled.push(Filled {
                name: field.name.clone(),
                kind: FieldType::of(field.kind()),
                widget_count: field.widgets.len(),
            }),
            Err(e) if e.code == PdfErrorCode::FieldUnsupported && options.unsupported == Policy::Skip => {
                skipped.push(Skipped {
                    name: a.name.clone(),
                    reason: "unsupported-field",
                });
            }
            Err(e) => return Err(e),
        }
    }

    let output = doc.save()?;
    drop(doc);
    within("output byte length", output.len(), limits.max_output_bytes)?;
    verify(engine, &output, assignments, &skipped, limits)?;

    Ok((
        output.clone(),
        FillReport {
            page_count,
            field_count,
            filled,
            skipped,
            output_bytes: output.len(),
            editable: true,
            read_back_verified: true,
        },
    ))
}

fn unique(assignments: &[Assignment]) -> PdfResult<()> {
    let mut seen = std::collections::HashSet::new();
    for a in assignments {
        if a.name.trim().is_empty() {
            return Err(PdfError::new(
                PdfErrorCode::InvalidInput,
                "PDF assignment name cannot be empty",
            ));
        }
        if !seen.insert(a.name.as_str()) {
            return Err(PdfError::with(
                PdfErrorCode::InvalidInput,
                "PDF assignment names must be unique",
                &a.name,
            ));
        }
    }
    Ok(())
}

fn invalid(name: &str, expected: &str) -> PdfError {
    PdfError::with(
        PdfErrorCode::FieldValueInvalid,
        "PDF assignment has an invalid value type",
        format!("{name}: expected {expected}"),
    )
}

fn as_string(name: &str, v: &Value) -> PdfResult<String> {
    match v {
        Value::Null => Ok(String::new()),
        Value::Text(s) => Ok(s.clone()),
        _ => Err(invalid(name, "string")),
    }
}

fn as_selections(name: &str, v: &Value) -> PdfResult<Vec<String>> {
    match v {
        Value::Null => Ok(Vec::new()),
        Value::Text(s) => Ok(vec![s.clone()]),
        Value::List(l) => Ok(l.clone()),
        Value::Bool(_) => Err(invalid(name, "string or string array")),
    }
}

fn as_bool(name: &str, v: &Value) -> PdfResult<bool> {
    match v {
        Value::Bool(b) => Ok(*b),
        Value::Null => Ok(false),
        Value::Text(s) => match s.trim().to_lowercase().as_str() {
            "true" | "yes" | "1" | "on" | "checked" => Ok(true),
            "false" | "no" | "0" | "off" | "" | "unchecked" => Ok(false),
            _ => Err(invalid(name, "boolean-compatible value")),
        },
        Value::List(_) => Err(invalid(name, "boolean-compatible value")),
    }
}

fn unsupported(field: &Field, why: &str) -> PdfError {
    PdfError::with(
        PdfErrorCode::FieldUnsupported,
        "PDF field type is not fillable",
        format!("{}: {why}", field.name),
    )
}

fn apply(doc: &Document<'_>, field: &Field, value: &Value) -> PdfResult<()> {
    let first = &field.widgets[0];
    match field.kind() {
        FIELD_TEXTFIELD => {
            let text = as_string(&field.name, value)?;
            if !doc.set_text(first, &text) {
                return Err(unsupported(field, "the field does not accept input"));
            }
        }
        FIELD_CHECKBOX => {
            let want = as_bool(&field.name, value)?;
            if doc.is_checked(first) != want && !doc.press(first) {
                return Err(unsupported(field, "the checkbox does not accept input"));
            }
        }
        FIELD_RADIOBUTTON => {
            let target = match value {
                Value::Null => None,
                Value::Text(s) if s.is_empty() => None,
                other => Some(as_string(&field.name, other)?),
            };
            match target {
                None => {
                    // Clearing a radio group is not something a form filler
                    // can do. Allowed only when nothing is selected already.
                    if field.widgets.iter().any(|w| doc.is_checked(w)) {
                        return Err(unsupported(field, "a selected radio group cannot be cleared"));
                    }
                }
                Some(t) => {
                    let Some(w) = field.widgets.iter().find(|w| doc.export_value(w) == t) else {
                        return Err(invalid(&field.name, "one of the radio group's options"));
                    };
                    if !doc.is_checked(w) && !doc.press(w) {
                        return Err(unsupported(field, "the radio button does not accept input"));
                    }
                }
            }
        }
        FIELD_COMBOBOX | FIELD_LISTBOX => {
            let wanted = as_selections(&field.name, value)?;
            let options = doc.options(first);
            let mut indices = Vec::new();
            for want in &wanted {
                match options.iter().position(|(label, _)| label == want) {
                    Some(i) => indices.push(i as i32),
                    None if field.kind() == FIELD_COMBOBOX
                        && field.flags() & FLAG_COMBO_EDIT != 0
                        && wanted.len() == 1 =>
                    {
                        // An editable combo box takes free text.
                        if !doc.set_text(first, want) {
                            return Err(unsupported(field, "the field does not accept input"));
                        }
                        return Ok(());
                    }
                    None => return Err(invalid(&field.name, "one of the field's options")),
                }
            }
            if !doc.select(first, &indices) {
                return Err(unsupported(field, "the field does not accept a selection"));
            }
        }
        _ => return Err(unsupported(field, "signatures and buttons are not filled")),
    }
    Ok(())
}

/// Re-opens the saved bytes and checks every filled field reads back as
/// assigned, and every widget of a filled field draws something.
pub fn verify_readback(engine: &Engine, output: &[u8], assignments: &[Assignment], limits: &Limits) -> PdfResult<()> {
    verify(engine, output, assignments, &[], limits)
}

/// What [`check_appearances`] found. Counts only, never values.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppearanceCheck {
    pub page_count: usize,
    pub field_count: usize,
    /// Text and choice fields holding a value, plus checked boxes and radios.
    pub fields_with_values: usize,
    pub widgets_checked: usize,
}

/// For a filled file whose assignments are not at hand: every text or choice
/// field that holds a value must have a normal appearance on every widget,
/// so a viewer that does not regenerate appearances still shows the value.
pub fn check_appearances(engine: &Engine, bytes: Vec<u8>, limits: &Limits) -> PdfResult<AppearanceCheck> {
    check_input(&bytes, limits)?;
    let (doc, fields) = open_checked(engine, bytes, limits)?;
    let mut fields_with_values = 0;
    let mut widgets_checked = 0;
    for field in &fields {
        let first = &field.widgets[0];
        let has_value = match field.kind() {
            FIELD_TEXTFIELD | FIELD_COMBOBOX | FIELD_LISTBOX => !doc.value(first).is_empty(),
            FIELD_CHECKBOX | FIELD_RADIOBUTTON => field.widgets.iter().any(|w| doc.is_checked(w)),
            _ => false,
        };
        if !has_value {
            continue;
        }
        fields_with_values += 1;
        if matches!(field.kind(), FIELD_TEXTFIELD | FIELD_COMBOBOX | FIELD_LISTBOX) {
            for w in &field.widgets {
                widgets_checked += 1;
                if !doc.has_normal_appearance(w) {
                    return Err(PdfError::with(
                        PdfErrorCode::QaFailed,
                        "PDF field has a widget without an appearance",
                        &field.name,
                    ));
                }
            }
        }
    }
    Ok(AppearanceCheck {
        page_count: doc.page_count(),
        field_count: fields.len(),
        fields_with_values,
        widgets_checked,
    })
}

fn verify(
    engine: &Engine,
    output: &[u8],
    assignments: &[Assignment],
    skipped: &[Skipped],
    limits: &Limits,
) -> PdfResult<()> {
    let (doc, fields) = open_checked(engine, output.to_vec(), limits)?;
    for a in assignments {
        if skipped.iter().any(|s| s.name == a.name) {
            continue;
        }
        let failed = || {
            PdfError::with(
                PdfErrorCode::ReadbackFailed,
                "PDF field failed post-save read-back",
                &a.name,
            )
        };
        let field = fields.iter().find(|f| f.name == a.name).ok_or_else(failed)?;
        let first = &field.widgets[0];
        let ok = match field.kind() {
            FIELD_TEXTFIELD => doc.value(first) == as_string(&field.name, &a.value)?,
            FIELD_CHECKBOX => doc.is_checked(first) == as_bool(&field.name, &a.value)?,
            FIELD_RADIOBUTTON => {
                let checked = field
                    .widgets
                    .iter()
                    .find(|w| doc.is_checked(w))
                    .map(|w| doc.export_value(w));
                match &a.value {
                    Value::Null => checked.is_none(),
                    Value::Text(s) if s.is_empty() => checked.is_none(),
                    v => checked == Some(as_string(&field.name, v)?),
                }
            }
            FIELD_COMBOBOX | FIELD_LISTBOX => {
                let want = as_selections(&field.name, &a.value)?;
                let selected: Vec<String> = doc
                    .options(first)
                    .into_iter()
                    .filter(|(_, s)| *s)
                    .map(|(l, _)| l)
                    .collect();
                // A free-text entry in an editable combo box is its value,
                // not one of its options.
                selected == want || (field.kind() == FIELD_COMBOBOX && want.len() == 1 && doc.value(first) == want[0])
            }
            _ => false,
        };
        if !ok {
            return Err(failed());
        }
        if matches!(field.kind(), FIELD_TEXTFIELD | FIELD_COMBOBOX | FIELD_LISTBOX)
            && !field.widgets.iter().all(|w| doc.has_normal_appearance(w))
        {
            return Err(PdfError::with(
                PdfErrorCode::QaFailed,
                "PDF field has a widget without an appearance",
                &a.name,
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_deserialise_from_plain_json() {
        let parsed: Vec<Assignment> = serde_json::from_str(
            r#"[{"name":"a","value":"x"},{"name":"b","value":true},{"name":"c","value":["1","2"]},{"name":"d","value":null},{"name":"e"}]"#,
        )
        .unwrap();
        let values: Vec<Value> = parsed.into_iter().map(|a| a.value).collect();
        assert_eq!(
            values,
            [
                Value::Text("x".into()),
                Value::Bool(true),
                Value::List(vec!["1".into(), "2".into()]),
                Value::Null,
                Value::Null,
            ]
        );
    }

    #[test]
    fn checkbox_coercion_accepts_the_usual_spellings_and_nothing_else() {
        for yes in ["true", "Yes", " on ", "1", "CHECKED"] {
            assert!(as_bool("f", &Value::Text(yes.into())).unwrap(), "{yes}");
        }
        for no in ["false", "no", "off", "0", "", "unchecked"] {
            assert!(!as_bool("f", &Value::Text(no.into())).unwrap(), "{no}");
        }
        assert!(!as_bool("f", &Value::Null).unwrap());
        let err = as_bool("f", &Value::Text("maybe".into())).unwrap_err();
        assert_eq!(err.code, PdfErrorCode::FieldValueInvalid);
        // The detail names the field and the expectation, never the value.
        assert!(!err.to_string().contains("maybe"));
    }

    #[test]
    fn text_and_selection_coercion() {
        assert_eq!(as_string("f", &Value::Null).unwrap(), "");
        assert!(as_string("f", &Value::Bool(true)).is_err());
        assert_eq!(as_selections("f", &Value::Text("a".into())).unwrap(), ["a"]);
        assert!(as_selections("f", &Value::Null).unwrap().is_empty());
        assert!(as_selections("f", &Value::Bool(false)).is_err());
    }

    #[test]
    fn assignment_names_must_be_present_and_unique() {
        let a = |n: &str| Assignment {
            name: n.into(),
            value: Value::Null,
        };
        assert!(unique(&[a("x"), a("y")]).is_ok());
        assert_eq!(unique(&[a("x"), a("x")]).unwrap_err().code, PdfErrorCode::InvalidInput);
        assert_eq!(unique(&[a("  ")]).unwrap_err().code, PdfErrorCode::InvalidInput);
    }

    #[test]
    fn input_is_sniffed_and_sized_before_parsing() {
        let limits = Limits {
            max_input_bytes: 10,
            ..Limits::default()
        };
        assert_eq!(
            check_input(b"<html>", &limits).unwrap_err().code,
            PdfErrorCode::InvalidInput
        );
        assert_eq!(
            check_input(b"%PDF-1.7 and more", &limits).unwrap_err().code,
            PdfErrorCode::LimitExceeded
        );
        assert!(check_input(b"%PDF-1.7", &limits).is_ok());
    }
}
