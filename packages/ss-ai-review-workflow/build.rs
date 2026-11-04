//! Binds focus-area reuse to the native generation implementation at build time.

use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// Selected native crates participate in generation or its shared runtime.
const SOURCE_ROOTS: &[&str] = &[
    "packages/ss-ai/src",
    "packages/ss-ai-review-workflow/src",
    "packages/ss-ai-review-context/src",
    "packages/ss-ai-runtime/src",
    "packages/ss-ai-git/src",
];
/// Workspace resolution, the selected crates, and this recipe affect the build.
const FIXED_INPUTS: &[&str] = &[
    "Cargo.toml",
    "Cargo.lock",
    "packages/ss-ai/Cargo.toml",
    "packages/ss-ai-review-workflow/Cargo.toml",
    "packages/ss-ai-review-workflow/build.rs",
    "packages/ss-ai-review-context/Cargo.toml",
    "packages/ss-ai-runtime/Cargo.toml",
    "packages/ss-ai-git/Cargo.toml",
];
/// Schema changes can alter generation even when native Rust sources are unchanged.
const SCHEMA_ROOT: &str = "skills/ss-review/schemas";
/// Separates this fingerprint from other SHA-256 uses.
const DOMAIN: &[u8] = b"ss-ai-focus-generation-build-v1";

/// Records stable paths and exact bytes, independent of the checkout location.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let package =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").ok_or("missing manifest directory")?);
    let root = package
        .parent()
        .and_then(Path::parent)
        .ok_or("missing workspace root")?;
    let mut paths: Vec<String> = FIXED_INPUTS
        .iter()
        .map(|value| (*value).to_owned())
        .collect();
    for source in SOURCE_ROOTS {
        collect(root, Path::new(source), "rs", &mut paths)?;
    }
    collect(root, Path::new(SCHEMA_ROOT), "json", &mut paths)?;
    paths.sort();
    let target = env::var("TARGET")?;
    let profile = env::var("PROFILE")?;
    let mut hash = Sha256::new();
    add_field(&mut hash, DOMAIN);
    add_field(&mut hash, target.as_bytes());
    add_field(&mut hash, profile.as_bytes());
    for path in paths {
        let full = root.join(&path);
        println!("cargo:rerun-if-changed={}", full.display());
        add_field(&mut hash, path.as_bytes());
        let bytes = fs::read(&full).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("Cannot read {}: {error}", full.display()),
            )
        })?;
        add_field(&mut hash, &bytes);
    }
    println!("cargo:rustc-env=SS_AI_FOCUS_BUILD_TARGET={target}");
    println!("cargo:rustc-env=SS_AI_FOCUS_BUILD_PROFILE={profile}");
    // Integration tests exercise this exact executable against isolated inputs.
    println!(
        "cargo:rustc-env=SS_AI_FOCUS_BUILD_SCRIPT={}",
        env::current_exe()?.display()
    );
    println!(
        "cargo:rustc-env=SS_AI_FOCUS_BUILD_FINGERPRINT={:x}",
        hash.finalize()
    );
    Ok(())
}

/// Tracks each directory so additions and removals also trigger a rebuild.
fn collect(
    root: &Path,
    relative: &Path,
    extension: &str,
    paths: &mut Vec<String>,
) -> io::Result<()> {
    let directory = root.join(relative);
    println!("cargo:rerun-if-changed={}", directory.display());
    let entries = fs::read_dir(&directory).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("Cannot read {}: {error}", directory.display()),
        )
    })?;
    for entry in entries {
        let entry = entry?;
        let kind = entry.file_type()?;
        let path = relative.join(entry.file_name());
        if kind.is_symlink() {
            return Err(io::Error::other(format!(
                "Build input is a link: {}",
                path.display()
            )));
        }
        if kind.is_dir() {
            collect(root, &path, extension, paths)?;
        } else if kind.is_file() && path.extension().is_some_and(|value| value == extension) {
            // A literal backslash cannot be distinguished from a normalized
            // separator in the fingerprint, so fail rather than hash an alias.
            let components = path
                .components()
                .map(|component| {
                    let name = component
                        .as_os_str()
                        .to_str()
                        .ok_or_else(|| io::Error::other("Build input path is not Unicode"))?;
                    if name.contains('\\') {
                        return Err(io::Error::other(format!(
                            "Build input path {path:?} contains a literal backslash; rename it to avoid an ambiguous fingerprint"
                        )));
                    }
                    Ok(name)
                })
                .collect::<io::Result<Vec<_>>>()?;
            paths.push(components.join("/"));
        }
    }
    Ok(())
}

/// Length-prefixing makes path and byte boundaries unambiguous.
fn add_field(hash: &mut Sha256, value: &[u8]) {
    hash.update((value.len() as u64).to_le_bytes());
    hash.update(value);
}
