use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use same_file::Handle;
use serde::Serialize;

use crate::InventoryError;
use crate::discovery::{discover_members, identity};
use crate::error::{
    DependencyPathFailure, DifferentFilesystemRoot, DuplicatePackage, MissingParent,
    NonUnicodePath, RelativePathFailure, ScanError, filesystem_failure,
};
use crate::manifest::{Manifest, read_manifest};

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
    pub line: NonZeroUsize,
}

/// Holds the parsed non-root members separately from their displayed entries.
struct MemberScans {
    members: Vec<MemberManifest>,
    scans: Vec<(PathBuf, Manifest)>,
    root_is_member: bool,
}

/// Supports the tidy heuristic without retaining duplicate manifest data.
struct MemberIndex {
    directories: HashMap<Handle, String>,
    packages: BTreeMap<String, String>,
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
    let root = fs::canonicalize(&root)
        .map_err(|error| filesystem_failure("access manifest", &root, error))?;
    let workspace_root = parent(&root)?;
    let root_scan = read_manifest(&root, workspace_root)?;
    let member_paths = discover_members(workspace_root, &root, &root_scan)?;
    let member_scans = scan_members(workspace_root, member_paths, &root_scan)?;
    let index = index_members(&root, &root_scan, &member_scans)?;
    let workspace_root = path_string(workspace_root)?;
    let workspace_manifest = WorkspaceManifest {
        path: root_scan.relative_path.clone(),
        package: root_scan.package.clone(),
        is_virtual_workspace: root_scan.has_workspace && !root_scan.has_package,
    };
    let all_scans = std::iter::once((root, root_scan)).chain(member_scans.scans);
    let dependencies = classify_dependencies(all_scans, &index)?;

    Ok(Inventory {
        schema_version: 1,
        workspace_root,
        workspace_manifest,
        member_manifests: member_scans.members,
        scanned_at: scanned_at.to_string(),
        dependencies,
    })
}

/// Scans each member once, borrowing the already-read root when it is a member.
fn scan_members(
    workspace_root: &Path,
    member_paths: Vec<PathBuf>,
    root_scan: &Manifest,
) -> Result<MemberScans, ScanError> {
    let root_identity = identity(workspace_root)?;
    let mut scans = Vec::new();
    let mut members = Vec::new();
    let mut root_is_member = false;
    for path in member_paths {
        if identity(parent(&path)?)? == root_identity {
            root_is_member = true;
            members.push(MemberManifest {
                path: root_scan.relative_path.clone(),
                package: root_scan.package.clone(),
            });
        } else {
            let scan = read_manifest(&path, workspace_root)?;
            members.push(MemberManifest {
                path: scan.relative_path.clone(),
                package: scan.package.clone(),
            });
            scans.push((path, scan));
        }
    }
    members.sort_by(|a, b| a.path.cmp(&b.path));
    scans.sort_by(|a, b| a.1.relative_path.cmp(&b.1.relative_path));
    Ok(MemberScans {
        members,
        scans,
        root_is_member,
    })
}

/// Indexes only actual members for the later classification passes.
fn index_members(
    root: &Path,
    root_scan: &Manifest,
    member_scans: &MemberScans,
) -> Result<MemberIndex, ScanError> {
    let mut index = MemberIndex {
        directories: HashMap::new(),
        packages: BTreeMap::new(),
    };
    let root_member = member_scans.root_is_member.then_some((root, root_scan));
    for (path, scan) in root_member.into_iter().chain(
        member_scans
            .scans
            .iter()
            .map(|(path, scan)| (path.as_path(), scan)),
    ) {
        index
            .directories
            .insert(identity(parent(path)?)?, scan.relative_path.clone());
        if let Some(name) = &scan.package
            && index
                .packages
                .insert(name.clone(), scan.relative_path.clone())
                .is_some()
        {
            return Err(DuplicatePackage::new(name.clone()).into());
        }
    }
    Ok(index)
}

/// Applies the tidy classification policy to the owned parsed declarations.
fn classify_dependencies(
    scans: impl IntoIterator<Item = (PathBuf, Manifest)>,
    index: &MemberIndex,
) -> Result<Vec<Dependency>, ScanError> {
    let mut dependencies = Vec::new();
    // This tidy heuristic gives paths precedence over name matches and then
    // applies workspace inheritance. Name matching is not Cargo resolution.
    // Ref: docs/design.md, same-workspace classification.
    for (path, scan) in scans {
        let directory = parent(&path)?;
        for mut dependency in scan.dependencies {
            if let Some(path) = &dependency.path {
                match Handle::from_path(directory.join(path)) {
                    Ok(resolved) => {
                        dependency.resolved_manifest = index.directories.get(&resolved).cloned();
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => {
                        return Err(DependencyPathFailure::caused_by(
                            path.clone(),
                            scan.relative_path,
                            error,
                        )
                        .into());
                    }
                }
            }
            if dependency.resolved_manifest.is_none() {
                dependency.resolved_manifest = index.packages.get(&dependency.name).cloned();
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

    Ok(dependencies)
}

/// Requires a parent directory for a manifest path.
fn parent(path: &Path) -> Result<&Path, ScanError> {
    path.parent()
        .ok_or_else(|| MissingParent::new(path.to_path_buf()).into())
}

/// Encodes paths without silently replacing invalid characters.
pub(crate) fn path_string(path: &Path) -> Result<String, ScanError> {
    let path = path
        .to_str()
        .ok_or_else(|| NonUnicodePath::new(path.to_path_buf()))?;
    #[cfg(windows)]
    let path = path.replace('\\', "/");
    #[cfg(not(windows))]
    let path = path.to_owned();
    Ok(path)
}

/// Computes a relative path without depending on the process working directory.
pub(crate) fn relative_path(path: &Path, root: &Path) -> Result<String, ScanError> {
    if let Ok(relative) = path.strip_prefix(root) {
        return path_string(relative);
    }
    // Ancestor handles distinguish filesystem identity from case spelling.
    let mut root_ancestors = Vec::new();
    for ancestor in root.ancestors() {
        root_ancestors.push(identity(ancestor)?);
    }
    let parent = parent(path)?;
    for ancestor in parent.ancestors() {
        let candidate = identity(ancestor)?;
        if let Some(steps) = root_ancestors.iter().position(|root| root == &candidate) {
            let mut relative = PathBuf::new();
            for _ in 0..steps {
                relative.push("..");
            }
            let suffix = path
                .strip_prefix(ancestor)
                .map_err(RelativePathFailure::caused_by)?;
            relative.push(suffix);
            return path_string(&relative);
        }
    }
    Err(DifferentFilesystemRoot::new(path.to_path_buf(), root.to_path_buf()).into())
}
