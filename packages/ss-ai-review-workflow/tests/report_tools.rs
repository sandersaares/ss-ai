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

use ss_ai_review_workflow::report_tools::{ReportTools, discover, install, readiness, tools_lock};
use ss_ai_runtime::{LockMode, Process, ProcessOutput, ResourceLock, SystemClock};
use tempfile::Builder;

/// Simulates npm and Puppeteer without downloading or executing installed tools.
struct Installer {
    browser: PathBuf,
    fail_browser: Cell<bool>,
    fail_npm: Cell<bool>,
    npm_calls: Cell<usize>,
    system_calls: RefCell<Vec<Vec<String>>>,
}

impl Process for Installer {
    fn execute(
        &self,
        _program: &str,
        arguments: &[String],
        _input: &[u8],
    ) -> io::Result<ProcessOutput> {
        if arguments.first().is_some_and(|arg| arg == "-e") {
            return Ok(ProcessOutput::new(
                true,
                self.browser.to_string_lossy().as_bytes().to_vec(),
                Vec::new(),
            ));
        }
        if arguments.first().is_some_and(|arg| arg == "-n") {
            self.system_calls.borrow_mut().push(arguments.to_vec());
            return Ok(ProcessOutput::new(true, Vec::new(), Vec::new()));
        }
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
        self.npm_calls
            .set(self.npm_calls.get().checked_add(1).unwrap());
        if self.fail_npm.get() {
            return Ok(ProcessOutput::new(
                false,
                Vec::new(),
                b"npm failure".to_vec(),
            ));
        }
        file(
            &directory.join("node_modules/@mermaid-js/mermaid-cli/src/cli.js"),
            "",
        );
        file(&directory.join("node_modules/puppeteer/install.mjs"), "");
        file(
            &directory.join("node_modules/puppeteer/package.json"),
            r#"{"main":"lib/main.js"}"#,
        );
        file(&directory.join("node_modules/puppeteer/lib/main.js"), "");
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
    file(&skill.join("tools/package.json"), "{}");
    file(
        &skill.join("tools/package-lock.json"),
        r#"{"lockfileVersion":3}"#,
    );
    let process = Installer {
        browser: root.join("browser"),
        fail_browser: Cell::new(false),
        fail_npm: Cell::new(false),
        npm_calls: Cell::new(0),
        system_calls: RefCell::new(Vec::new()),
    };
    let clock = SystemClock::default();
    let guard = tools_lock(&tools_dir, LockMode::Shared, &clock).unwrap();
    let initial = discover(&skill, &tools_dir, &process, &guard).unwrap();
    assert!(initial.missing.contains(&"mermaid-cli".to_owned()));
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
    process.fail_npm.set(false);
    #[cfg(target_os = "linux")]
    {
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
    }
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
    let clock = SystemClock::default();
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
