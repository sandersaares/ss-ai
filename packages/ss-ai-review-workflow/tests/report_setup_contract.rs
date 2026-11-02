//! Checks the consent boundary for unavailable report rendering tools.

use std::fs;
use std::io;
use std::path::PathBuf;
use std::time::Duration;

use jiff::Timestamp;
use ohno::AppError;
use ss_ai_review_workflow::report_html::write_report;
use ss_ai_review_workflow::report_tools::{ReportTools, assert_ready, tools_lock};
use ss_ai_runtime::{Clock, LockMode, Process, ProcessOutput};

/// Unexpected contention fails without using the real clock.
struct NoWait;

impl Clock for NoWait {
    fn now(&self) -> Timestamp {
        Timestamp::UNIX_EPOCH
    }

    fn wait(&self, _: Duration) -> Result<(), AppError> {
        panic!("this scenario must not wait")
    }
}

/// A blocked output directory cannot launch a renderer.
struct NoProcess;

impl Process for NoProcess {
    fn execute(&self, _: &str, _: &[String], _: &[u8]) -> io::Result<ProcessOutput> {
        panic!("rendering must not start")
    }
}

/// Owns the fixture below the checkout and removes it after the test.
struct Workspace(PathBuf);

impl Workspace {
    fn new() -> Self {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(format!(".report-setup-contract-{}", std::process::id()));
        #[expect(clippy::create_dir, reason = "Fixture collisions must fail")]
        fs::create_dir(&root).unwrap();
        Self(root)
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

/// Missing tools are named and installation requires the user's permission.
#[test]
fn missing_tools_request_permission_before_installation() {
    let tools = ReportTools::new(
        "pandoc".into(),
        None,
        "mermaid".into(),
        None,
        vec!["pandoc".into(), "mermaid-browser".into()],
        vec![],
    );
    let error = assert_ready(&tools, false).unwrap_err().to_string();
    assert!(error.contains("pandoc"));
    assert!(error.contains("mermaid-browser"));
    assert!(error.contains("Ask the user for installation permission"));
    let after_install = assert_ready(&tools, true).unwrap_err().to_string();
    assert!(after_install.contains("approved installation"));
    assert!(after_install.contains("separate approval"));
}

/// A destination that cannot be created must not cause a fallback output.
#[test]
fn inaccessible_output_never_falls_back_or_invokes_renderer() {
    let workspace = Workspace::new();
    let root = &workspace.0;
    let blocker = root.join("not-a-directory");
    fs::write(&blocker, b"preserve").unwrap();
    fs::write(root.join("source.md"), "# Report\n\nContent\n").unwrap();
    let tools = ReportTools::new(
        "pandoc".into(),
        None,
        "mermaid".into(),
        None,
        vec![],
        vec![],
    );
    let guard = tools_lock(&root.join("tools"), LockMode::Shared, &NoWait).unwrap();
    let result = write_report(
        &root.join("source.md"),
        &blocker.join("report.html"),
        root,
        "Report",
        &tools,
        &guard,
        &NoProcess,
        &NoWait,
    );
    result.unwrap_err();
    assert_eq!(fs::read(&blocker).unwrap(), b"preserve");
    assert!(!root.join("report.html").exists());
}
