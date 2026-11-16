//! Saved query identity, replay, and continuation transactions.
#![allow(
    clippy::indexing_slicing,
    reason = "Request and page shapes are validated before indexing their fields"
)]

use std::num::NonZeroU32;
use std::path::{Component, Path, PathBuf, absolute};

use ohno::AppError;
use serde_json::{Value, json};
use ss_ai_runtime::{Clock, FileTransaction, fail};

use crate::diagnostics::parse_json;
use crate::{
    document_hash, increment_diagnostic, normalize_options, powershell_json, query, read_capture,
    snapshot_id,
};

/// Creates a saved query after validating selectors and their capture.
pub fn prepare(
    path: &Path,
    compare_path: Option<&Path>,
    request_path: &Path,
    diagnostics_path: &Path,
    options: &Value,
    clock: &dyn Clock,
) -> Result<Value, AppError> {
    let diagnostics = full_path(diagnostics_path)?;
    if diagnostics == full_path(path)?
        || compare_path
            .map(full_path)
            .transpose()?
            .is_some_and(|path| path == diagnostics)
    {
        return Err(fail(
            "invalidInput",
            "Diagnostics path must differ from capture paths",
        ));
    }
    prepare_inner(request_path, diagnostics_path, clock, |diagnostics_path| {
        new_request(path, compare_path, diagnostics_path, options, clock)
    })
}

/// Records a failure discovered while constructing the request's input.
pub fn prepare_failure(
    request_path: &Path,
    diagnostics_path: &Path,
    error: AppError,
    clock: &dyn Clock,
) -> Result<Value, AppError> {
    prepare_inner(request_path, diagnostics_path, clock, |_| Err(error))
}

/// Serializes setup attempts before creating or rejecting a request.
fn prepare_inner(
    request_path: &Path,
    diagnostics_path: &Path,
    clock: &dyn Clock,
    build: impl FnOnce(&Path) -> Result<Value, AppError>,
) -> Result<Value, AppError> {
    let request_path = full_path(request_path)?;
    let diagnostics_path = full_path(diagnostics_path)?;
    if request_path == diagnostics_path {
        return Err(fail(
            "invalidInput",
            "Request and diagnostics paths must differ",
        ));
    }
    increment_diagnostic(&diagnostics_path, "setupCalls", NonZeroU32::MIN, clock)?;
    let transaction = FileTransaction::open(&request_path, clock)?;
    if transaction.read_bytes()?.is_some() {
        increment_diagnostic(
            &diagnostics_path,
            "repeatedSetupCalls",
            NonZeroU32::MIN,
            clock,
        )?;
        return Err(fail("invalidInput", "Query request already exists"));
    }
    let result = build(&diagnostics_path);
    let request = match result {
        Ok(request) => request,
        Err(error) => {
            increment_diagnostic(
                &diagnostics_path,
                "malformedQueryFailures",
                NonZeroU32::MIN,
                clock,
            )?;
            return Err(error);
        }
    };
    transaction.write_bytes(&serde_json::to_vec(&request)?)?;
    Ok(json!({
        "requestPath": path_string(&request_path)?,
        "requestId": request["requestId"],
        "snapshotId": request["snapshotId"]
    }))
}

/// Replays the last page, advances once, or restarts against current captures.
pub fn read_saved(
    request_path: &Path,
    next: bool,
    restart: bool,
    clock: &dyn Clock,
) -> Result<String, AppError> {
    if next && restart {
        return Err(fail(
            "invalidInput",
            "Next and Restart are mutually exclusive",
        ));
    }
    let request_path = full_path(request_path)?;
    let transaction = FileTransaction::open(&request_path, clock)?;
    let bytes = transaction
        .read_bytes()?
        .ok_or_else(|| fail("invalidInput", "Saved query request is missing"))?;
    let mut request = parse_json(&bytes)?;
    validate_request(&request)?;
    let path = PathBuf::from(required_string(&request, "path")?);
    let compare_path = optional_string(&request, "comparePath")?.map(PathBuf::from);
    let diagnostics_path = PathBuf::from(required_string(&request, "diagnosticsPath")?);
    let diagnostics = full_path(&diagnostics_path)?;
    let compare = compare_path.as_deref().map(full_path).transpose()?;
    if !path.is_absolute()
        || !diagnostics_path.is_absolute()
        || compare_path
            .as_ref()
            .is_some_and(|path| !path.is_absolute())
        || diagnostics == request_path
        || diagnostics == full_path(&path)?
        || compare.as_ref().is_some_and(|path| path == &diagnostics)
    {
        return Err(fail(
            "invalidInput",
            "Saved query paths must be absolute and distinct",
        ));
    }
    let options = normalize_options(&request["options"])?;
    if document_hash(&options)? != document_hash(&request["options"])?
        || request_id(&request)? != required_string(&request, "requestId")?
    {
        return Err(fail(
            "invalidInput",
            "Saved query selectors or identity were modified",
        ));
    }
    if restart {
        increment_diagnostic(&diagnostics_path, "cursorRestarts", NonZeroU32::MIN, clock)?;
        request = new_request(
            &path,
            compare_path.as_deref(),
            &diagnostics_path,
            &options,
            clock,
        )?;
    }
    let document = read_capture(&path, clock)?;
    let previous = compare_path
        .as_deref()
        .map(|path| read_capture(path, clock))
        .transpose()?;
    let snapshot = snapshot_id(&document)?;
    let baseline = previous.as_ref().map(snapshot_id).transpose()?;
    if request["snapshotId"].as_str() != Some(snapshot.as_str())
        || optional_string(&request, "baselineId")? != baseline.as_deref()
    {
        return Err(fail(
            "invalidInput",
            "Saved query capture changed; restart or create a new request",
        ));
    }
    let last_page = &request["lastPage"];
    let cursor = if last_page.is_null() {
        if next {
            return Err(fail(
                "invalidInput",
                "Read the first page before requesting Next",
            ));
        }
        None
    } else {
        if last_page["snapshotId"].as_str() != Some(snapshot.as_str())
            || last_page["query"] != options["query"]
            || last_page["complete"].as_bool() != Some(last_page["nextCursor"].is_null())
        {
            return Err(fail(
                "invalidInput",
                "Saved query page does not match the request",
            ));
        }
        let saved_json = powershell_json(last_page)?;
        if saved_json
            .len()
            .checked_add(line_ending_length())
            .is_none_or(|length| length > 16 * 1024)
        {
            return Err(fail(
                "invalidInput",
                "Saved query page exceeds the bounded output limit",
            ));
        }
        let start = required_string(&request, "lastPageCursor")?;
        let expected = query(&document, &options, previous.as_ref(), Some(start))?;
        if saved_json != expected {
            return Err(fail(
                "invalidInput",
                "Saved query page does not match the bound request and capture",
            ));
        }
        if !next {
            return Ok(expected);
        }
        if last_page["complete"] == true {
            return Err(fail("invalidInput", "The saved query is complete"));
        }
        last_page["nextCursor"].as_str().map(str::to_owned)
    };
    let json = query(&document, &options, previous.as_ref(), cursor.as_deref())?;
    let page: Value = serde_json::from_str(&json)
        .map_err(|error| fail("invalidInput", format!("Invalid query page: {error}")))?;
    request["lastPage"] = page;
    request["lastPageCursor"] = json!(cursor.as_deref().unwrap_or(""));
    validate_request(&request)?;
    transaction.write_bytes(&serde_json::to_vec(&request)?)?;
    Ok(json)
}

/// Builds the request only after validating selection against the actual captures.
fn new_request(
    path: &Path,
    compare_path: Option<&Path>,
    diagnostics_path: &Path,
    options: &Value,
    clock: &dyn Clock,
) -> Result<Value, AppError> {
    let options = normalize_options(options)?;
    validate_options(&options)?;
    let path = full_path(path)?;
    let compare_path = compare_path.map(full_path).transpose()?;
    let document = read_capture(&path, clock)?;
    let previous = compare_path
        .as_deref()
        .map(|path| read_capture(path, clock))
        .transpose()?;
    query(&document, &options, previous.as_ref(), None)?;
    let mut request = json!({
        "schemaVersion": 1,
        "path": path_string(&path)?,
        "comparePath": compare_path.as_deref().map(path_string).transpose()?,
        "snapshotId": snapshot_id(&document)?,
        "baselineId": previous.as_ref().map(snapshot_id).transpose()?,
        "options": options,
        "requestId": "",
        "diagnosticsPath": path_string(diagnostics_path)?,
        "lastPage": null,
        "lastPageCursor": null
    });
    request["requestId"] = json!(request_id(&request)?);
    validate_request(&request)?;
    Ok(request)
}

/// Binds the exact selector and capture identities to the request.
fn request_id(request: &Value) -> Result<String, AppError> {
    document_hash(&json!({
        "path": request["path"],
        "comparePath": request["comparePath"],
        "snapshotId": request["snapshotId"],
        "baselineId": request["baselineId"],
        "options": request["options"]
    }))
}

/// Validates the saved query envelope and page against the workflow schema.
fn validate_request(value: &Value) -> Result<(), AppError> {
    validate_request_fields(value).map_err(|error| {
        fail(
            "invalidInput",
            format!("Invalid 'queryRequest' workflow data: {error}"),
        )
    })
}

/// Checks each part of the request without accepting partial or extra fields.
fn validate_request_fields(value: &Value) -> Result<(), AppError> {
    exact_keys(
        value,
        &[
            "schemaVersion",
            "path",
            "comparePath",
            "snapshotId",
            "baselineId",
            "options",
            "requestId",
            "diagnosticsPath",
            "lastPage",
            "lastPageCursor",
        ],
    )?;
    if value["schemaVersion"] != 1
        || required_string(value, "path")?.trim().is_empty()
        || required_string(value, "diagnosticsPath")?.trim().is_empty()
        || !valid_hash(required_string(value, "snapshotId")?)
        || !valid_hash(required_string(value, "requestId")?)
        || optional_string(value, "baselineId")?.is_some_and(|s| !valid_hash(s))
    {
        return Err(fail("invalidInput", "Invalid saved query request"));
    }
    optional_string(value, "comparePath")?;
    validate_options(&value["options"])?;
    let page = &value["lastPage"];
    let cursor = optional_string(value, "lastPageCursor")?;
    if page.is_null() {
        if cursor.is_some() {
            return Err(fail(
                "invalidInput",
                "A request without a page cannot have a cursor",
            ));
        }
    } else {
        if !cursor.is_some_and(valid_cursor) {
            return Err(fail("invalidInput", "Invalid saved page cursor"));
        }
        exact_keys(
            page,
            &[
                "schemaVersion",
                "query",
                "snapshotId",
                "totalCount",
                "items",
                "nextCursor",
                "complete",
            ],
        )?;
        let items = page["items"].as_array();
        if page["schemaVersion"] != 1
            || required_string(page, "query")?.trim().is_empty()
            || !valid_hash(required_string(page, "snapshotId")?)
            || page["totalCount"].as_u64().is_none()
            || items
                .is_none_or(|items| items.len() > 50 || items.iter().any(|item| !item.is_object()))
            || page["complete"].as_bool().is_none()
        {
            return Err(fail("invalidInput", "Invalid saved query page"));
        }
        optional_string(page, "nextCursor")?;
    }
    Ok(())
}

/// Validates option types independently of selector normalization.
fn validate_options(options: &Value) -> Result<(), AppError> {
    exact_keys(
        options,
        &[
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
        ],
    )?;
    for name in [
        "query",
        "field",
        "scope",
        "kind",
        "resolution",
        "threadId",
        "commentId",
        "authorship",
        "text",
    ] {
        required_string(options, name)?;
    }
    let files = options["files"]
        .as_array()
        .ok_or_else(|| fail("invalidInput", "Invalid query file selectors"))?;
    if files
        .iter()
        .any(|file| file.as_str().is_none_or(|s| s.trim().is_empty()))
        || options["indexOnly"].as_bool().is_none()
        || !options["pageSize"]
            .as_u64()
            .is_some_and(|size| (1..=50).contains(&size))
        || !["metadata", "threads", "comments", "search", "changes"]
            .contains(&required_string(options, "query")?)
        || !["all", "files", "global"].contains(&required_string(options, "scope")?)
    {
        return Err(fail("invalidInput", "Invalid query options"));
    }
    Ok(())
}

/// Rejects missing and unknown fields at structured record boundaries.
fn exact_keys(value: &Value, keys: &[&str]) -> Result<(), AppError> {
    let object = value
        .as_object()
        .ok_or_else(|| fail("invalidInput", "Expected an object"))?;
    if object.len() != keys.len() || keys.iter().any(|key| !object.contains_key(*key)) {
        return Err(fail("invalidInput", "Unexpected or missing record fields"));
    }
    Ok(())
}

/// Retrieves a mandatory string without coercion.
fn required_string<'a>(value: &'a Value, name: &str) -> Result<&'a str, AppError> {
    value
        .get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| fail("invalidInput", format!("Invalid string field {name}")))
}

/// Retrieves a nullable string without accepting missing or differently typed fields.
fn optional_string<'a>(value: &'a Value, name: &str) -> Result<Option<&'a str>, AppError> {
    match value.get(name) {
        Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value)),
        _ => Err(fail(
            "invalidInput",
            format!("Invalid nullable string field {name}"),
        )),
    }
}

/// Checks the schema's uppercase SHA-256 representation.
fn valid_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'A'..=b'F').contains(&byte))
}

/// Checks a page's initial empty cursor or bounded-query continuation spelling.
fn valid_cursor(value: &str) -> bool {
    if value.is_empty() {
        return true;
    }
    let mut parts = value.split(':');
    let hash = parts.next().unwrap_or_default();
    let first = parts.next().unwrap_or_default();
    let second = parts.next().unwrap_or_default();
    valid_hash(hash)
        && !first.is_empty()
        && first.bytes().all(|b| b.is_ascii_digit())
        && !second.is_empty()
        && second.bytes().all(|b| b.is_ascii_digit())
        && parts.next().is_none()
}

/// Resolves relative paths and dot segments without requiring the destination to exist.
fn full_path(path: &Path) -> Result<PathBuf, AppError> {
    let mut result = PathBuf::new();
    for component in absolute(path)?.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                result.pop();
            }
            other => result.push(other.as_os_str()),
        }
    }
    Ok(result)
}

/// Ensures persistent paths can be represented losslessly in JSON.
fn path_string(path: &Path) -> Result<&str, AppError> {
    path.to_str()
        .ok_or_else(|| fail("invalidInput", "Path is not valid Unicode"))
}

/// Accounts for the platform newline in the bounded output.
fn line_ending_length() -> usize {
    if cfg!(windows) { 2 } else { 1 }
}
