//! Stable, exact-spelling source snapshots for focus-area preparation.
#![allow(
    clippy::indexing_slicing,
    reason = "BOM and chunk lengths are checked, and text offsets come from character boundaries"
)]
#![allow(
    clippy::arithmetic_side_effects,
    reason = "Line offsets and counters are bounded by the allocated source text"
)]

use std::collections::{BTreeMap, btree_map::Entry};
use std::fs;
use std::path::{Path, PathBuf};

use ohno::AppError;
use serde::{Deserialize, Serialize};
use ss_ai_runtime::{Clock, caused_by, fail, read_bytes};

/// A one-based, inclusive selection from a materialized instruction source.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[expect(
    clippy::exhaustive_structs,
    reason = "The wire schema defines these exact fields"
)]
pub struct Span {
    /// The named source tree.
    pub root: String,
    /// Exact relative spelling, with either separator.
    pub path: String,
    /// First selected line.
    pub start_line: u64,
    /// Last selected line.
    pub end_line: u64,
}

/// A complete in-memory generation of both instruction trees.
#[derive(Debug)]
pub struct SourceSnapshot {
    files: BTreeMap<(String, String), Vec<String>>,
}

impl SourceSnapshot {
    /// Builds a snapshot from caller-provided lines for pure validation tests.
    #[cfg(test)]
    pub(crate) fn from_files(files: BTreeMap<(String, String), Vec<String>>) -> Self {
        Self { files }
    }

    /// Reads both source trees while the caller owns the cache-operation lock.
    ///
    /// # Errors
    ///
    /// Returns an error for links, unreadable entries, invalid paths, or
    /// distinct paths that normalize to the same source name.
    pub fn read(session: &Path, clock: &dyn Clock) -> Result<Self, AppError> {
        let mut files = BTreeMap::new();
        for root in ["instruction-set-source", "instruction-set-raw"] {
            let directory = session.join(root);
            let metadata = fs::symlink_metadata(&directory)
                .map_err(|error| caused_by("io", "Cannot inspect source root", error))?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(fail(
                    "invalidInput",
                    "Source root must be a directory without links",
                ));
            }
            read_directory(&directory, &directory, root, clock, &mut files)?;
        }
        Ok(Self {
            files: files
                .into_iter()
                .map(|(key, (_, lines))| (key, lines))
                .collect(),
        })
    }

    /// Resolves an exact source selection and checks its contents.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid paths, line ranges, or blank spans.
    pub fn span(&self, span: &Span) -> Result<&[String], AppError> {
        let path = source_path(&span.path)?;
        let lines = self
            .files
            .get(&(span.root.clone(), path))
            .ok_or_else(|| fail("invalidInput", "Source file not found with exact spelling"))?;
        let start = usize::try_from(span.start_line)
            .ok()
            .and_then(|line| line.checked_sub(1))
            .ok_or_else(|| fail("invalidInput", "Invalid source line"))?;
        let end = usize::try_from(span.end_line)
            .map_err(|error| caused_by("invalidInput", "Invalid source line", error))?;
        let selected = lines
            .get(start..end)
            .filter(|lines| !lines.is_empty())
            .ok_or_else(|| fail("invalidInput", "Invalid source line range"))?;
        if selected.iter().all(|line| line.trim().is_empty()) {
            return Err(fail("invalidInput", "Source span is blank"));
        }
        Ok(selected)
    }

    /// Returns the full lines of an expanded raw output.
    ///
    /// # Errors
    ///
    /// Returns an error when the named output is missing or blank.
    pub fn raw_output(&self, path: &str) -> Result<&[String], AppError> {
        let path = source_path(path)?;
        let lines = self
            .files
            .get(&("instruction-set-raw".to_owned(), path))
            .ok_or_else(|| fail("invalidInput", "Expanded output does not exist"))?;
        if lines.iter().all(|line| line.trim().is_empty()) {
            return Err(fail("invalidInput", "Expanded output is blank"));
        }
        Ok(lines)
    }

    /// Iterates source files in a stable order for complete-coverage checks.
    pub fn files(&self) -> impl Iterator<Item = (&str, &str, &[String])> {
        self.files
            .iter()
            .map(|((root, path), lines)| (root.as_str(), path.as_str(), lines.as_slice()))
    }
}

/// Converts both accepted separators into a safe, exact relative spelling.
pub fn source_path(path: &str) -> Result<String, AppError> {
    let path = path.replace('/', "\\");
    if path.split('\\').any(|part| {
        part.is_empty()
            || matches!(part, "." | "..")
            || part
                .chars()
                .any(|ch| matches!(ch, ':' | '*' | '?' | '"' | '<' | '>' | '|'))
    }) {
        return Err(fail(
            "invalidInput",
            "Source path must be safe and relative",
        ));
    }
    Ok(path)
}

/// Captures each file only once so validation and rendering see identical bytes.
fn read_directory(
    root_path: &Path,
    directory: &Path,
    root: &str,
    clock: &dyn Clock,
    files: &mut BTreeMap<(String, String), (PathBuf, Vec<String>)>,
) -> Result<(), AppError> {
    let entries = fs::read_dir(directory)
        .map_err(|error| caused_by("io", "Cannot enumerate source directory", error))?;
    for entry in entries {
        let entry = entry.map_err(|error| caused_by("io", "Cannot read source entry", error))?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| caused_by("io", "Cannot inspect source entry", error))?;
        if metadata.file_type().is_symlink() {
            return Err(fail("invalidInput", "Source trees must not contain links"));
        }
        if metadata.is_dir() {
            read_directory(root_path, &path, root, clock, files)?;
        } else if metadata.is_file() {
            let relative = path
                .strip_prefix(root_path)
                .map_err(|error| caused_by("invalidInput", "Invalid source path", error))?;
            let relative = relative
                .components()
                .map(|part| {
                    part.as_os_str()
                        .to_str()
                        .ok_or_else(|| fail("invalidInput", "Source path is not Unicode"))
                })
                .collect::<Result<Vec<_>, _>>()?
                .join("\\");
            let key = (root.to_owned(), source_path(&relative)?);
            match files.entry(key) {
                Entry::Occupied(previous) => {
                    // Both separators are accepted in source names, so these
                    // files cannot be distinguished by coverage records.
                    return Err(fail(
                        "invalidInput",
                        format!(
                            "Source paths {:?} and {:?} collide after separator normalization; rename one of the files",
                            previous.get().0,
                            path
                        ),
                    ));
                }
                Entry::Vacant(entry) => {
                    let bytes = read_bytes(&path, clock)?.ok_or_else(|| {
                        fail("invalidInput", "Source disappeared during snapshot")
                    })?;
                    entry.insert((path, text_lines(&decode_text(&bytes)?)));
                }
            }
        } else {
            return Err(fail(
                "invalidInput",
                "Source tree contains a non-file entry",
            ));
        }
    }
    Ok(())
}

/// Matches the byte-order-mark detection used by the source reader.
pub(crate) fn decode_text(bytes: &[u8]) -> Result<String, AppError> {
    if bytes.starts_with(&[0xff, 0xfe, 0, 0]) || bytes.starts_with(&[0, 0, 0xfe, 0xff]) {
        let little = bytes.starts_with(&[0xff, 0xfe, 0, 0]);
        let mut result = String::new();
        for chunk in bytes[4..].chunks(4) {
            if chunk.len() != 4 {
                return Err(fail("invalidInput", "Incomplete UTF-32 source"));
            }
            let data = [chunk[0], chunk[1], chunk[2], chunk[3]];
            let scalar = if little {
                u32::from_le_bytes(data)
            } else {
                u32::from_be_bytes(data)
            };
            result.push(
                char::from_u32(scalar)
                    .ok_or_else(|| fail("invalidInput", "Invalid UTF-32 source"))?,
            );
        }
        return Ok(result);
    }
    if bytes.starts_with(&[0xff, 0xfe]) || bytes.starts_with(&[0xfe, 0xff]) {
        let little = bytes.starts_with(&[0xff, 0xfe]);
        let mut units = Vec::new();
        for chunk in bytes[2..].chunks(2) {
            if chunk.len() != 2 {
                return Err(fail("invalidInput", "Incomplete UTF-16 source"));
            }
            units.push(if little {
                u16::from_le_bytes([chunk[0], chunk[1]])
            } else {
                u16::from_be_bytes([chunk[0], chunk[1]])
            });
        }
        return String::from_utf16(&units)
            .map_err(|error| caused_by("invalidInput", "Invalid UTF-16 source", error).into());
    }
    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
    String::from_utf8(bytes.to_vec())
        .map_err(|error| caused_by("invalidInput", "Invalid UTF-8 source", error).into())
}

/// Splits the three newline forms without inventing a final empty line.
#[expect(
    clippy::string_slice,
    reason = "Offsets come from char_indices or the end of an ASCII newline"
)]
fn text_lines(text: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((index, ch)) = chars.next() {
        if ch == '\r' || ch == '\n' {
            result.push(text[start..index].to_owned());
            start = index + ch.len_utf8();
            if ch == '\r' && chars.peek().is_some_and(|(_, next)| *next == '\n') {
                chars.next();
                start += 1;
            }
        }
    }
    if start < text.len() {
        result.push(text[start..].to_owned());
    }
    result
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[allow(
    clippy::assertions_on_result_states,
    reason = "Boolean result checks make invalid-path cases concise"
)]
mod tests {
    use super::{decode_text, source_path, text_lines};

    #[test]
    fn paths_reject_escape_without_changing_case() {
        assert_eq!(source_path("A/b.md").unwrap(), "A\\b.md");
        for path in ["", "/a", "../a", "a\\..\\b", "C:\\a", "a//b"] {
            assert!(source_path(path).is_err());
        }
    }

    #[test]
    fn source_decoding_preserves_line_boundaries() {
        assert_eq!(text_lines("a\r\nb\rc\n"), ["a", "b", "c"]);
        assert_eq!(decode_text(&[0xff, 0xfe, b'a', 0]).unwrap(), "a");
        assert!(decode_text(&[0xff, 0xfe, b'a']).is_err());
    }
}
