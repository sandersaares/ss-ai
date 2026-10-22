use std::collections::HashSet;
use std::path::Path;

use ohno::AppError;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

use crate::Process;
use crate::error::fail;
use crate::github_approval::commit;
use crate::json::required_optional;
use crate::target::{Platform, Target};
use crate::transport::Transport;

/// Complete exact source-tree changes, including path, object, and mode differences.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Inventory {
    pub(crate) old_commit: String,
    pub(crate) new_commit: String,
    pub(crate) old_tree: String,
    pub(crate) new_tree: String,
    pub(crate) changes: Vec<Change>,
    pub(crate) complete: bool,
}

/// Each changed path pair has one stable ID and explicit absent or unreadable sides.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Change {
    pub(crate) id: String,
    pub(crate) kind: String,
    #[serde(deserialize_with = "required_optional")]
    pub(crate) old: Option<Side>,
    #[serde(deserialize_with = "required_optional")]
    pub(crate) new: Option<Side>,
}

/// Git paths remain data; no path is checked out or used to open a working-tree file.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Side {
    pub(crate) path: String,
    pub(crate) object: String,
    pub(crate) mode: String,
    pub(crate) content: Content,
}

/// Unknown content cannot masquerade as an empty file.
#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub(crate) enum Content {
    Text { text: String },
    Binary { size: u64 },
    TooLarge { size: u64 },
    Submodule,
}

impl Inventory {
    pub(crate) fn validate(&self) -> Result<(), AppError> {
        for id in [
            &self.old_commit,
            &self.new_commit,
            &self.old_tree,
            &self.new_tree,
        ] {
            commit(id)?;
        }
        if !self.complete || (self.old_tree == self.new_tree) != self.changes.is_empty() {
            return Err(fail(
                "invalidState",
                "Incomplete or contradictory source inventory",
            ));
        }
        let mut ids = HashSet::new();
        let mut old_paths = HashSet::new();
        let mut new_paths = HashSet::new();
        for change in &self.changes {
            if change.id != change_id(&change.kind, change.old.as_ref(), change.new.as_ref())?
                || !ids.insert(&change.id)
            {
                return Err(fail("invalidState", "Invalid or repeated change ID"));
            }
            let valid = match change.kind.as_str() {
                "A" => change.old.is_none() && change.new.is_some(),
                "D" => change.old.is_some() && change.new.is_none(),
                "M" | "T" | "R" => change.old.is_some() && change.new.is_some(),
                _ => false,
            };
            if !valid {
                return Err(fail("invalidState", "Invalid changed-path shape"));
            }
            if let (Some(old), Some(new)) = (&change.old, &change.new)
                && ((change.kind == "R") == (old.path == new.path)
                    || (old.path == new.path && old.object == new.object && old.mode == new.mode))
            {
                return Err(fail("invalidState", "Contradictory source change"));
            }
            for (side, paths) in [(&change.old, &mut old_paths), (&change.new, &mut new_paths)] {
                if let Some(side) = side {
                    commit(&side.object)?;
                    if side
                        .path
                        .split('/')
                        .any(|part| matches!(part, "" | "." | ".."))
                        || side.path.contains('\0')
                        || !paths.insert(&side.path)
                        || !matches!(
                            side.mode.as_str(),
                            "100644" | "100755" | "120000" | "160000"
                        )
                        || (side.mode == "160000") != matches!(side.content, Content::Submodule)
                    {
                        return Err(fail("invalidState", "Invalid inventory side"));
                    }
                }
            }
        }
        Ok(())
    }
}

impl Change {
    /// Absent sides describe additions/deletions; opaque present sides lack assessment evidence.
    pub(crate) fn content_available(&self) -> bool {
        self.old
            .iter()
            .chain(self.new.iter())
            .all(|side| matches!(side.content, Content::Text { .. }))
    }
}

/// Fetches only into an owned bare store and never executes source content or repository hooks.
pub(crate) fn prepare(
    process: &dyn Process,
    url: &str,
    old: &str,
    new: &str,
    tenant: Option<&str>,
) -> Result<Inventory, AppError> {
    commit(old)?;
    commit(new)?;
    let temporary = TempDir::new()?;
    let root = temporary.path();
    git(process, root, &["init", "--bare", "--template="])?;
    let arguments = [
        "fetch",
        "--no-tags",
        "--no-write-fetch-head",
        "--no-recurse-submodules",
        "--no-auto-maintenance",
        url,
        old,
        new,
    ];
    let target = Target::repository(url)?;
    if target.platform == Platform::AzureDevOps {
        let token = Transport::azure_access_token(process, tenant)?;
        // OAuth owns Azure authentication; never ask the GitHub helper for another host.
        // Redirects must not forward the invocation's OAuth header to another location.
        let mut args = vec!["-c".to_owned(), "http.followRedirects=false".to_owned()];
        args.extend(git_arguments(&arguments, None));
        let output = process.isolated_git_oauth(root, &args, url, &token)?;
        if !output.success {
            return Err(fail(
                "service",
                format!(
                    "Isolated Azure Git fetch failed: {}",
                    String::from_utf8_lossy(&output.stderr).replace(&token, "[redacted]")
                ),
            ));
        }
    } else {
        git(process, root, &arguments)?;
    }
    for id in [old, new] {
        if git(process, root, &["cat-file", "-t", id])? != b"commit\n" {
            return Err(fail("service", "Source endpoint is not a Git commit"));
        }
    }
    let old_tree = line(git(
        process,
        root,
        &["rev-parse", &format!("{old}^{{tree}}")],
    )?)?;
    let new_tree = line(git(
        process,
        root,
        &["rev-parse", &format!("{new}^{{tree}}")],
    )?)?;
    let raw = git(
        process,
        root,
        &[
            "diff",
            "--raw",
            "-z",
            "--no-abbrev",
            "--find-renames",
            "--no-ext-diff",
            "--no-textconv",
            "--no-relative",
            "--ignore-submodules=none",
            old,
            new,
            "--",
        ],
    )?;
    let mut changes = parse(&raw)?;
    // Bounded snapshots keep large source trees from consuming unbounded agent/state memory.
    // Crossing either budget is explicit unreadable evidence, never an empty-content success.
    let mut remaining = 64 * 1024 * 1024;
    for change in &mut changes {
        for side in [&mut change.old, &mut change.new].into_iter().flatten() {
            side.content = content(process, root, side, &mut remaining)?;
        }
        change.id = change_id(&change.kind, change.old.as_ref(), change.new.as_ref())?;
    }
    let inventory = Inventory {
        old_commit: old.to_owned(),
        new_commit: new.to_owned(),
        old_tree,
        new_tree,
        changes,
        complete: true,
    };
    inventory.validate()?;
    Ok(inventory)
}

/// NUL-delimited raw diff records preserve arbitrary UTF-8 path spelling without C-quote parsing.
fn parse(bytes: &[u8]) -> Result<Vec<Change>, AppError> {
    if !bytes.is_empty() && !bytes.ends_with(&[0]) {
        return Err(fail("service", "Truncated raw Git inventory"));
    }
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    let mut fields = bytes
        .strip_suffix(&[0])
        .ok_or_else(|| fail("service", "Missing raw diff terminator"))?
        .split(|byte| *byte == 0);
    let mut changes = Vec::new();
    while let Some(header) = fields.next() {
        let header = str::from_utf8(header)?;
        let parts = header
            .strip_prefix(':')
            .ok_or_else(|| fail("service", "Invalid Git raw header"))?
            .split(' ')
            .collect::<Vec<_>>();
        let [old_mode, new_mode, old_object, new_object, status] = parts.as_slice() else {
            return Err(fail("service", "Incomplete Git raw header"));
        };
        let kind = status
            .get(..1)
            .ok_or_else(|| fail("service", "Empty Git change status"))?;
        if !matches!(kind, "A" | "D" | "M" | "T" | "R")
            || (kind != "R" && status.len() != 1)
            || (kind == "R"
                && status
                    .get(1..)
                    .is_none_or(|score| !score.parse::<u8>().is_ok_and(|score| score <= 100)))
        {
            return Err(fail("service", "Unsupported Git change status"));
        }
        let path = str::from_utf8(
            fields
                .next()
                .ok_or_else(|| fail("service", "Missing Git path"))?,
        )?;
        let new_path = if kind == "R" {
            str::from_utf8(
                fields
                    .next()
                    .ok_or_else(|| fail("service", "Missing Git rename destination"))?,
            )?
        } else {
            path
        };
        let side = |mode: &str, object: &str, path: &str| -> Result<Option<Side>, AppError> {
            if mode == "000000" {
                if object != "0".repeat(40) {
                    return Err(fail("service", "Absent Git side has an object"));
                }
                return Ok(None);
            }
            commit(object)?;
            Ok(Some(Side {
                path: path.to_owned(),
                object: object.to_owned(),
                mode: mode.to_owned(),
                content: Content::Submodule,
            }))
        };
        changes.push(Change {
            id: String::new(),
            kind: kind.to_owned(),
            old: side(old_mode, old_object, path)?,
            new: side(new_mode, new_object, new_path)?,
        });
    }
    Ok(changes)
}

fn content(
    process: &dyn Process,
    root: &Path,
    side: &Side,
    remaining: &mut u64,
) -> Result<Content, AppError> {
    if side.mode == "160000" {
        return Ok(Content::Submodule);
    }
    let size: u64 = line(git(process, root, &["cat-file", "-s", &side.object])?)?.parse()?;
    // A single unusually large file should not consume the complete snapshot budget.
    if size > 1024 * 1024 || size > *remaining {
        return Ok(Content::TooLarge { size });
    }
    let bytes = git(process, root, &["cat-file", "blob", &side.object])?;
    if u64::try_from(bytes.len())? != size {
        return Err(fail("service", "Truncated Git blob"));
    }
    *remaining = remaining
        .checked_sub(size)
        .ok_or_else(|| fail("invalidState", "Content budget underflow"))?;
    if bytes.contains(&0) {
        return Ok(Content::Binary { size });
    }
    Ok(match String::from_utf8(bytes) {
        Ok(text) => Content::Text { text },
        Err(_) => Content::Binary { size },
    })
}

fn change_id(kind: &str, old: Option<&Side>, new: Option<&Side>) -> Result<String, AppError> {
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&(kind, old, new))?)
    ))
}

fn line(bytes: Vec<u8>) -> Result<String, AppError> {
    let text = String::from_utf8(bytes)?;
    Ok(text.trim_end_matches('\n').to_owned())
}

/// Configuration is invocation-owned; GitHub CLI alone supplies HTTPS credentials.
fn git(process: &dyn Process, root: &Path, arguments: &[&str]) -> Result<Vec<u8>, AppError> {
    let args = git_arguments(arguments, Some("!gh auth git-credential"));
    let output = process.isolated_git(root, &args)?;
    if !output.success {
        return Err(fail(
            "io",
            format!(
                "Isolated Git object read failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ),
        ));
    }
    Ok(output.stdout)
}

/// Keeps object reads independent of inherited hooks, replacement objects, and helpers.
fn git_arguments(arguments: &[&str], helper: Option<&str>) -> Vec<String> {
    let mut args = [
        "--no-pager",
        "--no-replace-objects",
        "-c",
        "core.hooksPath=disabled-hooks",
        "-c",
        "credential.helper=",
        "-c",
        "submodule.recurse=false",
        "-c",
        "fetch.fsckObjects=true",
        "-c",
        "maintenance.auto=false",
    ]
    .map(str::to_owned)
    .to_vec();
    if let Some(helper) = helper {
        args.extend(["-c".to_owned(), format!("credential.helper={helper}")]);
    }
    args.extend(arguments.iter().map(|value| (*value).to_owned()));
    args
}

#[cfg(test)]
mod tests {
    use super::parse;
    use uuid::Uuid;

    #[test]
    fn raw_inventory_requires_exact_complete_records() {
        let old = "a".repeat(40);
        let new = "b".repeat(40);
        let renamed = format!(":100644 100644 {old} {new} R100\0old name\0new\nname\0");
        let changes = parse(renamed.as_bytes()).unwrap();
        let change = changes.first().unwrap();
        assert_eq!(change.old.as_ref().unwrap().path, "old name");
        assert_eq!(change.new.as_ref().unwrap().path, "new\nname");
        for malformed in [
            format!(":100644 100644 {old} {new} R101\0old\0new\0"),
            format!(":100644 100644 {old} {new} R100\0old\0"),
            format!(":100644 100644 {old} {new} M\0unterminated"),
            format!(":000000 100644 {old} {new} A\0new\0"),
            format!(":100644 100644 {old} {new} Q\0path\0"),
        ] {
            parse(malformed.as_bytes()).unwrap_err();
        }
        parse(&[0xff, 0]).unwrap_err();
        assert!(parse(&[]).unwrap().is_empty());
    }

    #[test]
    fn bounded_generated_raw_records_terminate_without_panics() {
        // Fresh seeds cover truncated and arbitrary byte records without a separate fuzz runner.
        for _ in 0..64 {
            let seed = Uuid::new_v4();
            for end in 0..=seed.as_bytes().len() {
                let input = seed.as_bytes().get(..end).unwrap();
                drop(parse(input));
            }
        }
    }
}
