//! Exercises report presentation contracts.
#![allow(
    clippy::indexing_slicing,
    reason = "The test owns the small, fixed Pandoc fixture"
)]

use std::cell::RefCell;
use std::fs;
use std::io;
use std::path::Path;
use std::time::Duration;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use jiff::Timestamp;
use ohno::AppError;
use serde_json::{Value, json};
use ss_ai_review_workflow::report_html::write_report;
use ss_ai_review_workflow::report_tools::{ReportTools, tools_lock};
use ss_ai_runtime::{Clock, LockMode, Process, ProcessOutput, fail};
use tempfile::{Builder, TempDir};

/// Unexpected contention fails instead of waiting on a real clock.
struct NoWait;

impl Clock for NoWait {
    fn now(&self) -> Timestamp {
        Timestamp::UNIX_EPOCH
    }

    fn wait(&self, _: Duration) -> Result<(), AppError> {
        Err(fail("contention", "Unexpected wait"))
    }
}

/// Replaces external renderers and records the document submitted to Pandoc.
struct Renderers {
    svg: &'static str,
    ast: Value,
    rendered: RefCell<Option<Value>>,
}

impl Process for Renderers {
    fn execute(
        &self,
        program: &str,
        arguments: &[String],
        input: &[u8],
    ) -> io::Result<ProcessOutput> {
        if program == "node" {
            let output = arguments
                .windows(2)
                .find(|pair| pair[0] == "--output")
                .unwrap();
            fs::write(&output[1], self.svg)?;
            return Ok(ProcessOutput::new(true, Vec::new(), Vec::new()));
        }
        assert_eq!(program, "pandoc");
        if arguments.iter().any(|arg| arg == "--to=json") {
            return Ok(ProcessOutput::new(
                true,
                serde_json::to_vec(&self.ast)?,
                Vec::new(),
            ));
        }
        let ast: Value = serde_json::from_slice(input)?;
        let diagram = ast["blocks"]
            .as_array()
            .unwrap()
            .iter()
            .find_map(|block| (block["t"] == "RawBlock").then(|| block["c"][1].as_str().unwrap()))
            .unwrap_or("");
        let html = format!(
            "<!DOCTYPE html><html><meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; img-src data:; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'\"><body>{diagram}</body></html>"
        );
        *self.rendered.borrow_mut() = Some(ast);
        Ok(ProcessOutput::new(true, html.into_bytes(), Vec::new()))
    }
}

/// Keeps each report and its lock files inside an isolated checkout fixture.
fn workspace() -> TempDir {
    Builder::new()
        .prefix(".migration-report-presentation-")
        .tempdir_in(env!("CARGO_MANIFEST_DIR"))
        .unwrap()
}

/// Runs the public report pipeline with a Mermaid node and optional other code.
fn render(root: &Path, svg: &'static str, blocks: &[Value]) -> (Result<(), AppError>, Renderers) {
    let source = root.join("source.md");
    let output = root.join("report.html");
    fs::write(&source, "# Source\n").unwrap();
    let process = Renderers {
        svg,
        ast: json!({"pandoc-api-version":[1,23], "meta":{}, "blocks":blocks}),
        rendered: RefCell::new(None),
    };
    let tools = ReportTools::new(
        "pandoc".into(),
        Some("node".into()),
        "mermaid".into(),
        None,
        vec![],
        vec![],
    );
    let guard = tools_lock(root, LockMode::Shared, &NoWait).unwrap();
    let skill = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills/ss-review");
    let result = write_report(
        &source, &output, &skill, "Report", &tools, &guard, &process, &NoWait,
    )
    .map(|_| ());
    (result, process)
}

/// Active SVG and external references must never replace a previously published report.
#[test]
fn rejected_svg_preserves_existing_report() {
    let diagram = json!({"t":"CodeBlock","c":[["",["mermaid"],[]],"graph TD; A-->B"]});
    for svg in [
        "<svg><script>run()</script></svg>",
        "<svg><foreignObject>label</foreignObject></svg>",
        "<svg><image href=\"https://example.invalid/a.png\"/></svg>",
        "<svg onload=\"run()\"/>",
        "<svg><a href=\"file:///secret\">link</a></svg>",
        "<!DOCTYPE svg [<!ENTITY test SYSTEM \"file:///secret\">]><svg>&test;</svg>",
    ] {
        let root = workspace();
        let output = root.path().join("report.html");
        fs::write(&output, "existing report").unwrap();
        let (result, process) = render(root.path(), svg, std::slice::from_ref(&diagram));
        assert!(result.is_err(), "accepted {svg}");
        assert_eq!(fs::read_to_string(output).unwrap(), "existing report");
        assert!(process.rendered.borrow().is_none());
    }
}

/// Local fragment references remain embedded while only real Mermaid blocks change.
#[test]
fn fragment_styles_and_non_mermaid_code_survive_rendering() {
    let root = workspace();
    let svg = "<svg><style>.edge {marker-end:url(\"#arrow\");marker-start:url(#start)}</style><text>A</text></svg>";
    let ordinary = json!({"t":"CodeBlock","c":[["",["powershell"],[]],"```mermaid is just text"]});
    let diagram = json!({"t":"CodeBlock","c":[["",["mermaid"],[]],"graph TD; A-->B"]});
    let (result, process) = render(root.path(), svg, &[ordinary.clone(), diagram]);
    result.unwrap();
    let ast = process.rendered.borrow();
    let ast = ast.as_ref().unwrap();
    assert_eq!(ast["blocks"][0], ordinary);
    assert_eq!(ast["blocks"][1]["t"], "RawBlock");
    let raw = ast["blocks"][1]["c"][1].as_str().unwrap();
    assert!(raw.contains("Diagram source"));
    assert!(raw.contains("graph TD; A--&gt;B"));
    let encoded = raw
        .split_once("data:image/svg+xml;base64,")
        .unwrap()
        .1
        .split_once('"')
        .unwrap()
        .0;
    assert_eq!(STANDARD.decode(encoded).unwrap(), svg.as_bytes());
    assert!(
        fs::read_to_string(root.path().join("report.html"))
            .unwrap()
            .contains("<img")
    );
}
