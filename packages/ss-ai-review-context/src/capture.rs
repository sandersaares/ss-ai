//! Validates immutable captures and applies matching authorship decisions.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use ohno::AppError;
use serde_json::{Value, json};
use ss_ai_runtime::{Clock, fail, read_bytes};

use crate::{
    document_hash,
    json::{equivalent_key, parse_text, snapshot_hash},
};

// These versions bind the native implementation to the PowerShell model.
const CAPTURE_VERSION: i64 = 3;
const POLICY_VERSION: &str = "2";
const CLASSIFICATION_EVIDENCE: &str = "classification:reviewed";

/// Reads a local capture and applies its validated semantic decisions.
///
/// Field names consulted by the reader must use canonical casing.
///
/// # Errors
///
/// Rejects unreadable or malformed captures and incompatible sidecars.
pub fn read_capture(path: &Path, clock: &dyn Clock) -> Result<Value, AppError> {
    let bytes = read_bytes(path, clock)?.ok_or_else(|| {
        fail(
            "invalidInput",
            format!("Could not find file: {}", path.display()),
        )
    })?;
    let mut document = parse_file(&bytes)?;
    validate_capture_keys(&document)?;
    if !document.is_object()
        || document.get("schemaVersion").and_then(Value::as_i64) != Some(CAPTURE_VERSION)
    {
        return Err(fail("invalidInput", "Unsupported capture schema version"));
    }
    validate_capture_structure(&document)?;

    let mut sidecar = path.as_os_str().to_os_string();
    sidecar.push(".authorship.json");
    if let Some(bytes) = read_bytes(Path::new(&sidecar), clock)? {
        let decisions = parse_file(&bytes)?;
        apply_decisions(&mut document, &decisions)?;
    }
    Ok(document)
}

/// Binds the effective capture and authorship policy to a reading snapshot.
///
/// # Errors
///
/// Rejects captures that cannot be interpreted or hashed faithfully.
pub fn snapshot_id(document: &Value) -> Result<String, AppError> {
    validate_capture_keys(document)?;
    validate_capture_structure(document)?;
    snapshot_hash(POLICY_VERSION, document)
}

/// Fingerprints review evidence independently of semantic classification.
///
/// # Errors
///
/// Rejects captures that cannot be interpreted or hashed faithfully.
pub fn evidence_hash(document: &Value) -> Result<String, AppError> {
    validate_capture_keys(document)?;
    validate_capture_structure(document)?;
    let pull_request = property(document, "pullRequest");
    let mut selected = serde_json::Map::new();
    for key in [
        "title",
        "description",
        "author",
        "sourceRef",
        "targetRef",
        "reviewedSourceCommit",
        "reviewedMergeBaseCommit",
    ] {
        selected.insert(key.to_owned(), property(pull_request, key).clone());
    }
    let mut threads = array_property(document, "threads");
    for thread in &mut threads {
        let mut comments = array_property(thread, "comments");
        for comment in &mut comments {
            if let Some(object) = comment.as_object_mut() {
                object.shift_remove("authorship");
                let evidence =
                    filtered_evidence(object.get("authorshipEvidence").unwrap_or(&Value::Null));
                object.insert("authorshipEvidence".to_owned(), Value::Array(evidence));
            } else {
                return Err(fail("invalidInput", "Capture comment is not an object"));
            }
        }
        if let Some(object) = thread.as_object_mut() {
            // PowerShell only rewrites comments themselves, not the comments field.
            if object.get("comments").is_some_and(Value::is_array) {
                object.insert("comments".to_owned(), Value::Array(comments));
            } else if let Some(comment) = comments.into_iter().next() {
                object.insert("comments".to_owned(), comment);
            }
        } else {
            return Err(fail("invalidInput", "Capture thread is not an object"));
        }
    }
    document_hash(&json!({
        "platform": property(document, "platform"),
        "repositoryUrl": property(document, "repositoryUrl"),
        "pullRequestUrl": property(document, "pullRequestUrl"),
        "pullRequest": selected,
        "threads": threads
    }))
}

/// Binds a decision to the exact evidence the classifier received.
pub(crate) fn comment_input_hash(
    document: &Value,
    thread_id: &str,
    comment: &Value,
) -> Result<String, AppError> {
    document_hash(&json!({
        "schemaVersion": 1,
        "policyVersion": POLICY_VERSION,
        "platform": property(document, "platform"),
        "repositoryUrl": property(document, "repositoryUrl"),
        "pullRequestUrl": property(document, "pullRequestUrl"),
        "threadId": thread_id,
        "commentId": string_id(property(comment, "id")),
        "content": property(comment, "content"),
        "author": property(comment, "author"),
        "isAuthoredBySelf": property(comment, "isAuthoredBySelf"),
        "evidence": filtered_evidence(property(comment, "authorshipEvidence"))
    }))
}

/// Rejects field casing that case-sensitive consumers cannot safely interpret.
pub(crate) fn validate_capture_keys(document: &Value) -> Result<(), AppError> {
    // Restrict checks to structures whose keys readers interpret. Provider
    // metadata and identities remain opaque and retain their original spelling.
    const ROOT_FIELDS: &[&str] = &[
        "schemaVersion",
        "fetchedUtc",
        "platform",
        "repositoryUrl",
        "pullRequestUrl",
        "target",
        "authenticatedIdentity",
        "pullRequest",
        "reviewers",
        "revisions",
        "threads",
    ];
    const PULL_REQUEST_FIELDS: &[&str] = &[
        "id",
        "title",
        "description",
        "author",
        "createdUtc",
        "sourceRef",
        "targetRef",
        "reviewedSourceCommit",
        "reviewedMergeBaseCommit",
        "isAuthoredBySelf",
        "isDraft",
    ];
    const THREAD_FIELDS: &[&str] = &[
        "id",
        "url",
        "openingCommentId",
        "kind",
        "resolution",
        "isOutdated",
        "context",
        "decision",
        "eventUtc",
        "lastUpdatedUtc",
        "comments",
    ];
    const COMMENT_FIELDS: &[&str] = &[
        "id",
        "url",
        "content",
        "parentId",
        "author",
        "authorship",
        "authorshipEvidence",
        "isAuthoredBySelf",
        "publishedUtc",
        "lastEditedUtc",
    ];
    validate_fields(document, ROOT_FIELDS)?;
    validate_fields(property(document, "pullRequest"), PULL_REQUEST_FIELDS)?;
    for thread in list_values(property(document, "threads")) {
        validate_fields(thread, THREAD_FIELDS)?;
        validate_fields(property(thread, "context"), &["filePath"])?;
        for comment in list_values(property(thread, "comments")) {
            validate_fields(comment, COMMENT_FIELDS)?;
        }
    }
    Ok(())
}

/// Rejects casing ambiguity only for fields interpreted at this object level.
fn validate_fields(value: &Value, fields: &[&str]) -> Result<(), AppError> {
    if let Some(object) = value.as_object() {
        for name in object.keys() {
            if let Some(index) = equivalent_key(fields.iter().copied(), name)
                && fields.get(index).is_some_and(|expected| *expected != name)
            {
                return Err(fail(
                    "invalidInput",
                    format!("Capture field has noncanonical casing: {name}"),
                ));
            }
        }
    }
    Ok(())
}

/// Checks evidence that canonical capture producers always include.
fn validate_capture_structure(document: &Value) -> Result<(), AppError> {
    for name in ["platform", "repositoryUrl", "pullRequestUrl"] {
        if !nonempty(document.get(name)) {
            return Err(fail("invalidInput", format!("Capture requires {name}")));
        }
    }
    if !property(document, "pullRequest").is_object() {
        return Err(fail(
            "invalidInput",
            "Capture requires pull request evidence",
        ));
    }
    let pull_request = property(document, "pullRequest");
    for name in [
        "title",
        "description",
        "reviewedSourceCommit",
        "reviewedMergeBaseCommit",
    ] {
        require_nullable_string(pull_request, name)?;
    }
    for name in ["reviewers", "revisions"] {
        required_list(document, name)?;
    }
    for thread in required_list(document, "threads")? {
        if !valid_identifier(property(thread, "id")) {
            return Err(fail(
                "invalidInput",
                "Capture thread requires an identifier",
            ));
        }
        if !matches!(
            thread.get("kind").and_then(Value::as_str),
            Some("text" | "reviewSummary" | "refUpdate" | "reviewersUpdated" | "policy" | "system")
        ) || !matches!(
            thread.get("resolution").and_then(Value::as_str),
            Some("active" | "resolved" | "indeterminate")
        ) {
            return Err(fail(
                "invalidInput",
                "Capture thread requires a valid kind and resolution",
            ));
        }
        for name in ["isOutdated", "isAuthoredBySelf"] {
            if !thread.get(name).is_some_and(Value::is_boolean) {
                return Err(fail(
                    "invalidInput",
                    format!("Capture thread requires {name}"),
                ));
            }
        }
        for name in ["context", "decision"] {
            if !matches!(thread.get(name), Some(Value::Object(_) | Value::Null)) {
                return Err(fail(
                    "invalidInput",
                    format!("Capture thread requires {name}"),
                ));
            }
        }
        if let Some(context) = thread.get("context").filter(|value| value.is_object())
            && !nonempty(context.get("filePath"))
        {
            return Err(fail(
                "invalidInput",
                "Capture thread context requires a file path",
            ));
        }
        if !matches!(thread.get("openingCommentId"), Some(Value::Null))
            && !thread.get("openingCommentId").is_some_and(valid_identifier)
        {
            return Err(fail(
                "invalidInput",
                "Capture thread requires an opening comment identifier",
            ));
        }
        for name in ["url", "eventUtc", "lastUpdatedUtc"] {
            require_nullable_string(thread, name)?;
        }
        for comment in required_list(thread, "comments")? {
            if !valid_identifier(property(comment, "id")) {
                return Err(fail(
                    "invalidInput",
                    "Capture comment requires an identifier",
                ));
            }
            if !matches!(comment.get("content"), Some(Value::String(_) | Value::Null)) {
                return Err(fail("invalidInput", "Capture comment requires content"));
            }
            if !matches!(comment.get("author"), Some(Value::Object(_) | Value::Null)) {
                return Err(fail(
                    "invalidInput",
                    "Capture comment requires author evidence",
                ));
            }
            if !matches!(comment.get("parentId"), Some(Value::Null))
                && !comment.get("parentId").is_some_and(valid_identifier)
            {
                return Err(fail(
                    "invalidInput",
                    "Capture comment requires a parent identifier",
                ));
            }
            for name in ["url", "publishedUtc", "lastEditedUtc"] {
                require_nullable_string(comment, name)?;
            }
            if !matches!(
                comment.get("authorship").and_then(Value::as_str),
                Some("agent" | "human" | "indeterminate")
            ) {
                return Err(fail(
                    "invalidInput",
                    "Capture comment requires valid authorship",
                ));
            }
            match comment.get("authorshipEvidence") {
                Some(Value::Array(values)) if values.iter().all(Value::is_string) => {}
                Some(Value::String(_)) => {}
                _ => {
                    return Err(fail(
                        "invalidInput",
                        "Capture comment requires authorship evidence",
                    ));
                }
            }
            if !comment
                .get("isAuthoredBySelf")
                .is_some_and(Value::is_boolean)
            {
                return Err(fail(
                    "invalidInput",
                    "Capture comment requires self attribution",
                ));
            }
        }
    }
    Ok(())
}

/// Preserves provider-supplied null while rejecting a missing or malformed field.
fn require_nullable_string(value: &Value, name: &str) -> Result<(), AppError> {
    if matches!(value.get(name), Some(Value::String(_) | Value::Null)) {
        Ok(())
    } else {
        Err(fail("invalidInput", format!("Capture requires {name}")))
    }
}

/// Accepts the singleton-object form recognized by PowerShell's list accessor.
fn required_list<'a>(value: &'a Value, name: &str) -> Result<&'a [Value], AppError> {
    let result = match value.get(name) {
        Some(Value::Array(values)) => values.as_slice(),
        Some(value @ Value::Object(_)) => std::slice::from_ref(value),
        _ => return Err(fail("invalidInput", format!("Capture requires {name}"))),
    };
    if result.iter().any(|entry| !entry.is_object()) {
        return Err(fail(
            "invalidInput",
            format!("Capture {name} contains a non-object"),
        ));
    }
    Ok(result)
}

/// Treats a scalar object as a singleton and leaves absent evidence empty.
fn list_values(value: &Value) -> &[Value] {
    match value {
        Value::Array(values) => values,
        Value::Object(_) => std::slice::from_ref(value),
        _ => &[],
    }
}

/// Recognizes the string and integer identifiers used by both providers.
fn valid_identifier(value: &Value) -> bool {
    match value {
        Value::String(value) => !value.is_empty(),
        Value::Number(value) => value.as_i64().is_some() || value.as_u64().is_some(),
        _ => false,
    }
}

/// Loads the encodings recognized by the shared PowerShell stream reader.
fn parse_file(bytes: &[u8]) -> Result<Value, AppError> {
    parse_text(&decode_file_text(bytes))
}

/// Decodes text using the shared PowerShell stream reader's BOM handling.
///
/// UTF-8 is the default; a byte order mark selects UTF-8, UTF-16, or UTF-32.
/// Malformed encoded sequences are replaced as in the PowerShell reader.
#[must_use]
pub fn decode_file_text(bytes: &[u8]) -> String {
    if let Some(bytes) = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]) {
        String::from_utf8_lossy(bytes).into_owned()
    } else if let Some(bytes) = bytes.strip_prefix(&[0xff, 0xfe, 0x00, 0x00]) {
        decode_utf32(bytes, true)
    } else if let Some(bytes) = bytes.strip_prefix(&[0x00, 0x00, 0xfe, 0xff]) {
        decode_utf32(bytes, false)
    } else if let Some(bytes) = bytes.strip_prefix(&[0xff, 0xfe]) {
        decode_utf16(bytes, true)
    } else if let Some(bytes) = bytes.strip_prefix(&[0xfe, 0xff]) {
        decode_utf16(bytes, false)
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    }
}

/// Decodes UTF-16 using the stream reader's replacement behavior.
fn decode_utf16(bytes: &[u8], little: bool) -> String {
    let (words, remainder) = bytes.as_chunks::<2>();
    let mut text = String::from_utf16_lossy(
        &words
            .iter()
            .map(|word| {
                if little {
                    u16::from_le_bytes(*word)
                } else {
                    u16::from_be_bytes(*word)
                }
            })
            .collect::<Vec<_>>(),
    );
    if !remainder.is_empty() {
        text.push('\u{fffd}');
    }
    text
}

/// Decodes UTF-32 using the stream reader's replacement behavior.
fn decode_utf32(bytes: &[u8], little: bool) -> String {
    let (words, remainder) = bytes.as_chunks::<4>();
    let mut text: String = words
        .iter()
        .map(|word| {
            char::from_u32(if little {
                u32::from_le_bytes(*word)
            } else {
                u32::from_be_bytes(*word)
            })
            .unwrap_or('\u{fffd}')
        })
        .collect();
    if !remainder.is_empty() {
        text.push('\u{fffd}');
    }
    text
}

/// Enforces the sidecar schema before consulting any decision.
fn validate_decisions(value: &Value) -> Result<(), AppError> {
    let invalid = || fail("invalidInput", "Invalid comment authorship record");
    let object = value.as_object().ok_or_else(invalid)?;
    if object.len() != 4
        || object.get("schemaVersion").and_then(Value::as_i64) != Some(1)
        || !nonempty(object.get("policyVersion"))
        || !is_hash(object.get("captureHash"))
    {
        return Err(invalid());
    }
    let decisions = object
        .get("decisions")
        .and_then(Value::as_array)
        .ok_or_else(invalid)?;
    let mut seen = HashSet::new();
    for entry in decisions {
        let entry = entry.as_object().ok_or_else(invalid)?;
        if entry.len() != 5
            || !nonempty(entry.get("threadId"))
            || !nonempty(entry.get("commentId"))
            || !is_hash(entry.get("inputHash"))
            || !matches!(
                entry.get("kind").and_then(Value::as_str),
                Some("agent" | "human")
            )
            || !matches!(
                entry.get("origin").and_then(Value::as_str),
                Some("classified" | "reused")
            )
        {
            return Err(invalid());
        }
        let key = (
            entry.get("threadId").and_then(Value::as_str),
            entry.get("commentId").and_then(Value::as_str),
        );
        if !seen.insert(key) {
            return Err(fail(
                "invalidInput",
                "Duplicate comment authorship decision",
            ));
        }
    }
    Ok(())
}

/// Applies only decisions for the current capture, policy and exact inputs.
fn apply_decisions(document: &mut Value, sidecar: &Value) -> Result<(), AppError> {
    validate_decisions(sidecar)?;
    if property(sidecar, "captureHash").as_str() != Some(&document_hash(document)?) {
        return Err(fail(
            "invalidInput",
            "Authorship decisions do not belong to this capture",
        ));
    }
    if property(sidecar, "policyVersion").as_str() != Some(POLICY_VERSION) {
        return Ok(());
    }
    let index = comment_coordinates(document);
    for entry in sidecar["decisions"].as_array().into_iter().flatten() {
        let thread_id = entry["threadId"].as_str().unwrap_or_default();
        let comment_id = entry["commentId"].as_str().unwrap_or_default();
        let Some(coordinates) = index.get(thread_id) else {
            continue;
        };
        let Some(&comment_index) = coordinates.comments.get(comment_id) else {
            continue;
        };
        let thread_index = coordinates.thread;
        let thread_is_array = property(document, "threads").is_array();
        let thread = selected(property(document, "threads"), thread_is_array, thread_index);
        let comment_is_array = property(thread, "comments").is_array();
        let comment = selected(
            property(thread, "comments"),
            comment_is_array,
            comment_index,
        );
        if property(comment, "authorship").as_str() != Some("indeterminate")
            || entry["inputHash"].as_str()
                != Some(&comment_input_hash(document, thread_id, comment)?)
        {
            continue;
        }
        let threads = document
            .get_mut("threads")
            .ok_or_else(|| fail("invalidInput", "Missing capture threads"))?;
        let thread = selected_mut(threads, thread_is_array, thread_index);
        let comments = thread
            .get_mut("comments")
            .ok_or_else(|| fail("invalidInput", "Missing capture comments"))?;
        let comment = selected_mut(comments, comment_is_array, comment_index);
        let evidence = array_property(comment, "authorshipEvidence");
        let object = comment
            .as_object_mut()
            .ok_or_else(|| fail("invalidInput", "Capture comment is not an object"))?;
        object.insert("authorship".to_owned(), entry["kind"].clone());
        let mut evidence = evidence;
        evidence.push(Value::String(CLASSIFICATION_EVIDENCE.to_owned()));
        object.insert("authorshipEvidence".to_owned(), Value::Array(evidence));
    }
    Ok(())
}

/// Coordinates locate comments without retaining references during mutation.
struct ThreadCoordinates {
    thread: usize,
    comments: HashMap<String, usize>,
}

/// Indexes identifiers once so each decision can locate its captured comment.
fn comment_coordinates(document: &Value) -> HashMap<String, ThreadCoordinates> {
    let mut index = HashMap::new();
    for (thread_index, thread) in list_values(property(document, "threads"))
        .iter()
        .enumerate()
    {
        let mut comments = HashMap::new();
        for (comment_index, comment) in list_values(property(thread, "comments")).iter().enumerate()
        {
            comments.insert(string_id(property(comment, "id")), comment_index);
        }
        // The PowerShell index keeps the last thread and last comment for each
        // case-sensitive identifier, including when a thread ID is repeated.
        index.insert(
            string_id(property(thread, "id")),
            ThreadCoordinates {
                thread: thread_index,
                comments,
            },
        );
    }
    index
}

/// Recovers a value after locating its coordinate without changing its shape.
fn selected(value: &Value, array: bool, index: usize) -> &Value {
    if array { &value[index] } else { value }
}

/// Recovers the exclusive reference corresponding to a validated coordinate.
fn selected_mut(value: &mut Value, array: bool, index: usize) -> &mut Value {
    if array { &mut value[index] } else { value }
}

/// Reads missing properties as null, matching the capture model.
fn property<'a>(value: &'a Value, name: &str) -> &'a Value {
    value.get(name).unwrap_or(&Value::Null)
}

/// Treats a scalar as a singleton list, as the PowerShell model does.
fn array_property(value: &Value, name: &str) -> Vec<Value> {
    match property(value, name) {
        Value::Null => Vec::new(),
        Value::Array(values) => values.clone(),
        value => vec![value.clone()],
    }
}

/// Removes only the marker added by semantic classification.
fn filtered_evidence(value: &Value) -> Vec<Value> {
    let values = match value {
        Value::Null => Vec::new(),
        Value::Array(values) => values.clone(),
        value => vec![value.clone()],
    };
    values
        .into_iter()
        .filter(|value| value.as_str() != Some(CLASSIFICATION_EVIDENCE))
        .collect()
}

/// Converts provider identifiers using PowerShell's scalar string conversion.
fn string_id(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(value) => value.clone(),
        Value::Bool(value) => if *value { "True" } else { "False" }.to_owned(),
        _ => value.to_string(),
    }
}

/// Checks a required nonempty JSON string.
fn nonempty(value: Option<&Value>) -> bool {
    value
        .and_then(Value::as_str)
        .is_some_and(|value| !value.is_empty())
}

/// Checks the schema's uppercase SHA-256 representation.
fn is_hash(value: Option<&Value>) -> bool {
    value.and_then(Value::as_str).is_some_and(|value| {
        value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'A'..=b'F').contains(&byte))
    })
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[expect(
    clippy::indexing_slicing,
    reason = "The tests construct or load fixtures with known thread and comment coordinates"
)]
mod tests {
    use serde_json::{Value, json};

    use super::{
        apply_decisions, comment_input_hash, evidence_hash, parse_file, snapshot_id,
        validate_capture_keys, validate_capture_structure,
    };
    use crate::document_hash;

    fn fixture() -> Value {
        parse_file(include_bytes!(
            "../../../tests/fixtures/ss-review-query-capture.json"
        ))
        .unwrap()
    }

    #[test]
    fn existing_fixture_matches_powershell_hashes() {
        let document = fixture();
        assert_eq!(
            document_hash(&document).unwrap(),
            "9EBFC89794C925995FA3FC0BB44EFF7CF94001EA71C3F27ECA4F9A4E23053289"
        );
        assert_eq!(
            snapshot_id(&document).unwrap(),
            "FEF75D9836BAC16552C0569D1172931FFAC8B38B8B61E2F540DE07DDD4A10AF5"
        );
        assert_eq!(
            evidence_hash(&document).unwrap(),
            "EAF684FCF0E83CB9619A69CB5145868C12E5AFD3B1CB5D66B4F779233D27E94F"
        );
        assert_eq!(
            comment_input_hash(&document, "T", &document["threads"][0]["comments"][0]).unwrap(),
            "E164CBD830B371E31C58EB2D2819AF0D3EE07AEED04D4F491E4264825A24537C"
        );
    }

    #[test]
    fn decisions_require_matching_capture_policy_and_input() {
        let mut document = fixture();
        let evidence = evidence_hash(&document).unwrap();
        let snapshot = snapshot_id(&document).unwrap();
        let hash =
            comment_input_hash(&document, "T", &document["threads"][0]["comments"][0]).unwrap();
        let mut sidecar = json!({
            "schemaVersion": 1, "policyVersion": "2",
            "captureHash": document_hash(&document).unwrap(),
            "decisions": [{"threadId": "T", "commentId": "C", "inputHash": hash, "kind": "agent", "origin": "classified"}]
        });
        let mut stale = sidecar.clone();
        stale["policyVersion"] = json!("3");
        apply_decisions(&mut document, &stale).unwrap();
        assert_eq!(snapshot_id(&document).unwrap(), snapshot);
        stale["policyVersion"] = json!("2");
        stale["decisions"][0]["inputHash"] = json!("0".repeat(64));
        apply_decisions(&mut document, &stale).unwrap();
        assert_eq!(snapshot_id(&document).unwrap(), snapshot);
        apply_decisions(&mut document, &sidecar).unwrap();
        assert_eq!(document["threads"][0]["comments"][0]["authorship"], "agent");
        assert_eq!(evidence_hash(&document).unwrap(), evidence);
        assert_ne!(snapshot_id(&document).unwrap(), snapshot);
        apply_decisions(&mut document, &sidecar).unwrap_err();
        let duplicate = sidecar["decisions"][0].clone();
        sidecar["decisions"].as_array_mut().unwrap().push(duplicate);
        apply_decisions(&mut fixture(), &sidecar).unwrap_err();
    }

    #[test]
    fn sidecar_rejects_malformed_entries_and_unbound_captures() {
        let mut document = fixture();
        let sidecar = json!({
            "schemaVersion": 1, "policyVersion": "2",
            "captureHash": document_hash(&document).unwrap(),
            "decisions": [{"threadId": "T", "commentId": "C", "inputHash": "F".repeat(64), "kind": "human", "origin": "reused"}]
        });
        for change in [
            json!({"schemaVersion": 2}),
            json!({"policyVersion": ""}),
            json!({"captureHash": "a".repeat(64)}),
            json!({"extra": true}),
        ] {
            let mut invalid = sidecar.clone();
            invalid
                .as_object_mut()
                .unwrap()
                .extend(change.as_object().unwrap().clone());
            apply_decisions(&mut document.clone(), &invalid).unwrap_err();
        }
        let mut invalid = sidecar.clone();
        invalid["decisions"][0]["kind"] = json!("robot");
        apply_decisions(&mut document.clone(), &invalid).unwrap_err();
        let mut invalid = sidecar;
        invalid["captureHash"] = json!("0".repeat(64));
        apply_decisions(&mut document, &invalid).unwrap_err();
    }

    #[test]
    fn scalar_threads_and_comments_keep_their_shape() {
        let mut document = fixture();
        document["threads"] = document["threads"][0].clone();
        document["threads"]["comments"] = document["threads"]["comments"][0].clone();
        let comment = &document["threads"]["comments"];
        let sidecar = json!({
            "schemaVersion": 1, "policyVersion": "2",
            "captureHash": document_hash(&document).unwrap(),
            "decisions": [{"threadId": "T", "commentId": "C", "inputHash": comment_input_hash(&document, "T", comment).unwrap(), "kind": "human", "origin": "classified"}]
        });
        let evidence = evidence_hash(&document).unwrap();
        apply_decisions(&mut document, &sidecar).unwrap();
        assert_eq!(document["threads"]["comments"]["authorship"], "human");
        assert_eq!(evidence_hash(&document).unwrap(), evidence);
    }

    #[test]
    fn decisions_use_last_thread_and_comment_with_case_sensitive_ids() {
        let mut document = fixture();
        let mut first = document["threads"][0].clone();
        let mut old_only = first["comments"][0].clone();
        old_only["id"] = json!("old-only");
        first["comments"].as_array_mut().unwrap().push(old_only);
        let mut last = first.clone();
        let upper = last["comments"][0].clone();
        let mut lower = upper.clone();
        lower["id"] = json!("c");
        last["comments"] = json!([upper.clone(), upper, lower]);
        let mut lower_thread = last.clone();
        lower_thread["id"] = json!("t");
        document["threads"] = json!([first, last, lower_thread]);
        let upper_hash =
            comment_input_hash(&document, "T", &document["threads"][1]["comments"][1]).unwrap();
        let lower_hash =
            comment_input_hash(&document, "T", &document["threads"][1]["comments"][2]).unwrap();
        let lower_thread_hash =
            comment_input_hash(&document, "t", &document["threads"][2]["comments"][1]).unwrap();
        let old_hash =
            comment_input_hash(&document, "T", &document["threads"][0]["comments"][1]).unwrap();
        let sidecar = json!({
            "schemaVersion": 1, "policyVersion": "2",
            "captureHash": document_hash(&document).unwrap(),
            "decisions": [
                {"threadId": "T", "commentId": "C", "inputHash": upper_hash, "kind": "agent", "origin": "classified"},
                {"threadId": "T", "commentId": "c", "inputHash": lower_hash, "kind": "human", "origin": "classified"},
                {"threadId": "t", "commentId": "C", "inputHash": lower_thread_hash, "kind": "human", "origin": "classified"},
                {"threadId": "T", "commentId": "old-only", "inputHash": old_hash, "kind": "human", "origin": "classified"}
            ]
        });

        apply_decisions(&mut document, &sidecar).unwrap();
        assert_eq!(document["threads"][0], first);
        assert_eq!(
            document["threads"][1]["comments"][0]["authorship"],
            "indeterminate"
        );
        assert_eq!(document["threads"][1]["comments"][1]["authorship"], "agent");
        assert_eq!(document["threads"][1]["comments"][2]["authorship"], "human");
        assert_eq!(document["threads"][2]["comments"][1]["authorship"], "human");
    }

    #[test]
    fn provider_classification_takes_precedence() {
        let mut document = fixture();
        document["threads"][0]["comments"][0]["authorship"] = json!("human");
        let comment = &document["threads"][0]["comments"][0];
        let sidecar = json!({
            "schemaVersion": 1, "policyVersion": "2",
            "captureHash": document_hash(&document).unwrap(),
            "decisions": [{"threadId": "T", "commentId": "C", "inputHash": comment_input_hash(&document, "T", comment).unwrap(), "kind": "agent", "origin": "reused"}]
        });
        let before = document.clone();
        apply_decisions(&mut document, &sidecar).unwrap();
        assert_eq!(document, before);
    }

    #[test]
    fn sidecar_rejects_duplicate_properties_before_validation() {
        let document = fixture();
        let sidecar = format!(
            r#"{{"SchemaVersion":0,"schemaVersion":1,"policyVersion":"2","captureHash":"{}","decisions":[]}}"#,
            document_hash(&document).unwrap()
        );
        parse_file(sidecar.as_bytes()).unwrap_err();
    }

    #[test]
    fn noncanonical_capture_fields_are_rejected() {
        let mut document = fixture();
        let comment = document["threads"][0]["comments"][0]
            .as_object_mut()
            .unwrap();
        let content = comment.shift_remove("content").unwrap();
        comment.insert("Content".to_owned(), content);
        validate_capture_keys(&document).unwrap_err();
        evidence_hash(&document).unwrap_err();

        let mut document = fixture();
        let context = document["threads"][0]["context"].as_object_mut().unwrap();
        let path = context.shift_remove("filePath").unwrap();
        context.insert("FilePath".to_owned(), path);
        validate_capture_keys(&document).unwrap_err();
    }

    #[test]
    fn opaque_native_metadata_matches_powershell_hashes() {
        let mut document = fixture();
        document["threads"][0]["properties"] = json!({
            "Id":"native","Content":"metadata","Deep":{"FilePath":"opaque"},
            "é":"accent","ø":"letter"
        });
        document["threads"][0]["comments"][0]["author"] =
            json!({"Id":"A","DisplayName":"Reviewer"});
        document["reviewers"] = json!([{"Id":"R","Content":"native"}]);
        validate_capture_keys(&document).unwrap();
        validate_capture_structure(&document).unwrap();
        assert_eq!(
            document_hash(&document).unwrap(),
            "017C82871F9EDD1B05CE1866C354DE43147573E3643742F7AE79C6859FD5C838"
        );
        assert_eq!(
            snapshot_id(&document).unwrap(),
            "B93804742D01D49AA9CE4E29CFD12E9CF035B95EA065B7FB0E82217D20DEA5B6"
        );
        assert_eq!(
            evidence_hash(&document).unwrap(),
            "1910AFBDC9D53474E7645466191D68B52B053E31015C4794B0C88CB942C0E0A6"
        );
    }

    #[test]
    fn required_evidence_is_not_treated_as_an_empty_capture() {
        let document = fixture();
        validate_capture_structure(&document).unwrap();
        let mut empty = document.clone();
        empty["threads"] = json!([]);
        validate_capture_structure(&empty).unwrap();
        for field in ["threads", "reviewers", "revisions", "pullRequest"] {
            let mut invalid = document.clone();
            invalid.as_object_mut().unwrap().shift_remove(field);
            assert!(validate_capture_structure(&invalid).is_err(), "{field}");
            invalid[field] = json!(null);
            assert!(validate_capture_structure(&invalid).is_err(), "{field}");
        }
        for field in [
            "title",
            "description",
            "reviewedSourceCommit",
            "reviewedMergeBaseCommit",
        ] {
            let mut invalid = document.clone();
            invalid["pullRequest"]
                .as_object_mut()
                .unwrap()
                .shift_remove(field);
            assert!(validate_capture_structure(&invalid).is_err(), "{field}");
        }
        for field in [
            "id",
            "comments",
            "kind",
            "resolution",
            "isAuthoredBySelf",
            "isOutdated",
            "context",
            "decision",
            "openingCommentId",
            "url",
            "eventUtc",
            "lastUpdatedUtc",
        ] {
            let mut invalid = document.clone();
            invalid["threads"][0]
                .as_object_mut()
                .unwrap()
                .shift_remove(field);
            assert!(validate_capture_structure(&invalid).is_err(), "{field}");
        }
        for field in [
            "id",
            "content",
            "author",
            "authorship",
            "authorshipEvidence",
            "isAuthoredBySelf",
            "parentId",
            "url",
            "publishedUtc",
            "lastEditedUtc",
        ] {
            let mut invalid = document.clone();
            invalid["threads"][0]["comments"][0]
                .as_object_mut()
                .unwrap()
                .shift_remove(field);
            assert!(validate_capture_structure(&invalid).is_err(), "{field}");
        }
        let mut invalid = document.clone();
        invalid["threads"] = json!([42]);
        validate_capture_structure(&invalid).unwrap_err();
        invalid = document.clone();
        invalid["threads"][0]["comments"] = json!([null]);
        validate_capture_structure(&invalid).unwrap_err();
        invalid = document.clone();
        invalid["threads"][0]["context"] = json!({});
        validate_capture_structure(&invalid).unwrap_err();
        invalid = document;
        invalid["threads"][0]["comments"][0]["content"] = json!(null);
        validate_capture_structure(&invalid).unwrap();
    }

    #[test]
    fn queries_do_not_report_missing_discussion_as_empty() {
        let document = fixture();
        let mut incomplete = document.clone();
        incomplete.as_object_mut().unwrap().shift_remove("threads");
        crate::query(&incomplete, &json!({"query":"threads"}), None, None).unwrap_err();
        crate::query(
            &document,
            &json!({"query":"changes"}),
            Some(&incomplete),
            None,
        )
        .unwrap_err();
    }
}
