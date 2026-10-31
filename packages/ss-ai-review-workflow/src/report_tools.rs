//! Discovers and installs the tools used to render review reports.

use std::any::type_name;
use std::env;
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use ohno::AppError;
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use ss_ai_runtime::{Clock, LockMode, Process, ResourceLock, caused_by, fail};
use tempfile::Builder;

/// Matches the portable release selected by the report skill.
const PANDOC_VERSION: &str = "3.11";

/// Paths and diagnostics from discovery, without claiming rendering works.
#[derive(Serialize)]
#[non_exhaustive]
pub struct ReportTools {
    /// Pandoc on PATH or the pinned user-local executable.
    pub pandoc: PathBuf,
    /// Selected Node.js runtime, if available.
    pub node: Option<PathBuf>,
    /// Entry point in the lockfile-addressed Mermaid installation.
    pub mermaid: PathBuf,
    /// Puppeteer's configured browser executable.
    pub browser: Option<PathBuf>,
    /// Tools which require installation or repair.
    pub missing: Vec<String>,
    /// Browser discovery details retained for diagnosis.
    pub diagnostics: Vec<String>,
}

impl fmt::Debug for ReportTools {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct(type_name::<Self>())
            .field("pandoc", &self.pandoc)
            .field("node", &self.node)
            .field("mermaid", &self.mermaid)
            .field("browser", &self.browser)
            .field("missing_count", &self.missing.len())
            .field("diagnostics_count", &self.diagnostics.len())
            .finish()
    }
}

impl ReportTools {
    /// Supplies discovered paths for a rendering boundary.
    #[must_use]
    #[inline]
    pub fn new(
        pandoc: PathBuf,
        node: Option<PathBuf>,
        mermaid: PathBuf,
        browser: Option<PathBuf>,
        missing: Vec<String>,
        diagnostics: Vec<String>,
    ) -> Self {
        Self {
            pandoc,
            node,
            mermaid,
            browser,
            missing,
            diagnostics,
        }
    }
}

/// Resolves the user-local storage shared by skill checkouts.
pub fn tools_directory() -> Result<PathBuf, AppError> {
    let root = env::var_os("LOCALAPPDATA")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| nonempty_env("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .ok_or_else(|| {
            fail(
                "invalidInput",
                "Cannot determine user-local report tool storage",
            )
        })?;
    Ok(root.join("ss-review").join("tools"))
}

/// Holds the shared installation resource throughout discovery and rendering.
pub fn tools_lock(
    tools_dir: &Path,
    mode: LockMode,
    clock: &dyn Clock,
) -> Result<ResourceLock, AppError> {
    ResourceLock::open(&tools_dir.join("installation"), mode, clock)
}

/// Discovers tools while the caller retains the installation resource lock.
pub fn discover(
    skill_root: &Path,
    tools_dir: &Path,
    process: &dyn Process,
    _guard: &ResourceLock,
) -> Result<ReportTools, AppError> {
    let directory = mermaid_directory(skill_root, tools_dir)?;
    let pandoc = find_on_path("pandoc").unwrap_or_else(|| {
        tools_dir
            .join(format!("pandoc-{PANDOC_VERSION}"))
            .join(executable_name("pandoc"))
    });
    let node = node_path(process)?;
    let mermaid = directory.join("node_modules/@mermaid-js/mermaid-cli/src/cli.js");
    let packages_ready = packages_ready(&directory)?;
    let mut missing = Vec::new();
    let mut diagnostics = Vec::new();
    if !pandoc.is_file() {
        missing.push("pandoc".to_owned());
    }
    if node.is_none() {
        missing.push("node".to_owned());
    }
    if !packages_ready {
        missing.push("mermaid-cli".to_owned());
    }
    let mut browser = None;
    if let Some(node) = node.as_ref().filter(|_| packages_ready) {
        let arguments = vec![
            "-e".to_owned(),
            "require(process.argv[1]).executablePath().then(p => process.stdout.write(p))"
                .to_owned(),
            path_text(&directory.join("node_modules/puppeteer"))?,
        ];
        let result = process
            .execute(&path_text(node)?, &arguments, &[])
            .map_err(|error| caused_by("io", "Cannot discover Puppeteer browser", error))?;
        if result.success {
            let output = String::from_utf8(result.stdout)
                .map_err(|error| caused_by("invalidInput", "Browser path is not UTF-8", error))?;
            let path = PathBuf::from(output.trim_end_matches(['\r', '\n']));
            if !path.as_os_str().is_empty() {
                browser = Some(path);
            }
        } else {
            diagnostics.push(format!(
                "Browser discovery failed: {}",
                String::from_utf8_lossy(&result.stderr)
            ));
        }
        if !browser.as_ref().is_some_and(|path| path.is_file()) {
            missing.push("mermaid-browser".to_owned());
            if result.success {
                diagnostics.push(format!(
                    "Puppeteer's browser executable is absent: '{}'.",
                    browser
                        .as_ref()
                        .map_or_else(String::new, |path| path.display().to_string())
                ));
            }
        }
    }
    Ok(ReportTools {
        pandoc,
        node,
        mermaid,
        browser,
        missing,
        diagnostics,
    })
}

/// Checks discovery before a render, distinguishing consent from setup failures.
pub fn assert_ready(tools: &ReportTools, installation_attempted: bool) -> Result<(), AppError> {
    if tools.missing.is_empty() {
        return Ok(());
    }
    let action = if installation_attempted {
        "The approved installation did not provide all required tools; diagnose using report-tool-setup.md. Browser cache deletion requires separate approval."
    } else {
        "Ask the user for installation permission before retrying HTML rendering."
    };
    Err(fail(
        "invalidInput",
        format!(
            "Missing report tools: {}. {action} {}",
            tools.missing.join(", "),
            tools.diagnostics.join("\n")
        ),
    ))
}

/// Discovers under a shared lock and probes the production renderer when complete.
pub fn readiness(
    skill_root: &Path,
    tools_dir: &Path,
    process: &dyn Process,
    clock: &dyn Clock,
    probe: impl FnOnce(&ReportTools, &ResourceLock) -> Result<(), AppError>,
) -> Result<ReportTools, AppError> {
    let guard = tools_lock(tools_dir, LockMode::Shared, clock)?;
    let tools = discover(skill_root, tools_dir, process, &guard)?;
    if tools.missing.is_empty() {
        probe(&tools, &guard)?;
    }
    Ok(tools)
}

/// Installs approved dependencies and probes rendering while retaining exclusion.
pub fn install(
    skill_root: &Path,
    tools_dir: &Path,
    process: &dyn Process,
    clock: &dyn Clock,
    install_browser_dependencies: bool,
    probe: impl FnOnce(&ReportTools, &ResourceLock) -> Result<(), AppError>,
) -> Result<ReportTools, AppError> {
    if install_browser_dependencies && !cfg!(target_os = "linux") {
        return Err(fail(
            "invalidInput",
            "Browser system dependency installation requires Ubuntu or Debian Linux",
        ));
    }
    let guard = tools_lock(tools_dir, LockMode::Exclusive, clock)?;
    let tools = discover(skill_root, tools_dir, process, &guard)?;
    let node = tools.node.as_ref().ok_or_else(|| {
        fail(
            "invalidInput",
            "Install a supported Node.js LTS release (at least 22.12.0) with npm, then rerun setup",
        )
    })?;
    if tools.missing.iter().any(|name| name == "pandoc") {
        install_pandoc(tools_dir, process)?;
    }
    install_mermaid(skill_root, tools_dir, node, process)?;
    let installed = discover(skill_root, tools_dir, process, &guard)?;
    assert_ready(&installed, true)?;
    if install_browser_dependencies {
        install_system_dependencies(
            installed
                .browser
                .as_deref()
                .ok_or_else(|| fail("invalidInput", "Browser path is missing"))?,
            process,
        )?;
    }
    probe(&installed, &guard)?;
    Ok(installed)
}

/// Selects a cache by the exact dependency lock used by the active skill.
fn mermaid_directory(skill_root: &Path, tools_dir: &Path) -> Result<PathBuf, AppError> {
    let bytes = fs::read(skill_root.join("tools/package-lock.json"))
        .map_err(|error| caused_by("io", "Cannot read report dependency lockfile", error))?;
    Ok(tools_dir.join(format!("mermaid-{:x}", Sha256::digest(bytes))))
}

/// Checks package entry points and distinguishes missing files from read failures.
fn packages_ready(directory: &Path) -> Result<bool, AppError> {
    let puppet = directory.join("node_modules/puppeteer");
    if !package_file(&directory.join("node_modules/@mermaid-js/mermaid-cli/src/cli.js"))?
        || !package_file(&puppet.join("install.mjs"))?
        || !package_file(&puppet.join("package.json"))?
    {
        return Ok(false);
    }
    let manifest = fs::read(puppet.join("package.json"))
        .map_err(|error| caused_by("io", "Cannot read Puppeteer manifest", error))?;
    let Ok(manifest) = serde_json::from_slice::<Value>(&manifest) else {
        return Ok(false);
    };
    let Some(main) = manifest
        .get("main")
        .and_then(Value::as_str)
        .filter(|main| !main.trim().is_empty())
    else {
        return Ok(false);
    };
    package_file(&puppet.join(main))
}

/// Treats absent package files as repairable without hiding other I/O failures.
fn package_file(path: &Path) -> Result<bool, AppError> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(metadata.is_file()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => {
            Err(caused_by("io", format!("Cannot inspect {}", path.display()), error).into())
        }
    }
}

/// Looks up a runtime without evaluating shell profiles or repository configuration.
fn node_path(process: &dyn Process) -> Result<Option<PathBuf>, AppError> {
    if let Some(path) = find_on_path("node") {
        return Ok(Some(path));
    }
    let variable = if cfg!(windows) {
        "NVM_SYMLINK"
    } else {
        "NVM_BIN"
    };
    if let Some(path) = nonempty_env(variable)
        .map(|path| PathBuf::from(path).join(executable_name("node")))
        .filter(|path| path.is_file())
    {
        return Ok(Some(path));
    }
    if cfg!(windows) {
        return Ok(None);
    }
    let nvm = nonempty_env("NVM_DIR")
        .map(PathBuf::from)
        .or_else(|| nonempty_env("XDG_CONFIG_HOME").map(|path| PathBuf::from(path).join("nvm")))
        .or_else(|| nonempty_env("HOME").map(|path| PathBuf::from(path).join(".nvm")));
    let Some(nvm) = nvm.filter(|path| path.join("nvm.sh").is_file()) else {
        return Ok(None);
    };
    let shell = find_on_path("sh").ok_or_else(|| {
        fail(
            "invalidInput",
            "A POSIX shell is required to locate nvm's default Node.js",
        )
    })?;
    let arguments = vec![
        "-c".to_owned(),
        "export NVM_DIR=\"$1\"; set -- --no-use; . \"$NVM_DIR/nvm.sh\" >&2 && nvm which default"
            .to_owned(),
        "ss-review-node".to_owned(),
        path_text(&nvm)?,
    ];
    let output = run(process, &shell, &arguments)?;
    let output = String::from_utf8(output)
        .map_err(|error| caused_by("invalidInput", "nvm returned a non-UTF-8 path", error))?;
    let path = PathBuf::from(output.trim_end_matches(['\r', '\n']));
    if !path.is_absolute() || !path.is_file() {
        return Err(fail(
            "invalidInput",
            format!(
                "nvm did not return an installed absolute Node.js executable: '{}'",
                path.display()
            ),
        ));
    }
    Ok(Some(path))
}

/// Matches the shell's treatment of empty configuration variables as absent.
fn nonempty_env(name: &str) -> Option<OsString> {
    env::var_os(name).filter(|value| !value.is_empty())
}

/// Searches native executable locations in caller order.
fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    for directory in env::split_paths(&path) {
        let candidate = directory.join(executable_name(name));
        if executable(&candidate) {
            return Some(candidate);
        }
    }
    None
}

/// Excludes non-executable Unix files from PATH selection.
fn executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        fs::metadata(path)
            .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

/// Uses the platform's native binary suffix.
fn executable_name(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_owned()
    }
}

/// Rejects paths that cannot be passed through the process boundary faithfully.
fn path_text(path: &Path) -> Result<String, AppError> {
    path.to_str().map(str::to_owned).ok_or_else(|| {
        fail(
            "invalidInput",
            format!("Path is not UTF-8: {}", path.display()),
        )
    })
}

/// Retains the child's diagnostic when an invocation fails.
fn run(process: &dyn Process, program: &Path, arguments: &[String]) -> Result<Vec<u8>, AppError> {
    let output = process
        .execute(&path_text(program)?, arguments, &[])
        .map_err(|error| caused_by("io", format!("Cannot run {}", program.display()), error))?;
    if !output.success {
        return Err(fail(
            "io",
            format!(
                "Report tool '{}' failed: {}",
                program.display(),
                String::from_utf8_lossy(&output.stderr)
            ),
        ));
    }
    Ok(output.stdout)
}

/// Installs a complete package tree before replacing any existing tree.
fn install_mermaid(
    skill_root: &Path,
    tools_dir: &Path,
    node: &Path,
    process: &dyn Process,
) -> Result<(), AppError> {
    let destination = mermaid_directory(skill_root, tools_dir)?;
    let staging = if packages_ready(&destination)? {
        None
    } else {
        Some(
            Builder::new()
                .prefix(".mermaid-")
                .tempdir_in(tools_dir)
                .map_err(|error| {
                    caused_by("io", "Cannot create Mermaid staging directory", error)
                })?,
        )
    };
    let directory = staging
        .as_ref()
        .map_or(destination.as_path(), |staging| staging.path());
    let parent = node
        .parent()
        .ok_or_else(|| fail("invalidInput", "Node path has no parent"))?;
    let path = env::var_os("PATH").unwrap_or_default();
    let path =
        env::join_paths(std::iter::once(parent.to_path_buf()).chain(env::split_paths(&path)))
            .map_err(|error| caused_by("invalidInput", "Cannot construct npm PATH", error))?;
    if staging.is_some() {
        for name in ["package.json", "package-lock.json"] {
            fs::copy(skill_root.join("tools").join(name), directory.join(name)).map_err(
                |error| caused_by("io", "Cannot copy report dependency manifest", error),
            )?;
        }
        let copied = fs::read(directory.join("package-lock.json"))
            .map_err(|error| caused_by("io", "Cannot read staged dependency lockfile", error))?;
        if destination.file_name()
            != Some(OsString::from(format!("mermaid-{:x}", Sha256::digest(copied))).as_os_str())
        {
            return Err(fail(
                "invalidInput",
                "The skill dependency lockfile changed during setup",
            ));
        }
        // npm is paired with the selected runtime when available; the child gets
        // an isolated PATH and working directory rather than changing the process.
        let npm_name = if cfg!(windows) { "npm.cmd" } else { "npm" };
        let npm = parent.join(npm_name);
        let program = if npm.is_file() {
            path_text(&npm)?
        } else {
            "npm".to_owned()
        };
        let arguments = [
            "ci",
            "--ignore-scripts",
            "--engine-strict",
            "--no-audit",
            "--no-fund",
        ]
        .map(str::to_owned);
        let output = process
            .execute_in(&program, &arguments, &[], directory, Some(&path))
            .map_err(|error| caused_by("io", "Cannot launch npm", error))?;
        if !output.success {
            return Err(fail(
                "io",
                format!(
                    "Installing Mermaid CLI failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                ),
            ));
        }
        if !packages_ready(directory)? {
            return Err(fail(
                "invalidInput",
                "The npm installation did not provide the required Mermaid and Puppeteer package files",
            ));
        }
    }
    let arguments = vec![path_text(
        &directory.join("node_modules/puppeteer/install.mjs"),
    )?];
    // Keep Puppeteer's configuration relative to the invocation directory.
    let output = process
        .execute_with_path(&path_text(node)?, &arguments, &[], &path)
        .map_err(|error| caused_by("io", "Cannot launch browser installer", error))?;
    if !output.success {
        return Err(fail(
            "io",
            format!(
                "Installing Mermaid's browser failed: {}. Follow report-tool-setup.md; browser cache deletion requires separate approval.",
                String::from_utf8_lossy(&output.stderr)
            ),
        ));
    }
    if let Some(staging) = staging {
        publish_directory(staging.path(), &destination, tools_dir)?;
    }
    Ok(())
}

/// Preserves the previous installation if replacement fails.
fn publish_directory(staging: &Path, destination: &Path, tools_dir: &Path) -> Result<(), AppError> {
    let backup = Builder::new()
        .prefix(".mermaid-previous-")
        .tempdir_in(tools_dir)
        .map_err(|error| caused_by("io", "Cannot prepare installation backup", error))?;
    let previous = backup.path().join("previous");
    let had_previous = destination
        .try_exists()
        .map_err(|error| caused_by("io", "Cannot inspect existing installation", error))?;
    if had_previous {
        fs::rename(destination, &previous)
            .map_err(|error| caused_by("io", "Cannot retain existing installation", error))?;
    }
    if let Err(error) = fs::rename(staging, destination) {
        if had_previous && let Err(restore) = fs::rename(&previous, destination) {
            // Keep the old tree available for manual recovery if restoration fails.
            let retained = backup.keep();
            return Err(caused_by(
                    "io",
                    format!(
                        "Cannot restore installation after publication failed: {error}; backup retained at {}",
                        retained.join("previous").display()
                    ),
                    restore,
                ).into());
        }
        return Err(caused_by("io", "Cannot publish installation", error).into());
    }
    Ok(())
}

/// Installs browser system relations only after explicit opt-in.
fn install_system_dependencies(browser: &Path, process: &dyn Process) -> Result<(), AppError> {
    let metadata = browser
        .parent()
        .ok_or_else(|| fail("invalidInput", "Browser path has no parent"))?
        .join("deb.deps");
    let text = fs::read_to_string(&metadata).map_err(|error| {
        caused_by(
            "io",
            format!(
                "Cannot read browser dependency metadata '{}'",
                metadata.display()
            ),
            error,
        )
    })?;
    let relations = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(", ");
    if relations.is_empty() {
        return Err(fail("invalidInput", "Browser dependency metadata is empty"));
    }
    let sudo = find_on_path("sudo").ok_or_else(|| {
        fail(
            "invalidInput",
            "Browser system dependencies require noninteractive sudo",
        )
    })?;
    let apt = find_on_path("apt-get").ok_or_else(|| {
        fail(
            "invalidInput",
            "Browser system dependencies require apt-get",
        )
    })?;
    let apt = path_text(&apt)?;
    for operation in [
        vec!["-n".to_owned(), apt.clone(), "update".to_owned()],
        vec![
            "-n".to_owned(),
            apt.clone(),
            "satisfy".to_owned(),
            "--yes".to_owned(),
            "--no-install-recommends".to_owned(),
            relations,
        ],
        vec![
            "-n".to_owned(),
            apt,
            "install".to_owned(),
            "--yes".to_owned(),
            "--no-install-recommends".to_owned(),
            "fonts-liberation".to_owned(),
        ],
    ] {
        run(process, &sudo, &operation)?;
    }
    Ok(())
}

/// Installs the pinned official portable archive on Windows x64 only.
fn install_pandoc(tools_dir: &Path, process: &dyn Process) -> Result<(), AppError> {
    if !cfg!(all(windows, target_arch = "x86_64")) {
        return Err(fail(
            "invalidInput",
            format!(
                "Install Pandoc {PANDOC_VERSION} or a compatible release supporting --syntax-highlighting, then rerun setup"
            ),
        ));
    }
    let curl = find_on_path("curl")
        .ok_or_else(|| fail("invalidInput", "curl is required to download Pandoc"))?;
    let tar = find_on_path("tar")
        .ok_or_else(|| fail("invalidInput", "tar is required to extract Pandoc"))?;
    let release = run(
        process,
        &curl,
        &[
            "--fail".into(),
            "--silent".into(),
            "--show-error".into(),
            "--location".into(),
            format!("https://api.github.com/repos/jgm/pandoc/releases/tags/{PANDOC_VERSION}"),
        ],
    )?;
    let release: Value = serde_json::from_slice(&release)
        .map_err(|error| caused_by("invalidInput", "Invalid Pandoc release metadata", error))?;
    let name = format!("pandoc-{PANDOC_VERSION}-windows-x86_64.zip");
    let mut matches = release
        .get("assets")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|asset| asset.get("name").and_then(Value::as_str) == Some(name.as_str()));
    let asset = matches.next();
    if asset.is_none() || matches.next().is_some() {
        return Err(fail(
            "invalidInput",
            "Pandoc release metadata did not identify a unique archive",
        ));
    }
    let asset = asset.ok_or_else(|| fail("invalidInput", "Missing Pandoc archive"))?;
    let digest = asset
        .get("digest")
        .and_then(Value::as_str)
        .and_then(|value| value.strip_prefix("sha256:"))
        .filter(|value| value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .ok_or_else(|| fail("invalidInput", "Pandoc archive SHA-256 digest is missing"))?;
    let url = asset
        .get("browser_download_url")
        .and_then(Value::as_str)
        .ok_or_else(|| fail("invalidInput", "Pandoc archive URL is missing"))?;
    let archive = run(
        process,
        &curl,
        &[
            "--fail".into(),
            "--silent".into(),
            "--show-error".into(),
            "--location".into(),
            url.to_owned(),
        ],
    )?;
    if !format!("{:x}", Sha256::digest(&archive)).eq_ignore_ascii_case(digest) {
        return Err(fail(
            "invalidInput",
            "Downloaded Pandoc archive does not match the release digest",
        ));
    }
    let staging = Builder::new()
        .prefix(".pandoc-")
        .tempdir_in(tools_dir)
        .map_err(|error| caused_by("io", "Cannot create Pandoc staging directory", error))?;
    let archive_path = staging.path().join("pandoc.zip");
    fs::write(&archive_path, archive)
        .map_err(|error| caused_by("io", "Cannot write Pandoc archive", error))?;
    run(
        process,
        &tar,
        &[
            "-xf".into(),
            path_text(&archive_path)?,
            "-C".into(),
            path_text(staging.path())?,
        ],
    )?;
    let executable = staging
        .path()
        .join(format!("pandoc-{PANDOC_VERSION}"))
        .join("pandoc.exe");
    if !executable.is_file() {
        return Err(fail(
            "invalidInput",
            "The Pandoc archive did not contain the expected executable",
        ));
    }
    let replacement = Builder::new()
        .prefix(".pandoc-ready-")
        .tempdir_in(tools_dir)
        .map_err(|error| caused_by("io", "Cannot create Pandoc installation", error))?;
    fs::copy(executable, replacement.path().join("pandoc.exe"))
        .map_err(|error| caused_by("io", "Cannot stage Pandoc executable", error))?;
    publish_directory(
        replacement.path(),
        &tools_dir.join(format!("pandoc-{PANDOC_VERSION}")),
        tools_dir,
    )
}
