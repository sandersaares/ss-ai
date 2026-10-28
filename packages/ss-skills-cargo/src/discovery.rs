use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fs;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};

use glob::Pattern;
use same_file::Handle;

use crate::error::{
    InvalidPattern, MemberManifestMissing, MemberManifestNotFile, MissingMemberManifest,
    MissingMemberPattern, NonUnicodePath, ScanError, TraversalLimit, filesystem_failure,
};
use crate::manifest::Manifest;

/// Caps retained traversal state to limit memory and open filesystem handles.
///
/// The threshold is an arbitrary safety ceiling large enough for ordinary
/// workspaces; scans exceeding it fail rather than returning partial results.
const MAX_STATES: usize = 100_000;

/// Expands root member and exclude declarations into unique manifest paths.
pub(crate) fn discover_members(
    root: &Path,
    root_manifest: &Path,
    manifest: &Manifest,
) -> Result<Vec<PathBuf>, ScanError> {
    let mut excluded = HashSet::new();
    for pattern in &manifest.exclude {
        for directory in expand(root, pattern)? {
            excluded.insert(identity(&directory)?);
        }
    }

    let mut members = HashMap::new();
    if manifest.has_package || !manifest.has_workspace {
        let canonical_root = canonical(root)?;
        let root_id = identity(&canonical_root)?;
        if !excluded.contains(&root_id) {
            members.insert(root_id, root_manifest.to_path_buf());
        }
    }
    for pattern in &manifest.members {
        let dirs = expand(root, pattern)?;
        require_pattern_matches(pattern, !dirs.is_empty())?;
        let wildcard = pattern.contains(['*', '?', '[']);
        let mut found = false;
        for dir in dirs {
            let dir_id = identity(&dir)?;
            if excluded.contains(&dir_id) {
                found = true;
                continue;
            }
            let path = dir.join("Cargo.toml");
            match fs::metadata(&path) {
                Ok(metadata) if metadata.is_file() => {
                    members.entry(dir_id).or_insert(canonical(&path)?);
                    found = true;
                }
                Err(error) if error.kind() == ErrorKind::NotFound && wildcard => {}
                Ok(_) if wildcard => {}
                Err(error) if error.kind() == ErrorKind::NotFound => {
                    return Err(missing_member_manifest(path, error));
                }
                Err(error) => {
                    return Err(filesystem_failure("access", &path, error));
                }
                Ok(_) => return Err(MemberManifestNotFile::new(path).into()),
            }
        }
        if !found {
            return Err(MissingMemberManifest::new(pattern.clone()).into());
        }
    }
    let mut paths: Vec<_> = members.into_values().collect();
    paths.sort();
    Ok(paths)
}

/// Traverses each pattern position at most once per filesystem directory.
fn expand(root: &Path, pattern: &str) -> Result<Vec<PathBuf>, ScanError> {
    if pattern.is_empty() || Path::new(pattern).is_absolute() {
        return Err(InvalidPattern::new(pattern.to_owned()).into());
    }
    let segments: Vec<&str> = pattern
        .split(is_separator)
        .filter(|part| !part.is_empty())
        .collect();
    let mut queue = VecDeque::from([(canonical(root)?, 0_usize)]);
    let mut visited = HashSet::new();
    let mut matches = BTreeMap::new();
    while let Some((directory, index)) = queue.pop_front() {
        if !visited.insert((identity(&directory)?, index)) {
            continue;
        }
        check_traversal_limit(visited.len(), "visited state", pattern)?;
        let Some(segment) = segments.get(index) else {
            matches.insert(directory.clone(), directory);
            continue;
        };
        if *segment == "**" {
            enqueue(&mut queue, directory.clone(), index.saturating_add(1))?;
            for child in children(&directory)? {
                enqueue(&mut queue, canonical(&child)?, index)?;
            }
        } else if segment.contains(['*', '?', '[']) {
            let matcher = Pattern::new(segment)
                .map_err(|error| InvalidPattern::caused_by(pattern.to_owned(), error))?;
            for child in children(&directory)? {
                let name = child
                    .file_name()
                    .and_then(|name| name.to_str())
                    .ok_or_else(|| NonUnicodePath::new(child.clone()))?;
                if matcher.matches(name) {
                    enqueue(&mut queue, canonical(&child)?, index.saturating_add(1))?;
                }
            }
        } else {
            let next = directory.join(segment);
            match fs::metadata(&next) {
                Ok(metadata) if metadata.is_dir() => {
                    enqueue(&mut queue, canonical(&next)?, index.saturating_add(1))?;
                }
                Ok(_) => {}
                Err(error) if error.kind() == ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(filesystem_failure("access", &next, error));
                }
            }
        }
    }
    Ok(matches.into_values().collect())
}

/// Interprets native path separators while preserving Unix backslashes.
fn is_separator(character: char) -> bool {
    std::path::is_separator(character)
}

/// Retains an open handle so identity is not inferred from path spelling.
pub(crate) fn identity(path: &Path) -> Result<Handle, ScanError> {
    Handle::from_path(path).map_err(|error| filesystem_failure("identify", path, error))
}

/// Rejects missing member matches separately from manifest and I/O failures.
fn require_pattern_matches(pattern: &str, matched: bool) -> Result<(), ScanError> {
    if matched {
        Ok(())
    } else {
        Err(MissingMemberPattern::new(pattern.to_owned()).into())
    }
}

/// Preserves both the missing manifest path and the filesystem cause.
fn missing_member_manifest(path: PathBuf, error: io::Error) -> ScanError {
    MemberManifestMissing::caused_by(path, error).into()
}

/// Stops traversal before its retained state exceeds the configured bound.
fn check_traversal_limit(
    count: usize,
    scope: &'static str,
    subject: &str,
) -> Result<(), ScanError> {
    if count > MAX_STATES {
        return Err(TraversalLimit::new(scope, subject.to_owned()).into());
    }
    Ok(())
}

/// Bounds pending work independently of the already visited state count.
fn enqueue(
    queue: &mut VecDeque<(PathBuf, usize)>,
    path: PathBuf,
    index: usize,
) -> Result<(), ScanError> {
    if queue.len() >= MAX_STATES {
        return Err(TraversalLimit::new("pending work", path.display().to_string()).into());
    }
    queue.push_back((path, index));
    Ok(())
}

/// Enumerates children in stable order and reports incomplete reads.
fn children(directory: &Path) -> Result<Vec<PathBuf>, ScanError> {
    let entries = fs::read_dir(directory)
        .map_err(|error| filesystem_failure("enumerate", directory, error))?;
    let mut children = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| filesystem_failure("enumerate", directory, error))?;
        let path = entry.path();
        let metadata =
            fs::metadata(&path).map_err(|error| filesystem_failure("access", &path, error))?;
        if metadata.is_dir() {
            // Match on the directory entry name, then canonicalize when enqueued.
            if children.len() >= MAX_STATES {
                return Err(TraversalLimit::new(
                    "directory entries",
                    directory.display().to_string(),
                )
                .into());
            }
            children.push(path);
        }
    }
    children.sort();
    Ok(children)
}

/// Resolves filesystem identity without guessing case behavior from the OS.
fn canonical(path: &Path) -> Result<PathBuf, ScanError> {
    fs::canonicalize(path).map_err(|error| filesystem_failure("resolve", path, error))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use std::io;
    use std::path::PathBuf;

    use ohno::ErrorExt;

    use super::{
        MAX_STATES, check_traversal_limit, missing_member_manifest, require_pattern_matches,
    };
    use crate::error::{MemberManifestMissing, MissingMemberPattern, TraversalLimit};

    #[test]
    fn reports_missing_member_pattern_with_its_identity() {
        let error = require_pattern_matches("missing", false).unwrap_err();
        let leaf = error.find_source::<MissingMemberPattern>().unwrap();
        assert_eq!(leaf.pattern, "missing");
    }

    #[test]
    fn reports_the_specific_exceeded_limit() {
        let error =
            check_traversal_limit(MAX_STATES + 1, "visited state", "crates/**").unwrap_err();
        let leaf = error.find_source::<TraversalLimit>().unwrap();
        assert_eq!(leaf.scope, "visited state");
        assert_eq!(leaf.subject, "crates/**");
    }

    #[test]
    fn preserves_missing_manifest_identity_and_io_source() {
        let error = missing_member_manifest(
            PathBuf::from("member/Cargo.toml"),
            io::Error::from(io::ErrorKind::NotFound),
        );
        let leaf = error.find_source::<MemberManifestMissing>().unwrap();
        assert_eq!(leaf.path, PathBuf::from("member/Cargo.toml"));
        assert_eq!(
            error.find_source::<io::Error>().unwrap().kind(),
            io::ErrorKind::NotFound
        );
    }
}
