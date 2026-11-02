//! Prepares validated finding documents for audit and delivery rendering.

use std::any::type_name;
use std::collections::HashSet;
use std::fmt::{self, Debug, Formatter};
use std::path::{Path, PathBuf, absolute};

use ohno::AppError;
use serde_json::{Value, json};
use ss_ai_review_context::{decode_file_text, read_capture};
use ss_ai_runtime::{Clock, caused_by, fail, read_bytes};
use url::Url;

use crate::{approved, findings, markdown, workflow_plan::parse_json};

/// File inputs for rendering an artifact or approved finding set.
#[expect(
    clippy::exhaustive_structs,
    reason = "This internal command composition supplies every rendering input"
)]
pub struct Options<'a> {
    /// Path to the finding artifact or approved set.
    pub path: &'a Path,
    /// Directory receiving selected finding bodies.
    pub output_directory: &'a Path,
    /// Optional path receiving all findings.
    pub combined_path: Option<&'a Path>,
    /// Optional path receiving the selected findings for audit.
    pub audit_document_path: Option<&'a Path>,
    /// Selected identifiers; an empty selection renders all findings.
    pub finding_ids: &'a [String],
    /// Destination for the rendered prose.
    pub destination: &'a str,
    /// Optional capture supplying tracker links.
    pub context_path: Option<&'a Path>,
    /// Optional overview prepended to the audit document.
    pub overview_path: Option<&'a Path>,
}

impl Debug for Options<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct(type_name::<Self>())
            .field("path", &self.path)
            .field("output_directory", &self.output_directory)
            .field("combined_path", &self.combined_path)
            .field("audit_document_path", &self.audit_document_path)
            .field("finding_count", &self.finding_ids.len())
            .field("destination", &self.destination)
            .finish_non_exhaustive()
    }
}

/// Validates the source, then renders the requested files.
///
/// # Errors
///
/// Rejects incompatible or malformed inputs and failed file operations.
pub fn prepare(options: &Options<'_>, clock: &dyn Clock) -> Result<Value, AppError> {
    if options.context_path.is_some() && options.destination != "tracker" {
        return Err(fail(
            "invalidInput",
            "Context requires the tracker destination",
        ));
    }
    if options.overview_path.is_some() && options.audit_document_path.is_none() {
        return Err(fail("invalidInput", "Overview requires an audit document"));
    }
    let overview = options
        .overview_path
        .map(|path| {
            let bytes = read_bytes(path, clock)?
                .ok_or_else(|| fail("invalidInput", "Overview is missing"))?;
            let text = decode_file_text(&bytes);
            if text.trim().is_empty() {
                return Err(fail("invalidInput", "Overview is empty"));
            }
            Ok(text)
        })
        .transpose()?;
    let context = options
        .context_path
        .map(|path| read_capture(path, clock))
        .transpose()?;
    let bytes = read_bytes(options.path, clock)?
        .ok_or_else(|| fail("invalidInput", "Finding document is missing"))?;
    let document = parse_json(&bytes)?;
    if document.get("kind").is_some() {
        findings::validate_artifact(&document)?;
    } else {
        // Audit rendering requires the approved shape, not publication freshness.
        approved::check_approved_schema(&document)?;
    }
    let records = document
        .get("findings")
        .and_then(Value::as_array)
        .ok_or_else(|| fail("invalidInput", "Finding document lacks findings"))?;
    let ids = identifiers(options.finding_ids);
    for id in &ids {
        if !records
            .iter()
            .any(|finding| finding.get("id").and_then(Value::as_str) == Some(*id))
        {
            return Err(fail(
                "invalidInput",
                format!("Unknown finding identifier '{id}'"),
            ));
        }
    }
    let output = resolve(options.output_directory)?;
    let combined = options.combined_path.map(resolve).transpose()?;
    let audit = options.audit_document_path.map(resolve).transpose()?;
    let audit_uri = audit
        .as_ref()
        .map(|path| {
            Url::from_file_path(path)
                .map(|url| url.to_string())
                .map_err(|()| fail("invalidInput", "Cannot convert audit path to file URI"))
        })
        .transpose()?;
    let request = json!({
        "findings": records, "selectedFindingIds": ids, "destination": options.destination,
        "outputDirectory": output, "combinedPath": combined, "auditDocumentPath": audit,
        "context": context, "overview": overview,
    });
    let mut result = markdown::render_files(&request, clock)?;
    result
        .as_object_mut()
        .ok_or_else(|| fail("invalidState", "Renderer returned a non-object result"))?
        .insert("auditDocumentUri".into(), json!(audit_uri));
    Ok(result)
}

/// Accepts repeated and comma-separated selections without changing their text.
fn identifiers(values: &[String]) -> Vec<&str> {
    let mut seen = HashSet::new();
    values
        .iter()
        .flat_map(|entry| entry.split(','))
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .filter(|id| seen.insert(*id))
        .collect()
}

/// Makes paths independent of the current directory when passed to the renderer.
fn resolve(path: &Path) -> Result<PathBuf, AppError> {
    Ok(absolute(path).map_err(|error| caused_by("io", "Cannot resolve output path", error))?)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use std::path::Path;

    use super::{Options, identifiers};

    #[test]
    fn selection_accepts_commas_repeats_and_unicode() {
        let input = vec![" F1, é, F1 ".into(), String::new(), "é,F2".into()];
        assert_eq!(identifiers(&input), ["F1", "é", "F2"]);
        assert!(identifiers(&[" , ".into()]).is_empty());
    }

    #[test]
    fn options_debug_handles_empty_and_populated_selections() {
        let ids = vec!["F1".to_owned(), "F2".to_owned()];
        for finding_ids in [&[][..], ids.as_slice()] {
            let options = Options {
                path: Path::new("source"),
                output_directory: Path::new("output"),
                combined_path: None,
                audit_document_path: None,
                finding_ids,
                destination: "tracker",
                context_path: None,
                overview_path: None,
            };
            drop(format!("{options:?}"));
        }
    }
}
