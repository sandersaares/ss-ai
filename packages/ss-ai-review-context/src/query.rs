//! Projects captured pull request evidence into bounded, resumable pages.

use std::{
    borrow::Cow,
    collections::{HashMap, VecDeque},
    slice,
};

use ohno::AppError;
use regex_syntax::hir::{ClassUnicode, ClassUnicodeRange};
use serde_json::{Map, Value, json};
use ss_ai_runtime::{caused_by, fail};

use crate::{document_hash, evidence_hash, powershell_json, snapshot_id};

// The executable adds a newline to the JSON, so the page reserves room for it.
const MAX_BYTES: usize = 16 * 1024;
// Discovery excerpts are hints rather than a substitute for reading the comment.
const EXCERPT_LENGTH: usize = 240;

/// Validates and canonicalizes the selectors that define a query.
///
/// # Errors
///
/// Returns an error when the selectors are invalid or incompatible.
pub fn normalize_options(options: &Value) -> Result<Value, AppError> {
    let object = options
        .as_object()
        .ok_or_else(|| invalid("Options must be an object."))?;
    let allowed = [
        "query",
        "field",
        "files",
        "scope",
        "kind",
        "resolution",
        "threadId",
        "commentId",
        "authorship",
        "indexOnly",
        "text",
        "pageSize",
    ];
    if object.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err(invalid("Unknown query option."));
    }
    let query = string_option(options, "query")?.to_lowercase();
    let mut scope = string_option_default(options, "scope", "all")?.to_lowercase();
    let field = string_option(options, "field")?;
    let kind = string_option(options, "kind")?;
    let resolution = string_option(options, "resolution")?;
    let thread_id = string_option(options, "threadId")?;
    let comment_id = string_option(options, "commentId")?;
    let authorship = string_option(options, "authorship")?;
    let text = string_option(options, "text")?;
    let index_only = match options.get("indexOnly") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(value)) => *value,
        _ => return Err(invalid("IndexOnly must be a boolean.")),
    };
    let page_size = match options.get("pageSize") {
        None | Some(Value::Null) => 20,
        Some(value) => value
            .as_u64()
            .ok_or_else(|| invalid("PageSize must be between 1 and 50."))?,
    };
    if !["metadata", "threads", "comments", "search", "changes"].contains(&query.as_str()) {
        return Err(invalid(
            "Select metadata, threads, comments, search or changes.",
        ));
    }
    if !["all", "files", "global"].contains(&scope.as_str()) {
        return Err(invalid("Scope must be all, files or global."));
    }
    if !(1..=50).contains(&page_size) {
        return Err(invalid("PageSize must be between 1 and 50."));
    }
    let fields = [
        "platform",
        "repositoryUrl",
        "pullRequestUrl",
        "fetchedUtc",
        "target",
        "authenticatedIdentity",
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
        "reviewers",
        "revisions",
    ];
    if query == "metadata" && !fields.contains(&field.as_str()) {
        return Err(invalid(
            "Metadata requires one supported -Field; see query-pr-context.md.",
        ));
    }
    if query != "metadata" && !field.is_empty() {
        return Err(invalid("-Field is only valid for metadata."));
    }
    if query == "search" && text.trim().is_empty() {
        return Err(invalid("Search requires a nonempty literal -Text."));
    }
    if query != "search" && !text.is_empty() {
        return Err(invalid("-Text is only valid for search."));
    }
    if !kind.is_empty()
        && ![
            "text",
            "reviewSummary",
            "refUpdate",
            "reviewersUpdated",
            "policy",
            "system",
        ]
        .contains(&kind.as_str())
    {
        return Err(invalid("Unknown thread kind."));
    }
    if !resolution.is_empty()
        && !["active", "resolved", "indeterminate"].contains(&resolution.as_str())
    {
        return Err(invalid("Unknown resolution."));
    }
    if !authorship.is_empty() && !["agent", "human", "indeterminate"].contains(&authorship.as_str())
    {
        return Err(invalid("Unknown authorship."));
    }
    if query != "comments"
        && (!thread_id.is_empty() || !comment_id.is_empty() || !authorship.is_empty())
    {
        return Err(invalid(
            "ThreadId, CommentId and Authorship are only valid for comments.",
        ));
    }
    if index_only && query != "comments" {
        return Err(invalid("-IndexOnly is only valid for comments."));
    }
    if !comment_id.is_empty() && thread_id.is_empty() {
        return Err(invalid("-CommentId requires -ThreadId."));
    }
    let files = match options.get("files") {
        None | Some(Value::Null) => &[][..],
        Some(Value::Array(files)) => files.as_slice(),
        _ => return Err(invalid("File selectors must be strings.")),
    };
    if ["metadata", "changes"].contains(&query.as_str())
        && (!files.is_empty() || scope != "all" || !kind.is_empty() || !resolution.is_empty())
    {
        return Err(invalid(
            "Discussion filters are only valid for threads, comments or search.",
        ));
    }
    let mut paths = Vec::with_capacity(files.len());
    for file in files {
        let file = file
            .as_str()
            .ok_or_else(|| invalid("File selectors must be strings."))?;
        let path = normalize_path(file);
        if path.trim().is_empty() || path.split('/').any(|part| matches!(part, "" | "." | "..")) {
            return Err(invalid(
                "File selectors must be repository-relative paths without empty, dot or parent segments.",
            ));
        }
        paths.push(path);
    }
    if !paths.is_empty() {
        if scope == "global" {
            return Err(invalid(
                "File selectors cannot be combined with global scope.",
            ));
        }
        "files".clone_into(&mut scope);
    } else if scope == "files" {
        return Err(invalid("Files scope requires file selectors."));
    }
    if query == "comments" && thread_id.is_empty() && authorship.is_empty() && scope == "all" {
        return Err(invalid(
            "Comments requires a thread, authorship, file or global selector. Discover discussion with threads or search first.",
        ));
    }
    Ok(json!({
        "query": query, "field": field, "files": paths, "scope": scope,
        "kind": kind, "resolution": resolution, "threadId": thread_id, "commentId": comment_id,
        "authorship": authorship, "indexOnly": index_only, "text": text, "pageSize": page_size
    }))
}

/// Reads one bounded page from an immutable capture.
///
/// # Errors
///
/// Returns an error for invalid selectors, cursors, or unpageable evidence.
pub fn query(
    document: &Value,
    options: &Value,
    previous: Option<&Value>,
    cursor: Option<&str>,
) -> Result<String, AppError> {
    let options = normalize_options(options)?;
    if previous.is_some() && text(&options, "query") != "changes" {
        return Err(invalid("-ComparePath is only valid for changes."));
    }
    let snapshot = snapshot_id(document)?;
    let baseline = previous.map(document_hash).transpose()?;
    let fingerprint = document_hash(
        &json!({"version": 1, "snapshot": snapshot, "baseline": baseline, "options": options}),
    )?;
    let records = records(document, &options, previous)?;
    page(&records, &options, &snapshot, &fingerprint, cursor)
}

/// Carries attribution separately from text so large bodies can be split.
struct Record<'a> {
    data: Map<String, Value>,
    text: Option<&'a str>,
}

/// Creates the common error classification for malformed query inputs.
fn invalid(message: &str) -> AppError {
    fail("invalidInput", message)
}

/// Reads a selector string while rejecting malformed types.
fn string_option(options: &Value, name: &str) -> Result<String, AppError> {
    string_option_default(options, name, "")
}

/// Reads an optional selector and applies its default.
fn string_option_default(options: &Value, name: &str, default: &str) -> Result<String, AppError> {
    match options.get(name) {
        None | Some(Value::Null) => Ok(default.to_owned()),
        Some(Value::String(value)) => Ok(value.clone()),
        _ => Err(invalid("Query selectors must have the expected types.")),
    }
}

/// Returns a string property, using the empty string for absent values.
fn text<'a>(value: &'a Value, name: &str) -> &'a str {
    value.get(name).and_then(Value::as_str).unwrap_or("")
}

/// Returns an array property, treating absent properties as empty.
fn array<'a>(value: &'a Value, name: &str) -> &'a [Value] {
    match value.get(name) {
        None | Some(Value::Null) => &[],
        Some(Value::Array(values)) => values,
        Some(value) => slice::from_ref(value),
    }
}

/// Interprets provider identifiers as the string selectors used by the query.
fn identifier<'a>(value: &'a Value, name: &str) -> Cow<'a, str> {
    match value.get(name) {
        Some(Value::String(value)) => Cow::Borrowed(value),
        Some(Value::Number(value)) => Cow::Owned(value.to_string()),
        Some(Value::Bool(value)) => Cow::Borrowed(if *value { "True" } else { "False" }),
        _ => Cow::Borrowed(""),
    }
}

/// Copies a property into a projection, preserving explicit nulls.
fn property(value: &Value, name: &str) -> Value {
    value.get(name).cloned().unwrap_or(Value::Null)
}

/// Normalizes provider path separators without consulting the local filesystem.
fn normalize_path(path: &str) -> String {
    path.replace('\\', "/").trim_start_matches('/').to_owned()
}

/// Builds the stable thread attribution shared by discovery and comments.
fn thread_metadata(thread: &Value) -> Map<String, Value> {
    let mut data = Map::new();
    data.insert("threadId".into(), property(thread, "id"));
    for key in [
        "url",
        "openingCommentId",
        "kind",
        "resolution",
        "isOutdated",
        "context",
        "decision",
        "isAuthoredBySelf",
        "eventUtc",
        "lastUpdatedUtc",
    ] {
        data.insert(key.into(), property(thread, key));
    }
    data
}

/// Tests discussion filters against provider-independent thread metadata.
fn matches_thread(thread: &Value, options: &Value) -> bool {
    for key in ["kind", "resolution"] {
        if !text(options, key).is_empty() && text(options, key) != text(thread, key) {
            return false;
        }
    }
    let path = thread
        .get("context")
        .map_or("", |context| text(context, "filePath"));
    match text(options, "scope") {
        "global" => path.is_empty(),
        "files" => {
            !path.is_empty()
                && array(options, "files")
                    .iter()
                    .any(|file| file.as_str() == Some(normalize_path(path).as_str()))
        }
        _ => true,
    }
}

/// Reuses a linear-time literal search across discussion records.
struct LiteralSearch {
    needle: Vec<char>,
    prefixes: Vec<usize>,
    folded: HashMap<char, char>,
}

impl LiteralSearch {
    /// Builds the simple-folded pattern and its linear-time prefix table.
    #[expect(
        clippy::arithmetic_side_effects,
        clippy::indexing_slicing,
        reason = "the prefix table contains only positions strictly before the current index"
    )]
    fn new(needle: &str) -> Result<Self, AppError> {
        let mut folded = HashMap::new();
        let mut pattern = Vec::new();
        for character in needle.chars() {
            let value = match folded.get(&character) {
                Some(&value) => value,
                None => {
                    let value = simple_fold(character)?;
                    folded.insert(character, value);
                    value
                }
            };
            pattern.push(value);
        }
        // KMP fallback keeps the longest pattern prefix that is also a suffix
        // of the matched text, so overlapping matches need no rescan.
        let mut prefixes = vec![0; pattern.len()];
        for index in 1..pattern.len() {
            let mut matched = prefixes[index - 1];
            while matched > 0 && pattern[index] != pattern[matched] {
                let previous = matched;
                matched = prefixes[matched - 1];
                debug_assert!(matched < previous, "prefix fallback must make progress");
            }
            if pattern[index] == pattern[matched] {
                matched += 1;
            }
            prefixes[index] = matched;
        }
        Ok(Self {
            needle: pattern,
            prefixes,
            folded,
        })
    }

    /// Streams text and reports positions in UTF-16 units of the original text.
    #[expect(
        clippy::arithmetic_side_effects,
        clippy::indexing_slicing,
        reason = "the prefix table bounds all pattern indices and the scan returns on a full match"
    )]
    fn find(&self, haystack: &str) -> Result<Option<usize>, AppError> {
        if self.needle.is_empty() {
            return Ok(Some(0));
        }
        // Preserve original UTF-16 starts independently of folded characters.
        let mut positions = VecDeque::new();
        let mut offset = 0_usize;
        let mut matched = 0;
        for character in haystack.chars() {
            let value = match self.folded.get(&character) {
                Some(&value) => value,
                None => simple_fold(character)?,
            };
            positions.push_back(offset);
            if positions.len() > self.needle.len() {
                positions.pop_front();
            }
            offset += character.len_utf16();
            while matched > 0 && self.needle[matched] != value {
                let previous = matched;
                matched = self.prefixes[matched - 1];
                debug_assert!(matched < previous, "prefix fallback must make progress");
            }
            if self.needle[matched] == value {
                matched += 1;
            }
            if matched == self.needle.len() {
                return Ok(positions.front().copied());
            }
        }
        Ok(None)
    }
}

/// Selects a stable representative from the Unicode simple-fold equivalence class.
fn simple_fold(character: char) -> Result<char, AppError> {
    if character.is_ascii() {
        return Ok(character.to_ascii_uppercase());
    }
    let mut class = ClassUnicode::new([ClassUnicodeRange::new(character, character)]);
    class.try_case_fold_simple().map_err(|error| {
        caused_by(
            "invalidInput",
            "Unicode simple-fold data is unavailable.",
            error,
        )
    })?;
    Ok(class
        .ranges()
        .first()
        .map_or(character, ClassUnicodeRange::start))
}

/// Adds a UTF-16-indexed excerpt without splitting a surrogate pair.
#[expect(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::integer_division,
    reason = "the offset is a known text position and both endpoints are clamped to the UTF-16 length"
)]
fn add_excerpt(data: &mut Map<String, Value>, content: &str, offset: usize) {
    let chars: Vec<u16> = content.encode_utf16().collect();
    // Favor context following the match to help identify the surrounding discussion.
    let mut start = offset.saturating_sub(EXCERPT_LENGTH / 4);
    if start > 0 && start < chars.len() && (0xDC00..=0xDFFF).contains(&chars[start]) {
        start -= 1;
    }
    let mut end = (start + EXCERPT_LENGTH).min(chars.len());
    if end > start && (0xD800..=0xDBFF).contains(&chars[end - 1]) {
        end -= 1;
    }
    data.insert(
        "excerpt".into(),
        Value::String(String::from_utf16_lossy(&chars[start..end])),
    );
    data.insert("excerptOffset".into(), json!(start));
    data.insert("contentLength".into(), json!(chars.len()));
}

/// Materializes ordered records before applying the output budget.
fn records<'a>(
    document: &'a Value,
    options: &Value,
    previous: Option<&'a Value>,
) -> Result<Vec<Record<'a>>, AppError> {
    let query = text(options, "query");
    let mut result = Vec::new();
    if query == "metadata" {
        let field = text(options, "field");
        let source = if [
            "platform",
            "repositoryUrl",
            "pullRequestUrl",
            "fetchedUtc",
            "target",
            "authenticatedIdentity",
            "reviewers",
            "revisions",
        ]
        .contains(&field)
        {
            document
        } else {
            &document["pullRequest"]
        };
        if ["reviewers", "revisions"].contains(&field) {
            for entry in array(source, field) {
                result.push(Record {
                    data: map(json!({"field": field, "value": entry})),
                    text: None,
                });
            }
        } else if let Some(value) = source.get(field).and_then(Value::as_str) {
            result.push(Record {
                data: map(json!({"field": field})),
                text: Some(value),
            });
        } else {
            result.push(Record {
                data: map(json!({"field": field, "value": property(source, field)})),
                text: None,
            });
        }
        return Ok(result);
    }
    if query == "changes" {
        let previous = previous.ok_or_else(|| invalid("Changes requires -ComparePath."))?;
        if property(document, "pullRequestUrl") != property(previous, "pullRequestUrl") {
            return Err(invalid(
                "Cannot compare captures of different pull requests.",
            ));
        }
        return changes(previous, document);
    }
    let search = (query == "search")
        .then(|| LiteralSearch::new(text(options, "text")))
        .transpose()?;
    if let Some(search) = &search
        && text(options, "scope") != "files"
        && text(options, "kind").is_empty()
        && text(options, "resolution").is_empty()
    {
        let description = text(&document["pullRequest"], "description");
        if let Some(offset) = search.find(description)? {
            let mut data = map(
                json!({"field": "description", "url": property(document, "pullRequestUrl"), "matchOffset": offset}),
            );
            add_excerpt(&mut data, description, offset);
            result.push(Record { data, text: None });
        }
    }
    let mut thread_found = false;
    let mut comment_found = false;
    for thread in array(document, "threads") {
        if !text(options, "threadId").is_empty()
            && identifier(thread, "id") != text(options, "threadId")
        {
            continue;
        }
        thread_found = true;
        let comments = array(thread, "comments");
        if !text(options, "commentId").is_empty()
            && comments
                .iter()
                .any(|comment| identifier(comment, "id") == text(options, "commentId"))
        {
            comment_found = true;
        }
        if !matches_thread(thread, options) {
            continue;
        }
        if query == "threads" {
            let mut data = thread_metadata(thread);
            data.insert("commentCount".into(), json!(comments.len()));
            if let Some(comment) = comments
                .iter()
                .find(|comment| identifier(comment, "id") == identifier(thread, "openingCommentId"))
            {
                add_excerpt(&mut data, text(comment, "content"), 0);
            }
            result.push(Record { data, text: None });
            continue;
        }
        for comment in comments {
            if !text(options, "commentId").is_empty()
                && identifier(comment, "id") != text(options, "commentId")
            {
                continue;
            }
            let authorship = comment
                .get("authorship")
                .and_then(Value::as_str)
                .unwrap_or("indeterminate");
            if !text(options, "authorship").is_empty() && text(options, "authorship") != authorship
            {
                continue;
            }
            let content = text(comment, "content");
            let offset = if let Some(search) = &search {
                let Some(offset) = search.find(content)? else {
                    continue;
                };
                offset
            } else {
                0
            };
            let mut data = thread_metadata(thread);
            data.insert("commentId".into(), property(comment, "id"));
            data.insert("commentUrl".into(), property(comment, "url"));
            for key in [
                "parentId",
                "author",
                "authorship",
                "authorshipEvidence",
                "isAuthoredBySelf",
                "publishedUtc",
                "lastEditedUtc",
            ] {
                if key == "isAuthoredBySelf" {
                    data.insert("threadIsAuthoredBySelf".into(), property(thread, key));
                }
                data.insert(key.into(), property(comment, key));
            }
            let body = if query == "search" {
                data.insert("matchOffset".into(), json!(offset));
                add_excerpt(&mut data, content, offset);
                None
            } else if options["indexOnly"] == true {
                data.insert(
                    "contentLength".into(),
                    json!(content.encode_utf16().count()),
                );
                None
            } else {
                Some(content)
            };
            result.push(Record { data, text: body });
        }
    }
    if !text(options, "threadId").is_empty() && !thread_found {
        return Err(invalid(
            "The capture contains no thread with the requested identifier.",
        ));
    }
    if !text(options, "commentId").is_empty() && !comment_found {
        return Err(invalid(
            "The requested thread contains no comment with that identifier.",
        ));
    }
    Ok(result)
}

/// Converts a known object projection into its insertion-ordered map.
fn map(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(object) => object,
        _ => Map::new(),
    }
}

/// Produces freshness records in the same category and capture order as the model.
fn changes<'a>(previous: &'a Value, current: &'a Value) -> Result<Vec<Record<'a>>, AppError> {
    let before = indexed(array(previous, "threads"));
    let after = indexed(array(current, "threads"));
    let before_lookup: HashMap<Cow<'_, str>, &Value> = before
        .iter()
        .map(|value| (identifier(value, "id"), *value))
        .collect();
    let after_lookup: HashMap<Cow<'_, str>, &Value> = after
        .iter()
        .map(|value| (identifier(value, "id"), *value))
        .collect();
    let mut added_threads = Vec::new();
    let mut removed_threads = Vec::new();
    let mut added_comments = Vec::new();
    let mut removed_comments = Vec::new();
    let mut edited_comments = Vec::new();
    let mut resolutions = Vec::new();
    for thread in &after {
        let id = identifier(thread, "id");
        let matching = before_lookup.get(id.as_ref()).copied();
        if matching.is_none() {
            added_threads.push(json!({"threadId": id}));
        }
        let previous_comments =
            indexed(matching.map_or(&[][..], |thread| array(thread, "comments")));
        let current_comments = indexed(array(thread, "comments"));
        let previous_lookup: HashMap<Cow<'_, str>, &Value> = previous_comments
            .iter()
            .map(|value| (identifier(value, "id"), *value))
            .collect();
        let current_lookup: HashMap<Cow<'_, str>, &Value> = current_comments
            .iter()
            .map(|value| (identifier(value, "id"), *value))
            .collect();
        for comment in &current_comments {
            let comment_id = identifier(comment, "id");
            let old = previous_lookup.get(comment_id.as_ref()).copied();
            if let Some(old) = old {
                let old_hash = comment_input_hash(previous, &id, old)?;
                let new_hash = comment_input_hash(current, &id, comment)?;
                if property(old, "lastEditedUtc") != property(comment, "lastEditedUtc")
                    || old_hash != new_hash
                {
                    edited_comments.push(json!({"threadId": id, "commentId": comment_id}));
                }
            } else {
                added_comments.push(json!({"threadId": id, "commentId": comment_id}));
            }
        }
        if let Some(old_thread) = matching {
            for comment in &previous_comments {
                if !current_lookup.contains_key(identifier(comment, "id").as_ref()) {
                    removed_comments
                        .push(json!({"threadId": id, "commentId": identifier(comment, "id")}));
                }
            }
            let old = property(old_thread, "resolution");
            let new = property(thread, "resolution");
            if !case_insensitive_equal(&old, &new)? {
                resolutions.push(json!({"threadId": id, "from": old, "to": new}));
            }
        }
    }
    for thread in &before {
        let id = identifier(thread, "id");
        if !after_lookup.contains_key(id.as_ref()) {
            removed_threads.push(json!({"threadId": id}));
            for comment in indexed(array(thread, "comments")) {
                removed_comments
                    .push(json!({"threadId": id, "commentId": identifier(comment, "id")}));
            }
        }
    }
    let mut result = Vec::new();
    for (name, capture, values) in [
        ("threadsAdded", "current", added_threads),
        ("threadsRemoved", "baseline", removed_threads),
        ("commentsAdded", "current", added_comments),
        ("commentsRemoved", "baseline", removed_comments),
        ("commentsEdited", "both", edited_comments),
        ("resolutionChanged", "both", resolutions),
    ] {
        for value in values {
            let mut data = map(json!({"change": name, "capture": capture}));
            data.extend(map(value));
            result.push(Record { data, text: None });
        }
    }
    for key in ["reviewedSourceCommit", "reviewedMergeBaseCommit"] {
        let old = property(&previous["pullRequest"], key);
        let new = property(&current["pullRequest"], key);
        if !case_insensitive_equal(&old, &new)? {
            result.push(Record {
                data: map(json!({"change": key, "capture": "both", "from": old, "to": new})),
                text: None,
            });
        }
    }
    if evidence_hash(previous)? != evidence_hash(current)? {
        result.push(Record {
            data: map(json!({"change": "reviewEvidence", "capture": "both"})),
            text: None,
        });
    }
    Ok(result)
}

/// Summarizes the same changes exposed by bounded queries without returning bodies.
pub(crate) fn capture_delta(previous: &Value, current: &Value) -> Result<Value, AppError> {
    let records = changes(previous, current)?;
    let mut result = Map::new();
    for name in [
        "threadsAdded",
        "threadsRemoved",
        "commentsAdded",
        "commentsRemoved",
        "commentsEdited",
        "resolutionChanged",
    ] {
        let count = records
            .iter()
            .filter(|record| record.data.get("change").and_then(Value::as_str) == Some(name))
            .count();
        result.insert(format!("{name}Count"), json!(count));
    }
    let old = property(previous, "pullRequest");
    let new = property(current, "pullRequest");
    let mut commit_changed = false;
    for key in ["reviewedSourceCommit", "reviewedMergeBaseCommit"] {
        let before = property(&old, key);
        let after = property(&new, key);
        let changed = !case_insensitive_equal(&before, &after)?;
        commit_changed |= changed;
        result.insert(format!("{key}Changed"), json!(changed));
        let previous_key = match key {
            "reviewedSourceCommit" => "previousReviewedSourceCommit",
            _ => "previousReviewedMergeBaseCommit",
        };
        result.insert(previous_key.to_owned(), before);
        result.insert(key.to_owned(), after);
    }
    let evidence_changed = evidence_hash(previous)? != evidence_hash(current)?;
    result.insert("evidenceChanged".to_owned(), json!(evidence_changed));
    result.insert(
        "hasChanges".to_owned(),
        json!(evidence_changed || commit_changed),
    );
    Ok(Value::Object(result))
}

/// Indexes identifiers in capture order, retaining the last value for duplicates.
fn indexed(values: &[Value]) -> Vec<&Value> {
    let mut positions = HashMap::new();
    let mut ordered = Vec::new();
    for value in values {
        let id = identifier(value, "id");
        if let Some(position) = positions.get(&id) {
            if let Some(entry) = ordered.get_mut(*position) {
                *entry = value;
            }
        } else {
            positions.insert(id, ordered.len());
            ordered.push(value);
        }
    }
    ordered
}

/// Compares the fields used for authorship decisions, excluding classification progress.
fn comment_input_hash(
    document: &Value,
    thread_id: &str,
    comment: &Value,
) -> Result<String, AppError> {
    let evidence: Vec<Value> = array(comment, "authorshipEvidence")
        .iter()
        .filter(|value| value.as_str() != Some("classification:reviewed"))
        .cloned()
        .collect();
    document_hash(&json!({
        "schemaVersion": 1, "policyVersion": "2",
        "platform": property(document, "platform"), "repositoryUrl": property(document, "repositoryUrl"),
        "pullRequestUrl": property(document, "pullRequestUrl"), "threadId": thread_id,
        "commentId": identifier(comment, "id"), "content": property(comment, "content"),
        "author": property(comment, "author"), "isAuthoredBySelf": property(comment, "isAuthoredBySelf"),
        "evidence": evidence
    }))
}

/// Compares status and commit fields without guessing linguistic casing.
fn case_insensitive_equal(left: &Value, right: &Value) -> Result<bool, AppError> {
    match (left.as_str(), right.as_str()) {
        (Some(left), Some(right)) if left == right => Ok(true),
        (Some(left), Some(right))
            if left.bytes().all(|byte| (0x20..=0x7e).contains(&byte))
                && right.bytes().all(|byte| (0x20..=0x7e).contains(&byte)) =>
        {
            Ok(left.eq_ignore_ascii_case(right))
        }
        (Some(_), Some(_)) => Err(invalid(
            "Cannot compare these values exactly without matching PowerShell globalization data.",
        )),
        _ => Ok(left == right),
    }
}

/// Validates a continuation against the query identity and exact text boundary.
fn cursor_position(
    records: &[Record<'_>],
    fingerprint: &str,
    cursor: Option<&str>,
) -> Result<(usize, usize), AppError> {
    let Some(cursor) = cursor.filter(|cursor| !cursor.is_empty()) else {
        return Ok((0, 0));
    };
    let mut parts = cursor.split(':');
    let hash = parts.next().unwrap_or("");
    let index = parts.next().unwrap_or("");
    let offset = parts.next().unwrap_or("");
    if parts.next().is_some()
        || hash.len() != 64
        || !hash
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'A'..=b'F').contains(&c))
        || index.is_empty()
        || offset.is_empty()
        || !index.bytes().all(|c| c.is_ascii_digit())
        || !offset.bytes().all(|c| c.is_ascii_digit())
    {
        return Err(invalid("Invalid query cursor."));
    }
    if hash != fingerprint {
        return Err(invalid(
            "Stale or mismatched query cursor. Restart the query against the current capture with the intended arguments.",
        ));
    }
    let index = index
        .parse::<i32>()
        .ok()
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| invalid("Query cursor position is outside the result."))?;
    let offset = offset
        .parse::<i32>()
        .ok()
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| invalid("Query cursor position is outside the result."))?;
    let record = records
        .get(index)
        .ok_or_else(|| invalid("Query cursor position is outside the result."))?;
    match &record.text {
        None if offset != 0 => return Err(invalid("Query cursor text offset is invalid.")),
        Some(text) => {
            let units: Vec<u16> = text.encode_utf16().collect();
            if (offset >= units.len() && offset != 0)
                || (offset > 0
                    && units
                        .get(offset)
                        .is_some_and(|unit| (0xDC00..=0xDFFF).contains(unit)))
            {
                return Err(invalid("Query cursor text offset is invalid."));
            }
        }
        None => {}
    }
    Ok((index, offset))
}

/// Constructs and serializes the largest page that fits the executable budget.
#[expect(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::integer_division,
    reason = "cursor validation and the remaining-length bound constrain all indices and arithmetic"
)]
fn page(
    records: &[Record<'_>],
    options: &Value,
    snapshot: &str,
    fingerprint: &str,
    cursor: Option<&str>,
) -> Result<String, AppError> {
    let (mut index, mut offset) = cursor_position(records, fingerprint, cursor)?;
    let mut items: Vec<Value> = Vec::new();
    let mut serialized = None;
    let limit = usize::try_from(options["pageSize"].as_u64().unwrap_or(20))
        .map_err(|_error| invalid("PageSize must be between 1 and 50."))?;
    while index < records.len() && items.len() < limit {
        let record = &records[index];
        let units: Vec<u16> = record
            .text
            .as_ref()
            .map_or_else(Vec::new, |text| text.encode_utf16().collect());
        let remaining = units.len() - offset;
        let mut low = 0;
        let mut high = remaining.min(MAX_BYTES);
        let mut best: Option<(Value, usize, String)> = None;
        while low <= high {
            let previous_bounds = (low, high);
            let candidate = low + (high - low) / 2;
            let mut length = candidate;
            if length > 0 && (0xD800..=0xDBFF).contains(&units[offset + length - 1]) {
                length -= 1;
            }
            let item = fragment(record, &units, offset, length);
            let next_index = if length == remaining {
                index + 1
            } else {
                index
            };
            let next_offset = if next_index == index {
                offset + length
            } else {
                0
            };
            let complete = next_index == records.len();
            let next_cursor =
                (!complete).then(|| format!("{fingerprint}:{next_index}:{next_offset}"));
            items.push(item);
            let output = serialize_page(
                options,
                snapshot,
                records.len(),
                &items,
                next_cursor.as_deref(),
                complete,
            )?;
            let item = items.pop().expect("the trial item was just appended");
            let newline_bytes = if cfg!(windows) { 2 } else { 1 };
            if output.len() + newline_bytes <= MAX_BYTES {
                best = Some((item, length, output));
                low = candidate + 1;
            } else if candidate == 0 {
                break;
            } else {
                high = candidate - 1;
            }
            debug_assert!(
                low > high
                    || (low >= previous_bounds.0
                        && high <= previous_bounds.1
                        && (low > previous_bounds.0 || high < previous_bounds.1)),
                "page-size search must shrink its interval"
            );
        }
        let Some((item, length, output)) =
            best.filter(|(_, length, _)| remaining == 0 || *length > 0)
        else {
            if !items.is_empty() {
                break;
            }
            return Err(invalid(
                "A query item cannot fit its attribution and a text fragment in the output budget. Narrow the metadata selection.",
            ));
        };
        items.push(item);
        serialized = Some(output);
        if length < remaining {
            break;
        }
        index += 1;
        offset = 0;
    }
    match serialized {
        Some(output) => Ok(output),
        None => serialize_page(options, snapshot, records.len(), &items, None, true),
    }
}

/// Builds the evidence projection for one surrogate-safe text fragment.
#[expect(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    reason = "the page search bounds the fragment within the UTF-16 record"
)]
fn fragment(record: &Record<'_>, units: &[u16], offset: usize, length: usize) -> Value {
    let mut item = record.data.clone();
    if record.text.is_some() {
        item.insert(
            "content".into(),
            Value::String(String::from_utf16_lossy(&units[offset..offset + length])),
        );
        item.insert("contentOffset".into(), json!(offset));
        item.insert("contentLength".into(), json!(units.len()));
        item.insert(
            "contentComplete".into(),
            json!(offset + length == units.len()),
        );
    }
    Value::Object(item)
}

/// Serializes a page with the property order used by the query protocol.
fn serialize_page(
    options: &Value,
    snapshot: &str,
    count: usize,
    items: &[Value],
    cursor: Option<&str>,
    complete: bool,
) -> Result<String, AppError> {
    powershell_json(&json!({
        "schemaVersion": 1, "query": text(options, "query"), "snapshotId": snapshot,
        "totalCount": count, "items": items, "nextCursor": cursor, "complete": complete
    }))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[expect(
    clippy::indexing_slicing,
    clippy::assertions_on_result_states,
    reason = "fixture shapes and error assertions are explicit test expectations"
)]
mod tests {
    use serde_json::{Value, json};

    use super::{LiteralSearch, case_insensitive_equal, normalize_options, query};
    use crate::powershell_json;

    /// Loads the shared migration capture as an in-process test fixture.
    fn fixture() -> Value {
        serde_json::from_str(include_str!(
            "../../../tests/fixtures/ss-review-query-capture.json"
        ))
        .unwrap()
    }

    #[test]
    fn matches_serialized_migration_pages() {
        let document = fixture();
        let expected: Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/ss-review-query-expected.json"
        ))
        .unwrap();
        for (name, options) in [
            ("metadata", json!({"query":"metadata", "field":"title"})),
            (
                "comments",
                json!({"query":"comments", "threadId":"T", "commentId":"C"}),
            ),
        ] {
            assert_eq!(
                query(&document, &options, None, None).unwrap(),
                powershell_json(&expected[name]).unwrap()
            );
        }
    }

    #[test]
    fn validates_selectors_and_preserves_case_sensitive_ids() {
        assert!(normalize_options(&json!({"query":"comments"})).is_err());
        assert!(normalize_options(&json!({"query":"threads","files":["src/../file"]})).is_err());
        let document = fixture();
        assert!(
            query(
                &document,
                &json!({"query":"comments","threadId":"t"}),
                None,
                None
            )
            .is_err()
        );
        let page: Value = serde_json::from_str(
            &query(
                &document,
                &json!({"query":"threads","files":["src\\widget.rs"]}),
                None,
                None,
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(page["items"][0]["threadId"], "T");
    }

    #[test]
    fn supports_numeric_provider_ids_and_scalar_collections() {
        let mut document = fixture();
        let mut thread = document["threads"][0].clone();
        thread["id"] = json!(42);
        thread["openingCommentId"] = json!(7);
        let mut comment = thread["comments"][0].clone();
        comment["id"] = json!(7);
        thread["comments"] = comment;
        document["threads"] = thread;
        let output = query(
            &document,
            &json!({"query":"comments","threadId":"42","commentId":"7"}),
            None,
            None,
        )
        .unwrap();
        let page: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(page["items"][0]["threadId"], 42);
        assert_eq!(page["items"][0]["commentId"], 7);
        let mut current = document.clone();
        current["threads"]["comments"]["content"] = json!("edited");
        let changes: Value = serde_json::from_str(
            &query(&current, &json!({"query":"changes"}), Some(&document), None).unwrap(),
        )
        .unwrap();
        assert_eq!(changes["items"][0]["threadId"], "42");
        assert_eq!(changes["items"][0]["commentId"], "7");
    }

    #[test]
    fn searches_using_unicode_simple_case_folding() {
        for (left, right) in [
            ("ᾀ", "ᾈ"),
            ("σ", "ς"),
            ("𐐨", "𐐀"),
            ("ſ", "S"),
            ("ß", "ẞ"),
            ("K", "k"),
        ] {
            assert_eq!(
                LiteralSearch::new(right).unwrap().find(left).unwrap(),
                Some(0)
            );
            assert_eq!(
                LiteralSearch::new(left).unwrap().find(right).unwrap(),
                Some(0)
            );
        }
        for (left, right) in [
            ("ı", "I"),
            ("İ", "i"),
            ("ß", "ss"),
            ("ﬃ", "ffi"),
            ("é", "e\u{301}"),
        ] {
            assert_eq!(LiteralSearch::new(right).unwrap().find(left).unwrap(), None);
        }
        assert_eq!(
            LiteralSearch::new("foo").unwrap().find("🤖FOO").unwrap(),
            Some(2)
        );
        assert_eq!(
            LiteralSearch::new("ᾀ").unwrap().find("🤖ᾈ").unwrap(),
            Some(2)
        );
        assert_eq!(
            LiteralSearch::new("xyz").unwrap().find("🤖abc").unwrap(),
            None
        );
        assert_eq!(
            LiteralSearch::new("ᾀ").unwrap().find("ᾈᾀ").unwrap(),
            Some(0)
        );
    }

    #[test]
    fn treats_search_syntax_as_literal() {
        let search = LiteralSearch::new(".*[x](?i)\\^$").unwrap();
        assert_eq!(search.find("🤖.*[X](?I)\\^$").unwrap(), Some(2));
        assert_eq!(search.find("anything x").unwrap(), None);
        assert_eq!(LiteralSearch::new(".*").unwrap().find("abc").unwrap(), None);
    }

    #[test]
    fn searches_a_long_unicode_literal_without_a_pattern_limit() {
        // A text file can contain a long repeated search phrase.
        let needle = "K".repeat(100_000);
        let mut document = fixture();
        document["pullRequest"]["description"] = json!(format!("🤖{}", "k".repeat(100_000)));
        document["threads"] = json!([]);
        let page: Value = serde_json::from_str(
            &query(
                &document,
                &json!({"query":"search", "text":needle}),
                None,
                None,
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(page["totalCount"], 1);
        assert_eq!(page["items"][0]["matchOffset"], 2);
    }

    #[test]
    fn searches_overlapping_prefixes_without_losing_utf16_offsets() {
        let search = LiteralSearch::new("aBaBaC").unwrap();
        assert_eq!(search.find("🤖abababac").unwrap(), Some(4));
        assert_eq!(search.find("abababab").unwrap(), None);
    }

    #[test]
    fn unicode_search_reports_original_utf16_offsets() {
        let mut document = fixture();
        document["pullRequest"]["description"] = json!("🤖ᾀ");
        document["threads"] = json!([]);
        let result = query(
            &document,
            &json!({"query":"search", "text":"ᾈ"}),
            None,
            None,
        )
        .unwrap();
        let page: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(page["items"][0]["matchOffset"], 2);
    }

    #[test]
    fn searches_description_and_comments_with_shared_simple_folding() {
        let mut document = fixture();
        document["pullRequest"]["description"] = json!("🤖Keep");
        document["threads"][0]["comments"][0]["content"] = json!("à🤖keep");
        let page: Value = serde_json::from_str(
            &query(
                &document,
                &json!({"query":"search", "text":"kEeP"}),
                None,
                None,
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(page["totalCount"], 2);
        assert_eq!(page["items"][0]["field"], "description");
        assert_eq!(page["items"][0]["matchOffset"], 2);
        assert_eq!(page["items"][1]["commentId"], "C");
        assert_eq!(page["items"][1]["matchOffset"], 3);

        let page: Value = serde_json::from_str(
            &query(
                &document,
                &json!({"query":"search", "text":"kEeP", "files":["src/widget.rs"]}),
                None,
                None,
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(page["totalCount"], 1);
        assert_eq!(page["items"][0]["commentId"], "C");
    }

    #[test]
    fn refuses_ambiguous_linguistic_comparisons() {
        // PowerShell -eq uses linguistic comparison rather than ordinal search.
        assert!(case_insensitive_equal(&json!("ß"), &json!("ẞ")).is_err());
        assert!(case_insensitive_equal(&json!("é"), &json!("e\u{301}")).is_err());
        assert!(case_insensitive_equal(&json!("a\0b"), &json!("ab")).is_err());
        assert!(case_insensitive_equal(&json!("α"), &json!("α")).unwrap());
        assert!(case_insensitive_equal(&json!("ACTIVE"), &json!("active")).unwrap());
    }

    #[test]
    fn resumes_utf16_fragments_with_a_bound_cursor() {
        let mut document = fixture();
        let body = "\"🤖\u{2028}\n".repeat(9000);
        document["pullRequest"]["description"] = json!(body);
        let options = json!({"query":"metadata","field":"description"});
        let mut cursor = None;
        let mut reconstructed = String::new();
        for _ in 0..100 {
            let output = query(&document, &options, None, cursor.as_deref()).unwrap();
            assert!(output.len() + if cfg!(windows) { 2 } else { 1 } <= 16 * 1024);
            let page: Value = serde_json::from_str(&output).unwrap();
            let item = &page["items"][0];
            assert_eq!(
                usize::try_from(item["contentOffset"].as_u64().unwrap()).unwrap(),
                reconstructed.encode_utf16().count()
            );
            reconstructed.push_str(item["content"].as_str().unwrap());
            if page["complete"] == true {
                assert_eq!(reconstructed, body);
                return;
            }
            cursor = Some(page["nextCursor"].as_str().unwrap().to_owned());
            assert!(
                query(
                    &document,
                    &json!({"query":"metadata","field":"title"}),
                    None,
                    cursor.as_deref()
                )
                .is_err()
            );
        }
        panic!("pagination did not finish within the bounded fixture");
    }

    #[test]
    fn compares_comments_and_evidence_in_category_order() {
        let previous = fixture();
        let mut current = previous.clone();
        current["threads"][0]["resolution"] = json!("active");
        current["threads"][0]["comments"][0]["content"] = json!("edited");
        current["pullRequest"]["reviewedSourceCommit"] = json!("new source");
        let output = query(&current, &json!({"query":"changes"}), Some(&previous), None).unwrap();
        let page: Value = serde_json::from_str(&output).unwrap();
        let changes: Vec<&str> = page["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["change"].as_str().unwrap())
            .collect();
        assert_eq!(
            changes,
            [
                "commentsEdited",
                "resolutionChanged",
                "reviewedSourceCommit",
                "reviewEvidence"
            ]
        );
    }
}
