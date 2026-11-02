//! Offline report-tool setup and locking contracts.

use std::cell::{Cell, RefCell};
use std::env;
use std::ffi::OsStr;
use std::fs;
use std::io;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::panic::{RefUnwindSafe, UnwindSafe};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use jiff::Timestamp;
use ohno::AppError;
use ss_ai_review_workflow::report_tools::{
    ReportTools, assert_ready, discover, install, readiness, tools_lock,
};
use ss_ai_runtime::{Clock, LockMode, Process, ProcessOutput, ResourceLock};
use tempfile::Builder;

/// Fails immediately if an uncontended fixture unexpectedly attempts a retry.
struct FixedClock;

impl Clock for FixedClock {
    fn now(&self) -> Timestamp {
        Timestamp::UNIX_EPOCH
    }

    fn wait(&self, _duration: Duration) -> Result<(), AppError> {
        panic!("unexpected report-tool lock contention")
    }
}

/// Simulates npm and Puppeteer without downloading or executing installed tools.
struct Installer {
    browser: PathBuf,
    fail_browser: Cell<bool>,
    fail_discovery: Cell<bool>,
    fail_npm: Cell<bool>,
    omit_package: RefCell<Option<String>>,
    fail_system_operation: RefCell<Option<String>>,
    expected_old_tree: RefCell<Option<PathBuf>>,
    npm_calls: Cell<usize>,
    browser_installs: Cell<usize>,
    system_calls: RefCell<Vec<Vec<String>>>,
}

impl Process for Installer {
    fn execute(
        &self,
        program: &str,
        arguments: &[String],
        _input: &[u8],
    ) -> io::Result<ProcessOutput> {
        if arguments.first().is_some_and(|arg| arg == "-e") {
            assert_eq!(
                Path::new(program),
                self.browser
                    .parent()
                    .unwrap()
                    .join("bin")
                    .join(if cfg!(windows) { "node.exe" } else { "node" })
            );
            if self.fail_discovery.get() {
                return Ok(ProcessOutput::new(
                    false,
                    Vec::new(),
                    b"browser discovery failure".to_vec(),
                ));
            }
            return Ok(ProcessOutput::new(
                true,
                self.browser.to_string_lossy().as_bytes().to_vec(),
                Vec::new(),
            ));
        }
        if arguments.first().is_some_and(|arg| arg == "-n") {
            self.system_calls.borrow_mut().push(arguments.to_vec());
            if self
                .fail_system_operation
                .borrow()
                .as_ref()
                .is_some_and(|operation| arguments.contains(operation))
            {
                return Ok(ProcessOutput::new(
                    false,
                    Vec::new(),
                    b"system package failure".to_vec(),
                ));
            }
            return Ok(ProcessOutput::new(true, Vec::new(), Vec::new()));
        }
        assert_eq!(
            Path::new(program),
            self.browser
                .parent()
                .unwrap()
                .join("bin")
                .join(if cfg!(windows) { "node.exe" } else { "node" })
        );
        self.browser_installs
            .set(self.browser_installs.get().checked_add(1).unwrap());
        if self.fail_browser.get() {
            return Ok(ProcessOutput::new(
                false,
                Vec::new(),
                b"browser failure".to_vec(),
            ));
        }
        fs::write(&self.browser, b"browser")?;
        Ok(ProcessOutput::new(true, Vec::new(), Vec::new()))
    }

    fn execute_in(
        &self,
        _program: &str,
        arguments: &[String],
        _input: &[u8],
        directory: &Path,
        path: Option<&OsStr>,
    ) -> io::Result<ProcessOutput> {
        assert_eq!(
            arguments,
            [
                "ci",
                "--ignore-scripts",
                "--engine-strict",
                "--no-audit",
                "--no-fund"
            ]
        );
        assert!(path.is_some());
        assert_eq!(
            env::split_paths(path.unwrap()).next().unwrap(),
            self.browser.parent().unwrap().join("bin")
        );
        assert_eq!(fs::read_to_string(directory.join("package.json"))?, "{}");
        assert_eq!(
            fs::read_to_string(directory.join("package-lock.json"))?,
            r#"{"lockfileVersion":3}"#
        );
        if let Some(marker) = self.expected_old_tree.borrow().as_ref() {
            assert_eq!(fs::read_to_string(marker)?, "preserve");
        }
        self.npm_calls
            .set(self.npm_calls.get().checked_add(1).unwrap());
        if self.fail_npm.get() {
            return Ok(ProcessOutput::new(
                false,
                Vec::new(),
                b"npm failure".to_vec(),
            ));
        }
        for (name, content) in [
            ("node_modules/@mermaid-js/mermaid-cli/src/cli.js", ""),
            ("node_modules/puppeteer/install.mjs", ""),
            (
                "node_modules/puppeteer/package.json",
                r#"{"main":"lib/main.js"}"#,
            ),
            ("node_modules/puppeteer/lib/main.js", ""),
        ] {
            if self.omit_package.borrow().as_deref() != Some(name) {
                file(&directory.join(name), content);
            }
        }
        Ok(ProcessOutput::new(true, Vec::new(), Vec::new()))
    }

    fn execute_with_path(
        &self,
        program: &str,
        arguments: &[String],
        input: &[u8],
        path: &OsStr,
    ) -> io::Result<ProcessOutput> {
        assert!(env::split_paths(path).next().is_some());
        self.execute(program, arguments, input)
    }
}

/// Writes a minimal fixture without invoking package tools.
fn file(path: &Path, content: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

/// Runs the fixture in a separate process so PATH and nvm state do not race tests.
#[test]
#[cfg_attr(
    miri,
    ignore = "process isolation requires launching a child executable"
)]
fn offline_setup_and_retained_locks() {
    let workspace = Builder::new()
        .prefix(".report-tools-test-")
        .tempdir_in(env::current_dir().unwrap())
        .unwrap();
    let bin = workspace.path().join("bin");
    let executable = |name: &str| {
        if cfg!(windows) {
            format!("{name}.exe")
        } else {
            name.to_owned()
        }
    };
    for name in ["node", "pandoc", "sudo", "apt-get"] {
        let path = bin.join(executable(name));
        file(&path, "");
        #[cfg(unix)]
        {
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
    let output = Command::new(env::current_exe().unwrap())
        .args(["--exact", "offline_child", "--ignored", "--nocapture"])
        .env("REPORT_TOOLS_TEST_ROOT", workspace.path())
        .env("PATH", &bin)
        .env_remove("NVM_BIN")
        .env_remove("NVM_SYMLINK")
        .env_remove("NVM_DIR")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if String::from_utf8_lossy(&output.stderr).contains("Unreadable manifest check skipped") {
        eprintln!("{}", String::from_utf8_lossy(&output.stderr));
    }
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
}

/// Exercises installation and discovery with an offline process fixture.
///
/// The fixture simulates package and browser installation while conflicting
/// lock attempts verify that setup and readiness retain their respective locks.
/// Cache identity follows the active lockfile; failed repairs preserve the old
/// tree, and approved system setup forwards the browser's dependency relations.
/// See `skills/ss-review/docs/implementation.md`, "Report tools".
#[test]
#[ignore = "launched by offline_setup_and_retained_locks with an isolated environment"]
fn offline_child() {
    let root = PathBuf::from(env::var_os("REPORT_TOOLS_TEST_ROOT").unwrap());
    let skill = root.join("skill");
    let tools_dir = root.join("tools");
    let original_path = env::var_os("PATH");
    let original_directory = env::current_dir().unwrap();
    file(&skill.join("tools/package.json"), "{}");
    file(
        &skill.join("tools/package-lock.json"),
        r#"{"lockfileVersion":3}"#,
    );
    let process = Installer {
        browser: root.join("browser"),
        fail_browser: Cell::new(false),
        fail_discovery: Cell::new(false),
        fail_npm: Cell::new(false),
        omit_package: RefCell::new(None),
        fail_system_operation: RefCell::new(None),
        expected_old_tree: RefCell::new(None),
        npm_calls: Cell::new(0),
        browser_installs: Cell::new(0),
        system_calls: RefCell::new(Vec::new()),
    };
    let clock = FixedClock;
    let guard = tools_lock(&tools_dir, LockMode::Shared, &clock).unwrap();
    let initial = discover(&skill, &tools_dir, &process, &guard).unwrap();
    assert!(initial.missing.contains(&"mermaid-cli".to_owned()));
    let before_approval = assert_ready(&initial, false).unwrap_err();
    assert!(
        before_approval
            .to_string()
            .contains("installation permission")
    );
    let after_approval = assert_ready(&initial, true).unwrap_err();
    assert!(after_approval.to_string().contains("approved installation"));
    let not_ready = readiness(&skill, &tools_dir, &process, &clock, |_, _| {
        panic!("missing dependencies must not probe")
    })
    .unwrap();
    assert!(not_ready.missing.contains(&"mermaid-cli".to_owned()));
    assert!(
        ResourceLock::try_open(&tools_dir.join("installation"), LockMode::Exclusive)
            .unwrap()
            .is_none()
    );
    drop(guard);
    let installed = install(
        &skill,
        &tools_dir,
        &process,
        &clock,
        false,
        |tools, _guard| {
            assert!(tools.missing.is_empty());
            assert!(
                ResourceLock::try_open(&tools_dir.join("installation"), LockMode::Shared)
                    .unwrap()
                    .is_none()
            );
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(process.npm_calls.get(), 1);
    #[cfg(unix)]
    exercise_unreadable_manifest(&skill, &tools_dir, &process, &clock, &installed);
    // The executable is inside the Mermaid package beneath node_modules;
    // Puppeteer is a sibling package in that same node_modules directory.
    let puppet = installed
        .mermaid
        .ancestors()
        .nth(4)
        .unwrap()
        .join("puppeteer");
    let manifest = puppet.join("package.json");
    file(&manifest, "{broken");
    let guard = tools_lock(&tools_dir, LockMode::Shared, &clock).unwrap();
    assert!(
        discover(&skill, &tools_dir, &process, &guard)
            .unwrap()
            .missing
            .contains(&"mermaid-cli".to_owned())
    );
    file(&manifest, r#"{"main":"lib/main.js"}"#);
    process.fail_discovery.set(true);
    let discovery = discover(&skill, &tools_dir, &process, &guard).unwrap();
    assert!(discovery.missing.contains(&"mermaid-browser".to_owned()));
    assert!(
        discovery
            .diagnostics
            .join(" ")
            .contains("browser discovery failure")
    );
    assert!(
        assert_ready(&discovery, false)
            .unwrap_err()
            .to_string()
            .contains("browser discovery failure")
    );
    process.fail_discovery.set(false);
    fs::remove_file(&process.browser).unwrap();
    let absent = discover(&skill, &tools_dir, &process, &guard).unwrap();
    assert!(absent.missing.contains(&"mermaid-browser".to_owned()));
    assert!(!absent.diagnostics.is_empty());
    file(&process.browser, "browser");
    drop(guard);
    let another_skill = root.join("other-skill");
    file(
        &another_skill.join("tools/package-lock.json"),
        r#"{"lockfileVersion":3}"#,
    );
    let guard = tools_lock(&tools_dir, LockMode::Shared, &clock).unwrap();
    assert_eq!(
        discover(&another_skill, &tools_dir, &process, &guard)
            .unwrap()
            .mermaid,
        installed.mermaid
    );
    file(&another_skill.join("tools/package-lock.json"), "{}");
    assert_ne!(
        discover(&another_skill, &tools_dir, &process, &guard)
            .unwrap()
            .mermaid,
        installed.mermaid
    );
    drop(guard);
    readiness(&skill, &tools_dir, &process, &clock, |tools, _guard| {
        assert!(tools.missing.is_empty());
        assert!(
            ResourceLock::try_open(&tools_dir.join("installation"), LockMode::Exclusive)
                .unwrap()
                .is_none()
        );
        Ok(())
    })
    .unwrap();
    let old_marker = installed.mermaid.parent().unwrap().join("old-tree");
    file(&old_marker, "preserve");
    fs::remove_file(&installed.mermaid).unwrap();
    process.fail_browser.set(true);
    install(&skill, &tools_dir, &process, &clock, false, |_, _| {
        panic!("failed setup must not probe")
    })
    .unwrap_err();
    assert_eq!(fs::read_to_string(&old_marker).unwrap(), "preserve");
    assert_eq!(process.npm_calls.get(), 2);
    process.fail_browser.set(false);
    process.fail_npm.set(true);
    install(&skill, &tools_dir, &process, &clock, false, |_, _| {
        panic!("failed setup must not probe")
    })
    .unwrap_err();
    assert!(old_marker.is_file());
    assert_eq!(env::var_os("PATH"), original_path);
    assert_eq!(env::current_dir().unwrap(), original_directory);
    process.fail_npm.set(false);
    exercise_repairs(&skill, &tools_dir, &process, &clock, &installed);
    exercise_incomplete_first_install(&skill, &root.join("first-install"), &process, &clock);
    #[cfg(target_os = "linux")]
    {
        process.system_calls.borrow_mut().clear();
        file(
            &root.join("deb.deps"),
            "libnss3 (>= 3.35)\nlibasound2 | libasound2t64\n",
        );
        install(&skill, &tools_dir, &process, &clock, true, |_, _| Ok(())).unwrap();
        let calls = process.system_calls.borrow();
        assert_eq!(calls.len(), 3);
        assert_eq!(
            calls.get(1).unwrap().last().unwrap(),
            "libnss3 (>= 3.35), libasound2 | libasound2t64"
        );
        assert_eq!(calls.get(2).unwrap().last().unwrap(), "fonts-liberation");
        drop(calls);
        exercise_system_failures(&skill, &tools_dir, &process, &clock, &root);
    }
    #[cfg(not(target_os = "linux"))]
    install(&skill, &tools_dir, &process, &clock, true, |_, _| Ok(())).unwrap_err();
}

/// Prevents incomplete npm output from publishing even on a first install.
fn exercise_incomplete_first_install(
    skill: &Path,
    tools_dir: &Path,
    process: &Installer,
    clock: &FixedClock,
) {
    let guard = tools_lock(tools_dir, LockMode::Shared, clock).unwrap();
    // The Mermaid executable is nested under node_modules in the published
    // installation root, whose absence proves that staging was not published.
    let destination = discover(skill, tools_dir, process, &guard)
        .unwrap()
        .mermaid
        .ancestors()
        .nth(5)
        .unwrap()
        .to_path_buf();
    drop(guard);
    let browser_installs = process.browser_installs.get();
    *process.omit_package.borrow_mut() = Some("node_modules/puppeteer/lib/main.js".to_owned());
    let error = install(skill, tools_dir, process, clock, false, |_, _| {
        panic!("incomplete first installation must not probe")
    })
    .unwrap_err();
    *process.omit_package.borrow_mut() = None;
    assert!(error.to_string().contains("did not provide the required"));
    assert!(!destination.exists());
    assert_eq!(process.browser_installs.get(), browser_installs);
    assert!(fs::read_dir(tools_dir).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".mermaid-")
    }));
}

/// Keeps unreadable manifests distinct from repairable malformed manifests.
#[cfg(unix)]
fn exercise_unreadable_manifest(
    skill: &Path,
    tools_dir: &Path,
    process: &Installer,
    clock: &FixedClock,
    installed: &ReportTools,
) {
    // Puppeteer is a sibling of the Mermaid package under node_modules.
    let manifest = installed
        .mermaid
        .ancestors()
        .nth(4)
        .unwrap()
        .join("puppeteer/package.json");
    let permissions = fs::metadata(&manifest).unwrap().permissions();
    fs::set_permissions(&manifest, fs::Permissions::from_mode(0o000)).unwrap();
    if fs::read(&manifest).is_ok() {
        fs::set_permissions(&manifest, permissions).unwrap();
        eprintln!(
            "Unreadable manifest check skipped: filesystem permits reading without mode bits"
        );
        return;
    }

    let calls = process.npm_calls.get();
    let guard = tools_lock(tools_dir, LockMode::Shared, clock).unwrap();
    let discovery = discover(skill, tools_dir, process, &guard);
    drop(guard);
    let installation = install(skill, tools_dir, process, clock, false, |_, _| {
        panic!("unreadable manifest must not probe")
    });
    fs::set_permissions(&manifest, permissions).unwrap();

    assert!(
        discovery
            .unwrap_err()
            .to_string()
            .contains("Cannot read Puppeteer manifest")
    );
    assert!(
        installation
            .unwrap_err()
            .to_string()
            .contains("Cannot read Puppeteer manifest")
    );
    assert_eq!(process.npm_calls.get(), calls);
}

/// Checks repair, failed publication and process-environment isolation.
fn exercise_repairs(
    skill: &Path,
    tools_dir: &Path,
    process: &Installer,
    clock: &FixedClock,
    installed: &ReportTools,
) {
    // The executable is nested beneath node_modules in the published installation root.
    let directory = installed.mermaid.ancestors().nth(5).unwrap();
    let manifest = directory.join("node_modules/puppeteer/package.json");
    let marker = directory.join("old-tree");
    let original_path = env::var_os("PATH");
    let original_directory = env::current_dir().unwrap();

    for missing in [
        "node_modules/@mermaid-js/mermaid-cli/src/cli.js",
        "node_modules/puppeteer/install.mjs",
        "node_modules/puppeteer/package.json",
        "node_modules/puppeteer/lib/main.js",
    ] {
        // The published tree remains available when npm claims success but
        // omits a required entry; no partial replacement may be published.
        fs::remove_file(directory.join(missing))
            .unwrap_or_else(|error| assert_eq!(error.kind(), io::ErrorKind::NotFound, "{error}"));
        file(&marker, "preserve");
        *process.expected_old_tree.borrow_mut() = Some(marker.clone());
        *process.omit_package.borrow_mut() = Some(missing.to_owned());
        install(skill, tools_dir, process, clock, false, |_, _| {
            panic!("incomplete npm output must not probe")
        })
        .unwrap_err();
        assert_eq!(fs::read_to_string(&marker).unwrap(), "preserve");
        *process.omit_package.borrow_mut() = None;
        install(skill, tools_dir, process, clock, false, |_, _| Ok(())).unwrap();
        assert!(!marker.exists());
        *process.expected_old_tree.borrow_mut() = None;
    }

    for invalid in [
        "",
        " ",
        r#"{"main":"#,
        "null",
        "[]",
        r#""text""#,
        "3",
        "false",
        "{}",
        r#"{"main":[]}"#,
        r#"{"main":""}"#,
    ] {
        file(&manifest, invalid);
        let calls = process.npm_calls.get();
        install(skill, tools_dir, process, clock, false, |_, _| Ok(())).unwrap();
        assert_eq!(process.npm_calls.get(), calls.checked_add(1).unwrap());
    }

    let calls = process.npm_calls.get();
    file(&marker, "preserve");
    fs::remove_file(&process.browser).unwrap();
    process.fail_browser.set(true);
    install(skill, tools_dir, process, clock, false, |_, _| {
        panic!("failed browser repair must not probe")
    })
    .unwrap_err();
    assert_eq!(fs::read_to_string(&marker).unwrap(), "preserve");
    process.fail_browser.set(false);
    install(skill, tools_dir, process, clock, false, |_, _| Ok(())).unwrap();
    assert_eq!(process.npm_calls.get(), calls);
    assert_eq!(fs::read_to_string(&marker).unwrap(), "preserve");
    assert_eq!(env::var_os("PATH"), original_path);
    assert_eq!(env::current_dir().unwrap(), original_directory);
}

/// Checks that metadata and package failures stop before a success probe.
#[cfg(target_os = "linux")]
fn exercise_system_failures(
    skill: &Path,
    tools_dir: &Path,
    process: &Installer,
    clock: &FixedClock,
    root: &Path,
) {
    let metadata = root.join("deb.deps");
    for content in [None, Some(" \n")] {
        if let Some(content) = content {
            file(&metadata, content);
        } else {
            fs::remove_file(&metadata).unwrap();
        }
        process.system_calls.borrow_mut().clear();
        install(skill, tools_dir, process, clock, true, |_, _| {
            panic!("missing dependency metadata must not probe")
        })
        .unwrap_err();
        assert!(process.system_calls.borrow().is_empty());
    }

    file(&metadata, "libnss3\n");
    for program in ["sudo", "apt-get"] {
        let executable = root.join("bin").join(program);
        fs::remove_file(&executable).unwrap();
        process.system_calls.borrow_mut().clear();
        let error = install(skill, tools_dir, process, clock, true, |_, _| {
            panic!("missing system tool must not probe")
        })
        .unwrap_err();
        assert!(error.to_string().contains(program));
        assert!(process.system_calls.borrow().is_empty());
        file(&executable, "");
        fs::set_permissions(executable, fs::Permissions::from_mode(0o755)).unwrap();
    }

    for (operation, expected_calls) in [("update", 1), ("satisfy", 2), ("install", 3)] {
        process.system_calls.borrow_mut().clear();
        *process.fail_system_operation.borrow_mut() = Some(operation.to_owned());
        install(skill, tools_dir, process, clock, true, |_, _| {
            panic!("failed system setup must not probe")
        })
        .unwrap_err();
        assert_eq!(process.system_calls.borrow().len(), expected_calls);
    }
    *process.fail_system_operation.borrow_mut() = None;
}

/// Resolves the nvm default through its own alias implementation.
#[cfg(unix)]
#[test]
#[cfg_attr(
    miri,
    ignore = "process isolation requires launching a child executable"
)]
fn nvm_default_is_resolved_without_shell_interpolation() {
    let workspace = Builder::new()
        .prefix(".report-nvm-test-")
        .tempdir_in(env::current_dir().unwrap())
        .unwrap();
    let bin = workspace.path().join("bin");
    let shell = bin.join("sh");
    file(&shell, "");
    fs::set_permissions(shell, fs::Permissions::from_mode(0o755)).unwrap();
    let nvm = workspace.path().join("nvm 'quoted' $path");
    file(&nvm.join("nvm.sh"), "");
    file(&nvm.join("versions/node/node"), "");
    file(
        &workspace.path().join("skill/tools/package-lock.json"),
        "{}",
    );
    let output = Command::new(env::current_exe().unwrap())
        .args(["--exact", "nvm_child", "--ignored", "--nocapture"])
        .env("REPORT_TOOLS_TEST_ROOT", workspace.path())
        .env("NVM_DIR", &nvm)
        .env("PATH", &bin)
        .env_remove("NVM_BIN")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
    let active = Command::new(env::current_exe().unwrap())
        .args(["--exact", "nvm_child", "--ignored", "--nocapture"])
        .env("REPORT_TOOLS_TEST_ROOT", workspace.path())
        .env("NVM_DIR", &nvm)
        .env("NVM_BIN", nvm.join("versions/node"))
        .env("PATH", &bin)
        .output()
        .unwrap();
    assert!(
        active.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&active.stdout),
        String::from_utf8_lossy(&active.stderr)
    );
    assert!(String::from_utf8_lossy(&active.stdout).contains("1 passed"));
}

/// Records the discovery value's compatibility with panic-catching callers.
#[test]
fn report_tools_are_unwind_safe() {
    fn assert_traits<T: UnwindSafe + RefUnwindSafe>() {}
    assert_traits::<ReportTools>();
}

/// Formats discovery diagnostics without requiring an installed toolchain.
#[test]
fn report_tools_debug_formatting() {
    let tools = ReportTools::new(
        PathBuf::from("pandoc"),
        None,
        PathBuf::from("mermaid"),
        None,
        vec!["browser".to_owned()],
        vec!["browser discovery failed".to_owned()],
    );
    _ = format!("{tools:?}");
}

/// Records nvm's argument boundary and returns an installed default path.
#[cfg(unix)]
struct Nvm(PathBuf);

#[cfg(unix)]
impl Process for Nvm {
    fn execute(
        &self,
        _program: &str,
        arguments: &[String],
        _input: &[u8],
    ) -> io::Result<ProcessOutput> {
        assert!(
            env::var_os("NVM_BIN").is_none(),
            "active nvm runtime must take precedence"
        );
        assert_eq!(arguments.first().unwrap(), "-c");
        assert!(arguments.get(1).unwrap().contains("set -- --no-use"));
        assert_eq!(arguments.get(3).unwrap(), self.0.to_str().unwrap());
        Ok(ProcessOutput::new(
            true,
            format!("{}\n", self.0.join("versions/node/node").display()).into_bytes(),
            Vec::new(),
        ))
    }
}

/// Checks that the resolved runtime belongs to the explicit nvm directory.
#[cfg(unix)]
#[test]
#[ignore = "launched by nvm_default_is_resolved_without_shell_interpolation"]
fn nvm_child() {
    let root = PathBuf::from(env::var_os("REPORT_TOOLS_TEST_ROOT").unwrap());
    let nvm = PathBuf::from(env::var_os("NVM_DIR").unwrap());
    let clock = FixedClock;
    let guard = tools_lock(&root.join("tools"), LockMode::Shared, &clock).unwrap();
    let tools = discover(
        &root.join("skill"),
        &root.join("tools"),
        &Nvm(nvm.clone()),
        &guard,
    )
    .unwrap();
    assert_eq!(tools.node, Some(nvm.join("versions/node/node")));
}
