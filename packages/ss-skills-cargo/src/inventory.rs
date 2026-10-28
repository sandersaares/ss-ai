use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use same_file::Handle;
use serde::Serialize;

use crate::discovery::{discover_members, identity};
use crate::manifest::read_manifest;

/// A versioned inventory of the root manifest and declared workspace members.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Inventory {
    /// Version of this JSON record contract.
    pub schema_version: u32,
    /// Absolute workspace root directory.
    pub workspace_root: String,
    /// The manifest supplied as the workspace root.
    pub workspace_manifest: WorkspaceManifest,
    /// Member manifests in deterministic path order.
    pub member_manifests: Vec<MemberManifest>,
    /// Caller-supplied UTC scan timestamp.
    pub scanned_at: String,
    /// Dependency declarations in manifest and source order.
    pub dependencies: Vec<Dependency>,
}

/// Identifies the workspace root manifest.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct WorkspaceManifest {
    /// Workspace-relative manifest path.
    pub path: String,
    /// Root package name, if present.
    pub package: Option<String>,
    /// Whether this root declares a workspace without a package.
    pub is_virtual_workspace: bool,
}

/// Identifies a workspace member.
#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct MemberManifest {
    /// Workspace-relative manifest path.
    pub path: String,
    /// Member package name, if present.
    pub package: Option<String>,
}

/// A dependency declaration and its source location.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Dependency {
    /// Workspace-relative manifest path.
    pub manifest: String,
    /// Declaring package name, if present.
    pub package: Option<String>,
    /// Dependency section, independent of the target condition.
    pub section: String,
    /// Target condition, if specified.
    pub target: Option<String>,
    /// Declared dependency key.
    pub name: String,
    /// Source style of the declaration.
    pub style: String,
    /// Declared version requirement.
    pub version: Option<String>,
    /// Declared path.
    pub path: Option<String>,
    /// Declared workspace inheritance setting.
    pub workspace: Option<bool>,
    /// Declared default feature setting.
    pub default_features: Option<bool>,
    /// Declared feature names.
    pub features: Option<Vec<String>>,
    /// Whether the tidy heuristic identifies a workspace member.
    pub same_workspace: bool,
    /// Manifest identified by the tidy heuristic.
    pub resolved_manifest: Option<String>,
    /// Original source from the start through the end of the declaration.
    pub raw: String,
    /// One-based line number of the declaration.
    pub line: usize,
}

/// Failure to produce a complete inventory.
#[ohno::error]
#[from(ScanFailure)]
pub struct InventoryError;

/// Carries a diagnostic for an unsuccessful inventory operation.
#[ohno::error]
#[display("{message}")]
pub(crate) struct ScanFailure {
    pub(crate) message: String,
}

/// Creates an inventory without changing the workspace.
///
/// `root` identifies either a workspace manifest or its directory.
/// `scanned_at` is provided by the caller so scanning does not read a clock.
///
/// # Errors
///
/// Returns an error when a manifest, workspace declaration, or directory
/// cannot be read or interpreted completely.
pub fn generate_inventory(root: &Path, scanned_at: Timestamp) -> Result<Inventory, InventoryError> {
    let root = if root.is_dir() {
        root.join("Cargo.toml")
    } else {
        root.to_path_buf()
    };
    let root = fs::canonicalize(&root).map_err(|error| {
        failure(format!(
            "Cannot access manifest {}: {error}",
            root.display()
        ))
    })?;
    let workspace_root = root.parent().ok_or_else(|| {
        failure(format!(
            "Manifest has no parent directory: {}",
            root.display()
        ))
    })?;
    let root_scan = read_manifest(&root, workspace_root)?;
    let member_paths = discover_members(workspace_root, &root, &root_scan)?;
    let root_identity = identity(workspace_root)?;
    let mut scans = Vec::new();
    let mut members = Vec::new();
    for path in member_paths {
        let directory = path.parent().ok_or_else(|| {
            failure(format!(
                "Manifest has no parent directory: {}",
                path.display()
            ))
        })?;
        let is_root = identity(directory)? == root_identity;
        let scan = if is_root {
            root_scan.clone()
        } else {
            read_manifest(&path, workspace_root)?
        };
        members.push(MemberManifest {
            path: scan.relative_path.clone(),
            package: scan.package.clone(),
        });
        scans.push((path, scan, is_root));
    }
    members.sort_by(|a, b| a.path.cmp(&b.path));
    scans.sort_by(|a, b| a.1.relative_path.cmp(&b.1.relative_path));

    let mut by_directory = HashMap::new();
    let mut by_name = BTreeMap::new();
    for (path, scan, _) in &scans {
        if let Some(directory) = path.parent() {
            by_directory.insert(identity(directory)?, scan.relative_path.clone());
        }
        if let Some(name) = &scan.package
            && by_name
                .insert(name.clone(), scan.relative_path.clone())
                .is_some()
        {
            return Err(failure(format!("Duplicate workspace package name: {name}")).into());
        }
    }

    let mut all_scans = vec![(root.clone(), root_scan.clone())];
    all_scans.extend(
        scans
            .into_iter()
            .filter(|(_, _, is_root)| !is_root)
            .map(|(path, scan, _)| (path, scan)),
    );
    let mut dependencies = Vec::new();
    for (path, scan) in &all_scans {
        let directory = path.parent().ok_or_else(|| {
            failure(format!(
                "Manifest has no parent directory: {}",
                path.display()
            ))
        })?;
        for mut dependency in scan.dependencies.clone() {
            if let Some(path) = &dependency.path {
                match Handle::from_path(directory.join(path)) {
                    Ok(resolved) => {
                        dependency.resolved_manifest = by_directory.get(&resolved).cloned();
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => {
                        return Err(failure(format!(
                            "Cannot resolve dependency path {path} in {}: {error}",
                            scan.relative_path
                        ))
                        .into());
                    }
                }
            }
            if dependency.resolved_manifest.is_none() {
                dependency.resolved_manifest = by_name.get(&dependency.name).cloned();
            }
            dependency.same_workspace = dependency.resolved_manifest.is_some();
            dependencies.push(dependency);
        }
    }

    let inherited: BTreeMap<String, String> = dependencies
        .iter()
        .filter(|item| item.section == "workspace.dependencies")
        .filter_map(|item| {
            item.resolved_manifest
                .as_ref()
                .map(|path| (item.name.clone(), path.clone()))
        })
        .collect();
    for dependency in &mut dependencies {
        if !dependency.same_workspace && dependency.workspace == Some(true) {
            dependency.resolved_manifest = inherited.get(&dependency.name).cloned();
            dependency.same_workspace = dependency.resolved_manifest.is_some();
        }
    }

    Ok(Inventory {
        schema_version: 1,
        workspace_root: path_string(workspace_root)?,
        workspace_manifest: WorkspaceManifest {
            path: root_scan.relative_path.clone(),
            package: root_scan.package.clone(),
            is_virtual_workspace: root_scan.has_workspace && !root_scan.has_package,
        },
        member_manifests: members,
        scanned_at: scanned_at.to_string(),
        dependencies,
    })
}

/// Creates a contextual failure without hiding the operation that failed.
pub(crate) fn failure(message: String) -> ScanFailure {
    ScanFailure::new(message)
}

/// Encodes paths without silently replacing invalid characters.
pub(crate) fn path_string(path: &Path) -> Result<String, ScanFailure> {
    let text = path
        .to_str()
        .ok_or_else(|| failure(format!("Path is not Unicode: {}", path.display())))?;
    #[cfg(windows)]
    let text = text.replace('\\', "/");
    #[cfg(not(windows))]
    let text = text.to_owned();
    Ok(text)
}

/// Computes a relative path without depending on the process working directory.
pub(crate) fn relative_path(path: &Path, root: &Path) -> Result<String, ScanFailure> {
    if let Ok(relative) = path.strip_prefix(root) {
        return path_string(relative);
    }
    // Ancestor handles distinguish filesystem identity from case spelling.
    let mut root_ancestors = Vec::new();
    for ancestor in root.ancestors() {
        root_ancestors.push(identity(ancestor)?);
    }
    let parent = path.parent().ok_or_else(|| {
        failure(format!(
            "Manifest has no parent directory: {}",
            path.display()
        ))
    })?;
    for ancestor in parent.ancestors() {
        let candidate = identity(ancestor)?;
        if let Some(steps) = root_ancestors.iter().position(|root| root == &candidate) {
            let mut relative = PathBuf::new();
            for _ in 0..steps {
                relative.push("..");
            }
            let suffix = path
                .strip_prefix(ancestor)
                .map_err(|error| failure(format!("Cannot make manifest path relative: {error}")))?;
            relative.push(suffix);
            return path_string(&relative);
        }
    }
    Err(failure(format!(
        "Manifest {} is on a different filesystem root from {}",
        path.display(),
        root.display()
    )))
}
