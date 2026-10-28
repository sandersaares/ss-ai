use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use glob::Pattern;
use same_file::Handle;

use crate::inventory::{ScanFailure, failure};
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
) -> Result<Vec<PathBuf>, ScanFailure> {
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
        if dirs.is_empty() {
            return Err(failure(format!(
                "Workspace member pattern matched nothing: {pattern}"
            )));
        }
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
                Err(error) => {
                    return Err(failure(format!(
                        "Cannot access {}: {error}",
                        path.display()
                    )));
                }
                Ok(_) => return Err(failure(format!("Not a manifest file: {}", path.display()))),
            }
        }
        if !found {
            return Err(failure(format!("No member manifest matched: {pattern}")));
        }
    }
    let mut paths: Vec<_> = members.into_values().collect();
    paths.sort();
    Ok(paths)
}

/// Traverses each pattern position at most once per filesystem directory.
fn expand(root: &Path, pattern: &str) -> Result<Vec<PathBuf>, ScanFailure> {
    if pattern.is_empty() || Path::new(pattern).is_absolute() {
        return Err(failure(format!("Invalid workspace pattern: {pattern}")));
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
        if visited.len() > MAX_STATES {
            return Err(failure(format!(
                "Workspace pattern exceeds traversal limit: {pattern}"
            )));
        }
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
            let matcher = Pattern::new(segment).map_err(|error| {
                failure(format!("Invalid workspace pattern {pattern}: {error}"))
            })?;
            for child in children(&directory)? {
                let name = child
                    .file_name()
                    .and_then(|name| name.to_str())
                    .ok_or_else(|| {
                        failure(format!(
                            "Directory name is not Unicode: {}",
                            child.display()
                        ))
                    })?;
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
                    return Err(failure(format!(
                        "Cannot access {}: {error}",
                        next.display()
                    )));
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
pub(crate) fn identity(path: &Path) -> Result<Handle, ScanFailure> {
    Handle::from_path(path)
        .map_err(|error| failure(format!("Cannot identify {}: {error}", path.display())))
}

/// Bounds pending work independently of the already visited state count.
fn enqueue(
    queue: &mut VecDeque<(PathBuf, usize)>,
    path: PathBuf,
    index: usize,
) -> Result<(), ScanFailure> {
    if queue.len() >= MAX_STATES {
        return Err(failure(
            "Workspace traversal exceeds pending work limit".into(),
        ));
    }
    queue.push_back((path, index));
    Ok(())
}

/// Enumerates children in stable order and reports incomplete reads.
fn children(directory: &Path) -> Result<Vec<PathBuf>, ScanFailure> {
    let entries = fs::read_dir(directory)
        .map_err(|error| failure(format!("Cannot enumerate {}: {error}", directory.display())))?;
    let mut children = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| {
            failure(format!("Cannot enumerate {}: {error}", directory.display()))
        })?;
        let path = entry.path();
        let metadata = fs::metadata(&path)
            .map_err(|error| failure(format!("Cannot access {}: {error}", path.display())))?;
        if metadata.is_dir() {
            // Match on the directory entry name, then canonicalize when enqueued.
            if children.len() >= MAX_STATES {
                return Err(failure(format!(
                    "Too many directories in {}",
                    directory.display()
                )));
            }
            children.push(path);
        }
    }
    children.sort();
    Ok(children)
}

/// Resolves filesystem identity without guessing case behavior from the OS.
fn canonical(path: &Path) -> Result<PathBuf, ScanFailure> {
    fs::canonicalize(path)
        .map_err(|error| failure(format!("Cannot resolve {}: {error}", path.display())))
}
