//! Exercises SVG validation through the offline report pipeline.
#![no_main]

use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use jiff::Timestamp;
use libfuzzer_sys::fuzz_target;
use ohno::AppError;
use serde_json::{Value, json};
use ss_ai_review_workflow::report_html::write_report;
use ss_ai_review_workflow::report_tools::{ReportTools, tools_lock};
use ss_ai_runtime::{Clock, LockMode, Process, ProcessOutput};

// Bound both arbitrary XML and the number of generated nesting transitions.
const MAX_BYTES: usize = 2048;
const MAX_EVENTS: usize = 32;

/// Detects unexpected lock contention without consulting a real clock.
struct FakeClock;

impl Clock for FakeClock {
    fn now(&self) -> Timestamp {
        panic!("Report rendering must not inspect wall time");
    }

    fn wait(&self, _: Duration) -> Result<(), AppError> {
        panic!("Each fuzz input owns its files");
    }
}

/// Provides renderer replies without launching external tools or a browser.
struct FakeProcess<'a> {
    svg: &'a str,
}

impl Process for FakeProcess<'_> {
    fn execute(
        &self,
        program: &str,
        arguments: &[String],
        input: &[u8],
    ) -> io::Result<ProcessOutput> {
        if program == "node" {
            let output = arguments
                .windows(2)
                .find(|pair| pair.first().is_some_and(|s| s == "--output"))
                .unwrap();
            fs::write(output.get(1).unwrap(), self.svg)?;
            return Ok(ProcessOutput::new(true, vec![], vec![]));
        }
        assert_eq!(program, "pandoc");
        if arguments.iter().any(|arg| arg == "--to=json") {
            return Ok(ProcessOutput::new(
                true,
                serde_json::to_vec(&json!({
                    "pandoc-api-version": [1, 23], "meta": {},
                    "blocks": [{"t":"CodeBlock","c":[["",["mermaid"],[]],"graph LR\n A-->B"]}]
                }))?,
                vec![],
            ));
        }
        let ast: Value = serde_json::from_slice(input)?;
        let diagram = ast
            .get("blocks")
            .and_then(|value| value.get(0))
            .and_then(|value| value.get("c"))
            .and_then(|value| value.get(1))
            .and_then(Value::as_str)
            .unwrap();
        let html = format!(
            "<!DOCTYPE html><html><meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; img-src data:; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'\"><body>{diagram}</body></html>"
        );
        Ok(ProcessOutput::new(true, html.into_bytes(), vec![]))
    }
}

/// Isolates each input and removes all files after its report attempts.
struct Files(PathBuf);

impl Files {
    #[expect(
        clippy::create_dir,
        reason = "A fuzz input must not reuse an existing workspace"
    )]
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        // Relaxed ordering suffices because the counter only assigns unique names.
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(format!(".report-svg-fuzz-{}-{id}", std::process::id()));
        fs::create_dir(&root).unwrap();
        Self(root)
    }
}

impl Drop for Files {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

/// Checks publication and rejection through the public report pipeline.
fn render(files: &Files, svg: &str, must_reject: bool) {
    let source = files.0.join("source.md");
    let output = files.0.join("report.html");
    fs::write(&source, "# Source").unwrap();
    fs::write(&output, "previous report").unwrap();
    let tools = ReportTools::new(
        "pandoc".into(),
        Some("node".into()),
        "mermaid".into(),
        None,
        vec![],
        vec![],
    );
    let guard = tools_lock(&files.0, LockMode::Shared, &FakeClock).unwrap();
    let result = write_report(
        &source,
        &output,
        &files.0,
        "Report",
        &tools,
        &guard,
        &FakeProcess { svg },
        &FakeClock,
    );
    if must_reject {
        assert!(result.is_err());
    }
    if let Ok(result) = result {
        assert_eq!(result.diagram_count, 1);
        assert!(
            fs::read_to_string(&output)
                .unwrap()
                .contains("data:image/svg+xml;base64,")
        );
    } else {
        assert_eq!(fs::read(&output).unwrap(), b"previous report");
    }
}

fuzz_target!(|input: &[u8]| {
    let files = Files::new();
    let input = input.get(..input.len().min(MAX_BYTES)).unwrap();
    let raw = String::from_utf8_lossy(input);
    render(&files, &raw, false);

    // Each byte adds one event; all generated sequences terminate by construction.
    let mut svg = String::from("<svg>");
    for byte in input.iter().take(MAX_EVENTS) {
        svg.push_str(match byte % 10 {
            0 => "<g>",
            1 => "</g>",
            2 => "<desc>",
            3 => "</desc>",
            4 => "<style>",
            5 => "</style>",
            6 => "<![CDATA[text]]>",
            7 => "&amp;",
            8 => "<path/>",
            _ => "text",
        });
    }
    svg.push_str("</svg>");
    render(&files, &svg, false);
    render(&files, "<svg><script/></svg>", true);
    render(
        &files,
        "<svg><image href=\"https://example.com/image\"/></svg>",
        true,
    );
});
