//! Exercises the report pipeline through recorded renderer processes.

#![allow(
    clippy::indexing_slicing,
    clippy::assertions_on_result_states,
    reason = "Test fixtures and expected result checks favor direct JSON access and assertions"
)]

use std::cell::RefCell;
use std::fs;
use std::io;
use std::panic::{RefUnwindSafe, UnwindSafe};
use std::path::{Path, PathBuf};
use std::time::Duration;

use jiff::Timestamp;
use ohno::AppError;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use ss_ai_review_workflow::report_html::{ReportResult, probe, write_report};
use ss_ai_review_workflow::report_tools::{ReportTools, tools_lock};
use ss_ai_runtime::{Clock, LockMode, NativeProcess, Process, ProcessOutput, ResourceLock, fail};
use tempfile::{Builder, TempDir};

/// Keeps filesystem tests independent without using a clock to wait for locks.
struct TestClock;

impl Clock for TestClock {
    fn now(&self) -> Timestamp {
        "2024-05-06T07:08:09Z".parse().unwrap()
    }

    fn wait(&self, _: Duration) -> Result<(), AppError> {
        Err(fail("contention", "Unexpected lock contention"))
    }
}

/// Supplies predetermined Pandoc output and writes a chosen Mermaid result.
struct FakeProcess {
    ast: Value,
    svg: String,
    fail_diagram: bool,
    observed_ast: RefCell<Option<Value>>,
    seeds: RefCell<Vec<String>>,
    lock_checks: Option<(PathBuf, PathBuf)>,
    html_override: Option<String>,
}

impl Process for FakeProcess {
    fn execute(
        &self,
        program: &str,
        arguments: &[String],
        input: &[u8],
    ) -> io::Result<ProcessOutput> {
        if let Some((tools, output)) = &self.lock_checks {
            assert!(
                ResourceLock::try_open(tools, LockMode::Exclusive)
                    .unwrap()
                    .is_none()
            );
            assert!(
                ResourceLock::try_open(output, LockMode::Exclusive)
                    .unwrap()
                    .is_none()
            );
        }
        if program == "node" {
            if self.fail_diagram {
                return Ok(ProcessOutput::new(
                    false,
                    vec![],
                    b"renderer failed".to_vec(),
                ));
            }
            let output = arguments
                .windows(2)
                .find(|pair| pair[0] == "--output")
                .unwrap();
            let config = arguments
                .windows(2)
                .find(|pair| pair[0] == "--configFile")
                .unwrap();
            let settings: Value = serde_json::from_slice(&fs::read(&config[1])?).unwrap();
            assert_eq!(settings["securityLevel"], "strict");
            assert_eq!(settings["htmlLabels"], false);
            assert_eq!(settings["deterministicIds"], true);
            self.seeds
                .borrow_mut()
                .push(settings["deterministicIDSeed"].as_str().unwrap().to_owned());
            fs::write(&output[1], &self.svg)?;
            return Ok(ProcessOutput::new(true, vec![], vec![]));
        }
        assert_eq!(program, "pandoc");
        if arguments.iter().any(|arg| arg == "--to=json") {
            Ok(ProcessOutput::new(
                true,
                serde_json::to_vec(&self.ast).unwrap(),
                vec![],
            ))
        } else {
            if let Some(html) = &self.html_override {
                return Ok(ProcessOutput::new(true, html.as_bytes().to_vec(), vec![]));
            }
            let ast: Value = serde_json::from_slice(input).unwrap();
            let diagram = ast["blocks"]
                .as_array()
                .unwrap()
                .iter()
                .find_map(|block| {
                    (block["t"] == "RawBlock").then(|| block["c"][1].as_str().unwrap())
                })
                .unwrap_or("");
            let html = format!(
                "<!DOCTYPE html><html><meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; img-src data:; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'\"><body>{diagram}</body></html>"
            );
            *self.observed_ast.borrow_mut() = Some(ast);
            Ok(ProcessOutput::new(true, html.into_bytes(), vec![]))
        }
    }
}

/// Uses unique invocation-owned directories within the checkout for test artifacts.
fn directory() -> TempDir {
    Builder::new()
        .prefix(".report-html-test-")
        .tempdir_in(env!("CARGO_MANIFEST_DIR"))
        .unwrap()
}

/// Provides tool paths without discovering or launching system tools.
fn tools() -> ReportTools {
    ReportTools::new(
        "pandoc".into(),
        Some("node".into()),
        "mermaid".into(),
        None,
        vec![],
        vec![],
    )
}

/// Provides a GFM document with one Mermaid fence and caller-supplied nodes.
fn process(nodes: Vec<Value>, svg: &str) -> FakeProcess {
    let mut blocks = vec![json!({"t":"CodeBlock","c":[["",["mermaid"],[]],"graph LR\n A-->B"]})];
    blocks.extend(nodes);
    FakeProcess {
        ast: json!({"pandoc-api-version":[1,23], "meta":{"title":{"t":"MetaString","c":"untrusted"}}, "blocks":blocks}),
        svg: svg.into(),
        fail_diagram: false,
        observed_ast: RefCell::new(None),
        seeds: RefCell::new(Vec::new()),
        lock_checks: None,
        html_override: None,
    }
}

/// Returns the active template root; the fake verifies only the argument boundary.
fn skill_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills/ss-review")
}

/// The reading view preserves visible source, identity, and locked publication.
/// The conflicting lock attempts verify ownership during both renderer calls.
/// See the report presentation and HTML rendering sections in the skill docs.
#[test]
fn embeds_checked_diagrams_and_transforms_the_pandoc_ast() {
    let directory = directory();
    let source = directory.path().join("input.md");
    let output = directory.path().join("report #1.html");
    fs::write(&source, "# Test\n").unwrap();
    let mut process = process(
        vec![
            json!({"t":"RawBlock","c":["html","<script>alert(1)</script>"]}),
            json!({"t":"Para","c":[
                {"t":"RawInline","c":["html","<b>"]},
                {"t":"Link","c":[["",[],[]],[{"t":"Str","c":"File"}],["src/a.rs",""]]},
                {"t":"Link","c":[["",[],[]],[{"t":"Str","c":"Web"}],["https://example.com",""]]}
            ]}),
        ],
        "<svg viewBox=\"0 0 42.2 11\"><desc>A &amp; B</desc><path d=\"M0 0\"/></svg>",
    );
    process.lock_checks = Some((directory.path().join("installation"), output.clone()));
    let guard = tools_lock(directory.path(), LockMode::Shared, &TestClock).unwrap();
    let result = write_report(
        &source,
        &output,
        &skill_root(),
        "Report",
        &tools(),
        &guard,
        &process,
        &TestClock,
    )
    .unwrap();
    _ = format!("{result:?}");
    assert_eq!(result.diagram_count, 1);
    assert!(result.html_uri.contains("report%20%231.html"));
    let html = fs::read_to_string(output).unwrap();
    // The existing report renderer rounds intrinsic dimensions up for layout.
    assert!(html.contains("alt=\"A &amp; B\" width=\"43\" height=\"11\""));
    assert!(html.contains("data:image/svg+xml;base64,"));
    assert!(html.contains("graph LR\n A--&gt;B"));
    let ast = process.observed_ast.borrow();
    let ast = ast.as_ref().unwrap();
    assert_eq!(ast["meta"], json!({}));
    assert_eq!(ast["blocks"][1]["t"], "CodeBlock");
    assert_eq!(ast["blocks"][2]["c"][0]["t"], "Str");
    assert_eq!(
        ast["blocks"][2]["c"][1],
        json!({"t":"Span","c":[["",[],[]],[
            {"t":"Str","c":"File"},{"t":"Space"},{"t":"Str","c":"("},
            {"t":"Code","c":[["",[],[]],"src/a.rs"]},{"t":"Str","c":")"}
        ]]})
    );
    assert_eq!(ast["blocks"][2]["c"][2]["t"], "Link");
    assert_eq!(result.source_hash.len(), 64);
    assert_eq!(
        result.source_hash,
        format!("{:x}", Sha256::digest(b"# Test\n"))
    );
    assert_eq!(
        result.html_hash,
        format!("{:x}", Sha256::digest(html.as_bytes()))
    );
}

/// Records the report result's compatibility with panic-catching callers.
#[test]
fn report_result_is_unwind_safe() {
    fn assert_traits<T: UnwindSafe + RefUnwindSafe>() {}
    assert_traits::<ReportResult>();
}

/// Equivalent decoded Markdown has the same identity and deterministic rendering.
#[test]
fn text_bom_does_not_change_hash_or_rendered_result() {
    let directory = directory();
    let source = directory.path().join("input.md");
    let output = directory.path().join("report.html");
    let guard = tools_lock(directory.path(), LockMode::Shared, &TestClock).unwrap();
    let fake = process(vec![], "<svg/>");
    fs::write(&source, b"# Test\n").unwrap();
    let first = write_report(
        &source,
        &output,
        &skill_root(),
        "Report",
        &tools(),
        &guard,
        &fake,
        &TestClock,
    )
    .unwrap();
    let html = fs::read(&output).unwrap();
    fs::write(&source, b"\xef\xbb\xbf# Test\n").unwrap();
    let second = write_report(
        &source,
        &output,
        &skill_root(),
        "Report",
        &tools(),
        &guard,
        &fake,
        &TestClock,
    )
    .unwrap();
    assert_eq!(first.source_hash, second.source_hash);
    assert_eq!(first.html_hash, second.html_hash);
    assert_eq!(html, fs::read(&output).unwrap());
    let seeds = fake.seeds.borrow();
    assert_eq!(seeds.len(), 2);
    assert_eq!(seeds[0], seeds[1]);
}

/// UTF-16 decoding retains replacement characters for malformed code units.
#[test]
fn malformed_utf16_matches_decoded_source_identity() {
    let directory = directory();
    let source = directory.path().join("input.md");
    let output = directory.path().join("report.html");
    let guard = tools_lock(directory.path(), LockMode::Shared, &TestClock).unwrap();
    let fake = process(vec![], "<svg/>");
    for (bytes, expected) in [
        (
            vec![0xff, 0xfe, 0x41, 0, 0, 0xd8, 0x42, 0, 0xff],
            "A\u{fffd}B\u{fffd}",
        ),
        (
            vec![0xfe, 0xff, 0, 0x41, 0xd8, 0, 0, 0x42, 0xff],
            "A\u{fffd}B\u{fffd}",
        ),
    ] {
        fs::write(&source, bytes).unwrap();
        let result = write_report(
            &source,
            &output,
            &skill_root(),
            "Report",
            &tools(),
            &guard,
            &fake,
            &TestClock,
        )
        .unwrap();
        assert_eq!(
            result.source_hash,
            format!("{:x}", Sha256::digest(expected.as_bytes()))
        );
    }
}

/// SVG dimensions and escaped descriptions preserve the reading view contract.
#[test]
fn dimensions_and_text_are_checked_before_embedding() {
    let directory = directory();
    let source = directory.path().join("source.md");
    let output = directory.path().join("report.html");
    fs::write(&source, "# Source").unwrap();
    let guard = tools_lock(directory.path(), LockMode::Shared, &TestClock).unwrap();
    let fake = process(
        vec![],
        "<svg viewBox=\"0, 0 42.2,11\"><desc>A &amp; &lt;B&gt; &quot;C&quot; &apos;D&apos;</desc></svg>",
    );
    write_report(
        &source,
        &output,
        &skill_root(),
        "Report",
        &tools(),
        &guard,
        &fake,
        &TestClock,
    )
    .unwrap();
    assert!(fs::read_to_string(&output).unwrap().contains(
        "alt=\"A &amp; &lt;B&gt; &quot;C&quot; &#39;D&#39;\" width=\"43\" height=\"11\""
    ));
    for svg in [
        "<svg viewBox=\"0 0 1\"/>",
        "<svg viewBox=\"0 0 1 1 1\"/>",
        "<svg viewBox=\"0 0 0 1\"/>",
        "<svg viewBox=\"0 0 NaN 1\"/>",
    ] {
        let before = fs::read(&output).unwrap();
        let fake = process(vec![], svg);
        assert!(
            write_report(
                &source,
                &output,
                &skill_root(),
                "Report",
                &tools(),
                &guard,
                &fake,
                &TestClock
            )
            .is_err()
        );
        assert_eq!(fs::read(&output).unwrap(), before);
    }
}

/// Errors cannot replace a prior report or the canonical Markdown source.
#[test]
fn failures_preserve_existing_output_and_reject_input_aliases() {
    let directory = directory();
    let source = directory.path().join("source.md");
    let output = directory.path().join("report.html");
    fs::write(&source, "# Source").unwrap();
    fs::write(&output, "previous report").unwrap();
    let guard = tools_lock(directory.path(), LockMode::Shared, &TestClock).unwrap();
    let mut fake = process(vec![], "<svg/>");
    fake.fail_diagram = true;
    assert!(
        write_report(
            &source,
            &output,
            &skill_root(),
            "Report",
            &tools(),
            &guard,
            &fake,
            &TestClock
        )
        .is_err()
    );
    assert_eq!(fs::read_to_string(&output).unwrap(), "previous report");
    fake.fail_diagram = false;
    fake.html_override = Some("<!DOCTYPE html><script>bad()</script>".into());
    assert!(
        write_report(
            &source,
            &output,
            &skill_root(),
            "Report",
            &tools(),
            &guard,
            &fake,
            &TestClock
        )
        .is_err()
    );
    assert_eq!(fs::read_to_string(&output).unwrap(), "previous report");
    assert!(
        write_report(
            &source,
            &source,
            &skill_root(),
            "Report",
            &tools(),
            &guard,
            &fake,
            &TestClock
        )
        .is_err()
    );
    assert_eq!(fs::read_to_string(source).unwrap(), "# Source");
}

/// A hard-linked output cannot replace the canonical Markdown source.
#[test]
fn rejects_hard_linked_output_before_rendering() {
    let directory = directory();
    let source = directory.path().join("source.md");
    let alias = directory.path().join("hard-link.html");
    fs::write(&source, "# Source").unwrap();
    fs::hard_link(&source, &alias).unwrap();
    let guard = tools_lock(directory.path(), LockMode::Shared, &TestClock).unwrap();
    let fake = process(vec![], "<svg/>");

    let error = write_report(
        &source,
        &alias,
        &skill_root(),
        "Report",
        &tools(),
        &guard,
        &fake,
        &TestClock,
    )
    .unwrap_err();
    assert!(error.to_string().contains("must not replace"));
    assert!(fake.observed_ast.borrow().is_none());
    assert_eq!(fs::read_to_string(&source).unwrap(), "# Source");
    assert_eq!(fs::read_to_string(alias).unwrap(), "# Source");
}

/// Static images and links cannot introduce active or external content.
#[test]
fn rejects_unsafe_svg_links_and_images_before_publication() {
    let directory = directory();
    let source = directory.path().join("source.md");
    let output = directory.path().join("report.html");
    fs::write(&source, "# Source").unwrap();
    let guard = tools_lock(directory.path(), LockMode::Shared, &TestClock).unwrap();
    for svg in [
        "<svg><script/></svg>",
        "<svg><image href=\"https://example.com/a\"/></svg>",
        "<svg onload=\"alert(1)\"/>",
        "<svg><style>path { fill: url(https://example.com/x); }</style></svg>",
        "<svg><style>@import 'https://example.com/x';</style></svg>",
        "<svg><style>path { fill: u\\72l(https://example.com/x); }</style></svg>",
        "<svg><style>path { fill: url(&#104;ttps://example.com/x); }</style></svg>",
        "<svg><animate attributeName=\"href\" values=\"https://example.com/x\"/></svg>",
        "<svg xml:base=\"https://example.com/\"><use href=\"#x\"/></svg>",
        "<!DOCTYPE svg [<!ENTITY x 'test'>]><svg>&x;</svg>",
        "<svg viewBox=\"0 0 NaN 2\"/>",
    ] {
        let fake = process(vec![], svg);
        assert!(
            write_report(
                &source,
                &output,
                &skill_root(),
                "Report",
                &tools(),
                &guard,
                &fake,
                &TestClock
            )
            .is_err(),
            "{svg}"
        );
        assert!(!output.exists());
    }
    for node in [
        json!({"t":"Image","c":[["",[],[]],[],["https://example.com/image",""]]}),
        json!({"t":"Link","c":[["",[],[]],[],["javascript:alert(1)",""]]}),
    ] {
        let fake = process(vec![node], "<svg/>");
        assert!(
            write_report(
                &source,
                &output,
                &skill_root(),
                "Report",
                &tools(),
                &guard,
                &fake,
                &TestClock
            )
            .is_err()
        );
        assert!(!output.exists());
    }
}

/// Diagram configuration and external resources are refused before rendering.
#[test]
fn rejects_mermaid_directives_and_resources_before_rendering() {
    let directory = directory();
    let source = directory.path().join("source.md");
    let output = directory.path().join("report.html");
    fs::write(&source, "# Source").unwrap();
    let guard = tools_lock(directory.path(), LockMode::Shared, &TestClock).unwrap();
    for diagram in [
        "%%{init: {}}%%\ngraph LR",
        "---\ntitle: config\n---\ngraph LR",
        "graph LR\n A[https://example.com]",
        "graph LR\n A[<img src=x>]",
        "graph LR\n A@{ img: x }",
    ] {
        let mut fake = process(vec![], "<svg/>");
        fake.ast["blocks"][0]["c"][1] = json!(diagram);
        assert!(
            write_report(
                &source,
                &output,
                &skill_root(),
                "Report",
                &tools(),
                &guard,
                &fake,
                &TestClock
            )
            .is_err()
        );
        assert!(!output.exists());
    }
}

/// Readiness exercises the same renderer while the installation lock is held.
#[test]
fn readiness_uses_the_same_renderer_and_held_guard() {
    let directory = directory();
    let guard = tools_lock(directory.path(), LockMode::Exclusive, &TestClock).unwrap();
    let fake = process(vec![], "<svg/>");
    probe(
        directory.path(),
        &skill_root(),
        &tools(),
        &guard,
        &fake,
        &TestClock,
    )
    .unwrap();
    assert!(fake.observed_ast.borrow().is_some());
}

/// Records Pandoc's actual input AST while forwarding the production invocation.
struct RecordingPandoc {
    process: NativeProcess,
    ast: RefCell<Option<Value>>,
}

impl Process for RecordingPandoc {
    fn execute(
        &self,
        program: &str,
        arguments: &[String],
        input: &[u8],
    ) -> io::Result<ProcessOutput> {
        let output = self.process.execute(program, arguments, input)?;
        if output.success && arguments.iter().any(|arg| arg == "--to=json") {
            *self.ast.borrow_mut() = Some(serde_json::from_slice(&output.stdout)?);
        }
        Ok(output)
    }
}

/// Real Pandoc raw nodes remain visible text, never executable report markup.
#[test]
#[cfg_attr(miri, ignore = "Pandoc executes as a separate process")]
fn real_pandoc_preserves_raw_inline_and_block_source() {
    // Keep the normal suite independent of a system Pandoc installation. An
    // explicitly supplied executable enables this subprocess contract test.
    let Some(pandoc) = std::env::var_os("SS_AI_TEST_PANDOC") else {
        return;
    };
    let directory = directory();
    let source = directory.path().join("source.md");
    let output = directory.path().join("report.html");
    fs::write(
        &source,
        "Before <b>inline</b> after.\n\n<div class=\"example\">\nBlock <strong>text</strong>\n</div>\n\n<!-- comment -->\n",
    )
    .unwrap();
    let process = RecordingPandoc {
        process: NativeProcess::new(directory.path()),
        ast: RefCell::new(None),
    };
    let tools = ReportTools::new(pandoc.into(), None, "mermaid".into(), None, vec![], vec![]);
    let guard = tools_lock(directory.path(), LockMode::Shared, &TestClock).unwrap();
    write_report(
        &source,
        &output,
        &skill_root(),
        "Report",
        &tools,
        &guard,
        &process,
        &TestClock,
    )
    .unwrap();
    let ast = process.ast.borrow();
    let ast = ast.as_ref().unwrap();
    assert_eq!(ast["blocks"][0]["c"][2]["t"], "RawInline");
    assert_eq!(ast["blocks"][1]["t"], "RawBlock");
    assert_eq!(ast["blocks"][2]["t"], "RawBlock");
    let html = fs::read_to_string(output).unwrap();
    assert!(html.contains("Before &lt;b&gt;inline&lt;/b&gt; after."));
    assert!(html.contains("&lt;div class=&quot;example&quot;&gt;"));
    assert!(html.contains("Block &lt;strong&gt;text&lt;/strong&gt;"));
    assert!(html.contains("&lt;!-- comment --&gt;"));
    assert!(!html.contains("<div class=\"example\">"));
    assert!(!html.contains("<strong>text</strong>"));
}
