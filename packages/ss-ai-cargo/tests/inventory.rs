//! Exercises inventory discovery against real files and existing fixtures.
#![expect(
    clippy::indexing_slicing,
    reason = "The tests use fixed, asserted record layouts."
)]

use std::fs;
#[cfg(unix)]
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use jiff::Timestamp;
use ohno::ErrorExt;
use ss_ai_cargo::{
    Dependency, Inventory, InventoryError, MemberManifest, WorkspaceManifest, generate_inventory,
};
use static_assertions::{assert_impl_all, assert_not_impl_any};
use std::panic::{RefUnwindSafe, UnwindSafe};

assert_impl_all!(Inventory: UnwindSafe, RefUnwindSafe);
assert_impl_all!(Dependency: UnwindSafe, RefUnwindSafe);
// The error can retain a source error with interior mutable state.
assert_not_impl_any!(InventoryError: UnwindSafe, RefUnwindSafe);
assert_impl_all!(MemberManifest: UnwindSafe, RefUnwindSafe);
assert_impl_all!(WorkspaceManifest: UnwindSafe, RefUnwindSafe);

/// Keeps each integration test's writable files in the package directory.
struct Workspace(PathBuf);

impl Workspace {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        // The counter only supplies unique names; no synchronization of data
        // depends on it, so relaxed ordering is sufficient.
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(format!(".inventory-test-{}-{id}", std::process::id()));
        // An existing directory indicates a collision, not a reusable fixture.
        #[expect(clippy::create_dir, reason = "A collision must fail, not reuse files.")]
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn write(&self, path: &str, text: &str) {
        let path = self.0.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn scan(&self) -> Result<Inventory, InventoryError> {
        generate_inventory(&self.0, timestamp())
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

/// Provides a fixed instant to every test.
fn timestamp() -> Timestamp {
    "2020-01-02T03:04:05Z".parse().unwrap()
}

/// Reads the repository's existing immutable fixture manifests.
fn fixture(name: &str) -> Inventory {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/ss-cargo-deps-tidy/fixtures")
        .join(name);
    generate_inventory(&root.join("Cargo.toml"), timestamp()).unwrap()
}

#[test]
fn existing_workspace_shapes_are_discovered() {
    let cases = [
        ("virtual-workspace", vec!["alpha", "beta"], true),
        (
            "non-virtual-workspace",
            vec!["gamma", "non-virtual-root"],
            false,
        ),
        ("single-crate", vec!["lonely"], false),
        ("with-exclude", vec!["included"], true),
        ("glob-recursive", vec!["deep", "shallow"], true),
    ];
    for (name, expected, virtual_workspace) in cases {
        let inventory = fixture(name);
        let mut packages: Vec<_> = inventory
            .member_manifests
            .iter()
            .map(|member| member.package.as_deref().unwrap())
            .collect();
        packages.sort();
        assert_eq!(packages, expected, "{name}");
        assert_eq!(
            inventory.workspace_manifest.is_virtual_workspace,
            virtual_workspace
        );
    }
}

#[test]
fn existing_styles_and_inheritance_are_preserved() {
    let inventory = fixture("target-and-edge-cases");
    let serde = inventory
        .dependencies
        .iter()
        .find(|item| item.name == "serde" && item.style == "sub-table")
        .unwrap();
    assert!(serde.raw.starts_with("[dependencies.serde]"));
    assert!(serde.raw.contains("default-features = false"));
    assert_eq!(serde.features.as_deref(), Some(&["derive".into()][..]));
    let windows = inventory
        .dependencies
        .iter()
        .find(|item| item.name == "windows" && item.style == "dotted-key")
        .unwrap();
    assert_eq!(windows.workspace, Some(true));
    assert!(windows.raw.contains("windows.features"));
    assert!(
        inventory
            .dependencies
            .iter()
            .any(|item| item.target.as_deref() == Some("cfg(unix)") && item.name == "nix")
    );

    let inventory = fixture("path-deps-mixed");
    let inside = inventory
        .dependencies
        .iter()
        .find(|item| item.name == "inside" && item.workspace == Some(true))
        .unwrap();
    assert_eq!(
        inside.resolved_manifest.as_deref(),
        Some("packages/host/Cargo.toml")
    );
    let outside = inventory
        .dependencies
        .iter()
        .find(|item| item.name == "outside")
        .unwrap();
    assert!(!outside.same_workspace);
    let inventory = fixture("name-only-deps");
    assert!(
        inventory
            .dependencies
            .iter()
            .any(|item| item.name == "gamma" && item.same_workspace)
    );
}

#[test]
fn supports_build_dependencies_and_original_spans() {
    let workspace = Workspace::new();
    workspace.write("Cargo.toml", "[package]\nname = 'root'\n[build-dependencies]\nfoo = {\n version = '1', # comment\n features = [],\n}\n[target.'cfg(unix)'.build-dependencies]\nbar = '2'\n");
    let inventory = workspace.scan().unwrap();
    let foo = &inventory.dependencies[0];
    assert_eq!(foo.section, "build-dependencies");
    assert_eq!(foo.line.get(), 4);
    assert!(foo.raw.starts_with("foo = {"));
    assert!(foo.raw.contains("# comment"));
    assert_eq!(foo.features, Some(vec![]));
    assert_eq!(
        inventory.dependencies[1].target.as_deref(),
        Some("cfg(unix)")
    );
    let json = serde_json::to_value(&inventory).unwrap();
    assert_eq!(json["schemaVersion"], 1);
    assert_eq!(json["scannedAt"], "2020-01-02T03:04:05Z");
    assert!(json["dependencies"][0]["defaultFeatures"].is_null());
    assert_eq!(json["dependencies"][0]["line"], 4);
}

#[test]
fn path_precedes_name_and_workspace_inheritance_preserves_resolution() {
    let workspace = Workspace::new();
    workspace.write(
        "Cargo.toml",
        "[package]\nname = 'root'\n[workspace]\nmembers = ['a', 'b', 'consumer']\n[workspace.dependencies]\nalpha = { package = 'beta', path = 'b' }\nalias = { path = 'a' }\nunresolved = '1'\n[dependencies]\nexternal = '1'\n",
    );
    workspace.write(
        "a/Cargo.toml",
        "[package]\nname = 'alpha'\n[dependencies]\nalias.workspace = true\n",
    );
    workspace.write("b/Cargo.toml", "[package]\nname = 'beta'\n");
    // The parser reports declarations without requiring Cargo-valid combinations.
    // This path checks the fallback when the workspace entry has no resolution.
    workspace.write(
        "consumer/Cargo.toml",
        "[package]\nname = 'consumer'\n[dependencies]\nalpha = { path = '../b' }\nbeta = '1'\nunresolved = { workspace = true, path = '../a' }\nmissing.workspace = true\n[dev-dependencies]\nalpha.workspace = true\n[build-dependencies]\nbeta.workspace = true\nunresolved.workspace = true\n",
    );
    let inventory = workspace.scan().unwrap();
    let alpha = inventory
        .dependencies
        .iter()
        .find(|item| item.name == "alpha")
        .unwrap();
    assert_eq!(alpha.resolved_manifest.as_deref(), Some("b/Cargo.toml"));
    let alias = inventory
        .dependencies
        .iter()
        .find(|item| item.workspace == Some(true))
        .unwrap();
    assert_eq!(alias.resolved_manifest.as_deref(), Some("a/Cargo.toml"));
    let consumer = |name: &str, section: &str| {
        inventory
            .dependencies
            .iter()
            .find(|item| {
                item.manifest == "consumer/Cargo.toml"
                    && item.name == name
                    && item.section == section
            })
            .unwrap()
    };
    let inherited_alpha = consumer("alpha", "dev-dependencies");
    assert!(inherited_alpha.same_workspace);
    assert_eq!(
        inherited_alpha.resolved_manifest.as_deref(),
        Some("b/Cargo.toml")
    );
    for (name, expected) in [
        ("alpha", Some("b/Cargo.toml")),
        ("beta", Some("b/Cargo.toml")),
        ("unresolved", Some("a/Cargo.toml")),
        ("missing", None),
    ] {
        let dependency = consumer(name, "dependencies");
        assert_eq!(dependency.resolved_manifest.as_deref(), expected, "{name}");
        assert_eq!(dependency.same_workspace, expected.is_some(), "{name}");
    }
    let inherited_fallback = consumer("beta", "build-dependencies");
    assert!(inherited_fallback.same_workspace);
    assert_eq!(
        inherited_fallback.resolved_manifest.as_deref(),
        Some("b/Cargo.toml")
    );
    let unresolved = consumer("unresolved", "build-dependencies");
    assert!(!unresolved.same_workspace);
    assert_eq!(unresolved.resolved_manifest, None);
    assert_eq!(
        inventory
            .dependencies
            .iter()
            .filter(|item| item.name == "external")
            .count(),
        1
    );
}

#[test]
fn aliases_inherit_path_resolution_and_exclusions_are_patterns() {
    let workspace = Workspace::new();
    workspace.write("Cargo.toml", "[workspace]\nmembers = ['crates/*']\nexclude = ['crates/no*']\n[workspace.dependencies]\nalias = { path = 'crates/yes', package = 'actual' }\n");
    workspace.write(
        "crates/yes/Cargo.toml",
        "[package]\nname = 'actual'\n[dependencies]\nalias.workspace = true\n",
    );
    workspace.write("crates/nope/Cargo.toml", "[package]\nname = 'excluded'\n");
    let inventory = workspace.scan().unwrap();
    assert_eq!(inventory.member_manifests.len(), 1);
    let alias = inventory
        .dependencies
        .iter()
        .find(|item| item.workspace == Some(true))
        .unwrap();
    assert_eq!(
        alias.resolved_manifest.as_deref(),
        Some("crates/yes/Cargo.toml")
    );
}

#[test]
fn invalid_input_and_missing_members_fail_instead_of_skipping() {
    let workspace = Workspace::new();
    workspace.write("Cargo.toml", "[workspace]\nmembers = ['missing']\n");
    let error = workspace.scan().unwrap_err();
    assert!(error.to_string().contains("missing"));
    workspace.write("Cargo.toml", "[workspace]\nmembers = ['bad']\n");
    workspace.write("bad/Cargo.toml", "[package\n");
    let error = workspace.scan().unwrap_err();
    assert!(error.find_source::<toml_edit::TomlError>().is_some());
    assert!(error.to_string().contains("Cargo.toml"));
    assert!(error.to_string().contains("bad"));
    workspace.write(
        "bad/Cargo.toml",
        "[dependencies]\na = { workspace = 'true' }\n",
    );
    let error = workspace.scan().unwrap_err();
    let diagnostic = error.to_string();
    assert!(diagnostic.contains("Cargo.toml"));
    assert!(diagnostic.contains("bad"));
    assert!(diagnostic.contains("workspace must be a boolean"));
}

#[test]
fn ordinary_member_globs_reject_unusable_manifests() {
    let workspace = Workspace::new();
    workspace.write("Cargo.toml", "[workspace]\nmembers = ['crates/*']\n");
    workspace.write("crates/valid/Cargo.toml", "[package]\nname = 'valid'\n");
    let missing = workspace.0.join("crates/missing");
    fs::create_dir_all(&missing).unwrap();
    let error = workspace.scan().unwrap_err();
    assert!(error.to_string().contains("missing"));

    fs::create_dir_all(missing.join("Cargo.toml")).unwrap();
    let error = workspace.scan().unwrap_err();
    assert!(error.to_string().contains("Not a manifest file"));
}

#[test]
fn case_identity_follows_the_actual_filesystem() {
    let workspace = Workspace::new();
    workspace.write("Upper/Cargo.toml", "[package]\nname = 'actual'\n");
    let insensitive = same_file::is_same_file(workspace.0.join("upper"), workspace.0.join("Upper"))
        .unwrap_or(false);
    if !insensitive {
        workspace.write("upper/Cargo.toml", "[package]\nname = 'other'\n");
    }
    workspace.write(
        "Cargo.toml",
        "[workspace]\nmembers = ['Upper', 'upper']\n[workspace.dependencies]\nalias = { path = 'upper' }\nother = { path = 'Upper' }\n",
    );
    let inventory = workspace.scan().unwrap();
    assert_eq!(
        inventory.member_manifests.len(),
        if insensitive { 1 } else { 2 }
    );
    let alias = &inventory.dependencies[0];
    let other = &inventory.dependencies[1];
    assert!(alias.same_workspace);
    assert!(other.same_workspace);
    assert_eq!(
        alias.resolved_manifest == other.resolved_manifest,
        insensitive
    );

    workspace.write(
        "Cargo.toml",
        "[workspace]\nmembers = ['Upper', 'upper']\nexclude = ['upper']\n[workspace.dependencies]\nalias = { path = 'upper' }\n",
    );
    let inventory = workspace.scan().unwrap();
    assert_eq!(
        inventory.member_manifests.len(),
        if insensitive { 0 } else { 1 }
    );
    assert!(!inventory.dependencies[0].same_workspace);
}

#[cfg(unix)]
#[test]
fn unix_backslashes_remain_literal_in_paths_and_patterns() {
    let workspace = Workspace::new();
    workspace.write(
        "root\\directory/Cargo.toml",
        "[workspace]\nmembers = ['member\\name']\n[workspace.dependencies]\nalias = { path = 'member\\name' }\n",
    );
    workspace.write(
        "root\\directory/member\\name/Cargo.toml",
        "[package]\nname = 'example'\n",
    );
    let root = workspace.0.join("root\\directory");
    let inventory = generate_inventory(&root, timestamp()).unwrap();
    assert!(inventory.workspace_root.ends_with("root\\directory"));
    assert_eq!(
        inventory.member_manifests[0].path,
        "member\\name/Cargo.toml"
    );
    assert_eq!(
        inventory.dependencies[0].resolved_manifest.as_deref(),
        Some("member\\name/Cargo.toml")
    );
}

#[cfg(unix)]
#[test]
fn symbolic_aliases_share_membership_and_exclusion_identity() {
    let workspace = Workspace::new();
    workspace.write("real/Cargo.toml", "[package]\nname = 'actual'\n");
    symlink("real", workspace.0.join("alias")).unwrap();
    workspace.write(
        "Cargo.toml",
        "[workspace]\nmembers = ['real', 'alias']\n[workspace.dependencies]\ndep = { path = 'alias' }\n",
    );
    let inventory = workspace.scan().unwrap();
    assert_eq!(inventory.member_manifests.len(), 1);
    assert_eq!(
        inventory.dependencies[0].resolved_manifest.as_deref(),
        Some("real/Cargo.toml")
    );
    workspace.write(
        "Cargo.toml",
        "[workspace]\nmembers = ['real', 'alias']\nexclude = ['alias']\n",
    );
    assert!(workspace.scan().unwrap().member_manifests.is_empty());
}

#[cfg(unix)]
#[test]
fn recursive_globs_terminate_on_symlink_cycles() {
    let workspace = Workspace::new();
    workspace.write("Cargo.toml", "[workspace]\nmembers = ['crates/**']\n");
    workspace.write("crates/one/Cargo.toml", "[package]\nname = 'one'\n");
    symlink("..", workspace.0.join("crates/one/cycle")).unwrap();
    let inventory = workspace.scan().unwrap();
    assert_eq!(inventory.member_manifests.len(), 1);
}

#[cfg(unix)]
#[test]
fn enumeration_errors_do_not_return_partial_results() {
    let workspace = Workspace::new();
    workspace.write("Cargo.toml", "[workspace]\nmembers = ['crates/*']\n");
    workspace.write("crates/one/Cargo.toml", "[package]\nname = 'one'\n");
    symlink("missing", workspace.0.join("crates/broken")).unwrap();
    let error = workspace.scan().unwrap_err();
    assert_eq!(
        error.find_source::<std::io::Error>().unwrap().kind(),
        std::io::ErrorKind::NotFound
    );
    assert!(error.to_string().contains("broken"));
}
