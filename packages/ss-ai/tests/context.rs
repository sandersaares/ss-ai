//! Real-process compatibility between native and PowerShell context readers.
#![allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "The fixtures and their page counts are bounded"
)]

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Output, Stdio};
use std::time::Duration;

use jiff::Timestamp;
use ohno::AppError;
use serde_json::{Value, json};
use ss_ai_review_context::{increment_diagnostic, normalize_options, query};
use ss_ai_runtime::Clock;
use uuid::Uuid;

/// Isolates every process transaction while keeping files in the checkout.
struct Fixture {
    root: PathBuf,
    capture: PathBuf,
    request: PathBuf,
    diagnostics: PathBuf,
}

impl Fixture {
    /// Copies the immutable shared contract into an independent working directory.
    fn new() -> Self {
        let root = repository().join(format!("target/context-test-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let capture = root.join("capture space é.json");
        fs::copy(
            repository().join("tests/fixtures/ss-review-query-capture.json"),
            &capture,
        )
        .unwrap();
        Self {
            request: root.join("request.json"),
            diagnostics: root.join("input-diagnostics.json"),
            root,
            capture,
        }
    }

    /// Returns a path suitable for separate process arguments.
    fn path(path: &Path) -> &str {
        path.to_str().unwrap()
    }

    /// Invokes the compiled native executable without a provider.
    fn native(&self, arguments: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_ss-ai"))
            .args(["context"])
            .args(arguments)
            .current_dir(&self.root)
            .output()
            .unwrap()
    }

    /// Invokes the existing PowerShell executable as a separate process.
    fn powershell(&self, script: &str, arguments: &[&str]) -> Output {
        let scripts = repository().join("skills/ss-review/scripts");
        let mut command = Command::new("pwsh");
        command.args(["-NoProfile", "-NonInteractive"]);
        if script == "Set-CommentAuthorship.ps1" {
            command
                .args(["-File"])
                .arg(scripts.join(script))
                .args(arguments);
        } else {
            // The executable adapters now forward to native code; invoke the
            // retained PowerShell model directly to test a real language handoff.
            command
                .args(["-Command", Self::POWERSHELL_MODEL])
                .env("CONTEXT_LIB", scripts.join("lib"))
                .env("CONTEXT_OPERATION", script)
                .env(
                    "CONTEXT_ARGUMENTS",
                    serde_json::to_string(arguments).unwrap(),
                );
        }
        command.current_dir(&self.root).output().unwrap()
    }

    /// Runs the retained PowerShell producer and consumer in a separate process.
    const POWERSHELL_MODEL: &str = "#Requires -Version 7.6

    Set-StrictMode -Version Latest
    $ErrorActionPreference = 'Stop'
    $PSNativeCommandUseErrorActionPreference = $true
    if ($PSVersionTable.PSVersion -lt [version]'7.6') { throw 'PowerShell 7.6 is required' }
    . (Join-Path $env:CONTEXT_LIB 'JsonValue.ps1')
    . (Join-Path $env:CONTEXT_LIB 'PullRequestModel.ps1')
    . (Join-Path $env:CONTEXT_LIB 'PullRequestContextQuery.ps1')
    . (Join-Path $env:CONTEXT_LIB 'WorkflowData.ps1')
    . (Join-Path $env:CONTEXT_LIB 'ReviewInput.ps1')
    . (Join-Path $env:CONTEXT_LIB 'PullRequestQueryRequest.ps1')
    try {
        $argsList = @(ConvertFrom-Json -InputObject $env:CONTEXT_ARGUMENTS)
        $values = @{}
        for ($i = 0; $i -lt $argsList.Count; $i++) {
            $name = $argsList[$i].TrimStart('-')
            if ($name -in @('Next', 'Restart')) { $values[$name] = $true }
            else { $i++; $values[$name] = $argsList[$i] }
        }
        [Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
        switch ($env:CONTEXT_OPERATION) {
            'Read-PullRequestContext.ps1' {
                if ($values.ContainsKey('RequestPath')) {
                    $result = Read-SavedPullRequestQuery -RequestPath $values.RequestPath `
                        -Next:($values.ContainsKey('Next')) -Restart:($values.ContainsKey('Restart'))
                } else {
                    $selectors = @{ Query = $values.Query }
                    foreach ($name in @('Field', 'ThreadId', 'CommentId')) {
                        if ($values.ContainsKey($name)) { $selectors[$name] = $values[$name] }
                    }
                    if ($values.ContainsKey('PageSize')) { $selectors.PageSize = [int]$values.PageSize }
                    $options = New-PullRequestQueryOptions @selectors
                    $document = Read-PullRequestContextDocument -Path $values.Path
                    $cursor = if ($values.ContainsKey('Cursor')) { $values.Cursor } else { '' }
                    $result = Read-PullRequestContextQuery -Document $document -Options $options -Cursor $cursor
                }
                [Console]::WriteLine($result)
            }
            'New-PullRequestQueryRequest.ps1' {
                Add-ReviewInputDiagnostic -Path $values.DiagnosticsPath -Name setupCalls
                $selectors = @{ Query = $values.Query }
                foreach ($name in @('Field', 'ThreadId', 'CommentId')) {
                    if ($values.ContainsKey($name)) { $selectors[$name] = $values[$name] }
                }
                if ($values.ContainsKey('PageSize')) { $selectors.PageSize = [int]$values.PageSize }
                $options = New-PullRequestQueryOptions @selectors
                $request = New-PullRequestQueryRequest -Path $values.Path -Options $options -DiagnosticsPath $values.DiagnosticsPath
                Write-PullRequestOutputFileAtomically -Path $values.RequestPath -Content (ConvertTo-JsonText -Value $request)
                [Console]::WriteLine((ConvertTo-Json -Compress -InputObject @{ requestId = $request.requestId; snapshotId = $request.snapshotId }))
            }
            'Get-ReviewInputDiagnostics.ps1' {
                $record = Read-WorkflowData -Kind inputDiagnostics -Path $values.Path
                [Console]::WriteLine((ConvertTo-Json -InputObject $record -Depth 10 -Compress))
            }
            default { throw 'Unsupported PowerShell model operation' }
        }
    } catch {
        [Console]::Error.WriteLine($_.Exception.Message)
        exit 1
    }
    ";

    /// Writes a capture whose escaped content requires continuation.
    fn long_capture(&self) -> String {
        let mut capture: Value = serde_json::from_slice(&fs::read(&self.capture).unwrap()).unwrap();
        // Mixed-width characters exercise UTF-16 offsets and the encoded byte budget.
        let text = "🤖\"é\n".repeat(6000);
        capture["pullRequest"]["description"] = json!(text);
        fs::write(&self.capture, serde_json::to_vec(&capture).unwrap()).unwrap();
        text
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

/// Locates the active checkout without relying on the caller's working directory.
fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_owned()
}

/// Checks the process boundary, including the newline in the output budget.
fn page(output: Output) -> Value {
    let Output {
        status,
        stdout,
        stderr,
    } = output;
    assert!(
        status.success(),
        "process failed: {}",
        String::from_utf8_lossy(&stderr)
    );
    assert!(stderr.is_empty());
    assert!(stdout.len() <= 16_384);
    assert!(stdout.ends_with(b"\n"));
    serde_json::from_slice(&stdout).unwrap()
}

/// Ensures a rejected request neither leaks stdout nor mutates persisted data.
fn rejected(output: Output) {
    let Output {
        status,
        stdout,
        stderr,
    } = output;
    assert!(!status.success());
    assert!(stdout.is_empty());
    assert!(!stderr.is_empty());
}

/// Coordinates a real PowerShell diagnostic update with the native retry.
struct DiagnosticWriter {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
}

impl DiagnosticWriter {
    /// Holds the PowerShell diagnostic transaction just before publication.
    fn start(fixture: &Fixture) -> Self {
        let program = "#Requires -Version 7.6
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
if ($PSVersionTable.PSVersion -lt [version]'7.6') { throw 'PowerShell 7.6 is required' }
. (Join-Path $env:CONTEXT_LIB 'JsonValue.ps1')
. (Join-Path $env:CONTEXT_LIB 'PullRequestModel.ps1')
. (Join-Path $env:CONTEXT_LIB 'WorkflowData.ps1')
. (Join-Path $env:CONTEXT_LIB 'ReviewInput.ps1')
$script:publish = (Get-Command Write-PullRequestOutputFileAtomically).ScriptBlock
function Write-PullRequestOutputFileAtomically {
    param([string] $Path, [string] $Content)
    [Console]::WriteLine('held')
    if ([Console]::ReadLine() -cne 'release') { throw 'Expected release signal' }
    & $script:publish -Path $Path -Content $Content
}
Add-ReviewInputDiagnostic -Path $env:CONTEXT_DIAGNOSTICS -Name setupCalls
[Console]::WriteLine('released')
";
        let mut child = Command::new("pwsh")
            .args(["-NoProfile", "-NonInteractive", "-Command", program])
            .env(
                "CONTEXT_LIB",
                repository().join("skills/ss-review/scripts/lib"),
            )
            .env("CONTEXT_DIAGNOSTICS", &fixture.diagnostics)
            .current_dir(&fixture.root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let output = BufReader::new(child.stdout.take().unwrap());
        Self {
            child,
            input,
            output,
        }
    }

    /// Waits for an explicit protocol message instead of measuring time.
    fn expect(&mut self, expected: &str) {
        let mut line = String::new();
        assert_ne!(
            self.output.read_line(&mut line).unwrap(),
            0,
            "writer exited"
        );
        assert_eq!(line.trim_end(), expected);
    }

    /// Permits publication after the native operation encounters contention.
    fn release(&mut self) {
        writeln!(self.input, "release").unwrap();
        self.input.flush().unwrap();
        self.expect("released");
    }
}

impl Drop for DiagnosticWriter {
    fn drop(&mut self) {
        // A failed assertion must not leave the handshake process running.
        if let Err(_error) = self.child.kill() {}
        if let Err(_error) = self.child.wait() {}
    }
}

/// Advances the PowerShell writer only after the native lock reports contention.
struct DiagnosticClock<'a> {
    writer: RefCell<&'a mut DiagnosticWriter>,
    waits: Cell<u32>,
}

impl Clock for DiagnosticClock<'_> {
    fn now(&self) -> Timestamp {
        panic!("diagnostic transactions do not read wall-clock time")
    }

    fn wait(&self, _duration: Duration) -> Result<(), AppError> {
        assert_eq!(self.waits.get(), 0, "unexpected repeated contention");
        self.waits.set(1);
        self.writer.borrow_mut().release();
        Ok(())
    }
}

#[test]
#[cfg_attr(miri, ignore = "requires a PowerShell process and real file locks")]
#[cfg_attr(
    mutants,
    ignore = "the handshake requires both participants to progress"
)]
fn concurrent_powershell_and_native_diagnostic_updates_are_not_lost() {
    let fixture = Fixture::new();
    page(fixture.native(&[
        "diagnostics",
        "--path",
        Fixture::path(&fixture.diagnostics),
        "--record",
        "setupCalls",
    ]));
    let mut writer = DiagnosticWriter::start(&fixture);
    writer.expect("held");
    let clock = DiagnosticClock {
        writer: RefCell::new(&mut writer),
        waits: Cell::new(0),
    };
    increment_diagnostic(&fixture.diagnostics, "setupCalls", NonZeroU32::MIN, &clock).unwrap();
    assert_eq!(clock.waits.get(), 1);
    assert!(writer.child.wait().unwrap().success());
    let result =
        page(fixture.native(&["diagnostics", "--path", Fixture::path(&fixture.diagnostics)]));
    assert_eq!(result["counts"]["setupCalls"], 3);
}

#[test]
#[cfg_attr(miri, ignore = "requires native and PowerShell subprocesses")]
fn direct_queries_match_powershell_and_canonical_fixture() {
    let fixture = Fixture::new();
    let expected: Value = serde_json::from_slice(
        &fs::read(repository().join("tests/fixtures/ss-review-query-expected.json")).unwrap(),
    )
    .unwrap();
    let before = fs::read(&fixture.capture).unwrap();
    for (query, native, powershell) in [
        (
            "metadata",
            vec!["--field", "title"],
            vec!["-Field", "title"],
        ),
        (
            "comments",
            vec!["--thread-id", "T", "--comment-id", "C"],
            vec!["-ThreadId", "T", "-CommentId", "C"],
        ),
    ] {
        let mut args = vec![
            "read",
            "--path",
            Fixture::path(&fixture.capture),
            "--query",
            query,
        ];
        args.extend(native);
        let native_output = fixture.native(&args);
        let mut args = vec!["-Path", Fixture::path(&fixture.capture), "-Query", query];
        args.extend(powershell);
        let powershell_output = fixture.powershell("Read-PullRequestContext.ps1", &args);
        assert_eq!(native_output.stdout, powershell_output.stdout);
        let native = page(native_output);
        let powershell = page(powershell_output);
        assert_eq!(native, expected[query]);
        assert_eq!(native, powershell);
    }
    assert_eq!(fs::read(&fixture.capture).unwrap(), before);
}

#[test]
#[cfg_attr(miri, ignore = "requires native and PowerShell subprocesses")]
fn direct_pagination_preserves_utf16_offsets_across_languages() {
    let fixture = Fixture::new();
    let text = fixture.long_capture();
    let before = fs::read(&fixture.capture).unwrap();
    let mut cursor = String::new();
    let mut seen = HashSet::new();
    let mut content = String::new();
    // The chosen fixture needs only a few pages; the bound catches stalled pagination.
    for _ in 0..64 {
        let mut native = vec![
            "read",
            "--path",
            Fixture::path(&fixture.capture),
            "--query",
            "metadata",
            "--field",
            "description",
            "--page-size",
            "1",
        ];
        let mut powershell = vec![
            "-Path",
            Fixture::path(&fixture.capture),
            "-Query",
            "metadata",
            "-Field",
            "description",
            "-PageSize",
            "1",
        ];
        if !cursor.is_empty() {
            native.extend(["--cursor", &cursor]);
            powershell.extend(["-Cursor", &cursor]);
        }
        let native = page(fixture.native(&native));
        let powershell = page(fixture.powershell("Read-PullRequestContext.ps1", &powershell));
        assert_eq!(native, powershell);
        let items = native["items"].as_array().unwrap();
        assert!(!items.is_empty());
        for item in items {
            assert_eq!(
                usize::try_from(item["contentOffset"].as_u64().unwrap()).unwrap(),
                content.encode_utf16().count()
            );
            assert_eq!(
                usize::try_from(item["contentLength"].as_u64().unwrap()).unwrap(),
                text.encode_utf16().count()
            );
            content.push_str(item["content"].as_str().unwrap());
        }
        if native["complete"].as_bool().unwrap() {
            assert!(native["nextCursor"].is_null());
            assert_eq!(content, text);
            assert_eq!(fs::read(&fixture.capture).unwrap(), before);
            return;
        }
        cursor = native["nextCursor"].as_str().unwrap().to_owned();
        assert!(seen.insert(cursor.clone()), "cursor must advance");
    }
    panic!("query did not complete within the fixture page bound");
}

#[test]
#[cfg_attr(miri, ignore = "requires native and PowerShell subprocesses")]
fn saved_requests_cross_both_process_boundaries_and_restart_explicitly() {
    for native_producer in [false, true] {
        let fixture = Fixture::new();
        fixture.long_capture();
        let capture = Fixture::path(&fixture.capture);
        let request = Fixture::path(&fixture.request);
        let diagnostics = Fixture::path(&fixture.diagnostics);
        if native_producer {
            page(fixture.native(&[
                "prepare",
                "--path",
                capture,
                "--request-path",
                request,
                "--diagnostics-path",
                diagnostics,
                "--query",
                "metadata",
                "--field",
                "description",
                "--page-size",
                "1",
            ]));
        } else {
            page(fixture.powershell(
                "New-PullRequestQueryRequest.ps1",
                &[
                    "-Path",
                    capture,
                    "-RequestPath",
                    request,
                    "-DiagnosticsPath",
                    diagnostics,
                    "-Query",
                    "metadata",
                    "-Field",
                    "description",
                    "-PageSize",
                    "1",
                ],
            ));
        }
        let native_read = || fixture.native(&["read-saved", "--request-path", request]);
        let ps_read =
            || fixture.powershell("Read-PullRequestContext.ps1", &["-RequestPath", request]);
        let first = if native_producer {
            page(ps_read())
        } else {
            page(native_read())
        };
        assert_eq!(page(native_read()), first);
        assert_eq!(page(ps_read()), first);
        assert!(!first["complete"].as_bool().unwrap());
        let second = if native_producer {
            page(fixture.native(&["read-saved", "--request-path", request, "--next"]))
        } else {
            page(fixture.powershell(
                "Read-PullRequestContext.ps1",
                &["-RequestPath", request, "-Next"],
            ))
        };
        assert_ne!(first, second);
        assert_eq!(page(native_read()), second);
        assert_eq!(page(ps_read()), second);

        let mut capture_data: Value =
            serde_json::from_slice(&fs::read(&fixture.capture).unwrap()).unwrap();
        capture_data["pullRequest"]["title"] = json!("changed");
        fs::write(&fixture.capture, serde_json::to_vec(&capture_data).unwrap()).unwrap();
        let saved = fs::read(&fixture.request).unwrap();
        rejected(native_read());
        rejected(ps_read());
        assert_eq!(fs::read(&fixture.request).unwrap(), saved);
        let restarted = if native_producer {
            page(fixture.powershell(
                "Read-PullRequestContext.ps1",
                &["-RequestPath", request, "-Restart"],
            ))
        } else {
            page(fixture.native(&["read-saved", "--request-path", request, "--restart"]))
        };
        assert_ne!(restarted["snapshotId"], first["snapshotId"]);
        assert_eq!(page(native_read()), restarted);
        assert_eq!(page(ps_read()), restarted);
        let sidecar = page(fixture.native(&["diagnostics", "--path", diagnostics]));
        assert_eq!(sidecar["counts"]["setupCalls"], 1);
        assert_eq!(sidecar["counts"]["cursorRestarts"], 1);
        assert_eq!(
            page(fixture.powershell("Get-ReviewInputDiagnostics.ps1", &["-Path", diagnostics])),
            sidecar
        );
    }
}

#[test]
#[cfg_attr(miri, ignore = "requires native and PowerShell subprocesses")]
fn malformed_saved_requests_are_rejected_without_mutation() {
    for missing in [true, false] {
        let fixture = Fixture::new();
        let capture = Fixture::path(&fixture.capture);
        let request = Fixture::path(&fixture.request);
        let diagnostics = Fixture::path(&fixture.diagnostics);
        page(fixture.powershell(
            "New-PullRequestQueryRequest.ps1",
            &[
                "-Path",
                capture,
                "-RequestPath",
                request,
                "-DiagnosticsPath",
                diagnostics,
                "-Query",
                "metadata",
                "-Field",
                "title",
            ],
        ));
        let mut record: Value =
            serde_json::from_slice(&fs::read(&fixture.request).unwrap()).unwrap();
        if missing {
            record["options"].as_object_mut().unwrap().remove("files");
        } else {
            record["options"]["unknown"] = json!("unexpected");
        }

        fs::write(&fixture.request, serde_json::to_vec(&record).unwrap()).unwrap();
        let saved = fs::read(&fixture.request).unwrap();
        let captured = fs::read(&fixture.capture).unwrap();
        let sidecar = fs::read(&fixture.diagnostics).unwrap();
        rejected(fixture.native(&["read-saved", "--request-path", request]));
        rejected(fixture.powershell("Read-PullRequestContext.ps1", &["-RequestPath", request]));
        assert_eq!(fs::read(&fixture.request).unwrap(), saved);
        assert_eq!(fs::read(&fixture.capture).unwrap(), captured);
        assert_eq!(fs::read(&fixture.diagnostics).unwrap(), sidecar);
    }
}

#[test]
#[cfg_attr(miri, ignore = "requires a native subprocess")]
fn unicode_metadata_is_preserved_in_current_and_baseline() {
    let fixture = Fixture::new();
    let baseline = fixture.root.join("baseline.json");
    let mut capture: Value = serde_json::from_slice(&fs::read(&fixture.capture).unwrap()).unwrap();
    let author = json!({"displayName": "Élodie 東京", "地域": "München 🤖"});
    capture["pullRequest"]["author"] = author.clone();
    let original = serde_json::to_vec(&capture).unwrap();
    fs::write(&fixture.capture, &original).unwrap();
    fs::write(&baseline, &original).unwrap();
    let result = page(fixture.native(&[
        "read",
        "--path",
        Fixture::path(&fixture.capture),
        "--query",
        "metadata",
        "--field",
        "author",
    ]));
    assert_eq!(result["items"][0]["value"], author);
    let changes = page(fixture.native(&[
        "read",
        "--path",
        Fixture::path(&fixture.capture),
        "--query",
        "changes",
        "--compare-path",
        Fixture::path(&baseline),
    ]));
    assert_eq!(changes["totalCount"], 0);
    assert_eq!(fs::read(&fixture.capture).unwrap(), original);
    assert_eq!(fs::read(&baseline).unwrap(), original);
}

#[test]
#[cfg_attr(miri, ignore = "requires a native subprocess")]
fn duplicate_capture_keys_are_rejected_without_mutation() {
    let fixture = Fixture::new();
    let baseline = fixture.root.join("baseline.json");
    let original = fs::read_to_string(&fixture.capture).unwrap();
    let invalid = original.replacen('{', "{\"schemaVersion\":3,", 1);
    fs::write(&fixture.capture, &invalid).unwrap();
    rejected(fixture.native(&[
        "read",
        "--path",
        Fixture::path(&fixture.capture),
        "--query",
        "metadata",
        "--field",
        "title",
    ]));
    assert_eq!(fs::read_to_string(&fixture.capture).unwrap(), invalid);

    fs::write(&fixture.capture, &original).unwrap();
    fs::write(&baseline, &invalid).unwrap();
    rejected(fixture.native(&[
        "read",
        "--path",
        Fixture::path(&fixture.capture),
        "--query",
        "changes",
        "--compare-path",
        Fixture::path(&baseline),
    ]));
    assert_eq!(fs::read_to_string(&fixture.capture).unwrap(), original);
    assert_eq!(fs::read_to_string(&baseline).unwrap(), invalid);
}

#[test]
#[cfg_attr(miri, ignore = "requires a native subprocess")]
fn malformed_captures_fail_and_empty_discussions_remain_readable() {
    let fixture = Fixture::new();
    let mut capture: Value = serde_json::from_slice(&fs::read(&fixture.capture).unwrap()).unwrap();
    capture["threads"][0]["comments"][0]
        .as_object_mut()
        .unwrap()
        .remove("content");
    let malformed = serde_json::to_vec(&capture).unwrap();
    fs::write(&fixture.capture, &malformed).unwrap();
    rejected(fixture.native(&[
        "read",
        "--path",
        Fixture::path(&fixture.capture),
        "--query",
        "threads",
    ]));
    assert_eq!(fs::read(&fixture.capture).unwrap(), malformed);

    capture["threads"] = json!([]);
    fs::write(&fixture.capture, serde_json::to_vec(&capture).unwrap()).unwrap();
    let result = page(fixture.native(&[
        "read",
        "--path",
        Fixture::path(&fixture.capture),
        "--query",
        "threads",
    ]));
    assert_eq!(result["totalCount"], 0);
    assert_eq!(result["items"], json!([]));
    assert_eq!(result["complete"], true);
}

#[test]
fn unicode_simple_case_search_uses_literal_text_and_utf16_offsets() {
    let mut capture: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/ss-review-query-capture.json"
    ))
    .unwrap();
    // Pairs include non-ASCII, supplementary-plane and nontrivial simple folds.
    for (body, needle) in [
        ("É", "é"),
        ("Σ", "ς"),
        ("K", "k"),
        ("ẞ", "ß"),
        ("ſ", "S"),
        ("ᾀ", "ᾈ"),
        ("𐐀", "𐐨"),
    ] {
        let text = format!("a🤖{body} suffix");
        capture["pullRequest"]["description"] = json!(text);
        let options = normalize_options(&json!({"query":"search", "text":needle})).unwrap();
        let result: Value =
            serde_json::from_str(&query(&capture, &options, None, None).unwrap()).unwrap();
        assert_eq!(result["totalCount"], 1, "search for {needle:?} in {body:?}");
        let item = &result["items"][0];
        assert_eq!(item["field"], "description");
        assert_eq!(item["matchOffset"], 3);
        assert_eq!(item["excerptOffset"], 0);
        assert_eq!(item["excerpt"], text);
        assert_eq!(
            usize::try_from(item["contentLength"].as_u64().unwrap()).unwrap(),
            text.encode_utf16().count()
        );
    }

    capture["pullRequest"]["description"] = json!("a🤖Straße ﬃ é");
    for needle in ["STRASSE", "ffi", "e\u{301}", ".*"] {
        let options = normalize_options(&json!({"query":"search", "text":needle})).unwrap();
        let result: Value =
            serde_json::from_str(&query(&capture, &options, None, None).unwrap()).unwrap();
        assert_eq!(result["totalCount"], 0, "search for {needle:?}");
    }
}

#[test]
#[cfg_attr(miri, ignore = "requires a native subprocess")]
fn unicode_search_crosses_the_process_boundary() {
    let fixture = Fixture::new();
    let mut capture: Value = serde_json::from_slice(&fs::read(&fixture.capture).unwrap()).unwrap();
    capture["pullRequest"]["description"] = json!("a🤖É suffix");
    fs::write(&fixture.capture, serde_json::to_vec(&capture).unwrap()).unwrap();
    let result = page(fixture.native(&[
        "read",
        "--path",
        Fixture::path(&fixture.capture),
        "--query",
        "search",
        "--text",
        "é",
    ]));
    assert_eq!(result["totalCount"], 1);
    assert_eq!(result["items"][0]["matchOffset"], 3);
    assert_eq!(result["items"][0]["excerpt"], "a🤖É suffix");
}

#[test]
#[cfg_attr(miri, ignore = "requires native and PowerShell subprocesses")]
fn powershell_authorship_sidecar_invalidates_native_saved_snapshot() {
    let fixture = Fixture::new();
    let capture = Fixture::path(&fixture.capture);
    let request = Fixture::path(&fixture.request);
    let diagnostics = Fixture::path(&fixture.diagnostics);
    let prepared = page(fixture.native(&[
        "prepare",
        "--path",
        capture,
        "--request-path",
        request,
        "--diagnostics-path",
        diagnostics,
        "--query",
        "comments",
        "--thread-id",
        "T",
        "--comment-id",
        "C",
    ]));
    let snapshot = prepared["snapshotId"].as_str().unwrap();
    let raw = fs::read(&fixture.capture).unwrap();
    page(fixture.powershell(
        "Set-CommentAuthorship.ps1",
        &[
            "-Path",
            capture,
            "-SnapshotId",
            snapshot,
            "-Classification",
            r#"[{"threadId":"T","commentId":"C","kind":"human"}]"#,
        ],
    ));
    let sidecar = PathBuf::from(format!("{capture}.authorship.json"));
    let sidecar_before = fs::read(&sidecar).unwrap();
    let saved = fs::read(&fixture.request).unwrap();
    rejected(fixture.native(&["read-saved", "--request-path", request]));
    assert_eq!(fs::read(&fixture.request).unwrap(), saved);
    let native = page(fixture.native(&["read-saved", "--request-path", request, "--restart"]));
    let powershell =
        page(fixture.powershell("Read-PullRequestContext.ps1", &["-RequestPath", request]));
    assert_eq!(native, powershell);
    assert_eq!(native["items"][0]["authorship"], "human");
    assert_ne!(native["snapshotId"], snapshot);
    assert_eq!(fs::read(&fixture.capture).unwrap(), raw);
    assert_eq!(fs::read(&sidecar).unwrap(), sidecar_before);

    let mut invalid: Value = serde_json::from_slice(&sidecar_before).unwrap();
    invalid["schemaVersion"] = json!(99);
    fs::write(&sidecar, serde_json::to_vec(&invalid).unwrap()).unwrap();
    let malformed = fs::read(&sidecar).unwrap();
    rejected(fixture.native(&[
        "read",
        "--path",
        capture,
        "--query",
        "comments",
        "--thread-id",
        "T",
    ]));
    rejected(fixture.powershell(
        "Read-PullRequestContext.ps1",
        &["-Path", capture, "-Query", "comments", "-ThreadId", "T"],
    ));
    assert_eq!(fs::read(&sidecar).unwrap(), malformed);
    assert_eq!(fs::read(&fixture.capture).unwrap(), raw);
}
