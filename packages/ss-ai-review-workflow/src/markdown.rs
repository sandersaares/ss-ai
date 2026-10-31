//! Renders structured review findings for audit and delivery.
//!
//! The caller validates the artifact and chooses the findings to render. This
//! module validates the fields it consumes and never derives approval decisions.

use std::borrow::Cow;
use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use ohno::AppError;
use serde_json::{Map, Value, json};
use ss_ai_runtime::{Clock, FileTransaction, fail};

use crate::workflow_plan::parse_json;

const AUTHOR: &str = "[Copilot speaking]";
const SEPARATOR: &str = "\n\n---\n\n";

/// Renders a JSON request without reading or writing files.
///
/// The required `operation` selects the request form. `finding` takes
/// `finding`, `destination`, and optional `context`;
/// `trackerItem` takes `finding` and optional `context`. `document` and
/// `auditDocument` take `findings`, `destination`, and optional `context`.
/// `auditDocument` also accepts an optional `overview`.
///
/// `selfReview` takes `findings`, `destination`, optional `context`, and
/// `policyApprovedFindingIds` and `auditFindingIds` arrays. The arrays must
/// partition the findings and come from a completed classification.
///
/// `summary` takes `findings`, `platform`, optional `followUpCount`, and
/// optional `diagnostics`. `destination` is `pullRequest` or `tracker`;
/// `platform` is `gitHub` or `azureDevOps`.
///
/// `comment` takes `finding`. `trackerFields` takes `finding` and optional
/// `followUpUrl` and `pullRequestUrl`. `findingRender` takes a partial
/// `finding`, `destination`, and those optional URLs, and exposes the
/// rendered blocks to compatibility callers.
///
/// `trackerItem` returns `findingId`, `title`, and `body`. `trackerFields`
/// returns `title` and `body`. `selfReview` returns `policyApproved`,
/// `auditSkipped`, and `document`. `findingRender` returns a complete rendering
/// and its parts. Other operations return a string.
/// Invalid input returns an error.
pub fn render(request: &Value) -> Result<Value, String> {
    let fields = object(request, "request")?;
    let operation = required_text(request, "operation")?;
    let allowed: &[&str] = match operation {
        "finding" => &["operation", "finding", "destination", "context"],
        "trackerItem" => &[
            "operation",
            "finding",
            "context",
            "followUpUrl",
            "pullRequestUrl",
        ],
        "comment" => &["operation", "finding"],
        "trackerFields" => &["operation", "finding", "followUpUrl", "pullRequestUrl"],
        "findingRender" => &[
            "operation",
            "finding",
            "destination",
            "followUpUrl",
            "pullRequestUrl",
        ],
        "document" => &["operation", "findings", "destination", "context"],
        "auditDocument" => &[
            "operation",
            "findings",
            "destination",
            "context",
            "overview",
        ],
        "selfReview" => &[
            "operation",
            "findings",
            "destination",
            "context",
            "policyApprovedFindingIds",
            "auditFindingIds",
        ],
        "summary" => &[
            "operation",
            "findings",
            "platform",
            "followUpCount",
            "diagnostics",
        ],
        _ => return Err(format!("unknown Markdown operation '{operation}'")),
    };
    if let Some(field) = fields
        .keys()
        .find(|field| !allowed.contains(&field.as_str()))
    {
        return Err(format!("unknown field '{field}' for {operation}"));
    }
    match operation {
        "finding" => {
            let finding = required(request, "finding")?;
            validate_finding(finding)?;
            let destination = destination(request)?;
            let context = context(request)?;
            validate_context_destination(destination, context)?;
            Ok(Value::String(destination_text(
                finding,
                destination,
                context,
            )?))
        }
        "trackerItem" => {
            let finding = required(request, "finding")?;
            validate_finding(finding)?;
            let context = context(request)?;
            let direct = direct_urls(request)?;
            if context.is_some() && direct.is_some() {
                return Err("direct URLs cannot be combined with context".into());
            }
            tracker_item_with_urls(finding, context, direct)
        }
        "findingRender" => {
            let finding = required(request, "finding")?;
            object(finding, "finding")?;
            let tracker = destination(request)? == "tracker";
            let (follow_url, pr_url) = direct_urls(request)?.unwrap_or(("", ""));
            let blocks = finding_blocks(finding, tracker, follow_url, pr_url)?;
            let body = blocks.join("\n\n");
            let published = if tracker && body.is_empty() {
                AUTHOR.to_owned()
            } else {
                format!("{AUTHOR}\n\n{body}")
            };
            Ok(json!({
                "title": optional_text(finding, "title")?.unwrap_or(""),
                "body": body,
                "published": published,
                "blocks": blocks,
            }))
        }
        "comment" => {
            let finding = required(request, "finding")?;
            Ok(Value::String(format!(
                "{AUTHOR}\n\n{}",
                body(finding, false, "", "")?
            )))
        }
        "trackerFields" => {
            let finding = required(request, "finding")?;
            object(finding, "finding")?;
            let title = optional_text(finding, "title")?.unwrap_or("");
            let (follow_url, pr_url) = direct_urls(request)?.unwrap_or(("", ""));
            let mut blocks = vec![AUTHOR.to_owned()];
            blocks.extend(finding_blocks(finding, true, follow_url, pr_url)?);
            Ok(json!({"title": title, "body": blocks.join("\n\n")}))
        }
        "document" | "auditDocument" => {
            let findings = findings(request)?;
            let destination = destination(request)?;
            let context = context(request)?;
            validate_context_destination(destination, context)?;
            let audit = operation == "auditDocument";
            let mut text = document(&findings, destination, context, audit)?;
            if audit {
                if let Some(overview) = optional_text(request, "overview")? {
                    if overview.trim().is_empty() {
                        return Err("overview must not be empty".into());
                    }
                    let overview = overview.replace("\r\n", "\n");
                    let overview = overview.trim_end();
                    text = if text.is_empty() {
                        overview.to_owned()
                    } else {
                        format!("{overview}{SEPARATOR}{text}")
                    };
                }
            } else if request.get("overview").is_some() {
                return Err("overview is only valid for auditDocument".into());
            }
            Ok(Value::String(text))
        }
        "selfReview" => self_review(request),
        "summary" => summary(request),
        _ => Err(format!("unknown Markdown operation '{operation}'")),
    }
}

/// Parses and renders a JSON request, rejecting duplicate object keys.
///
/// # Errors
///
/// Returns an error for invalid JSON or a request that cannot be rendered.
pub fn render_json(bytes: &[u8]) -> Result<Value, AppError> {
    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
    let request = parse_json(bytes)?;
    render(&request).map_err(|error| fail("invalidInput", error))
}

/// Renders and atomically publishes selected findings and requested documents.
///
/// The request contains `findings`, `selectedFindingIds`, `destination`, and
/// `outputDirectory`. Optional fields are `context`, `combinedPath`,
/// `auditDocumentPath`, and `overview`. An empty selection means all findings.
/// The combined document always includes all findings; the audit document
/// includes only the selected findings. Paths must be absolute.
///
/// # Errors
///
/// Returns an error when the request is invalid or a file cannot be published.
pub fn render_files_json(bytes: &[u8], clock: &dyn Clock) -> Result<Value, AppError> {
    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
    let request = parse_json(bytes)?;
    render_files(&request, clock)
}

/// Validates and prepares every rendering before publishing any file.
fn render_files(request: &Value, clock: &dyn Clock) -> Result<Value, AppError> {
    let prepare = || -> Result<_, String> {
        let fields = object(request, "request")?;
        let allowed = [
            "findings",
            "selectedFindingIds",
            "destination",
            "context",
            "outputDirectory",
            "combinedPath",
            "auditDocumentPath",
            "overview",
        ];
        if let Some(field) = fields
            .keys()
            .find(|field| !allowed.contains(&field.as_str()))
        {
            return Err(format!("unknown field '{field}' for file rendering"));
        }
        let findings = findings(request)?;
        let destination = destination(request)?;
        let context = context(request)?;
        validate_context_destination(destination, context)?;
        let ids = required(request, "selectedFindingIds")?
            .as_array()
            .ok_or("selectedFindingIds must be an array")?;
        let ids = identifier_set(ids, "selectedFindingIds")?;
        let available: HashSet<_> = findings
            .iter()
            .map(|finding| required_text(finding, "id"))
            .collect::<Result<_, _>>()?;
        if !ids.is_subset(&available) {
            return Err("selectedFindingIds contains an unknown finding".into());
        }
        let output = absolute_path(required_text(request, "outputDirectory")?)?;
        let combined = optional_path(request, "combinedPath")?;
        let audit_path = optional_path(request, "auditDocumentPath")?;
        let overview = optional_text(request, "overview")?;
        if overview.is_some() && audit_path.is_none() {
            return Err("overview requires auditDocumentPath".into());
        }
        if overview.is_some_and(|text| text.trim().is_empty()) {
            return Err("overview must not be empty".into());
        }
        let selected: Vec<_> = findings
            .iter()
            .copied()
            .filter(|finding| ids.is_empty() || ids.contains(finding["id"].as_str().unwrap_or("")))
            .collect();
        let mut files = Vec::new();
        let mut rendered = Vec::new();
        for finding in &selected {
            let id = required_text(finding, "id")?;
            let mut components = Path::new(id).components();
            if !matches!(components.next(), Some(Component::Normal(_)))
                || components.next().is_some()
                || id.contains(['/', '\\'])
            {
                return Err("finding id must be a single filename component".into());
            }
            let path = output.join(format!("{id}.md"));
            files.push((
                path.clone(),
                destination_text(finding, destination, context)?,
            ));
            rendered.push(json!({"findingId": id, "path": path}));
        }
        if let Some(path) = &combined {
            files.push((
                path.clone(),
                document(&findings, destination, context, false)?,
            ));
        }
        if let Some(path) = &audit_path {
            let mut text = document(&selected, destination, context, true)?;
            if let Some(overview) = overview {
                let overview = overview.replace("\r\n", "\n");
                let overview = overview.trim_end();
                text = if text.is_empty() {
                    overview.to_owned()
                } else {
                    format!("{overview}{SEPARATOR}{text}")
                };
            }
            files.push((path.clone(), text));
        }
        let result = json!({
            "outputDirectory": output, "destination": destination,
            "combinedPath": combined, "auditDocumentPath": audit_path,
            "findings": rendered,
        });
        Ok((files, result))
    };
    let (files, result) = prepare().map_err(|error| fail("invalidInput", error))?;
    for (path, text) in files {
        // All published files use LF regardless of the prose author's platform.
        let text = text.replace("\r\n", "\n");
        FileTransaction::open(&path, clock)?.write_bytes(text.as_bytes())?;
    }
    Ok(result)
}

/// Keeps output publication independent of the caller's working directory.
fn absolute_path(value: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(value);
    if !path.is_absolute() {
        return Err("output paths must be absolute".into());
    }
    Ok(path)
}

/// Reads an optional output path without accepting malformed JSON.
fn optional_path(request: &Value, field: &str) -> Result<Option<PathBuf>, String> {
    optional_text(request, field)?
        .filter(|value| !value.is_empty())
        .map(absolute_path)
        .transpose()
}

/// Distinguishes findings whose audit was skipped without assigning a verdict.
fn self_review(request: &Value) -> Result<Value, String> {
    let findings = findings(request)?;
    let destination = destination(request)?;
    let context = context(request)?;
    validate_context_destination(destination, context)?;
    let skipped_ids = required(request, "auditFindingIds")?
        .as_array()
        .ok_or("auditFindingIds must be an array")?;
    let approved_ids = required(request, "policyApprovedFindingIds")?
        .as_array()
        .ok_or("policyApprovedFindingIds must be an array")?;
    let skipped_ids = identifier_set(skipped_ids, "auditFindingIds")?;
    let approved_ids = identifier_set(approved_ids, "policyApprovedFindingIds")?;
    let available: HashSet<_> = findings
        .iter()
        .map(|finding| required_text(finding, "id"))
        .collect::<Result<_, _>>()?;
    if !skipped_ids.is_disjoint(&approved_ids)
        || skipped_ids
            .union(&approved_ids)
            .copied()
            .collect::<HashSet<_>>()
            != available
    {
        return Err("classification must account for each finding exactly once".into());
    }
    let mut approved = Vec::new();
    let mut unaudited = Vec::new();
    for finding in ordered(&findings) {
        let id = required_text(finding, "id")?;
        let entry =
            json!({"findingId": id, "text": destination_text(finding, destination, context)?});
        if skipped_ids.contains(id) {
            unaudited.push(entry);
        } else {
            approved.push(entry);
        }
    }
    Ok(json!({
        "policyApproved": approved,
        "auditSkipped": unaudited,
        "document": document(&findings, destination, context, false)?,
    }))
}

/// Requires explicit classification identifiers rather than inferring a verdict.
fn identifier_set<'a>(values: &'a [Value], name: &str) -> Result<HashSet<&'a str>, String> {
    let mut ids = HashSet::new();
    for value in values {
        let id = value
            .as_str()
            .filter(|id| !id.trim().is_empty())
            .ok_or_else(|| format!("{name} must contain non-empty strings"))?;
        if !ids.insert(id) {
            return Err(format!("{name} contains duplicate id '{id}'"));
        }
    }
    Ok(ids)
}

/// Checks the fields used for rendering rather than coercing malformed JSON.
fn validate_finding(finding: &Value) -> Result<(), String> {
    object(finding, "finding")?;
    required_text(finding, "id")?;
    for field in [
        "title",
        "description",
        "reasoning",
        "consequence",
        "recommendedAction",
    ] {
        let text = required_text(finding, field)?;
        if text
            .chars()
            .any(|c| c <= '\u{1f}' && (field == "title" || (c != '\n' && c != '\t')))
        {
            return Err(format!("{field} contains a disallowed control character"));
        }
    }
    // The finding model limits UTF-16 code units, as PowerShell strings do.
    if required_text(finding, "title")?.encode_utf16().count() > 255 {
        return Err("title exceeds the finding model's length limit".into());
    }
    let origin = required_text(finding, "origin")?;
    if !matches!(origin, "reviewer" | "synthesis") {
        return Err("origin must be reviewer or synthesis".into());
    }
    let anchor = required(finding, "anchor")?;
    location(anchor)?;
    match required_text(anchor, "kind")? {
        "file"
            if anchor.get("startLine").is_none()
                && anchor.get("endLine").is_none()
                && anchor.get("side").is_none() => {}
        "line" if anchor.get("startLine").is_some() => {}
        _ => return Err("invalid anchor kind or line range".into()),
    }
    for reference in array_or_empty(finding, "references")? {
        object(reference, "reference")?;
        required_text(reference, "text")?;
        if reference.get("url").is_some() {
            required_text(reference, "url")?;
        }
    }
    for item in array_or_empty(finding, "impactedLocations")? {
        location(item)?;
    }
    if let Some(follow_up) = finding.get("followUp").filter(|value| !value.is_null()) {
        object(follow_up, "followUp")?;
        required_text(follow_up, "threadId")?;
        required_text(follow_up, "commentId")?;
    }
    Ok(())
}

/// Validates and formats the source position shared by tracker items and lists.
fn location(value: &Value) -> Result<String, String> {
    object(value, "location")?;
    let path = required_text(value, "path")?;
    let line = |name| -> Result<Option<u64>, String> {
        match value.get(name) {
            None => Ok(None),
            Some(number) => number
                .as_u64()
                .filter(|n| *n > 0 && *n <= i64::MAX as u64)
                .map(Some)
                .ok_or_else(|| format!("{name} must be a positive integer")),
        }
    };
    let start = line("startLine")?;
    let end = line("endLine")?;
    if end.is_some() && start.is_none() || end.zip(start).is_some_and(|(end, start)| end < start) {
        return Err("invalid location line range".into());
    }
    if let Some(side) = value.get("side")
        && (start.is_none() || !matches!(side.as_str(), Some("left" | "right")))
    {
        return Err("invalid location side".into());
    }
    Ok(match (start, end) {
        (Some(start), Some(end)) if end > start => format!("{path}:{start}-{end}"),
        (Some(start), _) => format!("{path}:{start}"),
        _ => path.to_owned(),
    })
}

/// Produces the same substantive blocks for both publication destinations.
fn body(finding: &Value, tracker: bool, follow_url: &str, pr_url: &str) -> Result<String, String> {
    Ok(finding_blocks(finding, tracker, follow_url, pr_url)?.join("\n\n"))
}

/// Builds publication blocks without assuming optional prose is present.
fn finding_blocks(
    finding: &Value,
    tracker: bool,
    follow_url: &str,
    pr_url: &str,
) -> Result<Vec<String>, String> {
    object(finding, "finding")?;
    let mut blocks = Vec::new();
    let title = optional_text(finding, "title")?;
    if !tracker && let Some(title) = title.filter(|text| !text.trim().is_empty()) {
        blocks.push(format!("# {title}"));
    }
    if let Some(description) =
        optional_text(finding, "description")?.filter(|text| !text.trim().is_empty())
    {
        blocks.push(description.to_owned());
    }
    if tracker && let Some(anchor) = finding.get("anchor").filter(|anchor| !anchor.is_null()) {
        blocks.push(format!("**Location**: `{}`", render_location(anchor)?));
    }
    for (field, label) in [
        ("reasoning", "Reproducible reasoning"),
        ("consequence", "Consequence"),
        ("recommendedAction", "Recommended action"),
    ] {
        if let Some(text) = optional_text(finding, field)?.filter(|text| !text.trim().is_empty()) {
            blocks.push(format!("**{label}**:\n\n{text}"));
        }
    }
    let references = array_or_empty(finding, "references")?;
    if !references.is_empty() {
        let mut items = Vec::new();
        for reference in references {
            items.push(format!("- {}", render_reference(reference)?));
        }
        blocks.push(format!("**References**:\n\n{}", items.join("\n")));
    }
    let locations = array_or_empty(finding, "impactedLocations")?;
    if !locations.is_empty() {
        let items = locations
            .iter()
            .map(|item| render_location(item).map(|location| format!("- `{location}`")))
            .collect::<Result<Vec<_>, _>>()?;
        blocks.push(format!("**Impacted locations**:\n\n{}", items.join("\n")));
    }
    if tracker {
        if !follow_url.trim().is_empty() {
            blocks.push(format!("**Follows up on**: {follow_url}"));
        }
        if !pr_url.trim().is_empty() {
            blocks.push(format!("**Reviewed in**: {pr_url}"));
        }
    }
    Ok(blocks)
}

/// Formats a reference as a link only when it has a target.
fn render_reference(value: &Value) -> Result<String, String> {
    object(value, "reference")?;
    let text = optional_text(value, "text")?.unwrap_or("");
    Ok(match optional_text(value, "url")? {
        Some(url) if !url.trim().is_empty() => format!("[{text}]({url})"),
        _ => text.to_owned(),
    })
}

/// Formats a location without imposing artifact-only anchor rules.
fn render_location(value: &Value) -> Result<String, String> {
    object(value, "location")?;
    let path = required_text(value, "path")?;
    let line = |name| -> Result<Option<u64>, String> {
        match value.get(name) {
            None | Some(Value::Null) => Ok(None),
            Some(value) => value
                .as_u64()
                .filter(|n| *n > 0 && *n <= i64::MAX as u64)
                .map(Some)
                .ok_or_else(|| format!("{name} must be a positive integer")),
        }
    };
    let start = line("startLine")?;
    let end = line("endLine")?;
    if end.is_some() && start.is_none() {
        return Err("invalid location line range".into());
    }
    Ok(match (start, end) {
        (Some(start), Some(end)) if end > start => format!("{path}:{start}-{end}"),
        (Some(start), _) => format!("{path}:{start}"),
        _ => path.to_owned(),
    })
}

/// Resolves captured discussion links for a standalone tracker item.
fn provenance<'a>(
    finding: &Value,
    context: Option<&'a Value>,
) -> Result<(&'a str, &'a str), String> {
    let Some(context) = context else {
        return Ok(("", ""));
    };
    let pr_url = optional_text(context, "pullRequestUrl")?.unwrap_or("");
    let Some(follow_up) = finding.get("followUp").filter(|value| !value.is_null()) else {
        return Ok(("", pr_url));
    };
    let thread_id = required_text(follow_up, "threadId")?;
    let comment_id = required_text(follow_up, "commentId")?;
    for thread in captured_list_or_empty(context, "threads")? {
        object(thread, "thread")?;
        if required_identifier(thread, "id")? != thread_id {
            continue;
        }
        for comment in captured_list_or_empty(thread, "comments")? {
            object(comment, "comment")?;
            if required_identifier(comment, "id")? == comment_id {
                let url = optional_text(comment, "url")?.unwrap_or("");
                return Ok((if url.trim().is_empty() { pr_url } else { url }, pr_url));
            }
        }
        break;
    }
    Ok((pr_url, pr_url))
}

/// Builds the fields a tracker stores separately.
fn tracker_item(finding: &Value, context: Option<&Value>) -> Result<Value, String> {
    tracker_item_with_urls(finding, context, None)
}

/// Uses resolved links when the caller already has provenance.
fn tracker_item_with_urls(
    finding: &Value,
    context: Option<&Value>,
    direct: Option<(&str, &str)>,
) -> Result<Value, String> {
    let (follow_url, pr_url) = match direct {
        Some(urls) => urls,
        None => provenance(finding, context)?,
    };
    Ok(json!({
        "findingId": required_text(finding, "id")?,
        "title": required_text(finding, "title")?,
        "body": format!("{AUTHOR}\n\n{}", body(finding, true, follow_url, pr_url)?),
    }))
}

/// Accepts optional direct links without coercing JSON values.
fn direct_urls(request: &Value) -> Result<Option<(&str, &str)>, String> {
    let follow = optional_text(request, "followUpUrl")?.unwrap_or("");
    let pull_request = optional_text(request, "pullRequestUrl")?.unwrap_or("");
    if follow.trim().is_empty() && pull_request.trim().is_empty() {
        Ok(None)
    } else {
        Ok(Some((follow, pull_request)))
    }
}

/// Presents the actual publication form during audit.
fn destination_text(
    finding: &Value,
    destination: &str,
    context: Option<&Value>,
) -> Result<String, String> {
    if destination == "pullRequest" {
        return Ok(format!("{AUTHOR}\n\n{}", body(finding, false, "", "")?));
    }
    let item = tracker_item(finding, context)?;
    Ok(format!(
        "Title: {}\n\n{}",
        required_text(&item, "title")?,
        required_text(&item, "body")?
    ))
}

/// Groups synthesis findings ahead of reviewer findings without disturbing either order.
fn ordered<'a>(findings: &'a [&'a Value]) -> impl Iterator<Item = &'a Value> + 'a {
    findings
        .iter()
        .copied()
        .filter(|finding| finding["origin"] == "synthesis")
        .chain(
            findings
                .iter()
                .copied()
                .filter(|finding| finding["origin"] != "synthesis"),
        )
}

/// Assembles a complete document, or an identifier-labelled audit document.
fn document(
    findings: &[&Value],
    destination: &str,
    context: Option<&Value>,
    audit: bool,
) -> Result<String, String> {
    let mut sections = Vec::new();
    for finding in ordered(findings) {
        let text = if !audit && destination == "pullRequest" {
            body(finding, false, "", "")?
        } else {
            destination_text(finding, destination, context)?
        };
        sections.push(if audit {
            format!("**Finding `{}`**\n\n{text}", required_text(finding, "id")?)
        } else {
            text
        });
    }
    let text = sections.join(SEPARATOR);
    if !audit && destination == "pullRequest" && !text.is_empty() {
        Ok(format!("{AUTHOR}\n\n{text}"))
    } else {
        Ok(text)
    }
}

/// Renders a publication summary without making an approval decision.
fn summary(request: &Value) -> Result<Value, String> {
    let findings = required(request, "findings")?
        .as_array()
        .ok_or("findings must be an array")?;
    let count = findings.len();
    let follow = match request.get("followUpCount") {
        None => 0,
        Some(value) => value
            .as_u64()
            .filter(|count| *count <= i32::MAX as u64)
            .ok_or("followUpCount must be a nonnegative 32-bit integer")?,
    };
    let platform = required_text(request, "platform")?;
    if !matches!(platform, "gitHub" | "azureDevOps") {
        return Err("platform must be gitHub or azureDevOps".into());
    }
    let lead = if count == 0 {
        "This review raises no findings.".to_owned()
    } else {
        let noun = if count == 1 { "finding" } else { "findings" };
        let follow_text = match follow {
            0 => "No finding follows up on an existing discussion thread.".to_owned(),
            1 => "One finding follows up on an existing discussion thread.".to_owned(),
            _ => format!("{follow} findings follow up on existing discussion threads."),
        };
        format!("Published {count} {noun}. {follow_text}")
    };
    let diagnostics = request.get("diagnostics");
    if let Some(diagnostics) = diagnostics {
        object(diagnostics, "diagnostics")?;
    }
    let cache = match diagnostics
        .map(|d| optional_text(d, "cache"))
        .transpose()?
        .flatten()
    {
        Some(value) if value.eq_ignore_ascii_case("hit") => "Hit",
        _ => "Miss",
    };
    let instruction_count = match diagnostics.and_then(|d| d.get("userInstructionFileCount")) {
        None | Some(Value::Null) => "Not recorded".to_owned(),
        Some(value) => value
            .as_u64()
            .filter(|count| *count <= i64::MAX as u64)
            .ok_or("userInstructionFileCount must be a nonnegative integer")?
            .to_string(),
    };
    let mut table = format!(
        "| Diagnostic | Value |\n|------------|-------|\n| Cache | {cache} |\n| User instruction files | {instruction_count} |"
    );
    if diagnostics
        .map(|d| optional_text(d, "inputState"))
        .transpose()?
        .flatten()
        .is_some_and(|state| state.eq_ignore_ascii_case("degraded"))
    {
        table.push_str("\n| Instruction expansion | Degraded |");
    }
    if platform == "gitHub" {
        table = format!("<details><summary>See diagnostics</summary>\n\n{table}\n\n</details>");
    }
    Ok(Value::String(format!("{AUTHOR}\n\n{lead}\n\n{table}")))
}

/// Validates findings and their identifiers before producing any output.
fn findings(request: &Value) -> Result<Vec<&Value>, String> {
    let findings = required(request, "findings")?
        .as_array()
        .ok_or("findings must be an array")?;
    let mut ids = HashSet::new();
    for finding in findings {
        validate_finding(finding)?;
        let id = required_text(finding, "id")?;
        if !ids.insert(id) {
            return Err(format!("duplicate finding id '{id}'"));
        }
    }
    Ok(findings.iter().collect())
}

/// Enforces the supported destination vocabulary.
fn destination(request: &Value) -> Result<&str, String> {
    let destination = required_text(request, "destination")?;
    if matches!(destination, "pullRequest" | "tracker") {
        Ok(destination)
    } else {
        Err("destination must be pullRequest or tracker".into())
    }
}

/// Checks the captured fields that can affect tracker provenance.
fn context(request: &Value) -> Result<Option<&Value>, String> {
    match request.get("context") {
        None | Some(Value::Null) => Ok(None),
        Some(context) => {
            object(context, "context")?;
            optional_text(context, "pullRequestUrl")?;
            for thread in captured_list_or_empty(context, "threads")? {
                object(thread, "thread")?;
                required_identifier(thread, "id")?;
                for comment in captured_list_or_empty(thread, "comments")? {
                    object(comment, "comment")?;
                    required_identifier(comment, "id")?;
                    optional_text(comment, "url")?;
                }
            }
            Ok(Some(context))
        }
    }
}

/// A pull request rendering does not use captured discussion links.
fn validate_context_destination(destination: &str, context: Option<&Value>) -> Result<(), String> {
    if destination == "pullRequest" && context.is_some() {
        return Err("context is only valid for tracker rendering".into());
    }
    Ok(())
}

/// Reads a required JSON property without coercion.
fn required<'a>(value: &'a Value, field: &str) -> Result<&'a Value, String> {
    value.get(field).ok_or_else(|| format!("missing {field}"))
}

/// Reads a required nonblank string.
fn required_text<'a>(value: &'a Value, field: &str) -> Result<&'a str, String> {
    required(value, field)?
        .as_str()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("{field} must be a non-empty string"))
}

/// Reads a captured provider identifier without losing integer precision.
fn required_identifier<'a>(value: &'a Value, field: &str) -> Result<Cow<'a, str>, String> {
    match required(value, field)? {
        Value::String(value) if !value.is_empty() => Ok(Cow::Borrowed(value)),
        Value::Number(value) if value.as_i64().is_some() || value.as_u64().is_some() => {
            Ok(Cow::Owned(value.to_string()))
        }
        _ => Err(format!("{field} must be a non-empty string or integer")),
    }
}

/// Reads an optional string without converting another JSON type.
fn optional_text<'a>(value: &'a Value, field: &str) -> Result<Option<&'a str>, String> {
    match value.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_str()
            .map(Some)
            .ok_or_else(|| format!("{field} must be a string")),
    }
}

/// Accepts omitted or null optional lists but rejects malformed lists.
fn array_or_empty<'a>(value: &'a Value, field: &str) -> Result<&'a [Value], String> {
    match value.get(field) {
        None | Some(Value::Null) => Ok(&[]),
        Some(value) => value
            .as_array()
            .map(Vec::as_slice)
            .ok_or_else(|| format!("{field} must be an array")),
    }
}

/// Accepts the singleton-object form used by the canonical capture reader.
fn captured_list_or_empty<'a>(value: &'a Value, field: &str) -> Result<&'a [Value], String> {
    match value.get(field) {
        Some(value @ Value::Object(_)) => Ok(std::slice::from_ref(value)),
        _ => array_or_empty(value, field),
    }
}

/// Rejects JSON values that cannot carry named properties.
fn object<'a>(value: &'a Value, name: &str) -> Result<&'a Map<String, Value>, String> {
    value
        .as_object()
        .ok_or_else(|| format!("{name} must be an object"))
}
