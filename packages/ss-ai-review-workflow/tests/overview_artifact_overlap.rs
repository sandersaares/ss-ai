//! Exercises overview artifact ownership with real retained filesystem locks.

use std::cell::Cell;
use std::env;
use std::fs;
use std::io;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::panic::{RefUnwindSafe, UnwindSafe};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use jiff::Timestamp;
use ohno::AppError;
use sha2::{Digest, Sha256};
use ss_ai_review_workflow::overview_artifact::{OverviewArtifact, Preparation, freeze};
use ss_ai_review_workflow::report_html::write_report;
use ss_ai_runtime::{Clock, FileTransaction, Process, ProcessOutput, fail};
use tempfile::Builder;

/// Converts contention into an explicit signal and a caller-controlled retry.
struct WaitClock {
    events: Sender<bool>,
    release: Receiver<()>,
    waited: Cell<bool>,
}

impl Clock for WaitClock {
    fn now(&self) -> Timestamp {
        Timestamp::MIN
    }

    fn wait(&self, _: Duration) -> Result<(), AppError> {
        if self.waited.replace(true) {
            return Err(fail("contention", "Unexpected repeated contention"));
        }
        _ = self.events.send(true);
        self.release
            .recv()
            .map_err(|error| fail("contention", error.to_string()))
    }
}

/// Rejects unexpected contention instead of waiting on real time.
struct NoWaitClock;

impl Clock for NoWaitClock {
    fn now(&self) -> Timestamp {
        Timestamp::MIN
    }

    fn wait(&self, _: Duration) -> Result<(), AppError> {
        Err(fail("contention", "Unexpected contention"))
    }
}

/// Supplies discovery and report output without executing installed programs.
struct FakeProcess {
    browser: PathBuf,
}

impl Process for FakeProcess {
    fn execute(&self, _: &str, arguments: &[String], _: &[u8]) -> io::Result<ProcessOutput> {
        let output = if arguments.first().is_some_and(|argument| argument == "-e") {
            self.browser.to_string_lossy().as_bytes().to_vec()
        } else if arguments.iter().any(|argument| argument == "--to=json") {
            br#"{"pandoc-api-version":[1,23],"meta":{},"blocks":[]}"#.to_vec()
        } else {
            concat!(
                "<!DOCTYPE html><html>",
                "<meta http-equiv=\"Content-Security-Policy\" content=\"",
                "default-src 'none'; img-src data:; style-src 'unsafe-inline'; ",
                "base-uri 'none'; form-action 'none'\">",
                "<body>overview</body></html>"
            )
            .as_bytes()
            .to_vec()
        };
        Ok(ProcessOutput::new(true, output, Vec::new()))
    }
}

/// Creates the discovery fixture without running a package manager.
fn file(path: &Path, content: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

#[test]
fn artifact_types_are_unwind_safe() {
    fn assert_traits<T: UnwindSafe + RefUnwindSafe>() {}
    assert_traits::<OverviewArtifact>();
    assert_traits::<Preparation>();
}

/// Isolates PATH before any locks are acquired so children never inherit them.
#[test]
#[cfg_attr(miri, ignore = "process isolation requires a child executable")]
fn overlapping_freezes_and_source_writer() {
    let workspace = Builder::new()
        .prefix(".overview-overlap-")
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
        file(&path, b"");
        #[cfg(unix)]
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let output = Command::new(env::current_exe().unwrap())
        .args(["--exact", "overlap_child", "--ignored", "--nocapture"])
        .env("OVERVIEW_OVERLAP_ROOT", workspace.path())
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

/// The first render owns both the manifest transaction and the source lock.
#[test]
#[ignore = "launched with isolated discovery environment"]
fn overlap_child() {
    let root = PathBuf::from(env::var_os("OVERVIEW_OVERLAP_ROOT").unwrap());
    let skill = root.join("skill");
    let tools = root.join("tools");
    let output = root.join("generation");
    let source = root.join("overview.md");
    let markdown = b"# Overview\n";
    let source_hash = format!("{:x}", Sha256::digest(markdown));
    let lockfile = b"{}";
    file(&skill.join("tools/package-lock.json"), lockfile);
    file(&source, markdown);
    fs::create_dir_all(&output).unwrap();
    let cache = tools.join(format!("mermaid-{:x}", Sha256::digest(lockfile)));
    file(
        &cache.join("node_modules/@mermaid-js/mermaid-cli/src/cli.js"),
        b"",
    );
    file(&cache.join("node_modules/puppeteer/install.mjs"), b"");
    file(
        &cache.join("node_modules/puppeteer/package.json"),
        br#"{"main":"main.js"}"#,
    );
    file(&cache.join("node_modules/puppeteer/main.js"), b"");
    let browser = root.join("browser");
    file(&browser, b"");
    let process = FakeProcess { browser };

    thread::scope(|scope| {
        let (render_events_tx, render_events_rx) = mpsc::channel();
        let (render_release_tx, render_release_rx) = mpsc::channel();
        let first = scope.spawn({
            let (source, source_hash, output, skill, tools, process) =
                (&source, &source_hash, &output, &skill, &tools, &process);
            move || {
                let result = freeze(
                    source,
                    source_hash,
                    output,
                    skill,
                    tools,
                    process,
                    &NoWaitClock,
                    |discovered, guard, source, html| {
                        _ = render_events_tx.send(true);
                        render_release_rx
                            .recv()
                            .map_err(|error| fail("io", error.to_string()))?;
                        write_report(
                            source,
                            html,
                            skill,
                            "Overview",
                            discovered,
                            guard,
                            process,
                            &NoWaitClock,
                        )
                    },
                );
                _ = render_events_tx.send(false);
                result
            }
        });
        let render_started = render_events_rx.recv().unwrap_or(false);

        let (second_events_tx, second_events_rx) = mpsc::channel();
        let (second_release_tx, second_release_rx) = mpsc::channel();
        let second = scope.spawn({
            let (source, source_hash, output, skill, tools, process) =
                (&source, &source_hash, &output, &skill, &tools, &process);
            move || {
                let clock = WaitClock {
                    events: second_events_tx.clone(),
                    release: second_release_rx,
                    waited: Cell::new(false),
                };
                let result = freeze(
                    source,
                    source_hash,
                    output,
                    skill,
                    tools,
                    process,
                    &clock,
                    |_, _, _, _| Err(fail("invalidInput", "Unexpected second render")),
                );
                _ = second_events_tx.send(false);
                result
            }
        });
        let second_waited = second_events_rx.recv().unwrap_or(false);

        let (writer_events_tx, writer_events_rx) = mpsc::channel();
        let (writer_release_tx, writer_release_rx) = mpsc::channel();
        let writer = scope.spawn({
            let source = &source;
            move || {
                let clock = WaitClock {
                    events: writer_events_tx.clone(),
                    release: writer_release_rx,
                    waited: Cell::new(false),
                };
                let result = FileTransaction::open(source, &clock)
                    .and_then(|transaction| transaction.write_bytes(markdown));
                _ = writer_events_tx.send(false);
                result
            }
        });
        let writer_waited = writer_events_rx.recv().unwrap_or(false);

        // Release all workers before inspecting results, including failure paths.
        _ = render_release_tx.send(());
        let first = first.join();
        _ = second_release_tx.send(());
        let second = second.join();
        _ = writer_release_tx.send(());
        let writer = writer.join();

        assert!(render_started, "first preparation did not reach rendering");
        assert!(second_waited, "second preparation did not contend");
        assert!(writer_waited, "source writer did not contend");
        let Preparation::Ready(first) = first.unwrap().unwrap() else {
            panic!("tools missing")
        };
        let Preparation::Ready(second) = second.unwrap().unwrap() else {
            panic!("tools missing")
        };
        writer.unwrap().unwrap();
        assert_eq!(first.html_path, second.html_path);
        assert_eq!(first.html_hash, second.html_hash);
        assert_eq!(first.archive_path, second.archive_path);
        assert_eq!(first.archive_hash, second.archive_hash);
        assert_eq!(
            first.html_hash,
            format!("{:x}", Sha256::digest(fs::read(&first.html_path).unwrap()))
        );
        assert_eq!(
            first.archive_hash,
            format!(
                "{:x}",
                Sha256::digest(fs::read(&first.archive_path).unwrap())
            )
        );
    });
}
