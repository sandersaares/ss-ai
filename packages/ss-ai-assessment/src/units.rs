//! Lossless bounded work manifests retain exact offsets into immutable evidence.

use std::collections::{BTreeMap, BTreeSet, HashMap};

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
    start_character: usize,
    identity: String,
    binding: Value,
}

impl EvidenceUnit {
    /// Ordered ranges preserve original bytes even when a thread's replies are interleaved.
    pub(crate) fn content(&self, document: &str) -> String {
        if self.ranges.iter().any(|range| range.end_character < range.start_character)
            || self
                .ranges
                .windows(2)
                .any(|ranges| matches!(ranges, [first, second] if first.end_character > second.start_character))
        {
            return self
                .ranges
                .iter()
                .flat_map(|range| {
                    document
                        .chars()
                        .skip(range.start_character)
                        .take(range.end_character.saturating_sub(range.start_character))
                })
                .collect();
        }
        let mut characters = document.chars();
        let mut offset = 0;
        let mut content = String::new();
        for range in &self.ranges {
            let skip = range.start_character.saturating_sub(offset);
            let length = range.end_character.saturating_sub(range.start_character);
            content.extend(characters.by_ref().skip(skip).take(length));
            offset = range.end_character;
        }
        content
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
    let mut previous_byte = 0;
    let mut character = 0_usize;
    for mut segment in segments {
        // Segments arrive in document order; count each intervening byte range only once.
        character = character.saturating_add(
            document
                .get(previous_byte..segment.start)
                .ok_or_else(error::InvalidUnitManifest::classified)?
                .chars()
                .count(),
        );
        segment.start_character = character;
        previous_byte = segment.start;
        groups
            .entry(segment.identity.clone())
            .or_default()
            .push(segment);
    }
    let mut units = Vec::new();
    for (identity, segments) in groups {
        let parent = format!("{parent}/{}", hash(&identity)?);
        let mut unit = new_unit(snapshot, &parent, reference);
        let mut unit_text = String::new();
        for segment in segments {
            let mut remaining = document
                .get(segment.start..segment.end)
                .ok_or_else(error::InvalidUnitManifest::classified)?;
            let mut character = segment.start_character;
            let length = remaining.chars().count();
            // Do not split a complete ordinary hunk/thread record merely to fill a unit.
            if !unit.ranges.is_empty()
                && (unit.total_characters.saturating_add(length) > UNIT_CHARACTERS
                    || unit.ranges.len() >= UNIT_RANGES)
            {
                finish_unit(&mut units, unit, &unit_text)?;
                unit_text.clear();
                unit = new_unit(snapshot, &parent, reference);
            }
            loop {
                let bytes = split_at(remaining);
                let text = remaining
                    .get(..bytes)
                    .ok_or_else(error::InvalidUnitManifest::classified)?;
                let text_length = text.chars().count();
                let end_character = character.saturating_add(text_length);
                unit.ranges.push(EvidenceRange {
                    start_character: character,
                    end_character,
                    content_digest: document_digest(text),
                });
                unit.total_characters = unit.total_characters.saturating_add(text_length);
                unit.bindings.push(segment.binding.clone());
                unit_text.push_str(text);
                if bytes == remaining.len() {
                    break;
                }
                if bytes == 0 {
                    return Err(error::InvalidUnitManifest::classified());
                }
                finish_unit(&mut units, unit, &unit_text)?;
                unit_text.clear();
                unit = new_unit(snapshot, &parent, reference);
                remaining = remaining
                    .get(bytes..)
                    .ok_or_else(error::InvalidUnitManifest::classified)?;
                character = end_character;
            }
        }
        if !unit.ranges.is_empty() {
            finish_unit(&mut units, unit, &unit_text)?;
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
    content: &str,
) -> Result<(), AssessmentError> {
    unit.content_digest = document_digest(content);
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
            start_character: 0,
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
            let is_array = span.value.is_array();
            let records = match &span.value {
                Value::Array(_) => array_spans(document, span.start)?
                    .into_iter()
                    .enumerate()
                    .map(|(index, span)| (index.to_string(), span))
                    .collect::<Vec<_>>(),
                Value::Object(_) if field == "hunks" => object_spans(document, span.start)?,
                _ => vec![(String::new(), span)],
            };
            let thread_records =
                if is_array && matches!(field.as_str(), "comments" | "reviewComments") {
                    records
                        .iter()
                        .map(|(_, span)| &span.value)
                        .collect::<Vec<_>>()
                } else {
                    Vec::new()
                };
            let mut thread_roots = ThreadRoots::new(&thread_records);
            for (index, span) in &records {
                let identity = if matches!(field.as_str(), "comments" | "reviewComments") {
                    thread_roots.identity(&field, index, &span.value, &thread_records)?
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
                    "indexDigest": hash(index)?,
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
                start_character: 0,
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

/// An id-less root uses the original record's index, not the root's index.
#[derive(Clone)]
enum ThreadRoot {
    Id(String),
    MissingId,
}

/// Index and memoize reply chains without changing provider thread bindings.
struct ThreadRoots<'a> {
    by_id: HashMap<&'a Value, usize>,
    roots: Vec<Option<ThreadRoot>>,
}

impl<'a> ThreadRoots<'a> {
    fn new(records: &[&'a Value]) -> Self {
        let mut by_id = HashMap::new();
        for (index, record) in records.iter().enumerate() {
            if let Some(id) = record.get("id") {
                // Duplicate IDs resolve to the first matching provider record.
                by_id.entry(id).or_insert(index);
            }
        }
        Self {
            by_id,
            roots: vec![None; records.len()],
        }
    }

    fn identity(
        &mut self,
        field: &str,
        index: &str,
        value: &Value,
        records: &[&Value],
    ) -> Result<String, AssessmentError> {
        let mut current = value;
        let mut position = index.parse::<usize>().ok().filter(|i| *i < records.len());
        let mut visited = BTreeSet::new();
        let mut path = Vec::new();
        let root = loop {
            if let Some(position) = position {
                if let Some(root) = self.roots.get(position).and_then(Option::as_ref) {
                    break root.clone();
                }
                if !visited.insert(position) {
                    return Err(error::InvalidUnitManifest::classified());
                }
                path.push(position);
            }
            let Some(parent) = current
                .get("in_reply_to_id")
                .filter(|value| !value.is_null())
            else {
                break current
                    .get("id")
                    .map_or(ThreadRoot::MissingId, |id| ThreadRoot::Id(id.to_string()));
            };
            let Some(parent_index) = self.by_id.get(parent).copied() else {
                // A retained parent ID binds the thread even when its body is absent.
                break ThreadRoot::Id(parent.to_string());
            };
            current = records
                .get(parent_index)
                .ok_or_else(error::InvalidUnitManifest::classified)?;
            position = Some(parent_index);
        };
        for position in path {
            let slot = self
                .roots
                .get_mut(position)
                .ok_or_else(error::InvalidUnitManifest::classified)?;
            *slot = Some(root.clone());
        }
        let root = match root {
            ThreadRoot::Id(id) => id,
            ThreadRoot::MissingId => index.to_owned(),
        };
        Ok(format!("/{field}/thread/{root}"))
    }
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::fmt::Write;

    use serde_json::{Value, json};

    use super::{ThreadRoots, document_digest, hash, partition, segments};

    /// Reference the original lookup semantics independently of the indexed traversal.
    fn original_thread_identity(index: &str, value: &Value, records: &[Value]) -> Option<String> {
        let mut current = value;
        let mut visited = BTreeSet::new();
        while let Some(parent) = current
            .get("in_reply_to_id")
            .filter(|value| !value.is_null())
        {
            if !visited.insert(parent.to_string()) {
                return None;
            }
            let Some(record) = records
                .iter()
                .find(|record| record.get("id") == Some(parent))
            else {
                return Some(format!("/comments/thread/{parent}"));
            };
            current = record;
        }
        Some(format!(
            "/comments/thread/{}",
            current
                .get("id")
                .map_or_else(|| index.to_owned(), Value::to_string)
        ))
    }

    #[test]
    fn indexed_threads_preserve_original_identity_and_cycle_handling() {
        let mut chains = (0..256)
            .map(|id| {
                if id == 0 {
                    json!({"id": id, "body": "root 😀"})
                } else {
                    json!({"id": id, "in_reply_to_id": id - 1, "body": "reply"})
                }
            })
            .collect::<Vec<_>>();
        chains.reverse();
        let cases = [
            chains,
            vec![
                json!({"id": "a", "in_reply_to_id": "missing"}),
                json!({"id": "b", "in_reply_to_id": "a"}),
                json!({"id": 2, "body": "first"}),
                json!({"id": 2, "in_reply_to_id": "a"}),
                json!({"id": 3, "in_reply_to_id": 2}),
                json!({"in_reply_to_id": 2}),
                json!({"id": null, "in_reply_to_id": null}),
                json!({"id": -0.0, "body": "signed zero"}),
                json!({"id": "zero-reply", "in_reply_to_id": 0.0}),
            ],
            vec![
                json!({"id": 1, "in_reply_to_id": 2}),
                json!({"id": 2, "in_reply_to_id": 1}),
                json!({"id": 3, "in_reply_to_id": 1}),
                json!({"id": 4}),
            ],
        ];
        for records in cases {
            let references = records.iter().collect::<Vec<_>>();
            let mut roots = ThreadRoots::new(&references);
            for (position, value) in records.iter().enumerate() {
                let index = position.to_string();
                let expected = original_thread_identity(&index, value, &records);
                let actual = roots.identity("comments", &index, value, &references).ok();
                assert_eq!(actual, expected);
            }
        }
    }

    #[test]
    fn large_interleaved_discussion_keeps_each_thread_and_all_original_text() {
        let mut records = Vec::new();
        for id in 0..128 {
            records.push(json!({"id": id, "body": "root 😀"}));
            records.push(json!({"id": id + 128, "in_reply_to_id": id, "body": "reply\r\n終"}));
        }
        for field in ["comments", "reviewComments"] {
            let document = json!({field: records}).to_string();
            let result = segments(&document, "discussion").unwrap();
            assert_eq!(result.len(), records.len());
            let mut reconstructed = String::new();
            for (index, (segment, record)) in result.iter().zip(&records).enumerate() {
                let expected = original_thread_identity(&index.to_string(), record, &records)
                    .unwrap()
                    .replace("/comments/", &format!("/{field}/"));
                assert_eq!(segment.identity, expected);
                reconstructed.push_str(document.get(segment.start..segment.end).unwrap());
            }
            assert_eq!(reconstructed, document);
        }
    }

    #[test]
    fn unicode_manifests_keep_existing_ranges_and_digests() {
        let discussion = json!({
            "pullRequest": {"body": format!("😀\r\n{}", "é".repeat(4_200))},
            "reviewComments": [
                {"id": 1, "body": "first 😀"},
                {"id": 2, "body": "other é"},
                {"id": 3, "in_reply_to_id": 1, "body": "reply\r\n終"}
            ],
            "hunks": {"h": "@@ -1 +1 @@\n-é\n+😀\n"}
        })
        .to_string();
        let patch = json!({
            "patch": format!("@@ -1 +1 @@\n-é\n+😀\n@@ -3 +3 @@\n-{}\n+終\n", "a".repeat(4_200)),
            "hunks": []
        })
        .to_string();
        // These digests bind the established serialized units, ranges, and provenance.
        for (reference, document, expected) in [
            (
                "discussion",
                discussion,
                "4b6a22e41c9f798b0665ca032f1975c5f48fb3305da908661dd91a364b0987ed",
            ),
            (
                "change:patch",
                patch,
                "b7bca585e6aed20387890c7005d9d503374f37357f0a8e8baeb790d8070ff50d",
            ),
        ] {
            let units = partition("snapshot", "parent", reference, &document).unwrap();
            assert_eq!(hash(&units).unwrap(), expected);
            for unit in units {
                let expected = unit
                    .ranges
                    .iter()
                    .flat_map(|range| {
                        document
                            .chars()
                            .skip(range.start_character)
                            .take(range.end_character.saturating_sub(range.start_character))
                    })
                    .collect::<String>();
                assert_eq!(unit.content(&document), expected);
            }
        }
    }

    #[test]
    fn many_hunks_preserve_all_offsets_content_and_digests() {
        let mut patch = String::new();
        for index in 0..256 {
            write!(patch, "@@ -{index} +{index} @@\n-entrée\r\n+😀{index}\r\n").unwrap();
        }
        let document = json!({"patch": patch, "hunks": []}).to_string();
        let units = partition("snapshot", "parent", "change:patch", &document).unwrap();
        assert!(units.len() > 1);
        let mut ranges = Vec::new();
        for unit in &units {
            let content = unit.content(&document);
            assert_eq!(unit.content_digest, document_digest(&content));
            for range in &unit.ranges {
                let text = document
                    .chars()
                    .skip(range.start_character)
                    .take(range.end_character.saturating_sub(range.start_character))
                    .collect::<String>();
                assert_eq!(range.content_digest, document_digest(&text));
                ranges.push((range.start_character, range.end_character, text));
            }
        }
        ranges.sort_by_key(|range| range.0);
        let mut cursor = 0;
        let mut reconstructed = String::new();
        for (start, end, text) in ranges {
            assert_eq!(start, cursor);
            cursor = end;
            reconstructed.push_str(&text);
        }
        assert_eq!(reconstructed, document);
        assert_eq!(cursor, document.chars().count());
    }
}
