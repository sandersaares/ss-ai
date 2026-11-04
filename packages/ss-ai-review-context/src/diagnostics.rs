//! Strict, shared input-cost records.
#![allow(
    clippy::indexing_slicing,
    reason = "Record shapes are validated before indexing their fields"
)]

use std::fs;
use std::num::NonZeroU32;
use std::path::{Path, absolute};

use ohno::AppError;
use serde_json::{Value, json};
use ss_ai_runtime::{Clock, FileTransaction, fail, read_bytes};

use crate::json::parse_text;

/// Names in the input diagnostics schema; unknown counters are not extensible.
const COUNTERS: [&str; 6] = [
    "setupCalls",
    "repeatedSetupCalls",
    "documentReadCalls",
    "firstReadFailures",
    "malformedQueryFailures",
    "cursorRestarts",
];

/// Reads one diagnostic record or sums records in a run directory.
pub fn read_diagnostics(path: &Path, clock: &dyn Clock) -> Result<Value, AppError> {
    let path = absolute(path)?;
    if path.is_dir() {
        let mut pending = vec![path];
        let mut total = empty();
        let mut found = false;
        while let Some(directory) = pending.pop() {
            for entry in fs::read_dir(&directory)? {
                let entry = entry?;
                let file_type = entry.file_type()?;
                if file_type.is_dir() {
                    pending.push(entry.path());
                } else if file_type.is_file() && entry.file_name() == "input-diagnostics.json" {
                    let record = read_record(&entry.path(), clock)?;
                    add_counts(&mut total, &record)?;
                    found = true;
                }
            }
        }
        if !found {
            return Err(fail(
                "invalidInput",
                "No input diagnostics records exist in the selected run directory",
            ));
        }
        Ok(total)
    } else {
        read_record(&path, clock)
    }
}

/// Counts an observed input event in one atomically published record.
pub fn increment_diagnostic(
    path: &Path,
    name: &str,
    count: NonZeroU32,
    clock: &dyn Clock,
) -> Result<(), AppError> {
    if !COUNTERS.contains(&name) || count.get() > i32::MAX as u32 {
        return Err(fail(
            "invalidInput",
            "Invalid input diagnostic name or count",
        ));
    }
    let path = absolute(path)?;
    let transaction = FileTransaction::open(&path, clock)?;
    let mut record = match transaction.read_bytes()? {
        Some(bytes) => parse(&bytes)?,
        None => empty(),
    };
    let counts = record["counts"]
        .as_object_mut()
        .ok_or_else(|| fail("invalidInput", "Invalid input diagnostics counts"))?;
    let old = counts
        .get(name)
        .and_then(Value::as_u64)
        .ok_or_else(|| fail("invalidInput", "Invalid input diagnostic counter"))?;
    let new = old
        .checked_add(std::num::NonZeroU64::from(count).get())
        .ok_or_else(|| fail("invalidInput", "Input diagnostic counter overflow"))?;
    counts.insert(name.to_owned(), json!(new));
    validate(&record)?;
    transaction.write_bytes(&serde_json::to_vec(&record)?)?;
    Ok(())
}

/// Produces the schema's explicit initial counts, only for a new file.
fn empty() -> Value {
    json!({
        "schemaVersion": 1,
        "counts": {
            "setupCalls": 0, "repeatedSetupCalls": 0, "documentReadCalls": 0,
            "firstReadFailures": 0, "malformedQueryFailures": 0, "cursorRestarts": 0
        }
    })
}

/// Reads a complete record without taking a writer's lock.
fn read_record(path: &Path, clock: &dyn Clock) -> Result<Value, AppError> {
    let bytes = read_bytes(path, clock)?.ok_or_else(|| {
        fail(
            "invalidInput",
            format!("Input diagnostics record missing: {}", path.display()),
        )
    })?;
    parse(&bytes)
}

/// Rejects malformed or incomplete records rather than filling missing fields.
fn parse(bytes: &[u8]) -> Result<Value, AppError> {
    let record = parse_json(bytes)?;
    validate(&record)?;
    Ok(record)
}

/// Reads workflow JSON using the Unicode encodings accepted by the shared reader.
pub(crate) fn parse_json(bytes: &[u8]) -> Result<Value, AppError> {
    let text = if let Some(bytes) = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]) {
        decode_utf8(bytes)?
    } else if let Some(bytes) = bytes.strip_prefix(&[0xff, 0xfe, 0x00, 0x00]) {
        decode_utf32(bytes, true)?
    } else if let Some(bytes) = bytes.strip_prefix(&[0x00, 0x00, 0xfe, 0xff]) {
        decode_utf32(bytes, false)?
    } else if let Some(bytes) = bytes.strip_prefix(&[0xff, 0xfe]) {
        decode_utf16(bytes, true)?
    } else if let Some(bytes) = bytes.strip_prefix(&[0xfe, 0xff]) {
        decode_utf16(bytes, false)?
    } else {
        decode_utf8(bytes)?
    };
    parse_text(&text)
}

/// Rejects invalid UTF-8 rather than changing data before validation.
fn decode_utf8(bytes: &[u8]) -> Result<String, AppError> {
    String::from_utf8(bytes.to_vec()).map_err(|error| {
        fail(
            "invalidInput",
            format!("Invalid UTF-8 workflow JSON: {error}"),
        )
    })
}

/// Decodes a complete UTF-16 record without replacing malformed characters.
fn decode_utf16(bytes: &[u8], little: bool) -> Result<String, AppError> {
    let (chunks, remainder) = bytes.as_chunks::<2>();
    let words: Vec<u16> = chunks
        .iter()
        .map(|word| {
            if little {
                u16::from_le_bytes(*word)
            } else {
                u16::from_be_bytes(*word)
            }
        })
        .collect();
    if !remainder.is_empty() {
        return Err(fail("invalidInput", "Invalid UTF-16 workflow JSON"));
    }
    String::from_utf16(&words).map_err(|error| {
        fail(
            "invalidInput",
            format!("Invalid UTF-16 workflow JSON: {error}"),
        )
    })
}

/// Decodes UTF-32 only when every code point has a valid Unicode representation.
fn decode_utf32(bytes: &[u8], little: bool) -> Result<String, AppError> {
    let (chunks, remainder) = bytes.as_chunks::<4>();
    let mut text = String::new();
    for &word in chunks {
        let code = if little {
            u32::from_le_bytes(word)
        } else {
            u32::from_be_bytes(word)
        };
        text.push(
            char::from_u32(code)
                .ok_or_else(|| fail("invalidInput", "Invalid UTF-32 workflow JSON"))?,
        );
    }
    if !remainder.is_empty() {
        return Err(fail("invalidInput", "Invalid UTF-32 workflow JSON"));
    }
    Ok(text)
}

/// Enforces the inputDiagnostics workflow schema.
fn validate(record: &Value) -> Result<(), AppError> {
    let object = record
        .as_object()
        .ok_or_else(|| fail("invalidInput", "Invalid input diagnostics record"))?;
    if object.len() != 2 || object.get("schemaVersion") != Some(&json!(1)) {
        return Err(fail("invalidInput", "Invalid input diagnostics envelope"));
    }
    let counts = object
        .get("counts")
        .and_then(Value::as_object)
        .ok_or_else(|| fail("invalidInput", "Invalid input diagnostics counts"))?;
    if counts.len() != COUNTERS.len()
        || COUNTERS
            .iter()
            .any(|name| counts.get(*name).and_then(Value::as_u64).is_none())
    {
        return Err(fail("invalidInput", "Invalid input diagnostics counters"));
    }
    Ok(())
}

/// Accumulates independent complete records without losing overflow errors.
fn add_counts(total: &mut Value, record: &Value) -> Result<(), AppError> {
    let counts = total["counts"]
        .as_object_mut()
        .ok_or_else(|| fail("invalidInput", "Invalid input diagnostics counts"))?;
    for name in COUNTERS {
        let left = counts
            .get(name)
            .and_then(Value::as_u64)
            .ok_or_else(|| fail("invalidInput", "Invalid input diagnostic counter"))?;
        let right = record["counts"][name]
            .as_u64()
            .ok_or_else(|| fail("invalidInput", "Invalid input diagnostic counter"))?;
        let sum = left
            .checked_add(right)
            .ok_or_else(|| fail("invalidInput", "Input diagnostic counter overflow"))?;
        counts.insert(name.to_owned(), json!(sum));
    }
    Ok(())
}
