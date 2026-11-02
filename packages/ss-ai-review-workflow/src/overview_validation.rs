//! Validates the overview presented to the review auditor.

use std::path::{Path, absolute};

use ohno::AppError;
use serde_json::{Value, json};
use ss_ai_review_context::{decode_file_text, read_capture};
use ss_ai_runtime::{Clock, caused_by, fail, read_bytes};

/// Checks the overview structure and its optional captured pull request link.
///
/// The caller supplies the same structure validator used for publication.
///
/// # Errors
///
/// Rejects unreadable inputs and overviews that do not meet the review contract.
pub fn validate(
    path: &Path,
    context_path: Option<&Path>,
    validate_structure: &dyn Fn(&str) -> Result<(), AppError>,
    clock: &dyn Clock,
) -> Result<Value, AppError> {
    let path =
        absolute(path).map_err(|error| caused_by("io", "Cannot resolve overview path", error))?;
    let bytes =
        read_bytes(&path, clock)?.ok_or_else(|| fail("invalidInput", "Overview is missing"))?;
    let text = decode_overview(&bytes)?;
    let context = context_path
        .map(|path| read_capture(path, clock))
        .transpose()?;
    let url = context
        .as_ref()
        .map(|value| {
            value
                .get("pullRequestUrl")
                .and_then(Value::as_str)
                .filter(|url| !url.is_empty())
                .ok_or_else(|| fail("invalidInput", "Capture lacks pullRequestUrl"))
        })
        .transpose()?;
    // Composition supplies the publication validator without making this
    // workflow library depend on the provider-backed review library.
    validate_structure(&text)?;
    validate_title_and_link(&text, url).map_err(|error| fail("invalidInput", error))?;
    Ok(json!({"isValid": true, "path": path}))
}

/// Matches `StreamReader`'s strict unmarked UTF-8 and BOM-selected decoders.
fn decode_overview(bytes: &[u8]) -> Result<String, AppError> {
    // StreamReader replaces malformed sequences after BOM detection, even when
    // its default UTF-8 encoding was configured to throw on malformed input.
    let has_bom = [
        &[0xef, 0xbb, 0xbf][..],
        &[0xff, 0xfe][..],
        &[0xfe, 0xff][..],
        &[0x00, 0x00, 0xfe, 0xff][..],
    ]
    .iter()
    .any(|mark| bytes.starts_with(mark));
    if has_bom {
        return Ok(decode_file_text(bytes));
    }
    Ok(std::str::from_utf8(bytes)
        .map_err(|error| caused_by("invalidInput", "Overview is not UTF-8", error))?
        .to_owned())
}

/// Checks the preparation-only title and captured-link placement.
fn validate_title_and_link(text: &str, url: Option<&str>) -> Result<(), String> {
    let mut lines = text
        .split(['\r', '\n'])
        .filter(|line| !line.trim().is_empty());
    if lines.next() != Some("# Content overview") {
        return Err("Expected the Content overview title.".into());
    }
    let second = lines
        .next()
        .ok_or("Overview lacks content beneath its title")?;
    if let Some(url) = url {
        if second != format!("**Pull request:** {url}") {
            return Err(
                "Expected the captured pull request URL immediately beneath the title.".into(),
            );
        }
    } else if second.starts_with("**Pull request:**") {
        return Err("A pull request link requires context.".into());
    }
    Ok(())
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::{decode_overview, validate_title_and_link};

    #[test]
    fn bom_selected_decoders_replace_invalid_sequences() {
        decode_overview(&[0xff]).unwrap_err();
        for bytes in [
            &[0xef, 0xbb, 0xbf, 0xff][..],
            &[0xff, 0xfe, 0x00, 0xd8][..],
            &[0xfe, 0xff, 0xd8, 0x00][..],
            &[0xff, 0xfe, 0x00, 0x00, 0x00, 0x00, 0x11, 0x00][..],
            &[0x00, 0x00, 0xfe, 0xff, 0x00, 0x11, 0x00, 0x00][..],
        ] {
            assert_eq!(decode_overview(bytes).unwrap(), "\u{fffd}");
        }
    }

    fn overview() -> String {
        let sections = [
            "Mission",
            "Substance",
            "Design changes",
            "Significant changes",
            "Structure",
            "Mechanism",
            "Completeness assessment",
            "Related history",
            "Related work",
        ];
        let mut text = String::from("# Content overview\n");
        for section in sections {
            text.push_str("## ");
            text.push_str(section);
            text.push_str("\nRésumé\n");
        }
        text
    }

    #[test]
    fn title_and_capture_link_are_exact() {
        let text = overview();
        let linked = text.replacen(
            "# Content overview",
            "# Content overview\n\n**Pull request:** https://example.test/é",
            1,
        );
        validate_title_and_link(&linked, Some("https://example.test/é")).unwrap();
        validate_title_and_link(&linked, None).unwrap_err();
        validate_title_and_link(&linked, Some("wrong")).unwrap_err();
        validate_title_and_link(
            &text.replace("# Content overview", "# content overview"),
            None,
        )
        .unwrap_err();
    }
}
