use std::sync::LazyLock;

use ohno::AppError;
use regex::Regex;
use serde::{Deserialize, Serialize};

use ss_ai_runtime::fail;

/// Changed path and commentable ranges in an exact unified comparison.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileDiff {
    path: String,
    previous_path: Option<String>,
    status: Status,
    hunks: Vec<Hunk>,
}

/// Side-specific ranges, including the context displayed by review services.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[allow(clippy::exhaustive_structs, reason = "Internal exact range record")]
pub struct Hunk {
    /// Original-side line start.
    pub old_start: u32,
    /// Original-side line count.
    pub old_count: u32,
    /// Current-side line start.
    pub new_start: u32,
    /// Current-side line count.
    pub new_count: u32,
}

/// File operation labels consumed by the existing review anchor model.
#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum Status {
    Added,
    Modified,
    Deleted,
    Renamed,
}

/// Accumulates a file until authoritative path headers have been read.
struct PendingFile {
    path: Option<String>,
    previous_path: Option<String>,
    fallback_path: Option<String>,
    status: Status,
    hunks: Vec<Hunk>,
}

impl PendingFile {
    fn finish(self) -> Result<FileDiff, AppError> {
        let path = self
            .path
            .or(self.fallback_path)
            .filter(|path| !path.is_empty())
            .ok_or_else(|| fail("invalidInput", "Diff has no unambiguous file path"))?;
        Ok(FileDiff {
            path,
            previous_path: (self.status == Status::Renamed)
                .then_some(self.previous_path)
                .flatten(),
            status: self.status,
            hunks: self.hunks,
        })
    }
}

/// Decodes Git's C-quoted UTF-8 bytes, not individual octal characters.
pub(crate) fn decode_path(value: &str) -> Result<String, AppError> {
    let Some(inner) = value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
    else {
        return Ok(value.to_owned());
    };
    let mut source = inner.bytes().peekable();
    let mut bytes = Vec::new();
    while let Some(byte) = source.next() {
        if byte != b'\\' {
            bytes.push(byte);
            continue;
        }
        let escaped = source
            .next()
            .ok_or_else(|| fail("invalidInput", "Unterminated Git path escape"))?;
        let byte = match escaped {
            b'a' => b'\x07',
            b'b' => b'\x08',
            b'f' => b'\x0c',
            b'n' => b'\n',
            b'r' => b'\r',
            b't' => b'\t',
            b'v' => b'\x0b',
            b'"' => b'"',
            b'\\' => b'\\',
            b'0'..=b'7' => {
                let mut value = u16::from(escaped.saturating_sub(b'0'));
                // Git C quoting encodes a byte in at most three octal digits.
                for _ in 0..2 {
                    let Some(next @ b'0'..=b'7') = source.peek().copied() else {
                        break;
                    };
                    source.next();
                    value = value
                        .saturating_mul(8)
                        .saturating_add(u16::from(next.saturating_sub(b'0')));
                }
                u8::try_from(value)?
            }
            _ => return Err(fail("invalidInput", "Unsupported Git path escape")),
        };
        bytes.push(byte);
    }
    Ok(String::from_utf8(bytes)?)
}

/// Normalizes service-facing paths without treating backslashes as separators.
pub fn normalize_path(value: &str, side_prefix: bool) -> Result<String, AppError> {
    let path = decode_path(value)?;
    let path = if side_prefix {
        path.strip_prefix("a/")
            .or_else(|| path.strip_prefix("b/"))
            .unwrap_or(&path)
    } else {
        &path
    };
    Ok(path
        .trim_start_matches("./")
        .trim_start_matches('/')
        .to_owned())
}

/// Resolves mode-only and binary headers without splitting spaces inside names.
fn header_path(header: &str) -> Result<Option<String>, AppError> {
    if header.starts_with('"') {
        let mut escaped = false;
        for (index, character) in header.char_indices().skip(1) {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                let rest = header
                    .get(index.saturating_add(1)..)
                    .and_then(|rest| rest.strip_prefix(' '))
                    .ok_or_else(|| fail("invalidInput", "Missing Git header path separator"))?;
                return normalize_path(rest, true).map(Some);
            }
        }
        return Err(fail("invalidInput", "Unterminated Git header path"));
    }
    for (index, _) in header.match_indices(" b/") {
        let old = header.get(..index).unwrap_or_default().strip_prefix("a/");
        let new = header.get(index.saturating_add(3)..);
        if old.is_some() && old == new {
            return Ok(new.map(str::to_owned));
        }
    }
    // Nonmatching names require authoritative content or rename headers.
    Ok(None)
}

/// Parses complete unified hunks; incomplete evidence is never a partial success.
pub fn parse(text: &str) -> Result<Vec<FileDiff>, AppError> {
    let mut files = Vec::new();
    let mut current: Option<PendingFile> = None;
    let mut remaining = (0_u32, 0_u32);
    for line in text.lines() {
        if remaining != (0, 0) {
            remaining = consume_line(line, remaining)?;
            continue;
        }
        if let Some(header) = line.strip_prefix("diff --git ") {
            if let Some(file) = current.take() {
                files.push(file.finish()?);
            }
            current = Some(PendingFile {
                path: None,
                previous_path: None,
                fallback_path: header_path(header)?,
                status: Status::Modified,
                hunks: Vec::new(),
            });
            continue;
        }
        let Some(file) = &mut current else {
            if !line.is_empty() {
                return Err(fail("invalidInput", "Expected a Git diff header"));
            }
            continue;
        };
        if line.starts_with("new file mode ") {
            file.status = Status::Added;
        } else if line.starts_with("deleted file mode ") {
            file.status = Status::Deleted;
        } else if let Some(path) = line.strip_prefix("rename from ") {
            file.status = Status::Renamed;
            file.previous_path = Some(normalize_path(path, false)?);
        } else if let Some(path) = line.strip_prefix("rename to ") {
            file.status = Status::Renamed;
            file.path = Some(normalize_path(path, false)?);
        } else if let Some(path) = line.strip_prefix("--- ") {
            let path = path.strip_suffix('\t').unwrap_or(path);
            if path == "/dev/null" {
                file.status = Status::Added;
            } else if file.previous_path.is_none() {
                file.previous_path = Some(normalize_path(path, true)?);
            }
        } else if let Some(path) = line.strip_prefix("+++ ") {
            let path = path.strip_suffix('\t').unwrap_or(path);
            if path == "/dev/null" {
                file.status = Status::Deleted;
                file.path.clone_from(&file.previous_path);
            } else if file.status != Status::Renamed || file.path.is_none() {
                file.path = Some(normalize_path(path, true)?);
            }
        } else if line.starts_with("@@") {
            let hunk = parse_hunk(line)?;
            remaining = (hunk.old_count, hunk.new_count);
            file.hunks.push(hunk);
        }
    }
    if remaining != (0, 0) {
        return Err(fail("invalidInput", "Truncated Git diff hunk"));
    }
    if let Some(file) = current {
        files.push(file.finish()?);
    }
    Ok(files)
}

/// Hunk counts distinguish header-looking source lines from actual diff headers.
fn consume_line(line: &str, (old, new): (u32, u32)) -> Result<(u32, u32), AppError> {
    let reduced = match line.as_bytes().first().copied() {
        Some(b' ') | None => old.checked_sub(1).zip(new.checked_sub(1)),
        Some(b'-') => old.checked_sub(1).map(|old| (old, new)),
        Some(b'+') => new.checked_sub(1).map(|new| (old, new)),
        Some(b'\\') if line == "\\ No newline at end of file" => Some((old, new)),
        _ => None,
    };
    reduced.ok_or_else(|| fail("invalidInput", "Git diff body disagrees with hunk counts"))
}

/// Parses numeric ranges without allowing malformed or overflowing line numbers.
fn parse_hunk(line: &str) -> Result<Hunk, AppError> {
    static HEADER: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^@@ -([0-9]+)(?:,([0-9]+))? \+([0-9]+)(?:,([0-9]+))? @@")
            .expect("The fixed unified-diff header pattern is valid")
    });
    let captures = HEADER
        .captures(line)
        .ok_or_else(|| fail("invalidInput", "Invalid Git diff hunk header"))?;
    let number = |index, default| -> Result<u32, AppError> {
        captures
            .get(index)
            .map_or(Ok(default), |value| Ok(value.as_str().parse()?))
    };
    let hunk = Hunk {
        old_start: number(1, 0)?,
        old_count: number(2, 1)?,
        new_start: number(3, 0)?,
        new_count: number(4, 1)?,
    };
    for (start, count) in [
        (hunk.old_start, hunk.old_count),
        (hunk.new_start, hunk.new_count),
    ] {
        if count > 0 && (start == 0 || start.checked_add(count.saturating_sub(1)).is_none()) {
            return Err(fail("invalidInput", "Invalid Git diff line range"));
        }
    }
    Ok(hunk)
}

/// Retains exact hunk bytes from an isolated blob comparison, without synthetic paths.
pub fn blob_patch(diff: &str) -> Result<&str, AppError> {
    let mut offset = 0_usize;
    for line in diff.split_inclusive('\n') {
        if line.starts_with("@@ ") {
            return diff
                .get(offset..)
                .ok_or_else(|| fail("invalidState", "Invalid patch boundary"));
        }
        if !line.ends_with('\n')
            || !["diff --git ", "index ", "--- ", "+++ "]
                .iter()
                .any(|prefix| line.starts_with(prefix))
        {
            return Err(fail("invalidState", "Unexpected isolated blob diff header"));
        }
        offset = offset.saturating_add(line.len());
    }
    Ok("")
}

/// Proves that complete ordered hunks reconstruct captured text byte for byte.
///
/// Unlike the service range parser, this retains CRLF and requires exact missing-newline
/// markers. Unchanged gaps must match on both sides, so omitted edits cannot pass.
pub fn validate_patch(patch: &str, old: &str, new: &str) -> Result<Vec<Hunk>, AppError> {
    let old = old.split_inclusive('\n').collect::<Vec<_>>();
    let new = new.split_inclusive('\n').collect::<Vec<_>>();
    let mut lines = patch.split_inclusive('\n').peekable();
    let mut cursors = (0_usize, 0_usize);
    let mut hunks = Vec::new();
    while let Some(header) = lines.next() {
        let header = header
            .strip_suffix('\n')
            .ok_or_else(|| fail("invalidState", "Unterminated patch header"))?;
        let hunk = parse_hunk(header)?;
        let start = |line: u32, count: u32| -> Result<usize, AppError> {
            Ok(usize::try_from(if count == 0 {
                line
            } else {
                line.saturating_sub(1)
            })?)
        };
        let starts = (
            start(hunk.old_start, hunk.old_count)?,
            start(hunk.new_start, hunk.new_count)?,
        );
        if old.get(cursors.0..starts.0).is_none()
            || old.get(cursors.0..starts.0) != new.get(cursors.1..starts.1)
        {
            return Err(fail(
                "invalidState",
                "Patch omits or reorders changed source",
            ));
        }
        cursors = starts;
        let mut remaining = (hunk.old_count, hunk.new_count);
        let mut changed = false;
        while remaining != (0, 0) {
            let line = lines
                .next()
                .ok_or_else(|| fail("invalidState", "Truncated exact patch"))?;
            let (kind, mut text) = line
                .split_at_checked(1)
                .ok_or_else(|| fail("invalidState", "Missing patch line indicator"))?;
            if !text.ends_with('\n') {
                return Err(fail("invalidState", "Unterminated patch body"));
            }
            if lines.peek() == Some(&"\\ No newline at end of file\n") {
                lines.next();
                text = text
                    .strip_suffix('\n')
                    .ok_or_else(|| fail("invalidState", "Invalid newline marker"))?;
            }
            remaining = consume_line(line, remaining)?;
            match kind {
                " " => {
                    exact_line(&old, &mut cursors.0, text)?;
                    exact_line(&new, &mut cursors.1, text)?;
                }
                "-" => {
                    exact_line(&old, &mut cursors.0, text)?;
                    changed = true;
                }
                "+" => {
                    exact_line(&new, &mut cursors.1, text)?;
                    changed = true;
                }
                _ => return Err(fail("invalidState", "Invalid exact patch line")),
            }
        }
        if !changed {
            return Err(fail("invalidState", "Patch hunk contains no edit"));
        }
        hunks.push(hunk);
    }
    if old.get(cursors.0..) != new.get(cursors.1..) {
        return Err(fail("invalidState", "Patch omits changed source tail"));
    }
    Ok(hunks)
}

/// Checks a body line against its captured side before advancing.
fn exact_line(lines: &[&str], cursor: &mut usize, text: &str) -> Result<(), AppError> {
    if lines.get(*cursor).copied() != Some(text) {
        return Err(fail(
            "invalidState",
            "Patch text differs from captured source",
        ));
    }
    *cursor = cursor
        .checked_add(1)
        .ok_or_else(|| fail("invalidState", "Patch line offset overflow"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_patches_preserve_line_endings_and_reject_omitted_reordered_or_corrupt_text() {
        let patch =
            "@@ -1,3 +1,3 @@\n before\r\n-old\r\n+new\r\n last\n\\ No newline at end of file\n";
        validate_patch(patch, "before\r\nold\r\nlast", "before\r\nnew\r\nlast").unwrap();
        for corrupt in [
            patch.replace("\r\n", "\n"),
            patch.replace("-old", "-bad"),
            patch.replace("+new", "+bad"),
            patch.replace("\\ No newline at end of file\n", ""),
            patch.replace("1,3", "2,3"),
            format!("{patch}{patch}"),
            String::new(),
        ] {
            validate_patch(&corrupt, "before\r\nold\r\nlast", "before\r\nnew\r\nlast").unwrap_err();
        }
        validate_patch("", "", "").unwrap();
        validate_patch("@@ -0,0 +1 @@\n+\u{1f916}\n", "", "\u{1f916}\n").unwrap();
        validate_patch(
            "@@ -1 +0,0 @@\n-\u{1f916}\n\\ No newline at end of file\n",
            "\u{1f916}",
            "",
        )
        .unwrap();
        validate_patch(
            "@@ -2 +2 @@\n-old\n+new\n",
            "omitted\nold\n",
            "changed\nnew\n",
        )
        .unwrap_err();
        validate_patch(
            "@@ -1 +1 @@\n-old\n+new\n",
            "old\nomitted\n",
            "new\nchanged\n",
        )
        .unwrap_err();
    }

    #[test]
    fn quoted_paths_are_lossless() {
        for (encoded, expected) in [
            (r#""a/caf\303\251.rs""#, "a/caf\u{e9}.rs"),
            (r#""a/\360\237\244\226.rs""#, "a/\u{1f916}.rs"),
            (r#""a/name\\with\tquote\".rs""#, "a/name\\with\tquote\".rs"),
            ("a/trailing ", "a/trailing "),
        ] {
            assert_eq!(decode_path(encoded).unwrap(), expected);
        }
        for encoded in [r#""\777""#, r#""\q""#, r#""\377""#, "\"a\\\""] {
            decode_path(encoded).unwrap_err();
        }
    }

    #[test]
    fn service_paths_preserve_case_backslashes_and_spaces() {
        assert_eq!(normalize_path("./a/File ", false).unwrap(), "a/File ");
        assert_eq!(normalize_path("a/a/File", true).unwrap(), "a/File");
        assert_eq!(normalize_path(r".\File", false).unwrap(), r".\File");
    }

    #[test]
    fn truncated_or_overflowing_evidence_is_rejected() {
        let prefix = "diff --git a/file b/file\n--- a/file\n+++ b/file\n";
        for hunk in [
            "@@ -1,2 +1,2 @@\n-a\n+b",
            "@@ -1 +1 @@\n--- file\n+++ file\n@@ broken",
            "@@ -4294967295,2 +1 @@\n-a\n+b",
            "@@ -1 +1 @@\n-a",
        ] {
            parse(&format!("{prefix}{hunk}")).unwrap_err();
        }
    }

    #[test]
    fn bounded_generated_parser_inputs_always_terminate() {
        let fragments = [
            "diff --git a/file b/file\n",
            "@@ -1 +1 @@\n",
            "--- a/file\n",
            "+++ b/file\n",
            "-a\n",
            "+a\n",
            " context\n",
            "\\ No newline at end of file\n",
        ];
        // Exhaust every short fragment sequence; length bounds guarantee progress
        // even when fragments resemble headers inside an incomplete hunk.
        for first in fragments {
            for second in fragments {
                for third in fragments {
                    let _result = parse(&format!("{first}{second}{third}"));
                    let _exact = validate_patch(&format!("{first}{second}{third}"), "a\n", "b\n");
                }
            }
        }
    }
}
