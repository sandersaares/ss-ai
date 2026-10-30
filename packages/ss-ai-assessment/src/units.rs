//! Lossless bounded work manifests retain exact offsets into immutable evidence.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::assessment::{document_digest, hash};
use crate::error::{self, AssessmentError};

/// Reserve worker budget for JSON escaping, metadata, policy, and a completed note.
pub(crate) const UNIT_CHARACTERS: usize = 4_096;
/// Reserve most of a metadata response for the unit identity and pagination.
const UNIT_RANGES: usize = 8;

/// An immutable work item names original evidence, never a summary of that evidence.
#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct EvidenceUnit {
    pub(crate) id: String,
    pub(crate) snapshot_digest: String,
    pub(crate) parent: String,
    pub(crate) reference: String,
    pub(crate) ranges: Vec<EvidenceRange>,
    pub(crate) bindings: Vec<Value>,
    pub(crate) total_characters: usize,
    pub(crate) content_digest: String,
}

/// Half-open Unicode scalar ranges preserve split oversized lines without overlap.
#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct EvidenceRange {
    pub(crate) start_character: usize,
    pub(crate) end_character: usize,
    pub(crate) content_digest: String,
}

/// A preferred whole record can belong to a noncontiguous provider thread.
struct Segment {
    start: usize,
    end: usize,
    identity: String,
    binding: Value,
}

impl EvidenceUnit {
    /// Ordered ranges preserve original bytes even when a thread's replies are interleaved.
    pub(crate) fn content(&self, document: &str) -> String {
        self.ranges
            .iter()
            .flat_map(|range| {
                document
                    .chars()
                    .skip(range.start_character)
                    .take(range.end_character.saturating_sub(range.start_character))
            })
            .collect()
    }

    pub(crate) fn reference(&self) -> String {
        format!("unit:{}", self.id)
    }

    /// Binding metadata can be larger than any one response.
    pub(crate) fn binding_reference(&self) -> String {
        format!("binding:{}", self.id)
    }

    /// Unit lists expose ranges separately from potentially large bindings.
    pub(crate) fn metadata(&self) -> Value {
        json!({
            "id": self.id, "snapshotDigest": self.snapshot_digest,
            "parent": self.parent, "reference": self.reference,
            "ranges": self.ranges, "totalCharacters": self.total_characters,
            "contentDigest": self.content_digest
        })
    }
}

/// Structural grouping is a preference; every original character remains covered once.
pub(crate) fn partition(
    snapshot: &str,
    parent: &str,
    reference: &str,
    document: &str,
) -> Result<Vec<EvidenceUnit>, AssessmentError> {
    let segments = segments(document, reference)?;
    let mut groups = BTreeMap::<String, Vec<Segment>>::new();
    for segment in segments {
        groups
            .entry(segment.identity.clone())
            .or_default()
            .push(segment);
    }
    let mut units = Vec::new();
    for (identity, segments) in groups {
        let parent = format!("{parent}/{}", hash(&identity)?);
        let mut unit = new_unit(snapshot, &parent, reference);
        for segment in segments {
            let mut remaining = document
                .get(segment.start..segment.end)
                .ok_or_else(error::InvalidUnitManifest::classified)?;
            let mut character = document
                .get(..segment.start)
                .ok_or_else(error::InvalidUnitManifest::classified)?
                .chars()
                .count();
            let length = remaining.chars().count();
            // Do not split a complete ordinary hunk/thread record merely to fill a unit.
            if !unit.ranges.is_empty()
                && (unit.total_characters.saturating_add(length) > UNIT_CHARACTERS
                    || unit.ranges.len() >= UNIT_RANGES)
            {
                finish_unit(&mut units, unit, document)?;
                unit = new_unit(snapshot, &parent, reference);
            }
            loop {
                let bytes = split_at(remaining);
                let text = remaining
                    .get(..bytes)
                    .ok_or_else(error::InvalidUnitManifest::classified)?;
                let end_character = character.saturating_add(text.chars().count());
                unit.ranges.push(EvidenceRange {
                    start_character: character,
                    end_character,
                    content_digest: document_digest(text),
                });
                unit.total_characters = unit.total_characters.saturating_add(text.chars().count());
                unit.bindings.push(segment.binding.clone());
                if bytes == remaining.len() {
                    break;
                }
                if bytes == 0 {
                    return Err(error::InvalidUnitManifest::classified());
                }
                finish_unit(&mut units, unit, document)?;
                unit = new_unit(snapshot, &parent, reference);
                remaining = remaining
                    .get(bytes..)
                    .ok_or_else(error::InvalidUnitManifest::classified)?;
                character = end_character;
            }
        }
        if !unit.ranges.is_empty() {
            finish_unit(&mut units, unit, document)?;
        }
    }
    // Ordering by the first original range makes default progress follow the source document.
    units.sort_by_key(|unit| unit.ranges.first().map(|range| range.start_character));
    Ok(units)
}

fn new_unit(snapshot: &str, parent: &str, reference: &str) -> EvidenceUnit {
    EvidenceUnit {
        id: String::new(),
        snapshot_digest: snapshot.to_owned(),
        parent: parent.to_owned(),
        reference: reference.to_owned(),
        ranges: Vec::new(),
        bindings: Vec::new(),
        total_characters: 0,
        content_digest: String::new(),
    }
}

fn finish_unit(
    units: &mut Vec<EvidenceUnit>,
    mut unit: EvidenceUnit,
    document: &str,
) -> Result<(), AssessmentError> {
    unit.content_digest = document_digest(&unit.content(document));
    unit.id = hash(&unit)?;
    units.push(unit);
    Ok(())
}

/// Locate records using the JSON parser's consumed byte offsets.
fn segments(document: &str, reference: &str) -> Result<Vec<Segment>, AssessmentError> {
    let Ok(Value::Object(object)) = serde_json::from_str::<Value>(document) else {
        return Ok(vec![Segment {
            start: 0,
            end: document.len(),
            identity: String::new(),
            binding: Value::Null,
        }]);
    };
    let mut boundaries = Vec::new();
    if reference == "discussion" {
        for (field, span) in object_spans(document, 0)? {
            if !matches!(
                field.as_str(),
                "pullRequest" | "reviews" | "comments" | "reviewComments" | "threads" | "hunks"
            ) {
                continue;
            }
            let thread_records = if matches!(field.as_str(), "comments" | "reviewComments") {
                value_array(Some(&span.value)).to_vec()
            } else {
                Vec::new()
            };
            let records = match &span.value {
                Value::Array(_) => array_spans(document, span.start)?
                    .into_iter()
                    .enumerate()
                    .map(|(index, span)| (index.to_string(), span))
                    .collect::<Vec<_>>(),
                Value::Object(_) if field == "hunks" => object_spans(document, span.start)?,
                _ => vec![(String::new(), span)],
            };
            for (index, span) in records {
                let identity = if matches!(field.as_str(), "comments" | "reviewComments") {
                    thread_identity(&field, &index, &span.value, &thread_records)?
                } else {
                    format!(
                        "/{field}/{}",
                        span.value
                            .get("id")
                            .map_or_else(|| index.clone(), Value::to_string)
                    )
                };
                let binding = json!({
                    "collection": field,
                    "indexDigest": hash(&index)?,
                    "identityDigest": hash(&identity)?
                });
                boundaries.push((span.end, identity, binding));
            }
        }
    }
    if reference != "discussion" && object.contains_key("patch") && boundaries.is_empty() {
        let mut binding = object;
        let patch = binding
            .remove("patch")
            .ok_or_else(error::InvalidUnitManifest::classified)?;
        let patch = patch
            .as_str()
            .ok_or_else(error::InvalidUnitManifest::classified)?;
        binding.remove("hunks");
        let binding = json!({"collection":"patch"});
        let patch_start = document
            .find("\"patch\":")
            .ok_or_else(error::InvalidUnitManifest::classified)?
            .saturating_add("\"patch\":".len())
            .saturating_add(1);
        let mut source_offset = 0;
        let mut document_offset = patch_start;
        // Headers in decoded patch text cannot be confused with a source string's literal \n.
        // Disjoint encoded chunks avoid rescanning every prefix of a many-hunk document.
        for (index, (offset, _)) in patch.match_indices("\n@@ ").enumerate() {
            let end = offset.saturating_add(1);
            let chunk = patch
                .get(source_offset..end)
                .ok_or_else(error::InvalidUnitManifest::classified)?;
            document_offset = document_offset
                .saturating_add(serde_json::to_string(chunk)?.len().saturating_sub(2));
            source_offset = end;
            boundaries.push((document_offset, format!("/hunk/{index}"), binding.clone()));
        }
        boundaries.push((
            document.len(),
            format!("/hunk/{}", boundaries.len()),
            binding,
        ));
    }
    boundaries.sort_by_key(|(end, _, _)| *end);
    boundaries.dedup_by_key(|(end, _, _)| *end);
    if let Some((end, _, _)) = boundaries.last_mut() {
        *end = document.len();
    } else {
        boundaries.push((document.len(), String::new(), Value::Null));
    }
    let mut start = 0;
    Ok(boundaries
        .into_iter()
        .map(|(end, identity, binding)| {
            let segment = Segment {
                start,
                end,
                identity,
                binding,
            };
            start = end;
            segment
        })
        .collect())
}

/// One parsed value retains its exact byte span in the original document.
struct JsonSpan {
    start: usize,
    end: usize,
    value: Value,
}

/// Parse one value with serde so string escaping and nested values stay exact.
fn json_span(document: &str, start: usize) -> Result<JsonSpan, AssessmentError> {
    let start = skip_whitespace(document, start);
    let text = document
        .get(start..)
        .ok_or_else(error::InvalidUnitManifest::classified)?;
    let mut stream = serde_json::Deserializer::from_str(text).into_iter::<Value>();
    let value = stream
        .next()
        .ok_or_else(error::InvalidUnitManifest::classified)??;
    let end = start.saturating_add(stream.byte_offset());
    Ok(JsonSpan { start, end, value })
}

/// Structural punctuation is checked even though the full document was parsed first.
fn delimiter(document: &str, offset: usize, expected: u8) -> Result<usize, AssessmentError> {
    let offset = skip_whitespace(document, offset);
    if document.as_bytes().get(offset) != Some(&expected) {
        return Err(error::InvalidUnitManifest::classified());
    }
    Ok(offset.saturating_add(1))
}

fn skip_whitespace(document: &str, offset: usize) -> usize {
    let mut offset = offset;
    while document
        .as_bytes()
        .get(offset)
        .is_some_and(|byte| matches!(byte, b' ' | b'\n' | b'\r' | b'\t'))
    {
        offset = offset.saturating_add(1);
    }
    offset
}

/// Iterate object members in document order without reconstructing their values.
fn object_spans(document: &str, start: usize) -> Result<Vec<(String, JsonSpan)>, AssessmentError> {
    let mut cursor = delimiter(document, start, b'{')?;
    let mut fields = Vec::new();
    loop {
        cursor = skip_whitespace(document, cursor);
        if document.as_bytes().get(cursor) == Some(&b'}') {
            return Ok(fields);
        }
        let key = json_span(document, cursor)?;
        let key_text = key
            .value
            .as_str()
            .ok_or_else(error::InvalidUnitManifest::classified)?
            .to_owned();
        let value = json_span(document, delimiter(document, key.end, b':')?)?;
        cursor = skip_whitespace(document, value.end);
        fields.push((key_text, value));
        if document.as_bytes().get(cursor) != Some(&b',') {
            delimiter(document, cursor, b'}')?;
            return Ok(fields);
        }
        cursor = cursor.saturating_add(1);
    }
}

/// Iterate array records using the same exact value boundaries.
fn array_spans(document: &str, start: usize) -> Result<Vec<JsonSpan>, AssessmentError> {
    let mut cursor = delimiter(document, start, b'[')?;
    let mut values = Vec::new();
    loop {
        cursor = skip_whitespace(document, cursor);
        if document.as_bytes().get(cursor) == Some(&b']') {
            return Ok(values);
        }
        let value = json_span(document, cursor)?;
        cursor = skip_whitespace(document, value.end);
        values.push(value);
        if document.as_bytes().get(cursor) != Some(&b',') {
            delimiter(document, cursor, b']')?;
            return Ok(values);
        }
        cursor = cursor.saturating_add(1);
    }
}

fn value_array(value: Option<&Value>) -> &[Value] {
    value.and_then(Value::as_array).map_or(&[], Vec::as_slice)
}

/// Reply chains retain thread identity even when provider ordering interleaves threads.
fn thread_identity(
    field: &str,
    index: &str,
    value: &Value,
    records: &[Value],
) -> Result<String, AssessmentError> {
    let mut current = value;
    let mut visited = BTreeSet::new();
    while let Some(parent) = current
        .get("in_reply_to_id")
        .filter(|value| !value.is_null())
    {
        if !visited.insert(parent.to_string()) {
            return Err(error::InvalidUnitManifest::classified());
        }
        let Some(record) = records
            .iter()
            .find(|record| record.get("id") == Some(parent))
        else {
            // A retained parent ID still binds the thread when the provider omits its body.
            return Ok(format!("/{field}/thread/{parent}"));
        };
        current = record;
    }
    Ok(format!(
        "/{field}/thread/{}",
        current
            .get("id")
            .map_or_else(|| index.to_owned(), Value::to_string)
    ))
}

/// Prefer complete physical/JSON-escaped lines, but never stall on a long single line.
fn split_at(text: &str) -> usize {
    let end = text
        .char_indices()
        .nth(UNIT_CHARACTERS)
        .map_or(text.len(), |(offset, _)| offset);
    if end == text.len() {
        return end;
    }
    let prefix = text.get(..end).unwrap_or_default();
    let newline = prefix.rfind('\n').map(|offset| offset.saturating_add(1));
    let escaped = prefix
        .rmatch_indices("\\n")
        .find(|(offset, _)| {
            prefix.get(..*offset).is_some_and(|before| {
                before
                    .bytes()
                    .rev()
                    .take_while(|byte| *byte == b'\\')
                    .count()
                    .is_multiple_of(2)
            })
        })
        .map(|(offset, _)| offset.saturating_add(2));
    newline.into_iter().chain(escaped).max().unwrap_or(end)
}
