//! Exercises report workflow dispatch with offline process boundaries.

use std::cell::RefCell;
use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use ss_ai::{Process, ProcessOutput, SystemClock, run_workflow};
use tempfile::Builder;

/// Replaces report subprocesses without installing or launching dependencies.
struct ReportProcesses {
    browser: PathBuf,
    titles: RefCell<Vec<String>>,
}

impl Process for ReportProcesses {
    fn execute(
        &self,
        program: &str,
        arguments: &[String],
        input: &[u8],
    ) -> io::Result<ProcessOutput> {
        if arguments.first().is_some_and(|argument| argument == "-e") {
            return Ok(ProcessOutput::new(
                true,
                self.browser.to_string_lossy().as_bytes().to_vec(),
                vec![],
            ));
        }
        if arguments.iter().any(|argument| argument == "--input") {
            let output = arguments
                .windows(2)
                .find(|pair| pair.first().is_some_and(|arg| arg == "--output"))
                .unwrap();
            fs::write(output.get(1).unwrap(), "<svg><desc>Report</desc></svg>")?;
            return Ok(ProcessOutput::new(true, vec![], vec![]));
        }
        assert_eq!(Path::new(program).file_stem().unwrap(), "pandoc");
        if arguments.iter().any(|argument| argument == "--to=json") {
            let blocks = if input
                .windows(b"```mermaid".len())
                .any(|part| part == b"```mermaid")
            {
                vec![json!({"t":"CodeBlock","c":[["",["mermaid"],[]],"flowchart LR; A-->B"]})]
            } else {
                vec![]
            };
            return Ok(ProcessOutput::new(
                true,
                serde_json::to_vec(&json!({
                    "pandoc-api-version": [1, 23], "meta": {}, "blocks": blocks
                }))?,
                vec![],
            ));
        }
        let metadata = arguments
            .windows(2)
            .find(|pair| pair.first().is_some_and(|arg| arg == "--metadata"))
            .unwrap();
        self.titles
            .borrow_mut()
            .push(metadata.get(1).unwrap().clone());
        let ast: Value = serde_json::from_slice(input)?;
        let body = ast
            .get("blocks")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .find_map(|block| {
                (block.get("t") == Some(&json!("RawBlock")))
                    .then(|| block.get("c").unwrap().get(1).unwrap().as_str().unwrap())
            })
            .unwrap_or("");
        let html = format!(
            "<!DOCTYPE html><html><meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; img-src data:; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'\"><body>{body}</body></html>"
        );
        Ok(ProcessOutput::new(true, html.into_bytes(), vec![]))
    }
}

/// Supplies a file required by discovery without executing it.
fn fixture_file(path: &Path, contents: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

/// Isolates PATH and nvm configuration from concurrent tests.
#[test]
#[cfg_attr(miri, ignore = "requires a native subprocess")]
fn report_dispatch_with_offline_dependencies() {
    let workspace = Builder::new()
        .prefix(".report-cli-test-")
        .tempdir_in(env!("CARGO_MANIFEST_DIR"))
        .unwrap();
    let bin = workspace.path().join("bin");
    for name in ["node", "pandoc"] {
        let name = if cfg!(windows) {
            format!("{name}.exe")
        } else {
            name.to_owned()
        };
        let path = bin.join(name);
        fixture_file(&path, b"");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
    let output = Command::new(env::current_exe().unwrap())
        .args([
            "--exact",
            "report_dispatch_child",
            "--ignored",
            "--nocapture",
        ])
        .env("REPORT_CLI_TEST_ROOT", workspace.path())
        .env("PATH", &bin)
        .env("HOME", workspace.path())
        .env_remove("NVM_BIN")
        .env_remove("NVM_SYMLINK")
        .env_remove("NVM_DIR")
        .env_remove("XDG_CONFIG_HOME")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Checks output contracts and the production readiness probe using fake processes.
#[test]
#[ignore = "launched with an isolated environment"]
fn report_dispatch_child() {
    let root = PathBuf::from(env::var_os("REPORT_CLI_TEST_ROOT").unwrap());
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
    let process = ReportProcesses {
        browser,
        titles: RefCell::new(Vec::new()),
    };
    let clock = SystemClock::default();
    let skill_arg = format!("--skill-root={}", skill.display());
    let tools_arg = format!("--tools-directory={}", tools.display());
    let readiness = run_workflow(
        ["ss-ai", "test-review-report-tools", &skill_arg, &tools_arg],
        &process,
        &clock,
    )
    .unwrap();
    let readiness: Value = serde_json::from_str(&readiness).unwrap();
    assert_eq!(readiness.get("missing"), Some(&json!([])));

    let source = root.join("source.md");
    let output = root.join("report.html");
    let title = root.join("title.txt");
    fixture_file(&source, "# Source\n".as_bytes());
    fixture_file(&title, "Review – overview".as_bytes());
    let result = run_workflow(
        [
            "ss-ai",
            "render-review-html",
            &format!("--path={}", source.display()),
            &format!("--output-path={}", output.display()),
            &skill_arg,
            &tools_arg,
            &format!("--title-path={}", title.display()),
        ],
        &process,
        &clock,
    )
    .unwrap();
    let result: Value = serde_json::from_str(&result).unwrap();
    assert_eq!(result.get("sourcePath").unwrap(), source.to_str().unwrap());
    assert_eq!(result.get("htmlPath").unwrap(), output.to_str().unwrap());
    assert_eq!(
        result.get("sourceHash").unwrap().as_str().unwrap(),
        format!("{:x}", Sha256::digest(fs::read(&source).unwrap()))
    );
    assert_eq!(
        result.get("htmlHash").unwrap().as_str().unwrap(),
        format!("{:x}", Sha256::digest(fs::read(&output).unwrap()))
    );
    assert_eq!(result.get("diagramCount").unwrap(), 0);
    assert!(
        result
            .get("htmlUri")
            .unwrap()
            .as_str()
            .unwrap()
            .starts_with("file:")
    );
    assert_eq!(
        process.titles.borrow().as_slice(),
        ["title=Review report", "title=Review – overview"]
    );
}
