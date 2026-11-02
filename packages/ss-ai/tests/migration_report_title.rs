//! Exercises the native title-file and report-path contracts without real tools.
#![allow(
    clippy::indexing_slicing,
    reason = "The test checks a fixed native result and renderer argument shape"
)]

use std::cell::RefCell;
use std::env;
use std::fs;
use std::io;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use jiff::Timestamp;
use ohno::AppError;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use ss_ai::{Process, ProcessOutput, run_workflow};
use ss_ai_runtime::{Clock, fail};
use tempfile::Builder;

/// Detects unexpected lock contention without using real time.
struct NoWait;

impl Clock for NoWait {
    fn now(&self) -> Timestamp {
        Timestamp::UNIX_EPOCH
    }

    fn wait(&self, _: Duration) -> Result<(), AppError> {
        Err(fail("contention", "Unexpected wait"))
    }
}

/// Captures the renderer title while replacing all dependency processes.
struct ReportProcesses {
    browser: PathBuf,
    title: RefCell<Option<String>>,
}

impl Process for ReportProcesses {
    fn execute(&self, program: &str, arguments: &[String], _: &[u8]) -> io::Result<ProcessOutput> {
        if arguments.first().is_some_and(|arg| arg == "-e") {
            return Ok(ProcessOutput::new(
                true,
                self.browser.to_string_lossy().as_bytes().to_vec(),
                Vec::new(),
            ));
        }
        assert_eq!(Path::new(program).file_stem().unwrap(), "pandoc");
        if arguments.iter().any(|arg| arg == "--to=json") {
            return Ok(ProcessOutput::new(
                true,
                serde_json::to_vec(&json!({"pandoc-api-version":[1,23],"meta":{},"blocks":[]}))?,
                Vec::new(),
            ));
        }
        let metadata = arguments
            .windows(2)
            .find(|pair| pair[0] == "--metadata")
            .unwrap();
        *self.title.borrow_mut() = Some(metadata[1].clone());
        Ok(ProcessOutput::new(
            true,
            b"<!DOCTYPE html><html><meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; img-src data:; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'\"><body></body></html>".to_vec(),
            Vec::new(),
        ))
    }
}

/// Writes discovered dependency files within this test's isolated workspace.
fn fixture_file(path: &Path, contents: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

/// Isolates process-wide environment and working directory from other tests.
#[test]
#[cfg_attr(miri, ignore = "requires a native subprocess")]
#[cfg_attr(mutants, ignore = "a mutated subprocess could otherwise hang")]
fn title_and_relative_output_keep_invocation_directory() {
    let workspace = Builder::new()
        .prefix(".migration-report-title-")
        .tempdir_in(env!("CARGO_MANIFEST_DIR"))
        .unwrap();
    let bin = workspace.path().join("bin");
    for name in ["node", "pandoc"] {
        let name = if cfg!(windows) {
            format!("{name}.exe")
        } else {
            name.to_owned()
        };
        let executable = bin.join(name);
        fixture_file(&executable, b"");
        #[cfg(unix)]
        {
            fs::set_permissions(executable, fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
    let invocation = workspace.path().join("invocation");
    fs::create_dir_all(&invocation).unwrap();
    let result = Command::new(env::current_exe().unwrap())
        .args(["--exact", "title_child", "--ignored", "--nocapture"])
        .current_dir(&invocation)
        .env("REPORT_TITLE_TEST_ROOT", workspace.path())
        .env("PATH", &bin)
        .env("HOME", workspace.path())
        .env_remove("NVM_BIN")
        .env_remove("NVM_SYMLINK")
        .env_remove("NVM_DIR")
        .env_remove("XDG_CONFIG_HOME")
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}

/// Verifies Unicode and line breaks through the title file and returned paths.
#[test]
#[ignore = "launched with an isolated environment"]
fn title_child() {
    let root = PathBuf::from(env::var_os("REPORT_TITLE_TEST_ROOT").unwrap());
    let invocation = root.join("invocation");
    let skill = root.join("skill");
    let tools = root.join("tools");
    let lock = b"{\"lockfileVersion\":3}";
    fixture_file(&skill.join("tools/package-lock.json"), lock);
    let hash = format!("{:x}", Sha256::digest(lock));
    let packages = tools.join(format!("mermaid-{hash}")).join("node_modules");
    fixture_file(&packages.join("@mermaid-js/mermaid-cli/src/cli.js"), b"");
    fixture_file(&packages.join("puppeteer/install.mjs"), b"");
    fixture_file(
        &packages.join("puppeteer/package.json"),
        br#"{"main":"lib/main.js"}"#,
    );
    fixture_file(&packages.join("puppeteer/lib/main.js"), b"");
    let browser = root.join("browser");
    fixture_file(&browser, b"");
    fixture_file(&invocation.join("source.md"), b"# Source\n");
    fixture_file(
        &invocation.join("title.txt"),
        "Review – overview\nTitle".as_bytes(),
    );
    fs::create_dir_all(root.join("reports")).unwrap();
    let process = ReportProcesses {
        browser,
        title: RefCell::new(None),
    };
    let initial_directory = env::current_dir().unwrap();
    let result = run_workflow(
        [
            "ss-ai",
            "render-review-html",
            "--path=source.md",
            "--output-path=../reports/report.html",
            &format!("--skill-root={}", skill.display()),
            &format!("--tools-directory={}", tools.display()),
            "--title-path=title.txt",
        ],
        &process,
        &NoWait,
    )
    .unwrap();
    let result: Value = serde_json::from_str(&result).unwrap();
    assert_eq!(env::current_dir().unwrap(), initial_directory);
    assert_eq!(
        process.title.borrow().as_deref(),
        Some("title=Review – overview\nTitle")
    );
    let source = fs::canonicalize(invocation.join("source.md")).unwrap();
    let output = fs::canonicalize(root.join("reports/report.html")).unwrap();
    assert_eq!(
        fs::canonicalize(result["sourcePath"].as_str().unwrap()).unwrap(),
        source
    );
    assert_eq!(
        fs::canonicalize(result["htmlPath"].as_str().unwrap()).unwrap(),
        output
    );
    assert_eq!(
        result["sourceHash"],
        format!("{:x}", Sha256::digest(b"# Source\n"))
    );
    assert_eq!(
        result["htmlHash"],
        format!("{:x}", Sha256::digest(fs::read(output).unwrap()))
    );
    assert_eq!(result["diagramCount"], 0);
    assert!(result["htmlUri"].as_str().unwrap().starts_with("file:"));
    assert_eq!(
        fs::read_to_string(invocation.join("title.txt")).unwrap(),
        "Review – overview\nTitle"
    );
}
