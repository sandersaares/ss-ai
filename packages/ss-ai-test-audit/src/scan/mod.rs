//! Deterministic source inventory with parser-backed Rust analysis.

use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, Read as _};
use std::iter::{once, repeat_n};
use std::mem::{replace, take};
use std::num::NonZeroUsize;
use std::ops::ControlFlow::{self, Break, Continue};
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};
use std::slice::from_ref;

use ohno::AppError;
use proc_macro2::{Ident, LineColumn, Span};
use sha2::{Digest, Sha256};
use ss_ai_runtime::{caused_by, fail};
use syn::spanned::Spanned;
use syn::visit::Visit;
use syn::{
    Arm, Attribute, Block, Expr, ExprCall, ExprClosure, ExprConst, ExprMethodCall, ExprPath, Field,
    FnArg, ImplItem, ImplItemConst, ImplItemFn, Item, ItemConst, ItemFn, ItemImpl, ItemMod,
    ItemStatic, ItemUse, Local, Macro, Meta, Pat, PatIdent, Signature, Stmt, StmtMacro, TraitItem,
    TraitItemConst, Type, UseTree, Variant, Visibility, WherePredicate, punctuated::Punctuated,
    token::Comma, visit,
};

use crate::model::{
    AUDIT_STATE_SCHEMA_VERSION, AssertionSite, CallerEvidence, CandidateDisposition, CandidateRole,
    IndexEntry, IndexedFile, Inventory, InventoryCorrection, RevisionKind, ScanDiagnostic,
    TestCase,
};

/// Names every structural role emitted by the evidence index.
///
/// Contract profiles reuse this closed vocabulary so initialization can reject
/// labels that native citation provenance can never produce.
pub(crate) const SOURCE_ROLES: [&str; 8] = [
    "markdownDocumentation",
    "markdownExample",
    "rustExample",
    "rustItemDoc",
    "rustModuleDoc",
    "rustPublicExample",
    "rustTraitDoc",
    "sourceLine",
];

/// Prevents one generated or unexpected file from making an audit unbounded.
///
/// This cap accommodates ordinary source and generated documentation files
/// while preserving a predictable local-memory ceiling per file.
const MAX_FILE_BYTES: usize = 4 * 1024 * 1024;
/// Keeps search previews concise while the index retains exact range text.
const MAX_INDEX_EXCERPT_CHARACTERS: usize = 2_000;

/// Scans selected repository paths without modifying the working tree.
pub(crate) fn inventory(
    repository_root: &Path,
    repository_revision: &str,
    revision_kind: RevisionKind,
    scope_paths: &[String],
) -> Result<(Inventory, Vec<IndexEntry>), AppError> {
    let mut indexed_files = Vec::new();
    let mut tests = Vec::new();
    let mut assertions = Vec::new();
    let mut diagnostics = Vec::new();
    let mut entries = Vec::new();
    let evidence_files = collect_files(repository_root, &[".".to_owned()], &mut diagnostics)?;
    let mut scope_diagnostics = Vec::new();
    let assertion_files = collect_files(repository_root, scope_paths, &mut scope_diagnostics)?;
    for diagnostic in scope_diagnostics {
        if !diagnostics
            .iter()
            .any(|entry| entry.path == diagnostic.path && entry.kind == diagnostic.kind)
        {
            diagnostics.push(diagnostic);
        }
    }
    // An explicitly selected root overrides ordinary traversal exclusions.
    // Add only those selected files to the default evidence walk, so assertions
    // have provenance without exposing neighboring excluded source.
    let files = evidence_files.union(&assertion_files);
    for path in files {
        let relative = relative_path(repository_root, path)?;
        let bytes = match read_source(path) {
            Ok(Some(bytes)) => bytes,
            Ok(None) => {
                diagnostics.push(ScanDiagnostic {
                    kind: "oversizedFile".to_owned(),
                    path: relative,
                    line: None,
                    detail: "The file exceeds the scan safety limit and was not analyzed"
                        .to_owned(),
                });
                continue;
            }
            Err(error) => {
                diagnostics.push(ScanDiagnostic {
                    kind: "unreadableFile".to_owned(),
                    path: relative,
                    line: None,
                    detail: error.to_string(),
                });
                continue;
            }
        };
        if bytes.contains(&0) {
            diagnostics.push(ScanDiagnostic {
                kind: "binaryFile".to_owned(),
                path: relative,
                line: None,
                detail: "The file contains NUL bytes and cannot be indexed as text".to_owned(),
            });
            continue;
        }
        let text = match String::from_utf8(bytes) {
            Ok(text) => text,
            Err(error) => {
                diagnostics.push(ScanDiagnostic {
                    kind: "nonUtf8File".to_owned(),
                    path: relative,
                    line: None,
                    detail: error.to_string(),
                });
                continue;
            }
        };
        let language = language_for(path);
        let file_sha256 = digest(text.as_bytes());
        let lines: Vec<&str> = text.lines().collect();
        indexed_files.push(IndexedFile {
            path: relative.clone(),
            language: language.to_owned(),
            kind: file_kind(&relative),
            sha256: file_sha256.clone(),
            line_count: lines.len(),
            byte_count: text.len(),
        });
        append_index(
            &mut entries,
            &relative,
            language,
            repository_revision,
            &file_sha256,
            &lines,
        );
        if assertion_files.contains(path) {
            scan_file(
                &mut tests,
                &mut assertions,
                &mut diagnostics,
                ScanInput {
                    path: &relative,
                    language,
                    file_sha256: &file_sha256,
                    text: &text,
                    lines: &lines,
                },
            );
        }
    }

    let mut assertion_ids = BTreeSet::new();
    for assertion in &assertions {
        if !assertion_ids.insert(assertion.id.clone()) {
            return Err(fail(
                "invalidState",
                format!("Assertion ID collision at {}", assertion.id),
            ));
        }
    }
    // Exclusion happens after source-site IDs have been assigned, so removing
    // one proven fixture check cannot renumber later assertions.
    let corrections = assertions
        .iter()
        .filter(|assertion| assertion.fixture_excluded)
        .map(|assertion| InventoryCorrection {
            assertion_id: assertion.id.clone(),
            path: assertion.path.clone(),
            line: assertion.line,
            end_line: assertion.end_line,
            column: assertion.column,
            expression: assertion.expression.clone(),
            file_sha256: assertion.file_sha256.clone(),
            candidate_role: CandidateRole::FixtureSetup,
            candidate_disposition: CandidateDisposition::Excluded,
            reason: assertion
                .fixture_exclusion_reason
                .clone()
                .unwrap_or_else(|| {
                    "The source extracts test-owned fixture state rather than a fallible audited API value"
                        .to_owned()
                }),
        })
        .collect::<Vec<_>>();
    diagnostics.extend(corrections.iter().map(|correction| ScanDiagnostic {
        kind: "fixtureAccessExcluded".to_owned(),
        path: correction.path.clone(),
        line: Some(correction.line.get()),
        detail: format!(
            "Assertion '{}' was excluded from this audit: {}",
            correction.assertion_id, correction.reason
        ),
    }));
    assertions.retain(|assertion| !assertion.fixture_excluded);
    resolve_imported_callers(repository_root, &tests, &mut assertions)?;
    attach_assertions(&mut tests, &mut assertions);
    refresh_cross_checks(&mut diagnostics, &assertions);
    verify_indexed_files(repository_root, &indexed_files)?;

    let inventory = Inventory {
        schema_version: AUDIT_STATE_SCHEMA_VERSION,
        audit_id: String::new(),
        repository_root: repository_root
            .to_str()
            .ok_or_else(|| fail("invalidInput", "Repository path must be UTF-8"))?
            .to_owned(),
        repository_revision: repository_revision.to_owned(),
        revision_kind,
        scope_paths: scope_paths.to_vec(),
        tool_version: String::new(),
        rubric_version: String::new(),
        content_sha256: String::new(),
        contract_profile: None,
        files: indexed_files,
        tests,
        assertions,
        diagnostics,
        corrections,
    };
    Ok((inventory, entries))
}

/// Reads at most one byte past the cap so a growing file cannot bypass it.
fn read_source(path: &Path) -> io::Result<Option<Vec<u8>>> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take(MAX_FILE_BYTES.saturating_add(1) as u64)
        .read_to_end(&mut bytes)?;
    Ok((bytes.len() <= MAX_FILE_BYTES).then_some(bytes))
}

/// Computes a stable hash used for source and snapshot identity.
pub(crate) fn digest(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

/// Expands repository-relative or absolute scopes into an ordered unique file set.
fn collect_files(
    repository_root: &Path,
    scopes: &[String],
    diagnostics: &mut Vec<ScanDiagnostic>,
) -> Result<BTreeSet<PathBuf>, AppError> {
    let mut paths = BTreeSet::new();
    for scope in scopes {
        let requested = Path::new(scope);
        let requested = if requested.is_absolute() {
            requested.to_path_buf()
        } else {
            repository_root.join(requested)
        };
        let canonical = fs::canonicalize(&requested).map_err(|error| {
            caused_by(
                "invalidInput",
                format!("Cannot access scope '{}'", requested.display()),
                error,
            )
        })?;
        if !canonical.starts_with(repository_root) {
            return Err(fail(
                "invalidInput",
                format!("Scope '{}' is outside the repository", requested.display()),
            ));
        }
        if canonical
            .strip_prefix(repository_root)
            .is_ok_and(|relative| relative.iter().any(|component| component == ".git"))
        {
            return Err(fail(
                "invalidInput",
                "Git administrative files cannot be audited",
            ));
        }
        collect_path(&canonical, repository_root, true, &mut paths, diagnostics)?;
    }
    Ok(paths)
}

/// Walks a scope without following symbolic links outside or inside the repository.
fn collect_path(
    path: &Path,
    repository_root: &Path,
    is_scope_root: bool,
    paths: &mut BTreeSet<PathBuf>,
    diagnostics: &mut Vec<ScanDiagnostic>,
) -> Result<(), AppError> {
    // Worktrees and submodules can have a Git administrative file instead of a directory.
    if path != repository_root && path.file_name() == Some(OsStr::new(".git")) {
        return Ok(());
    }
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        diagnostics.push(ScanDiagnostic {
            kind: "symbolicLinkSkipped".to_owned(),
            path: relative_path(repository_root, path)?,
            line: None,
            detail: "Symbolic links are not followed during source inventory".to_owned(),
        });
        return Ok(());
    }
    if metadata.is_file() {
        paths.insert(path.to_owned());
        return Ok(());
    }
    if !metadata.is_dir() || (!is_scope_root && should_skip_directory(path, repository_root)?) {
        return Ok(());
    }
    let mut children = fs::read_dir(path)?.collect::<Result<Vec<_>, _>>()?;
    children.sort_by_key(fs::DirEntry::file_name);
    for entry in children {
        collect_path(&entry.path(), repository_root, false, paths, diagnostics)?;
    }
    Ok(())
}

/// Excludes reserved directories and build outputs identified by their project layout.
fn should_skip_directory(path: &Path, root: &Path) -> io::Result<bool> {
    if path == root {
        return Ok(false);
    }
    let name = path.file_name().and_then(OsStr::to_str).unwrap_or_default();
    if matches!(
        name,
        ".hg" | ".svn" | "node_modules" | ".venv" | ".idea" | ".vs" | ".ss-ai-test-audit"
    ) {
        return Ok(true);
    }
    let Some(parent) = path.parent() else {
        return Ok(false);
    };
    match name {
        "target" => parent.join("Cargo.toml").try_exists(),
        "venv" => path.join("pyvenv.cfg").try_exists(),
        // Cargo reserves `src/bin` for source targets, even in a mixed-language project.
        "bin" if parent.file_name() == Some(OsStr::new("src")) => Ok(false),
        "bin" | "obj" => {
            for entry in fs::read_dir(parent)? {
                let entry = entry?;
                if matches!(
                    entry.path().extension().and_then(OsStr::to_str),
                    Some("csproj" | "fsproj" | "vbproj")
                ) && entry.file_type()?.is_file()
                {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        _ => Ok(false),
    }
}

/// Produces stable slash-separated paths for reports on every platform.
fn relative_path(repository_root: &Path, path: &Path) -> Result<String, AppError> {
    let relative = path.strip_prefix(repository_root).map_err(|_error| {
        fail(
            "invalidState",
            format!("Scanned path '{}' left the repository", path.display()),
        )
    })?;
    Ok(relative.to_string_lossy().replace('\\', "/"))
}

/// Assigns the parser or heuristic profile used for a text file.
fn language_for(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(OsStr::to_str)
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "rs" => "rust",
        "md" | "mdx" => "markdown",
        "cs" => "csharp",
        "fs" => "fsharp",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "ts" | "tsx" => "typescript",
        "py" => "python",
        "go" => "go",
        "java" => "java",
        "kt" | "kts" => "kotlin",
        "toml" => "toml",
        "yaml" | "yml" => "yaml",
        "json" => "json",
        "ps1" | "psm1" | "psd1" => "powershell",
        "sh" | "bash" => "shell",
        "c" | "h" => "c",
        "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" => "cpp",
        _ => "text",
    }
}

/// Summarizes why a file is likely useful as source, test, or evidence.
fn file_kind(path: &str) -> String {
    let lower = path.to_ascii_lowercase();
    if lower.starts_with("docs/")
        || lower.contains("/docs/")
        || lower.starts_with("readme")
        || lower.starts_with("changelog")
    {
        "documentation".to_owned()
    } else if lower.contains("/test/")
        || lower.contains("/tests/")
        || lower.starts_with("test/")
        || lower.starts_with("tests/")
        || lower.contains("_test.")
        || lower.ends_with(".tests.rs")
    {
        "test".to_owned()
    } else {
        "source".to_owned()
    }
}

/// Adds one provenance-bearing searchable entry per nonempty source line.
fn append_index(
    entries: &mut Vec<IndexEntry>,
    path: &str,
    language: &str,
    repository_revision: &str,
    file_sha256: &str,
    lines: &[&str],
) {
    let ranges = evidence_ranges(lines, language);
    for range in ranges {
        let Some(excerpt) = range.excerpt(lines) else {
            continue;
        };
        if !has_meaningful_excerpt(&excerpt) {
            continue;
        }
        let text = excerpt.trim().to_owned();
        let (preview, preview_truncated) = preview_text(&text, MAX_INDEX_EXCERPT_CHARACTERS);
        entries.push(IndexEntry {
            path: path.to_owned(),
            line: parser_line(range.start),
            end_line: parser_line(range.end),
            language: language.to_owned(),
            kind: index_kind(path, &excerpt),
            owner: range.owner,
            source_role: range.source_role,
            text,
            excerpt: preview,
            preview_truncated,
            repository_revision: repository_revision.to_owned(),
            file_sha256: file_sha256.to_owned(),
        });
    }
}

/// Bounds only presentation while retaining exact text for matching and citations.
fn preview_text(text: &str, max: usize) -> (String, bool) {
    let mut characters = text.chars();
    let preview = characters.by_ref().take(max).collect::<String>();
    let truncated = characters.next().is_some();
    (
        if truncated {
            format!("{preview}...")
        } else {
            preview
        },
        truncated,
    )
}

/// Bounds one complete paragraph, list item, doc block, or source line.
struct EvidenceRange {
    start: usize,
    end: usize,
    owner: String,
    source_role: String,
}

impl EvidenceRange {
    /// Joins its exact source lines before the bounded index projection.
    fn excerpt(&self, lines: &[&str]) -> Option<String> {
        let slice = lines.get(self.start.saturating_sub(1)..self.end)?;
        Some(slice.join("\n"))
    }
}

/// Groups documentation without broadening ordinary source lines.
fn evidence_ranges(lines: &[&str], language: &str) -> Vec<EvidenceRange> {
    let mut ranges = Vec::new();
    let mut current_heading = String::new();
    let mut index = 0;
    while index < lines.len() {
        let Some(text) = lines.get(index).copied() else {
            break;
        };
        let trimmed = text.trim_start();
        if text.trim().is_empty() {
            index = index.saturating_add(1);
            continue;
        }
        if language == "markdown" {
            if trimmed.starts_with('#') {
                current_heading.clear();
                current_heading.push_str(trimmed.trim_start_matches('#').trim());
            }
            if trimmed.starts_with("```") {
                let mut fence_end = index.saturating_add(1);
                while fence_end < lines.len() {
                    let Some(next) = lines.get(fence_end) else {
                        break;
                    };
                    fence_end = fence_end.saturating_add(1);
                    if next.trim_start().starts_with("```") {
                        break;
                    }
                }
                ranges.push(EvidenceRange {
                    start: index.saturating_add(1),
                    end: fence_end,
                    owner: current_heading.clone(),
                    source_role: "markdownExample".to_owned(),
                });
                index = fence_end;
                continue;
            }
            let end = lines
                .iter()
                .enumerate()
                .skip(index.saturating_add(1))
                .find(|(_, next)| next.trim().is_empty())
                .map_or(lines.len(), |(next, _)| next);
            ranges.push(EvidenceRange {
                start: index.saturating_add(1),
                end,
                owner: current_heading.clone(),
                source_role: "markdownDocumentation".to_owned(),
            });
            index = end.saturating_add(1);
            continue;
        }
        if trimmed.starts_with("///") || trimmed.starts_with("//!") {
            let end = lines
                .iter()
                .enumerate()
                .skip(index.saturating_add(1))
                .find(|(_, next)| {
                    !next.trim_start().starts_with("///") && !next.trim_start().starts_with("//!")
                })
                .map_or(lines.len(), |(next, _)| next);
            let owner = lines
                .iter()
                .skip(end)
                .take(12)
                .map(|line| line.trim())
                .find(|line| {
                    !line.is_empty()
                        && !line.starts_with('#')
                        && !line.starts_with("///")
                        && !line.starts_with("//!")
                })
                .map_or_else(|| "module documentation".to_owned(), path_owner);
            rust_doc_ranges(
                lines,
                index,
                end,
                &owner,
                trimmed.starts_with("//!"),
                &mut ranges,
            );
            index = end;
            continue;
        }
        ranges.push(EvidenceRange {
            start: index.saturating_add(1),
            end: index.saturating_add(1),
            owner: path_owner(text),
            source_role: "sourceLine".to_owned(),
        });
        index = index.saturating_add(1);
    }
    ranges
}

/// Splits rustdoc prose from fenced examples while retaining complete paragraphs.
fn rust_doc_ranges(
    lines: &[&str],
    start: usize,
    end: usize,
    owner: &str,
    module_doc: bool,
    ranges: &mut Vec<EvidenceRange>,
) {
    let mut cursor = start;
    while cursor < end {
        let Some(line) = lines.get(cursor).copied() else {
            break;
        };
        let content = rust_doc_content(line);
        if content.trim().is_empty() {
            cursor = cursor.saturating_add(1);
            continue;
        }
        let mut range_end = cursor.saturating_add(1);
        let example = content.trim_start().starts_with("```");
        if example {
            let mut closed = false;
            while range_end < end {
                let Some(line) = lines.get(range_end).copied() else {
                    break;
                };
                let following = rust_doc_content(line);
                range_end = range_end.saturating_add(1);
                if following.trim_start().starts_with("```") {
                    closed = true;
                    break;
                }
            }
            if !closed {
                range_end = end;
            }
        } else {
            while range_end < end {
                let Some(line) = lines.get(range_end).copied() else {
                    break;
                };
                let following = rust_doc_content(line);
                if following.trim().is_empty() || following.trim_start().starts_with("```") {
                    break;
                }
                range_end = range_end.saturating_add(1);
            }
        }
        let source_role = if example && owner.trim_start().starts_with("pub ") {
            "rustPublicExample"
        } else if example {
            "rustExample"
        } else if module_doc {
            "rustModuleDoc"
        } else if owner.trim_start().starts_with("pub trait")
            || owner.trim_start().starts_with("trait ")
        {
            "rustTraitDoc"
        } else {
            "rustItemDoc"
        }
        .to_owned();
        ranges.push(EvidenceRange {
            start: cursor.saturating_add(1),
            end: range_end,
            owner: owner.to_owned(),
            source_role,
        });
        cursor = range_end;
    }
}

/// Removes the documentation marker before paragraph and fence detection.
fn rust_doc_content(line: &str) -> &str {
    let line = line.trim_start();
    line.strip_prefix("///")
        .or_else(|| line.strip_prefix("//!"))
        .unwrap_or(line)
        .trim_start()
}

/// Names the nearest obvious declaration without treating it as semantic authority.
fn path_owner(line: &str) -> String {
    let line = line.trim();
    if line.starts_with("fn ")
        || line.starts_with("pub fn ")
        || line.starts_with("pub(crate) fn ")
        || line.starts_with("struct ")
        || line.starts_with("pub struct ")
        || line.starts_with("trait ")
        || line.starts_with("pub trait ")
    {
        line.split(['{', ';'])
            .next()
            .unwrap_or(line)
            .trim()
            .to_owned()
    } else {
        String::new()
    }
}

/// Rejects comment-marker-only records rather than presenting them as evidence.
fn has_meaningful_excerpt(excerpt: &str) -> bool {
    excerpt.lines().any(|line| {
        let line = line
            .trim()
            .trim_start_matches("///")
            .trim_start_matches("//!")
            .trim();
        !line.is_empty() && line != "*" && line != "/**" && line != "*/"
    })
}

/// Labels evidence shapes while retaining every indexed source line.
fn index_kind(path: &str, text: &str) -> String {
    if file_kind(path) == "documentation"
        || text.trim_start().starts_with("///")
        || text.trim_start().starts_with("//!")
        || text.trim_start().starts_with("'''")
        || text.trim_start().starts_with("\"\"\"")
    {
        "documentation".to_owned()
    } else if text.contains(" impl ") || text.trim_start().starts_with("impl ") {
        "traitImplementation".to_owned()
    } else if text.contains("pub fn ")
        || text.contains("pub struct ")
        || text.contains("pub trait ")
        || text.contains("public ")
        || text.contains("export ")
    {
        "publicDeclaration".to_owned()
    } else if file_kind(path) == "test" {
        "test".to_owned()
    } else {
        "source".to_owned()
    }
}

/// Truncates long lines on a Unicode character boundary for bounded agent reads.
/// Bundles one parsed file so scanner state stays separate from source identity.
#[derive(Clone, Copy)]
struct ScanInput<'a> {
    path: &'a str,
    language: &'a str,
    file_sha256: &'a str,
    text: &'a str,
    lines: &'a [&'a str],
}

/// Selects parser-backed Rust scanning or explicit heuristic profiles.
fn scan_file(
    tests: &mut Vec<TestCase>,
    assertions: &mut Vec<AssertionSite>,
    diagnostics: &mut Vec<ScanDiagnostic>,
    ScanInput {
        path,
        language,
        file_sha256,
        text,
        lines,
    }: ScanInput<'_>,
) {
    if language == "rust" {
        match syn::parse_file(text) {
            Ok(file) => {
                let fixture_struct_fields = fixture_struct_fields(&file.items);
                let mut collector = RustCollector {
                    tests,
                    assertions,
                    diagnostics,
                    path,
                    language,
                    file_sha256,
                    lines,
                    functions: FunctionContext::default(),
                    modules: vec![&file.items],
                    blocks: Vec::new(),
                    cfg: Vec::new(),
                    in_const: false,
                    test_context: is_test_path(path),
                    test_drop_impl: false,
                    calls: BTreeMap::new(),
                    candidate_calls: BTreeMap::new(),
                    helper_assertions: Vec::new(),
                    visiting_direct_call_target: false,
                    fixture_struct_fields,
                    current_fixture_fields: BTreeMap::new(),
                };
                collector.visit_file(&file);
                collector.link_helper_assertions();
                scan_doctests(
                    tests,
                    assertions,
                    diagnostics,
                    ScanInput {
                        path,
                        language,
                        file_sha256,
                        text,
                        lines,
                    },
                );
            }
            Err(error) => {
                diagnostics.push(ScanDiagnostic {
                    kind: "rustParseFallback".to_owned(),
                    path: path.to_owned(),
                    line: Some(error.span().start().line),
                    detail: error.to_string(),
                });
                scan_heuristically(
                    tests,
                    assertions,
                    diagnostics,
                    path,
                    language,
                    file_sha256,
                    text,
                );
            }
        }
        cross_check(
            diagnostics,
            path,
            text,
            assertions
                .iter()
                .filter(|assertion| assertion.path == path)
                .count(),
        );
    } else if matches!(
        language,
        "csharp" | "javascript" | "typescript" | "python" | "go" | "java" | "kotlin" | "powershell"
    ) {
        scan_heuristically(
            tests,
            assertions,
            diagnostics,
            path,
            language,
            file_sha256,
            text,
        );
        cross_check(
            diagnostics,
            path,
            text,
            assertions
                .iter()
                .filter(|assertion| assertion.path == path)
                .count(),
        );
    } else if matches!(language, "text" | "fsharp" | "shell" | "c" | "cpp") {
        diagnostics.push(ScanDiagnostic {
            kind: "unsupportedTestLanguage".to_owned(),
            path: path.to_owned(),
            line: None,
            detail: format!(
                "The audit indexes {language} text but does not inventory its tests or assertions"
            ),
        });
    }
}

/// Resolves explicit crate imports and qualified calls across Rust source files.
fn resolve_imported_callers(
    repository_root: &Path,
    tests: &[TestCase],
    assertions: &mut [AssertionSite],
) -> Result<(), AppError> {
    let mut crates: BTreeMap<PathBuf, Vec<String>> = BTreeMap::new();
    for path in tests.iter().map(|test| &test.path).chain(
        assertions
            .iter()
            .filter(|assertion| assertion.helper_name.is_some())
            .map(|assertion| &assertion.path),
    ) {
        if let Some(root) = rust_crate_root(repository_root, path) {
            crates.entry(root).or_default().push(path.clone());
        }
    }
    for (crate_root, mut paths) in crates {
        paths.sort();
        paths.dedup();
        resolve_crate_callers(repository_root, &crate_root, &paths, tests, assertions)?;
    }
    Ok(())
}

/// Locates the nearest Cargo package owning one repository-relative Rust path.
fn rust_crate_root(repository_root: &Path, path: &str) -> Option<PathBuf> {
    let absolute = repository_root.join(path);
    for ancestor in absolute.ancestors().skip(1) {
        if ancestor.join("Cargo.toml").is_file() {
            return Some(ancestor.to_owned());
        }
        if ancestor == repository_root {
            break;
        }
    }
    if absolute.starts_with(repository_root.join("src")) {
        return Some(repository_root.to_owned());
    }
    None
}

/// Maps one helper's module and exported name to all of its source assertions.
#[derive(Clone, Debug)]
struct HelperTarget {
    module: Vec<String>,
    name: String,
    assertion_indexes: Vec<usize>,
}

/// Adds unambiguous imported or qualified callers within one Cargo package.
fn resolve_crate_callers(
    repository_root: &Path,
    crate_root: &Path,
    paths: &[String],
    tests: &[TestCase],
    assertions: &mut [AssertionSite],
) -> Result<(), AppError> {
    let mut targets: BTreeMap<(Vec<String>, String), Vec<usize>> = BTreeMap::new();
    for (index, assertion) in assertions.iter().enumerate() {
        let Some(name) = assertion.helper_name.as_deref() else {
            continue;
        };
        let Some(module) = rust_module(crate_root, repository_root, &assertion.path) else {
            continue;
        };
        targets
            .entry((module, name.to_owned()))
            .or_default()
            .push(index);
    }
    if targets.is_empty() {
        return Ok(());
    }
    let helper_targets = targets
        .iter()
        .map(|((module, name), assertion_indexes)| HelperTarget {
            module: module.clone(),
            name: name.clone(),
            assertion_indexes: assertion_indexes.clone(),
        })
        .collect::<Vec<_>>();

    let mut exports: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    let crate_source = crate_root.join("src/lib.rs");
    if let (true, Some(bytes)) = (crate_source.is_file(), read_source(&crate_source)?)
        && let Ok(source) = String::from_utf8(bytes)
        && let Ok(file) = syn::parse_file(&source)
    {
        collect_crate_exports(&file, &helper_targets, &mut exports);
    }

    for relative in paths {
        if !relative.ends_with(".rs") {
            continue;
        }
        let path = repository_root.join(relative);
        let Some(bytes) = read_source(&path)? else {
            continue;
        };
        let Ok(source) = String::from_utf8(bytes) else {
            continue;
        };
        let Ok(file) = syn::parse_file(&source) else {
            continue;
        };
        let lines = source.lines().collect::<Vec<_>>();
        let mut collector = ImportedCallerCollector {
            path: relative,
            lines: &lines,
            tests,
            imports: Vec::new(),
            targets: &helper_targets,
            exports: &exports,
            current_test_id: None,
            additions: Vec::new(),
            candidate_additions: Vec::new(),
        };
        collector.visit_file(&file);
        for (index, evidence) in collector.additions {
            let Some(assertion) = assertions.get_mut(index) else {
                return Err(fail(
                    "invalidState",
                    format!("Caller resolver references unknown assertion index {index}"),
                ));
            };
            if !assertion.calling_test_ids.contains(&evidence.test_id) {
                assertion.calling_test_ids.push(evidence.test_id.clone());
            }
            assertion.caller_evidence.push(evidence);
        }
        for (index, evidence) in collector.candidate_additions {
            let Some(assertion) = assertions.get_mut(index) else {
                return Err(fail(
                    "invalidState",
                    format!("Caller resolver references unknown assertion index {index}"),
                ));
            };
            assertion.unresolved_caller_candidates.push(evidence);
        }
    }
    for assertion in assertions {
        assertion.calling_test_ids.sort();
        assertion.calling_test_ids.dedup();
        assertion.caller_evidence.sort_by(|left, right| {
            (
                left.test_id.as_str(),
                left.kind.as_str(),
                left.path.as_str(),
                left.line,
                left.column,
            )
                .cmp(&(
                    right.test_id.as_str(),
                    right.kind.as_str(),
                    right.path.as_str(),
                    right.line,
                    right.column,
                ))
        });
        assertion.caller_evidence.dedup_by(|left, right| {
            left.test_id == right.test_id
                && left.kind == right.kind
                && left.path == right.path
                && left.line == right.line
                && left.column == right.column
        });
        assertion
            .unresolved_caller_candidates
            .sort_by(|left, right| {
                (
                    left.test_id.as_str(),
                    left.path.as_str(),
                    left.line,
                    left.column,
                )
                    .cmp(&(
                        right.test_id.as_str(),
                        right.path.as_str(),
                        right.line,
                        right.column,
                    ))
            });
        assertion
            .unresolved_caller_candidates
            .dedup_by(|left, right| {
                left.test_id == right.test_id
                    && left.path == right.path
                    && left.line == right.line
                    && left.column == right.column
            });
        assertion
            .unresolved_caller_candidates
            .sort_by(|left, right| {
                (
                    left.test_id.as_str(),
                    left.path.as_str(),
                    left.line,
                    left.column,
                )
                    .cmp(&(
                        right.test_id.as_str(),
                        right.path.as_str(),
                        right.line,
                        right.column,
                    ))
            });
        assertion
            .unresolved_caller_candidates
            .dedup_by(|left, right| {
                left.test_id == right.test_id
                    && left.path == right.path
                    && left.line == right.line
                    && left.column == right.column
            });
    }
    Ok(())
}

/// Converts a `src`-rooted Rust filename into its declared module path.
fn rust_module(crate_root: &Path, repository_root: &Path, relative: &str) -> Option<Vec<String>> {
    let absolute = repository_root.join(relative);
    let relative = absolute.strip_prefix(crate_root.join("src")).ok()?;
    let mut components = relative
        .with_extension("")
        .iter()
        .map(|component| component.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    if components.last().is_some_and(|name| name == "mod") {
        _ = components.pop();
    }
    if components.first().is_some_and(|name| name == "lib")
        || components.first().is_some_and(|name| name == "main")
    {
        return Some(Vec::new());
    }
    Some(components)
}

/// Represents one exact import target from a crate-qualified use declaration.
#[derive(Clone, Debug, Eq, PartialEq)]
enum ImportedSymbol {
    Root(String),
    Module(Vec<String>, String),
}

/// Retains only unambiguous crate imports directly visible in one item scope.
fn scope_imports(items: &[Item]) -> BTreeMap<String, Option<ImportedSymbol>> {
    let mut imports = BTreeMap::new();
    for item in items {
        if let Item::Use(import) = item {
            record_import(import, &mut imports);
        }
    }
    imports
}

/// Retains function-local crate imports without applying them outside the body.
fn scope_imports_from_block(block: &Block) -> BTreeMap<String, Option<ImportedSymbol>> {
    let mut imports = BTreeMap::new();
    for statement in &block.stmts {
        if let Stmt::Item(Item::Use(import)) = statement {
            record_import(import, &mut imports);
        }
    }
    imports
}

/// Records one explicit crate import and marks conflicting aliases ambiguous.
fn record_import(import: &ItemUse, imports: &mut BTreeMap<String, Option<ImportedSymbol>>) {
    for (segments, local) in use_routes(&import.tree, &[]) {
        if !segments.first().is_some_and(|segment| segment == "crate") {
            continue;
        }
        let Some(route) = segments.get(1..) else {
            continue;
        };
        let target = match route {
            [name] => ImportedSymbol::Root(name.clone()),
            [module @ .., name] if !module.is_empty() => {
                ImportedSymbol::Module(module.to_vec(), name.clone())
            }
            _ => continue,
        };
        match imports.entry(local) {
            Entry::Vacant(entry) => {
                entry.insert(Some(target));
            }
            Entry::Occupied(mut entry) => {
                if entry
                    .get()
                    .as_ref()
                    .is_some_and(|existing| existing != &target)
                {
                    entry.insert(None);
                }
            }
        }
    }
}

/// Flattens use declarations while retaining alias and crate path information.
fn use_routes(tree: &UseTree, parent: &[String]) -> Vec<(Vec<String>, String)> {
    match tree {
        UseTree::Path(path) => {
            let mut segments = parent.to_vec();
            segments.push(path.ident.to_string());
            use_routes(&path.tree, &segments)
        }
        UseTree::Name(name) => {
            let ident = name.ident.to_string();
            let mut segments = parent.to_vec();
            segments.push(ident.clone());
            vec![(segments, ident)]
        }
        UseTree::Rename(rename) => {
            let mut segments = parent.to_vec();
            segments.push(rename.ident.to_string());
            vec![(segments, rename.rename.to_string())]
        }
        UseTree::Group(group) => group
            .items
            .iter()
            .flat_map(|item| use_routes(item, parent))
            .collect(),
        UseTree::Glob(_) => {
            let mut segments = parent.to_vec();
            segments.push("*".to_owned());
            vec![(segments, "*".to_owned())]
        }
    }
}

/// Records helper names exported from one package root.
fn collect_crate_exports(
    file: &syn::File,
    helpers: &[HelperTarget],
    exports: &mut BTreeMap<String, Vec<usize>>,
) {
    for item in &file.items {
        let Item::Use(import) = item else {
            continue;
        };
        if matches!(import.vis, Visibility::Inherited) {
            continue;
        }
        for (segments, local) in use_routes(&import.tree, &[]) {
            let segments = if segments.first().is_some_and(|first| first == "crate") {
                segments.get(1..).unwrap_or_default().to_vec()
            } else {
                segments
            };
            match segments.as_slice() {
                [module @ .., wildcard] if wildcard == "*" && !module.is_empty() => {
                    for (helper_index, helper) in helpers
                        .iter()
                        .enumerate()
                        .filter(|(_, helper)| helper.module == module)
                    {
                        exports
                            .entry(helper.name.clone())
                            .or_default()
                            .push(helper_index);
                    }
                }
                [module @ .., name] if !module.is_empty() => {
                    for (helper_index, _helper) in helpers
                        .iter()
                        .enumerate()
                        .filter(|(_, helper)| helper.module == module && helper.name == *name)
                    {
                        exports.entry(local.clone()).or_default().push(helper_index);
                    }
                }
                _ => {}
            }
        }
    }
    for indexes in exports.values_mut() {
        indexes.sort_unstable();
        indexes.dedup();
    }
}

/// Resolves registered test calls only from explicit crate imports or paths.
struct ImportedCallerCollector<'a> {
    path: &'a str,
    lines: &'a [&'a str],
    tests: &'a [TestCase],
    imports: Vec<BTreeMap<String, Option<ImportedSymbol>>>,
    targets: &'a [HelperTarget],
    exports: &'a BTreeMap<String, Vec<usize>>,
    current_test_id: Option<&'a str>,
    additions: Vec<(usize, CallerEvidence)>,
    candidate_additions: Vec<(usize, CallerEvidence)>,
}

impl ImportedCallerCollector<'_> {
    /// Resolves one call path to a unique helper definition.
    fn resolve(&self, path: &ExprPath) -> Option<(Vec<usize>, &'static str)> {
        if path.qself.is_some() {
            return None;
        }
        let names = path
            .path
            .segments
            .iter()
            .map(|segment| segment.ident.to_string())
            .collect::<Vec<_>>();
        if names.first().is_some_and(|name| name == "crate") {
            return self.resolve_segments(names.get(1..).unwrap_or_default(), "nativeQualified");
        }
        if names.len() == 1 {
            let local = names.first()?;
            let target = self
                .imports
                .iter()
                .rev()
                .take(2)
                .find_map(|scope| scope.get(local))
                .and_then(Option::as_ref)?;
            return self.resolve_imported(target, "nativeImport");
        }
        None
    }

    /// Resolves an explicit imported alias without accepting broad globs.
    fn resolve_imported(
        &self,
        imported: &ImportedSymbol,
        kind: &'static str,
    ) -> Option<(Vec<usize>, &'static str)> {
        match imported {
            ImportedSymbol::Root(name) => self.resolve_export(name).map(|indexes| (indexes, kind)),
            ImportedSymbol::Module(module, name) => unique_helper_indexes(
                self.targets
                    .iter()
                    .filter(|target| target.module == *module && target.name == *name),
            )
            .map(|x| (x, kind)),
        }
    }

    /// Resolves crate-qualified symbols through module identity or root exports.
    fn resolve_segments(
        &self,
        segments: &[String],
        kind: &'static str,
    ) -> Option<(Vec<usize>, &'static str)> {
        match segments {
            [name] => self.resolve_export(name).map(|indexes| (indexes, kind)),
            [module @ .., name] if !module.is_empty() => unique_helper_indexes(
                self.targets
                    .iter()
                    .filter(|target| target.module == module && target.name == *name),
            )
            .map(|x| (x, kind)),
            _ => None,
        }
    }

    /// Resolves a crate export only when it names one helper definition.
    fn resolve_export(&self, name: &str) -> Option<Vec<usize>> {
        let mut unique = self
            .exports
            .get(name)?
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        let target = unique.pop_first()?;
        if !unique.is_empty() {
            return None;
        }
        Some(self.targets.get(target)?.assertion_indexes.clone())
    }
}

impl<'ast> Visit<'ast> for ImportedCallerCollector<'ast> {
    fn visit_file(&mut self, i: &'ast syn::File) {
        self.imports.push(scope_imports(&i.items));
        visit::visit_file(self, i);
        _ = self.imports.pop();
    }

    fn visit_item_mod(&mut self, i: &'ast ItemMod) {
        if let Some((_, items)) = &i.content {
            self.imports.push(scope_imports(items));
            for item in items {
                self.visit_item(item);
            }
            _ = self.imports.pop();
        }
    }

    fn visit_item_fn(&mut self, i: &'ast ItemFn) {
        let test = self.tests.iter().find(|test| {
            test.path == self.path
                && i.sig.ident == test.name
                && test.line.get() <= i.sig.ident.span().start().line
                && i.block.span().end().line <= test.end_line
        });
        let previous = self.current_test_id;
        if let Some(test) = test {
            self.current_test_id = Some(&test.id);
        }
        self.imports.push(scope_imports_from_block(&i.block));
        visit::visit_item_fn(self, i);
        _ = self.imports.pop();
        self.current_test_id = previous;
    }

    fn visit_expr_call(&mut self, i: &'ast ExprCall) {
        let resolved = if let (Some(test_id), Expr::Path(path)) =
            (self.current_test_id, i.func.as_ref())
            && let Some((indexes, kind)) = self.resolve(path)
        {
            let evidence = caller_evidence(self.path, self.lines, test_id, i.span(), kind, i);
            for index in indexes {
                self.additions.push((index, evidence.clone()));
            }
            true
        } else {
            false
        };
        if !resolved {
            self.visit_expr(&i.func);
        }
        for argument in &i.args {
            self.visit_expr(argument);
        }
    }

    fn visit_expr_path(&mut self, i: &'ast ExprPath) {
        if let (Some(test_id), Some((indexes, _))) = (self.current_test_id, self.resolve(i)) {
            let evidence = CallerEvidence {
                kind: "unresolvedCandidate".to_owned(),
                test_id: test_id.to_owned(),
                path: self.path.to_owned(),
                line: parser_line(i.span().start().line),
                end_line: i.span().end().line,
                column: NonZeroUsize::MIN.saturating_add(
                    source_line(self.lines, i.span().start().line)
                        .map_or(0, |text| byte_column(text, i.span().start())),
                ),
                expression: source_slice(self.lines, i.span().start(), i.span().end()),
            };
            for index in indexes {
                self.candidate_additions.push((index, evidence.clone()));
            }
        }
        visit::visit_expr_path(self, i);
    }
}

/// Collapses multiple assertion sites from one helper, rejecting ambiguous names.
fn unique_helper_indexes<'a>(
    targets: impl Iterator<Item = &'a HelperTarget>,
) -> Option<Vec<usize>> {
    let targets = targets.collect::<Vec<_>>();
    let [target] = targets.as_slice() else {
        return None;
    };
    Some(target.assertion_indexes.clone())
}

/// Visits Rust syntax and keeps resolution local to one parsed file.
struct RustCollector<'a, 'ast> {
    tests: &'a mut Vec<TestCase>,
    assertions: &'a mut Vec<AssertionSite>,
    diagnostics: &'a mut Vec<ScanDiagnostic>,
    path: &'a str,
    language: &'a str,
    file_sha256: &'a str,
    lines: &'a [&'a str],
    functions: FunctionContext,
    modules: Vec<&'ast [Item]>,
    blocks: Vec<BlockScope<'ast>>,
    cfg: Vec<String>,
    in_const: bool,
    test_context: bool,
    test_drop_impl: bool,
    calls: BTreeMap<FunctionId, BTreeMap<String, NativeCall>>,
    candidate_calls: BTreeMap<FunctionId, BTreeMap<String, CallerEvidence>>,
    helper_assertions: Vec<(usize, FunctionId)>,
    visiting_direct_call_target: bool,
    fixture_struct_fields: BTreeMap<String, BTreeMap<String, FixtureKind>>,
    current_fixture_fields: BTreeMap<String, FixtureKind>,
}

/// Separates diagnostic display names from the identities used for linking.
#[derive(Default)]
struct FunctionContext {
    role: FunctionRole,
    name: Option<String>,
    helper: Option<FunctionId>,
    expects_panic: bool,
    test_destructor: bool,
    owner_line: Option<NonZeroUsize>,
    owner_end_line: usize,
    // Function items cannot capture variables from an enclosing function.
    first_binding_scope: usize,
}

/// Distinguishes named test ownership from unregistered test-context code.
#[derive(Default)]
enum FunctionRole {
    #[default]
    Ordinary,
    TestContext,
    RegisteredTest(String),
}

impl FunctionRole {
    /// Gets the inventory owner for assertions directly in a registered test.
    fn test_id(&self) -> Option<&String> {
        match self {
            Self::RegisteredTest(id) => Some(id),
            Self::Ordinary | Self::TestContext => None,
        }
    }

    /// Determines whether test-specific predicate checks apply.
    fn is_test(&self) -> bool {
        !matches!(self, Self::Ordinary)
    }
}

/// Identifies a declaration by its position within the current parsed file.
type FunctionId = (usize, usize);

/// Retains the syntax edge needed to prove one native test-to-helper call.
struct NativeCall {
    normal: bool,
    evidence: CallerEvidence,
}

/// Holds block-local declarations and the bindings seen before the current use.
#[derive(Default)]
struct BlockScope<'ast> {
    items: Vec<&'ast Item>,
    bindings: BTreeMap<String, Option<FixtureKind>>,
}

/// Identifies receivers tied to standard synchronization or thread fixtures.
#[derive(Clone, Copy, Eq, PartialEq)]
enum FixtureKind {
    Mutex,
    RwLock,
    JoinHandle,
    OptionHolder,
}

impl<'ast> RustCollector<'_, 'ast> {
    /// Adds a test and visits its body with direct-predicate ownership established.
    fn visit_function(
        &mut self,
        attributes: &'ast [Attribute],
        signature: &'ast Signature,
        block: &'ast Block,
        helper: Option<FunctionId>,
        test_destructor: bool,
    ) {
        for attribute in attributes {
            self.visit_attribute(attribute);
        }
        let test = test_attributes(attributes);
        let function_is_test = self.test_context
            || test.is_some()
            || attributes.iter().any(attribute_enables_test_code);
        let panic_attribute = attributes
            .iter()
            .find(|attribute| attribute.path().is_ident("should_panic"));
        let name = signature.ident.to_string();
        let role = if let Some(kind) = test {
            let id = next_test_id(self.tests);
            let line = attributes
                .iter()
                .filter(|attribute| {
                    attribute.path().is_ident("should_panic")
                        || test_attributes(from_ref(attribute)).is_some()
                })
                .map(|attribute| attribute.span().start().line)
                .min()
                .unwrap_or_else(|| signature.ident.span().start().line);
            self.tests.push(TestCase {
                id: id.clone(),
                name: name.clone(),
                kind: kind.to_owned(),
                language: self.language.to_owned(),
                path: self.path.to_owned(),
                line: parser_line(line),
                end_line: block.span().end().line,
                direct_assertion_ids: Vec::new(),
                shared_assertion_ids: Vec::new(),
            });
            FunctionRole::RegisteredTest(id)
        } else if function_is_test {
            FunctionRole::TestContext
        } else {
            FunctionRole::Ordinary
        };
        let previous = replace(
            &mut self.functions,
            FunctionContext {
                role,
                name: Some(name),
                helper,
                expects_panic: panic_attribute.is_some(),
                test_destructor: test_destructor && function_is_test,
                owner_line: Some(parser_line(
                    attributes
                        .iter()
                        .map(|attribute| attribute.span().start().line)
                        .chain(once(signature.ident.span().start().line))
                        .min()
                        .unwrap_or_else(|| signature.ident.span().start().line),
                )),
                owner_end_line: block.span().end().line,
                first_binding_scope: self.blocks.len(),
            },
        );
        let previous_const = replace(&mut self.in_const, false);
        if self.functions.role.test_id().is_some()
            && let Some(attribute) = panic_attribute
        {
            self.push_span("panicExpectation", attribute.span());
        }

        let mut parameters = BlockScope::default();
        for input in &signature.inputs {
            if let FnArg::Typed(argument) = input {
                record_bindings(&mut parameters.bindings, &argument.pat);
                if let Some((name, kind)) = self.typed_fixture(&argument.pat, &argument.ty) {
                    parameters.bindings.insert(name, Some(kind));
                }
            }
        }
        self.blocks.push(parameters);
        self.visit_signature(signature);
        self.visit_block(block);
        self.blocks.pop();
        self.functions = previous;
        self.in_const = previous_const;
    }

    /// Records a parser-confirmed source predicate with deterministic ownership.
    fn push_span(&mut self, kind: &str, span: Span) {
        self.push_range(kind, span.start(), span.end());
    }

    /// Uses explicit endpoints when a node's span also includes outer attributes.
    fn push_range(&mut self, kind: &str, start: LineColumn, end: LineColumn) {
        let line = start.line;
        let end_line = end.line;
        let mut containing_test_id = self.functions.role.test_id().cloned();
        let id = next_assertion_id(self.assertions);
        if !self.cfg.is_empty() {
            self.diagnostics.push(ScanDiagnostic {
                kind: "conditionalAssertion".to_owned(),
                path: self.path.to_owned(),
                line: Some(line),
                detail: format!("Assertion '{id}' is conditional on {}", self.cfg.join(" ")),
            });
        }
        if containing_test_id.is_none() && kind == "compileTimeAssertion" {
            let id = next_test_id(self.tests);
            self.tests.push(TestCase {
                id: id.clone(),
                name: format!("compile-time check at {line}"),
                kind: "compileTime".to_owned(),
                language: self.language.to_owned(),
                path: self.path.to_owned(),
                line: parser_line(line),
                end_line,
                direct_assertion_ids: Vec::new(),
                shared_assertion_ids: Vec::new(),
            });
            containing_test_id = Some(id);
        }
        if containing_test_id.is_none()
            && let Some(helper) = self.functions.helper
        {
            self.helper_assertions.push((self.assertions.len(), helper));
        }
        self.assertions.push(AssertionSite {
            id,
            kind: kind.to_owned(),
            language: self.language.to_owned(),
            path: self.path.to_owned(),
            line: parser_line(line),
            end_line,
            column: NonZeroUsize::MIN.saturating_add(
                source_line(self.lines, line).map_or(0, |text| byte_column(text, start)),
            ),
            expression: source_slice(self.lines, start, end),
            file_sha256: self.file_sha256.to_owned(),
            owner_line: self
                .functions
                .owner_line
                .unwrap_or_else(|| parser_line(line)),
            owner_end_line: if self.functions.owner_end_line == 0 {
                end_line
            } else {
                self.functions.owner_end_line
            },
            test_code: self.functions.role.is_test(),
            fixture_excluded: false,
            fixture_exclusion_reason: None,
            containing_test_id,
            helper_name: if self.functions.role.test_id().is_none() {
                self.functions.name.clone()
            } else {
                None
            },
            calling_test_ids: Vec::new(),
            caller_evidence: Vec::new(),
            unresolved_caller_candidates: Vec::new(),
        });
    }

    /// Applies calls after visiting the whole file, regardless of declaration order.
    fn link_helper_assertions(&mut self) {
        for &(index, helper) in &self.helper_assertions {
            let Some(assertion) = self.assertions.get_mut(index) else {
                continue;
            };
            if let Some(callers) = self.calls.get(&helper) {
                assertion.calling_test_ids.extend(
                    callers
                        .iter()
                        .filter(|(_, call)| call.normal || assertion.kind != "unexpectedBranch")
                        .map(|(test_id, _)| test_id.clone()),
                );
                assertion.caller_evidence.extend(
                    callers
                        .iter()
                        .filter(|(_, call)| call.normal || assertion.kind != "unexpectedBranch")
                        .map(|(_, call)| call.evidence.clone()),
                );
                assertion.caller_evidence.sort_by(|left, right| {
                    (left.test_id.as_str(), &left.path, left.line, left.column).cmp(&(
                        right.test_id.as_str(),
                        &right.path,
                        right.line,
                        right.column,
                    ))
                });
                assertion.caller_evidence.dedup_by(|left, right| {
                    left.test_id == right.test_id
                        && left.path == right.path
                        && left.line == right.line
                        && left.column == right.column
                });
            }
            assertion.unresolved_caller_candidates.extend(
                self.candidate_calls
                    .get(&helper)
                    .into_iter()
                    .flat_map(|candidates| candidates.values().cloned()),
            );
            assertion
                .unresolved_caller_candidates
                .sort_by(|left, right| {
                    (
                        left.test_id.as_str(),
                        left.path.as_str(),
                        left.line,
                        left.column,
                    )
                        .cmp(&(
                            right.test_id.as_str(),
                            right.path.as_str(),
                            right.line,
                            right.column,
                        ))
                });
            assertion
                .unresolved_caller_candidates
                .dedup_by(|left, right| {
                    left.test_id == right.test_id
                        && left.path == right.path
                        && left.line == right.line
                        && left.column == right.column
                });
        }
    }

    /// Bounds attributes to their AST owner rather than its following source line.
    fn with_attributes(&mut self, visit: impl FnOnce(&mut Self)) {
        let depth = self.cfg.len();
        visit(self);
        self.cfg.truncate(depth);
    }

    /// Distinguishes evaluated constant blocks from ordinary runtime assertions.
    fn in_constant(&mut self, visit: impl FnOnce(&mut Self)) {
        let previous = replace(&mut self.in_const, true);
        visit(self);
        self.in_const = previous;
    }

    /// Visits call arguments while suppressing deliberately triggered expected panics.
    fn visit_call_children(&mut self, i: &'ast ExprCall) {
        let previous = self.functions.expects_panic;
        if self.functions.role.is_test() && is_panic_expectation_call(&i.func) {
            self.push_range("panicExpectation", i.func.span().start(), i.span().end());
            self.functions.expects_panic = true;
        }
        let previous_target = replace(&mut self.visiting_direct_call_target, true);
        self.visit_expr(&i.func);
        self.visiting_direct_call_target = previous_target;
        for argument in &i.args {
            self.visit_expr(argument);
        }
        self.functions.expects_panic = previous;
    }

    /// Resolves only paths whose declaration is visible in the parsed file.
    fn resolve_function(&self, expression: &Expr) -> Option<&'ast ItemFn> {
        let Expr::Path(path) = unparenthesized(expression) else {
            return None;
        };
        self.resolve_function_path(path)
    }

    /// Resolves one already-unwrapped function path.
    fn resolve_function_path(&self, path: &ExprPath) -> Option<&'ast ItemFn> {
        if path.qself.is_some() || path.path.leading_colon.is_some() {
            return None;
        }
        let names = path
            .path
            .segments
            .iter()
            .map(|segment| segment.ident.to_string())
            .collect::<Vec<_>>();
        let (first, remaining) = names.split_first()?;
        let current = self.modules.last()?;
        let target = match first.as_str() {
            "crate" if is_crate_root(self.path) => walk_items(self.modules.first()?, remaining),
            "crate" | "Self" => None,
            "self" => walk_items(current, remaining),
            "super" => {
                let parents = remaining.iter().take_while(|name| *name == "super").count();
                let depth = self.modules.len().checked_sub(parents.saturating_add(2))?;
                walk_items(self.modules.get(depth)?, remaining.get(parents..)?)
            }
            _ => self.resolve_relative(current, first, remaining),
        };
        match target {
            Some(Item::Fn(function)) => Some(function),
            _ => None,
        }
    }

    /// Checks lexical items before the module and never guesses import aliases.
    fn resolve_relative(
        &self,
        module: &'ast [Item],
        first: &str,
        remaining: &[String],
    ) -> Option<&'ast Item> {
        for (index, block) in self.blocks.iter().enumerate().rev() {
            if index >= self.functions.first_binding_scope && block.bindings.contains_key(first) {
                return None;
            }
            if let Some(item) = unique_named_item(block.items.iter().copied(), first).ok()? {
                return walk_child(item, remaining);
            }
            if block
                .items
                .iter()
                .any(|item| import_could_supply(item, first))
            {
                return None;
            }
        }
        if let Some(item) = unique_named_item(module.iter(), first).ok()? {
            return walk_child(item, remaining);
        }
        // `use super::*` is a common test-module layout. Accept it only
        // when no other import could supply the same name.
        let mut imports = module
            .iter()
            .filter(|item| import_could_supply(item, first));
        if let Some(Item::Use(import)) = imports.next()
            && imports.next().is_none()
            && import.leading_colon.is_none()
            && matches!(&import.tree, UseTree::Path(path)
                if path.ident == "super" && matches!(*path.tree, UseTree::Glob(_)))
        {
            let parent = self.modules.get(self.modules.len().checked_sub(2)?)?;
            return walk_child(unique_named_item(parent.iter(), first).ok()??, remaining);
        }
        None
    }

    /// Records a fixture type only for an explicitly typed or constructed local.
    fn local_fixture(&self, local: &Local) -> Option<(String, FixtureKind)> {
        let (pattern, kind) = match &local.pat {
            Pat::Type(typed) => {
                let kind = match typed.ty.as_ref() {
                    Type::Path(path) if path.qself.is_none() => {
                        let names = path
                            .path
                            .segments
                            .iter()
                            .map(|segment| &segment.ident)
                            .collect::<Vec<_>>();
                        self.fixture_type(&names, path.path.leading_colon.is_some())
                    }
                    _ => None,
                }
                .or_else(|| self.fixture_constructor(&local.init.as_ref()?.expr))?;
                (typed.pat.as_ref(), kind)
            }
            pattern => (
                pattern,
                self.fixture_constructor(&local.init.as_ref()?.expr)?,
            ),
        };
        let Pat::Ident(ident) = pattern else {
            return None;
        };
        (ident.by_ref.is_none() && ident.subpat.is_none()).then(|| (ident.ident.to_string(), kind))
    }

    /// Recognizes standard constructors without trusting a receiver's variable name.
    fn fixture_constructor(&self, expression: &Expr) -> Option<FixtureKind> {
        if let Some(kind) = self.fixture_holder_constructor(expression) {
            return Some(kind);
        }
        match unparenthesized(expression) {
            Expr::Reference(reference) => self.fixture_constructor(&reference.expr),
            Expr::Call(call) => {
                let Expr::Path(function) = call.func.as_ref() else {
                    return None;
                };
                if function.qself.is_some() {
                    return None;
                }
                let names = function
                    .path
                    .segments
                    .iter()
                    .map(|segment| &segment.ident)
                    .collect::<Vec<_>>();
                let (&constructor, qualifiers) = names.split_last()?;
                if constructor == "new" {
                    return self.fixture_type(qualifiers, function.path.leading_colon.is_some());
                }
                (constructor == "spawn"
                    && matches!(qualifiers, [root, namespace] if *root == "std" && *namespace == "thread"))
                .then_some(FixtureKind::JoinHandle)
            }
            Expr::MethodCall(call) => {
                let method = call.method.to_string();
                let holder_access = call.args.is_empty()
                    && matches!(
                        method.as_str(),
                        "borrow" | "borrow_mut" | "lock" | "try_lock"
                    )
                    && self.fixture_kind(&call.receiver) == Some(FixtureKind::OptionHolder);
                let extracted_holder = call.args.len() == 1
                    && method == "expect"
                    && self.is_fixture_access(&call.receiver);
                (holder_access || extracted_holder).then_some(FixtureKind::OptionHolder)
            }
            _ => None,
        }
    }

    /// Recognizes test-owned option holders and their ordinary Rc/Arc clones.
    fn fixture_holder_constructor(&self, expression: &Expr) -> Option<FixtureKind> {
        let Expr::Call(call) = unparenthesized(expression) else {
            return None;
        };
        let Expr::Path(function) = call.func.as_ref() else {
            return None;
        };
        let names = function
            .path
            .segments
            .iter()
            .map(|segment| segment.ident.to_string())
            .collect::<Vec<_>>();
        if names.last().is_some_and(|name| name == "clone")
            && call.args.len() == 1
            && names.len() == 2
            && names
                .first()
                .is_some_and(|owner| matches!(owner.as_str(), "Rc" | "Arc"))
        {
            return matches!(
                call.args.first(),
                Some(Expr::Reference(reference))
                    if self.fixture_kind(&reference.expr) == Some(FixtureKind::OptionHolder)
            )
            .then_some(FixtureKind::OptionHolder);
        }
        let argument = call.args.first()?;
        if names.last().is_some_and(|name| name == "new")
            && call.args.len() == 1
            && names.len() == 2
            && names
                .first()
                .is_some_and(|owner| matches!(owner.as_str(), "Rc" | "Arc" | "RefCell" | "Mutex"))
        {
            let is_holder = |inner: &Expr| {
                let Expr::Call(inner) = unparenthesized(inner) else {
                    return false;
                };
                let Expr::Path(function) = inner.func.as_ref() else {
                    return false;
                };
                function.path.is_ident("Some")
            };
            let inner = if names
                .first()
                .is_some_and(|name| matches!(name.as_str(), "Rc" | "Arc"))
            {
                let Expr::Call(inner) = unparenthesized(argument) else {
                    return None;
                };
                let Expr::Path(function) = inner.func.as_ref() else {
                    return None;
                };
                if !function
                    .path
                    .segments
                    .last()
                    .is_some_and(|segment| segment.ident == "new")
                {
                    return None;
                }
                let Expr::Path(container) = inner.func.as_ref() else {
                    return None;
                };
                if !container.path.segments.iter().any(|segment| {
                    matches!(segment.ident.to_string().as_str(), "RefCell" | "Mutex")
                }) {
                    return None;
                }
                inner.args.first()?
            } else {
                argument
            };
            return is_holder(inner).then_some(FixtureKind::OptionHolder);
        }
        None
    }

    /// Resolves only locally proven fixture bindings, never variable naming.
    fn fixture_kind(&self, expression: &Expr) -> Option<FixtureKind> {
        if let Some(kind) = self.fixture_field_kind(expression) {
            return Some(kind);
        }
        if let Expr::Path(path) = unparenthesized(expression)
            && let Some(name) = path.path.get_ident()
        {
            return self
                .blocks
                .iter()
                .skip(self.functions.first_binding_scope)
                .rev()
                .find_map(|block| block.bindings.get(&name.to_string()).copied())
                .flatten();
        }
        self.fixture_constructor(expression)
    }

    /// Names unqualified concrete types used by test-only impls.
    fn simple_type_name(ty: &Type) -> Option<String> {
        match ty {
            Type::Reference(reference) => Self::simple_type_name(&reference.elem),
            Type::Path(path) if path.qself.is_none() => path
                .path
                .segments
                .last()
                .map(|segment| segment.ident.to_string()),
            _ => None,
        }
    }

    /// Resolves an explicit standard-library path or directly imported type.
    fn fixture_type(&self, names: &[&Ident], absolute: bool) -> Option<FixtureKind> {
        let terminal = names.last()?.to_string();
        let (namespace, kind) = match terminal.as_str() {
            "Mutex" => ("sync", FixtureKind::Mutex),
            "RwLock" => ("sync", FixtureKind::RwLock),
            "JoinHandle" => ("thread", FixtureKind::JoinHandle),
            _ => return None,
        };
        if matches!(names, [root, module, _] if *root == "std" && *module == namespace) {
            return Some(kind);
        }
        if absolute || names.len() != 1 {
            return None;
        }
        let module = self.modules.last()?;
        for block in self.blocks.iter().rev() {
            if let Break(imported) =
                fixture_import(block.items.iter().copied(), namespace, &terminal)
            {
                return imported.then_some(kind);
            }
        }
        matches!(
            fixture_import(module.iter(), namespace, &terminal),
            Break(true)
        )
        .then_some(kind)
    }

    /// Resolves a fixture marker from a typed parameter or local binding.
    fn typed_fixture(&self, pattern: &Pat, ty: &Type) -> Option<(String, FixtureKind)> {
        let Pat::Ident(ident) = pattern else {
            return None;
        };
        if ident.by_ref.is_some() || ident.subpat.is_some() {
            return None;
        }
        let kind = self.fixture_type_from_type(ty)?;
        Some((ident.ident.to_string(), kind))
    }

    /// Looks through references to identify standard fixture-bearing parameters.
    fn fixture_type_from_type(&self, ty: &Type) -> Option<FixtureKind> {
        let selected = match ty {
            Type::Reference(reference) => reference.elem.as_ref(),
            Type::Path(_) => ty,
            _ => return None,
        };
        let Type::Path(path) = selected else {
            return None;
        };
        if path.qself.is_some() {
            return None;
        }
        let names = path
            .path
            .segments
            .iter()
            .map(|segment| &segment.ident)
            .collect::<Vec<_>>();
        self.fixture_type(&names, path.path.leading_colon.is_some())
    }

    /// Excludes calls only when their syntax identifies a fixture operation.
    fn is_fixture_access(&self, expression: &Expr) -> bool {
        let expression = unparenthesized(expression);
        if let Expr::Unsafe(unsafe_block) = expression {
            return unsafe_block.block.stmts.last().is_some_and(|statement| {
                matches!(statement, Stmt::Expr(tail, None) if is_pointer_fixture_access(tail))
            });
        }
        let Expr::MethodCall(call) = expression else {
            return false;
        };
        if !call.args.is_empty() {
            return false;
        }
        let fixture = self.fixture_kind(&call.receiver);
        if matches!(call.method.to_string().as_str(), "as_mut" | "take")
            && self.fixture_field_kind(&call.receiver) == Some(FixtureKind::OptionHolder)
        {
            return true;
        }
        if matches!(call.method.to_string().as_str(), "pop")
            && let Expr::Path(path) = unparenthesized(&call.receiver)
            && let Some(name) = path.path.get_ident()
            && self.has_nonempty_witness(&name.to_string(), call.span().start().line)
        {
            return true;
        }
        matches!(
            (fixture, call.method.to_string().as_str()),
            (Some(FixtureKind::Mutex), "lock" | "try_lock")
                | (
                    Some(FixtureKind::RwLock),
                    "read" | "try_read" | "write" | "try_write"
                )
                | (Some(FixtureKind::JoinHandle), "join")
                | (
                    Some(FixtureKind::OptionHolder),
                    "as_ref" | "as_mut" | "take" | "borrow" | "borrow_mut" | "lock" | "try_lock"
                )
        )
    }

    /// Proves that a pop receiver was checked nonempty before this extraction.
    fn has_nonempty_witness(&self, receiver: &str, call_line: usize) -> bool {
        let start = self
            .functions
            .owner_line
            .map_or(1, NonZeroUsize::get)
            .min(call_line);
        for line in (start..call_line).rev() {
            let Some(source) = source_line(self.lines, line) else {
                continue;
            };
            if source.contains(&format!("{receiver}.pop(")) {
                return false;
            }
            if source.contains("assert") && length_asserts_nonempty(source, receiver) {
                return true;
            }
        }
        false
    }

    /// Explains why one syntactically proven fixture extraction is excluded.
    fn fixture_exclusion_reason(expression: &Expr) -> String {
        let expression = unparenthesized(expression);
        if matches!(expression, Expr::Unsafe(_)) {
            "The unsafe expression extracts fixture storage through a pointer or UnsafeCell-like accessor"
                .to_owned()
        } else if matches!(
            expression,
            Expr::MethodCall(call) if call.method == "pop"
        ) {
            "The collection was proven nonempty immediately before this pop extraction".to_owned()
        } else {
            "The receiver acquires or extracts test-owned synchronization or holder state"
                .to_owned()
        }
    }

    /// Resolves fields of the current impl only when syntax proves fixture access.
    fn fixture_field_kind(&self, expression: &Expr) -> Option<FixtureKind> {
        let Expr::Field(field) = unparenthesized(expression) else {
            return None;
        };
        if !matches!(
            unparenthesized(&field.base),
            Expr::Path(path) if path.path.is_ident("self")
        ) {
            return None;
        }
        match &field.member {
            syn::Member::Named(name) => self.current_fixture_fields.get(&name.to_string()).copied(),
            syn::Member::Unnamed(_) => None,
        }
    }
}

impl<'ast> Visit<'ast> for RustCollector<'_, 'ast> {
    fn visit_file(&mut self, i: &'ast syn::File) {
        self.test_context |= i.attrs.iter().any(attribute_enables_test_code);
        visit::visit_file(self, i);
    }

    fn visit_item(&mut self, i: &'ast Item) {
        self.with_attributes(|this| visit::visit_item(this, i));
    }

    fn visit_impl_item(&mut self, i: &'ast ImplItem) {
        self.with_attributes(|this| visit::visit_impl_item(this, i));
    }

    fn visit_trait_item(&mut self, i: &'ast TraitItem) {
        self.with_attributes(|this| visit::visit_trait_item(this, i));
    }

    fn visit_expr(&mut self, i: &'ast Expr) {
        self.with_attributes(|this| visit::visit_expr(this, i));
    }

    fn visit_stmt_macro(&mut self, i: &'ast StmtMacro) {
        self.with_attributes(|this| visit::visit_stmt_macro(this, i));
    }

    fn visit_arm(&mut self, i: &'ast Arm) {
        let mut bindings = BlockScope::default();
        record_bindings(&mut bindings.bindings, &i.pat);
        self.blocks.push(bindings);
        self.with_attributes(|this| visit::visit_arm(this, i));
        self.blocks.pop();
    }

    fn visit_variant(&mut self, i: &'ast Variant) {
        self.with_attributes(|this| visit::visit_variant(this, i));
    }

    fn visit_field(&mut self, i: &'ast Field) {
        self.with_attributes(|this| visit::visit_field(this, i));
    }

    fn visit_attribute(&mut self, i: &'ast Attribute) {
        // The audit already selects `cfg(test)` code; other configurations
        // restrict which builds exercise the check within that selection.
        // cfg_attr only gates inclusion when one of its nested attributes is cfg.
        if attribute_is_inclusion_gate(i) && !attribute_enables_test_code(i) {
            self.cfg
                .push(source_slice(self.lines, i.span().start(), i.span().end()));
        }
    }

    fn visit_item_fn(&mut self, i: &'ast ItemFn) {
        self.visit_function(&i.attrs, &i.sig, &i.block, Some(function_id(i)), false);
    }

    fn visit_impl_item_fn(&mut self, i: &'ast ImplItemFn) {
        // An associated function needs type resolution; a terminal method name
        // cannot establish this identity.
        self.visit_function(
            &i.attrs,
            &i.sig,
            &i.block,
            None,
            self.test_drop_impl && i.sig.ident == "drop",
        );
    }

    fn visit_item_mod(&mut self, i: &'ast ItemMod) {
        let previous = self.test_context;
        self.test_context |= i.attrs.iter().any(attribute_enables_test_code);
        if let Some((_, items)) = &i.content {
            self.modules.push(items);
            let blocks = take(&mut self.blocks);
            let functions = take(&mut self.functions);
            let in_const = replace(&mut self.in_const, false);
            visit::visit_item_mod(self, i);
            self.in_const = in_const;
            self.functions = functions;
            self.blocks = blocks;
            self.modules.pop();
        }
        self.test_context = previous;
    }

    fn visit_item_impl(&mut self, i: &'ast ItemImpl) {
        let previous = self.test_context;
        self.test_context |= i.attrs.iter().any(attribute_enables_test_code);
        let drop_impl = i.trait_.as_ref().is_some_and(|(_, path, _)| {
            path.segments
                .last()
                .is_some_and(|segment| segment.ident == "Drop")
        });
        let previous_drop_impl = replace(&mut self.test_drop_impl, drop_impl);
        let previous_fixture_fields = replace(
            &mut self.current_fixture_fields,
            self.fixture_struct_fields
                .get(&RustCollector::simple_type_name(&i.self_ty).unwrap_or_default())
                .cloned()
                .unwrap_or_default(),
        );
        visit::visit_item_impl(self, i);
        self.current_fixture_fields = previous_fixture_fields;
        self.test_drop_impl = previous_drop_impl;
        self.test_context = previous;
    }

    fn visit_block(&mut self, i: &'ast Block) {
        self.blocks.push(BlockScope {
            items: i
                .stmts
                .iter()
                .filter_map(|statement| match statement {
                    Stmt::Item(item) => Some(item),
                    _ => None,
                })
                .collect(),
            ..BlockScope::default()
        });
        visit::visit_block(self, i);
        self.blocks.pop();
    }

    fn visit_local(&mut self, i: &'ast Local) {
        self.with_attributes(|this| visit::visit_local(this, i));
        let fixture = self
            .functions
            .role
            .is_test()
            .then(|| self.local_fixture(i))
            .flatten();
        if let Some(block) = self.blocks.last_mut() {
            record_bindings(&mut block.bindings, &i.pat);
            if let Some((name, kind)) = fixture {
                block.bindings.insert(name, Some(kind));
            }
        }
    }

    fn visit_expr_closure(&mut self, i: &'ast ExprClosure) {
        let mut parameters = BlockScope::default();
        for input in &i.inputs {
            record_bindings(&mut parameters.bindings, input);
        }
        self.blocks.push(parameters);
        // A closure body is deferred even if the closure is constructed in a const.
        let previous = replace(&mut self.in_const, false);
        visit::visit_expr_closure(self, i);
        self.in_const = previous;
        self.blocks.pop();
    }

    fn visit_item_const(&mut self, i: &'ast ItemConst) {
        self.in_constant(|this| visit::visit_item_const(this, i));
    }

    fn visit_item_static(&mut self, i: &'ast ItemStatic) {
        self.in_constant(|this| visit::visit_item_static(this, i));
    }

    fn visit_impl_item_const(&mut self, i: &'ast ImplItemConst) {
        self.in_constant(|this| visit::visit_impl_item_const(this, i));
    }

    fn visit_trait_item_const(&mut self, i: &'ast TraitItemConst) {
        self.in_constant(|this| visit::visit_trait_item_const(this, i));
    }

    fn visit_expr_const(&mut self, i: &'ast ExprConst) {
        self.in_constant(|this| visit::visit_expr_const(this, i));
    }

    fn visit_macro(&mut self, i: &'ast Macro) {
        let Some(name) = i
            .path
            .segments
            .last()
            .map(|segment| segment.ident.to_string())
        else {
            return;
        };
        let Some(mut kind) = rust_macro_kind(&name) else {
            return;
        };
        // A test-owned destructor can deliberately trigger an unwind. An
        // ordinary function named `drop` carries no such meaning.
        if kind == "unexpectedBranch"
            && (self.functions.expects_panic || self.functions.test_destructor)
        {
            return;
        }
        if kind == "assertion" && self.in_const {
            kind = "compileTimeAssertion";
        }
        let test_owned = self.test_context || self.functions.role.is_test();
        if kind == "compileTimeAssertion" && !test_owned {
            return;
        }
        if kind == "compileTimeAssertion" || self.functions.name.is_some() {
            self.push_span(kind, i.span());
        }
    }

    fn visit_expr_call(&mut self, i: &'ast ExprCall) {
        for attribute in &i.attrs {
            self.visit_attribute(attribute);
        }
        if self.functions.role.is_test()
            && let Some(function) = self.resolve_function(&i.func)
        {
            if let Some(test_id) = self.functions.role.test_id() {
                // A regular call is enough to link even if this caller also
                // invokes the helper inside a panic expectation.
                let call = self
                    .calls
                    .entry(function_id(function))
                    .or_default()
                    .entry(test_id.clone())
                    .or_insert_with(|| NativeCall {
                        normal: false,
                        evidence: caller_evidence(
                            self.path,
                            self.lines,
                            test_id,
                            i.span(),
                            if matches!(i.func.as_ref(), Expr::Path(path) if path.path.segments.len() > 1)
                            {
                                "nativeQualified"
                            } else {
                                "nativeDirect"
                            },
                            i,
                        ),
                    });
                call.normal |= !self.functions.expects_panic;
            }
            if function.block.stmts.is_empty() && function_has_bounded_type_parameter(&function.sig)
            {
                self.push_range(
                    "compileTimeAssertion",
                    i.func.span().start(),
                    i.span().end(),
                );
            }
        }
        self.visit_call_children(i);
    }

    fn visit_expr_method_call(&mut self, i: &'ast ExprMethodCall) {
        for attribute in &i.attrs {
            self.visit_attribute(attribute);
        }
        if self.functions.role.is_test()
            && i.method == "withf"
            && i.args.len() == 1
            && let Some(argument) = i.args.first()
            && matches!(
                unparenthesized(&i.receiver),
                Expr::MethodCall(expectation) if expectation.method.to_string().starts_with("expect_")
            )
        {
            self.push_range(
                "mockArgumentCheck",
                argument.span().start(),
                argument.span().end(),
            );
        }
        if self.functions.role.is_test()
            && i.method == "times"
            && !i.args.is_empty()
            && matches!(
                unparenthesized(&i.receiver),
                Expr::MethodCall(expectation) if expectation.method.to_string().starts_with("expect_")
                    || expectation.method == "withf"
            )
        {
            self.push_range("mockCallCount", i.method.span().start(), i.span().end());
        }
        if i.method == "expect" && self.functions.role.is_test() {
            self.push_range(
                "fallibleValueCheck",
                i.receiver.span().start(),
                i.span().end(),
            );
            if self.is_fixture_access(&i.receiver) {
                let reason = Self::fixture_exclusion_reason(&i.receiver);
                // Keep this candidate's ID stable for subsequent source sites,
                // while removing the false predicate after scanning the file.
                if let Some(assertion) = self.assertions.last_mut() {
                    assertion.fixture_excluded = true;
                    assertion.fixture_exclusion_reason = Some(reason);
                }
            }
        }
        self.visit_expr(&i.receiver);
        if let Some(arguments) = &i.turbofish {
            self.visit_angle_bracketed_generic_arguments(arguments);
        }
        for argument in &i.args {
            self.visit_expr(argument);
        }
    }

    fn visit_expr_path(&mut self, i: &'ast ExprPath) {
        if self.visiting_direct_call_target || !self.functions.role.is_test() {
            visit::visit_expr_path(self, i);
            return;
        }
        if let (Some(function), Some(test_id)) =
            (self.resolve_function_path(i), self.functions.role.test_id())
        {
            self.candidate_calls
                .entry(function_id(function))
                .or_default()
                .entry(test_id.clone())
                .or_insert_with(|| CallerEvidence {
                    kind: "unresolvedCandidate".to_owned(),
                    test_id: test_id.clone(),
                    path: self.path.to_owned(),
                    line: parser_line(i.span().start().line),
                    end_line: i.span().end().line,
                    column: NonZeroUsize::MIN.saturating_add(
                        source_line(self.lines, i.span().start().line)
                            .map_or(0, |text| byte_column(text, i.span().start())),
                    ),
                    expression: source_slice(self.lines, i.span().start(), i.span().end()),
                });
        }
        visit::visit_expr_path(self, i);
    }
}

/// Restricts shared assertion ownership to files that contain test-only modules.
fn is_test_path(path: &str) -> bool {
    let lower = path.replace('\\', "/").to_ascii_lowercase();
    lower.starts_with("tests/")
        || lower.contains("/tests/")
        || lower.ends_with("_test.rs")
        || lower.ends_with(".tests.rs")
}

/// Recognizes an exact test-only cfg without treating production feature gates as tests.
fn attribute_enables_test_code(attribute: &Attribute) -> bool {
    if !attribute.path().is_ident("cfg") {
        return false;
    }
    let Meta::List(list) = &attribute.meta else {
        return false;
    };
    list.tokens.to_string() == "test"
}

/// Identifies attributes that can change whether their owner is compiled.
fn attribute_is_inclusion_gate(attribute: &Attribute) -> bool {
    if attribute.path().is_ident("cfg") {
        return true;
    }
    if !attribute.path().is_ident("cfg_attr") {
        return false;
    }
    let Meta::List(list) = &attribute.meta else {
        return false;
    };
    let Ok(arguments) = syn::parse2::<CfgAttrArguments>(list.tokens.clone()) else {
        return false;
    };
    arguments
        .attributes
        .iter()
        .any(|meta| meta.path().is_ident("cfg"))
}

/// Parses the condition and applied metadata of one `cfg_attr`.
struct CfgAttrArguments {
    attributes: Vec<Meta>,
}

impl syn::parse::Parse for CfgAttrArguments {
    fn parse(input: syn::parse::ParseStream<'_>) -> syn::Result<Self> {
        let condition: Meta = input.parse()?;
        let _condition = condition;
        _ = input.parse::<Comma>()?;
        let attributes = Punctuated::<Meta, Comma>::parse_terminated(input)?
            .into_iter()
            .collect();
        Ok(Self { attributes })
    }
}

/// Determines whether a file can anchor an explicit `crate::` path.
fn is_crate_root(path: &str) -> bool {
    let segments = path.split('/').collect::<Vec<_>>();
    // Files declared via custom Cargo targets or module paths need build
    // metadata to resolve `crate::`; only conventional roots are inferred.
    matches!(
        segments.as_slice(),
        ["lib.rs" | "main.rs"]
            | [.., "src", "lib.rs" | "main.rs"]
            | [.., "src", "bin", _]
            | [.., "src", "bin", _, "main.rs"]
            | [.., "tests", _]
    )
}

/// Identifies established test helpers that assert a closure unwinds.
fn is_panic_expectation_call(expression: &Expr) -> bool {
    matches!(unparenthesized(expression), Expr::Path(path)
        if path.qself.is_none() && path.path.segments.last()
            .is_some_and(|segment| segment.ident == "assert_panics" || segment.ident == "assert_panics_with"))
}

/// Uses declaration coordinates independent of how the caller spelled its path.
fn function_id(function: &ItemFn) -> FunctionId {
    let start = function.sig.ident.span().start();
    (start.line, start.column)
}

/// Selects an exact name only when alternative declarations do not compete.
fn unique_named_item<'ast>(
    items: impl Iterator<Item = &'ast Item>,
    name: &str,
) -> Result<Option<&'ast Item>, ()> {
    let mut candidates = items.filter(|item| {
        let ident = match item {
            Item::Fn(item) => &item.sig.ident,
            Item::Mod(item) => &item.ident,
            Item::Const(item) => &item.ident,
            Item::Static(item) => &item.ident,
            Item::Struct(item) => &item.ident,
            Item::Enum(item) => &item.ident,
            Item::Type(item) => &item.ident,
            _ => return false,
        };
        ident == name
    });
    let candidate = candidates.next();
    if candidates.next().is_some() {
        Err(())
    } else {
        Ok(candidate)
    }
}

/// Follows module qualifiers, stopping at an external module or unknown binding.
fn walk_items<'ast>(items: &'ast [Item], names: &[String]) -> Option<&'ast Item> {
    let (first, remaining) = names.split_first()?;
    walk_child(unique_named_item(items.iter(), first).ok()??, remaining)
}

/// Requires every nonterminal component to be an inline module declaration.
fn walk_child<'ast>(item: &'ast Item, remaining: &[String]) -> Option<&'ast Item> {
    if remaining.is_empty() {
        return Some(item);
    }
    let Item::Mod(module) = item else {
        return None;
    };
    walk_items(&module.content.as_ref()?.1, remaining)
}

/// Treats unresolvable imports as possible competing names instead of guessing.
fn import_could_supply(item: &Item, name: &str) -> bool {
    /// Reports whether a use tree could introduce the name into its scope.
    fn introduces(tree: &UseTree, name: &str) -> bool {
        match tree {
            UseTree::Path(path) => {
                matches!(path.tree.as_ref(), UseTree::Name(leaf) if leaf.ident == "self" && path.ident == name)
                    || introduces(&path.tree, name)
            }
            UseTree::Name(leaf) => leaf.ident == name,
            UseTree::Rename(rename) => rename.rename == name,
            UseTree::Glob(_) => true,
            UseTree::Group(group) => group.items.iter().any(|item| introduces(item, name)),
        }
    }
    matches!(item, Item::Use(import) if introduces(&import.tree, name))
}

/// Identifies a standard fixture imported in this scope, if the name is present.
fn fixture_import<'ast>(
    items: impl Iterator<Item = &'ast Item> + Clone,
    namespace: &str,
    name: &str,
) -> ControlFlow<bool> {
    /// Searches groups without erasing any of the import's path qualifiers.
    fn imports_path(tree: &UseTree, path: &[&str]) -> bool {
        match (tree, path) {
            (UseTree::Path(tree), [first, rest @ ..]) => {
                tree.ident == *first && imports_path(&tree.tree, rest)
            }
            (UseTree::Name(tree), [last]) => tree.ident == *last,
            (UseTree::Group(group), _) => group.items.iter().any(|tree| imports_path(tree, path)),
            _ => false,
        }
    }
    /// Distinguishes explicit imports from glob imports that compete only by possibility.
    fn explicitly_introduces(tree: &UseTree, name: &str) -> bool {
        match tree {
            UseTree::Path(path) => explicitly_introduces(&path.tree, name),
            UseTree::Name(leaf) => leaf.ident == name,
            UseTree::Rename(rename) => rename.rename == name,
            UseTree::Group(group) => group
                .items
                .iter()
                .any(|item| explicitly_introduces(item, name)),
            UseTree::Glob(_) => false,
        }
    }
    if !matches!(unique_named_item(items.clone(), name), Ok(None)) {
        return Break(false);
    }
    let mut standard = false;
    let mut possible = false;
    let mut explicit_conflict = false;
    for import in items.filter_map(|item| match item {
        Item::Use(import) if import_could_supply(item, name) => Some(import),
        _ => None,
    }) {
        possible = true;
        if imports_path(&import.tree, &["std", namespace, name]) {
            standard |= true;
        } else if explicitly_introduces(&import.tree, name) {
            explicit_conflict = true;
        }
    }
    // An explicit Rust import takes precedence over competing glob imports.
    (standard && !explicit_conflict)
        .then_some(Break(true))
        .or_else(|| possible.then_some(Break(false)))
        .unwrap_or(Continue(()))
}

/// Marks pattern bindings as shadows until a fixture constructor proves their type.
fn record_bindings(bindings: &mut BTreeMap<String, Option<FixtureKind>>, pattern: &Pat) {
    struct Bindings<'a>(&'a mut BTreeMap<String, Option<FixtureKind>>);

    impl<'ast> Visit<'ast> for Bindings<'_> {
        fn visit_pat_ident(&mut self, i: &'ast PatIdent) {
            self.0.insert(i.ident.to_string(), None);
            visit::visit_pat_ident(self, i);
        }
    }
    Bindings(bindings).visit_pat(pattern);
}

/// Captures the exact syntax that links a registered test to a native helper.
fn caller_evidence(
    path: &str,
    lines: &[&str],
    test_id: &str,
    span: Span,
    kind: &str,
    call: &ExprCall,
) -> CallerEvidence {
    let start = span.start();
    CallerEvidence {
        kind: kind.to_owned(),
        test_id: test_id.to_owned(),
        path: path.to_owned(),
        line: parser_line(start.line),
        end_line: span.end().line,
        column: NonZeroUsize::MIN.saturating_add(
            source_line(lines, start.line).map_or(0, |text| byte_column(text, start)),
        ),
        expression: source_slice(lines, call.span().start(), call.span().end()),
    }
}

/// Resolves synchronization fields and `Option` holders in test-owned structs.
fn fixture_struct_fields(items: &[Item]) -> BTreeMap<String, BTreeMap<String, FixtureKind>> {
    fn sync_fixture_type(items: &[Item], ty: &Type) -> Option<FixtureKind> {
        let Type::Path(path) = ty else {
            return None;
        };
        if path.qself.is_some() {
            return None;
        }
        let names = path
            .path
            .segments
            .iter()
            .map(|segment| &segment.ident)
            .collect::<Vec<_>>();
        let terminal = names.last()?.to_string();
        let (namespace, kind) = match terminal.as_str() {
            "Mutex" => ("sync", FixtureKind::Mutex),
            "RwLock" => ("sync", FixtureKind::RwLock),
            "JoinHandle" => ("thread", FixtureKind::JoinHandle),
            _ => return None,
        };
        if matches!(
            names.as_slice(),
            [root, module, _] if *root == "std" && *module == namespace
        ) {
            return Some(kind);
        }
        if path.path.leading_colon.is_some() || names.len() != 1 {
            return None;
        }
        matches!(
            fixture_import(items.iter(), namespace, &terminal),
            Break(true)
        )
        .then_some(kind)
    }

    fn collect(items: &[Item], result: &mut BTreeMap<String, BTreeMap<String, FixtureKind>>) {
        let option_fields = items
            .iter()
            .filter_map(|item| match item {
                Item::Struct(struct_) => {
                    let fields = struct_
                        .fields
                        .iter()
                        .filter_map(|field| {
                            let Type::Path(path) = &field.ty else {
                                return None;
                            };
                            let is_option = path
                                .path
                                .segments
                                .last()
                                .is_some_and(|segment| segment.ident == "Option");
                            let name = field.ident.as_ref()?.to_string();
                            is_option.then_some(name)
                        })
                        .collect::<BTreeSet<_>>();
                    (!fields.is_empty()).then(|| (struct_.ident.to_string(), fields))
                }
                _ => None,
            })
            .collect::<BTreeMap<_, _>>();
        for item in items {
            let Item::Struct(struct_) = item else {
                continue;
            };
            let fields = struct_
                .fields
                .iter()
                .filter_map(|field| {
                    Some((
                        field.ident.as_ref()?.to_string(),
                        sync_fixture_type(items, &field.ty)?,
                    ))
                })
                .collect::<BTreeMap<_, _>>();
            if !fields.is_empty() {
                result
                    .entry(struct_.ident.to_string())
                    .or_default()
                    .extend(fields);
            }
        }
        for item in items {
            if let Item::Impl(impl_) = item
                && let Some(name) = RustCollector::simple_type_name(&impl_.self_ty)
                && let Some(fields) = option_fields.get(&name)
            {
                let mut initialized = BTreeSet::new();
                for impl_item in &impl_.items {
                    let ImplItem::Fn(function) = impl_item else {
                        continue;
                    };
                    if function.sig.ident != "new" {
                        continue;
                    }
                    let mut visitor = SomeFieldVisitor {
                        fields,
                        initialized: BTreeSet::new(),
                    };
                    visitor.visit_block(&function.block);
                    initialized.extend(visitor.initialized);
                }
                result.entry(name).or_default().extend(
                    initialized
                        .into_iter()
                        .map(|field| (field, FixtureKind::OptionHolder)),
                );
            }
            if let Item::Mod(module) = item
                && let Some((_, nested)) = &module.content
            {
                collect(nested, result);
            }
        }
    }

    struct SomeFieldVisitor<'a> {
        fields: &'a BTreeSet<String>,
        initialized: BTreeSet<String>,
    }

    impl<'ast> Visit<'ast> for SomeFieldVisitor<'_> {
        fn visit_expr_struct(&mut self, i: &'ast syn::ExprStruct) {
            for field in &i.fields {
                let syn::Member::Named(member) = &field.member else {
                    continue;
                };
                let name = member.to_string();
                if !self.fields.contains(&name) {
                    continue;
                }
                let Expr::Call(call) = unparenthesized(&field.expr) else {
                    continue;
                };
                let Expr::Path(path) = call.func.as_ref() else {
                    continue;
                };
                if path.path.is_ident("Some") {
                    self.initialized.insert(name);
                }
            }
            visit::visit_expr_struct(self, i);
        }
    }

    let mut result = BTreeMap::new();
    collect(items, &mut result);
    result
}

/// Recognizes pointer conversions and explicitly constructed literal fixtures.
fn is_pointer_fixture_access(expression: &Expr) -> bool {
    let Expr::MethodCall(call) = unparenthesized(expression) else {
        return false;
    };
    if !call.args.is_empty() || (call.method != "as_ref" && call.method != "as_mut") {
        return false;
    }

    match unparenthesized(&call.receiver) {
        Expr::RawAddr(_) => true,
        Expr::Cast(cast) => matches!(cast.ty.as_ref(), Type::Ptr(_)),
        Expr::MethodCall(get) if get.args.is_empty() && get.method == "get" => true,
        // A literal wrapped directly in `Some` cannot probe an API result.
        Expr::Call(constructor) => {
            matches!(constructor.func.as_ref(), Expr::Path(path) if path.path.is_ident("Some"))
                && matches!(constructor.args.first(), Some(Expr::Lit(_)))
        }
        _ => false,
    }
}

/// Recognizes common direct nonempty assertions without treating variable names as contracts.
fn length_asserts_nonempty(source: &str, receiver: &str) -> bool {
    let not_empty = format!("!{receiver}.is_empty()");
    if source.contains(&not_empty) {
        return true;
    }
    let len_marker = format!("{receiver}.len()");
    let Some(after_len) = source.split_once(&len_marker).map(|(_, tail)| tail) else {
        return false;
    };
    if !source.contains("assert_eq!(") {
        return after_len.contains("> 0")
            || after_len.contains(">= 1")
            || after_len.contains("!= 0");
    }
    let Some((_, arguments)) = after_len.split_once(',') else {
        return false;
    };
    arguments
        .chars()
        .find(char::is_ascii_digit)
        .is_some_and(|digit| digit != '0')
}

/// Recognizes a generic type parameter whose trait bound can reject a call.
fn function_has_bounded_type_parameter(signature: &Signature) -> bool {
    signature
        .generics
        .type_params()
        .any(|parameter| !parameter.bounds.is_empty())
        || signature
            .generics
            .where_clause
            .as_ref()
            .is_some_and(|clause| {
                clause.predicates.iter().any(|predicate| {
                    matches!(
                        predicate,
                        WherePredicate::Type(predicate)
                            if !predicate.bounds.is_empty()
                    )
                })
            })
}

/// Removes grouping without interpreting any runtime expression.
fn unparenthesized(mut expression: &Expr) -> &Expr {
    loop {
        match expression {
            Expr::Group(group) => expression = &group.expr,
            Expr::Paren(paren) => expression = &paren.expr,
            _ => return expression,
        }
    }
}

/// Classifies supported attributes without interpreting their test framework semantics.
fn test_attributes(attributes: &[Attribute]) -> Option<&'static str> {
    attributes.iter().find_map(|attribute| {
        let name = attribute
            .path()
            .segments
            .last()
            .map(|segment| segment.ident.to_string())?;
        match name.as_str() {
            "test" => Some("attributeTest"),
            "test_case" | "rstest" => Some("parameterizedTest"),
            "quickcheck" => Some("propertyTest"),
            _ => None,
        }
    })
}

/// Identifies assertion and failure macros while leaving custom names explicit diagnostics.
fn rust_macro_kind(name: &str) -> Option<&'static str> {
    if matches!(
        name,
        "assert_impl_all"
            | "assert_not_impl_any"
            | "assert_eq_size"
            | "assert_eq_size_val"
            | "const_assert"
            | "const_assert_eq"
            | "const_assert_ne"
            | "assert_type_eq_all"
            | "static_assert"
            | "static_assertions_const"
    ) {
        Some("compileTimeAssertion")
    } else if name.starts_with("assert")
        || name.starts_with("debug_assert")
        || name.starts_with("prop_assert")
        || name == "ensure"
    {
        Some("assertion")
    } else if matches!(name, "panic" | "unreachable") {
        Some("unexpectedBranch")
    } else {
        None
    }
}

/// Extracts one macro or attribute from the original source rather than reformatting syntax.
fn source_slice(lines: &[&str], start: LineColumn, end: LineColumn) -> String {
    let (Some(first), Some(last)) = (source_line(lines, start.line), source_line(lines, end.line))
    else {
        return String::new();
    };
    let start_byte = byte_column(first, start);
    let end_byte = byte_column(last, end);
    if start.line == end.line {
        return first.get(start_byte..end_byte).unwrap_or("").to_owned();
    }
    let middle = lines
        .get(start.line..end.line.saturating_sub(1))
        .unwrap_or_default();
    let first = first.get(start_byte..).unwrap_or("");
    let last = last.get(..end_byte).unwrap_or("");
    once(first)
        .chain(middle.iter().copied())
        .chain(once(last))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned()
}

/// Looks up a line using the parser's one-based line numbering.
fn source_line<'a>(lines: &[&'a str], line: usize) -> Option<&'a str> {
    lines.get(line.checked_sub(1)?).copied()
}

/// Converts line numbers from real Syn spans, which are always one-based.
fn parser_line(line: usize) -> NonZeroUsize {
    NonZeroUsize::new(line).expect("parsed source spans have positive line numbers")
}

/// Converts proc-macro character offsets to UTF-8 byte offsets.
fn byte_column(line: &str, location: LineColumn) -> usize {
    // `syn::parse_file` strips a leading BOM, but the inventory retains its bytes.
    let parsed = if location.line == 1 {
        line.strip_prefix('\u{feff}').unwrap_or(line)
    } else {
        line
    };
    let offset = parsed
        .char_indices()
        .nth(location.column)
        .map_or(parsed.len(), |(offset, _)| offset);
    line.len()
        .saturating_sub(parsed.len())
        .saturating_add(offset)
}

/// Keeps non-Rust and ignored fences open so their contents cannot start tests.
struct DocFence {
    marker: char,
    width: usize,
    test: Option<DocTest>,
}

/// Associates a Rust fence's assertions with its inventory entry.
struct DocTest {
    index: usize,
    mode: DocMode,
    body: String,
}

/// Determines which predicates can run in a recognized rustdoc fence.
#[derive(Clone, Copy, PartialEq, Eq)]
enum DocMode {
    Runs,
    ShouldPanic,
    CompileOnly,
    Ignored,
}

impl DocFence {
    /// Reads the Markdown delimiter independently of rustdoc's info string.
    fn delimiter(line: &str) -> Option<(char, usize, &str)> {
        let marker = line.chars().next()?;
        if !matches!(marker, '`' | '~') {
            return None;
        }
        let width = line
            .chars()
            .take_while(|&character| character == marker)
            .count();
        // Markdown fences require at least three matching delimiters.
        (width >= 3).then(|| (marker, width, line.get(width..).unwrap_or("").trim()))
    }

    /// A closing fence must use the same marker and be at least as wide.
    fn closes(&self, line: &str) -> bool {
        Self::delimiter(line).is_some_and(|(marker, width, info)| {
            marker == self.marker && width >= self.width && info.is_empty()
        })
    }
}

/// Interprets rustdoc flags independently of their position in the info string.
fn doctest_mode(info: &str) -> Option<DocMode> {
    let mut expects_panic = false;
    let mut compile_only = false;
    let flags = info
        .split(|character: char| character == ',' || character.is_whitespace())
        .filter(|flag| !flag.is_empty())
        .collect::<Vec<_>>();
    let ignored = flags
        .iter()
        .any(|flag| *flag == "ignore" || flag.starts_with("ignore-"));
    let mut in_reason = false;
    for flag in flags {
        match flag {
            "should_panic" => expects_panic = true,
            "compile_fail" | "no_run" => compile_only = true,
            "ignore" | "rust" | "test_harness" | "edition2015" | "edition2018" | "edition2021"
            | "edition2024" => {}
            _ if flag.starts_with("ignore-") => {}
            _ if in_reason || flag.starts_with('(') && ignored => {
                in_reason = !flag.ends_with(')');
            }
            _ => return None,
        }
    }
    if in_reason {
        return None;
    }
    Some(if ignored {
        DocMode::Ignored
    } else if compile_only {
        DocMode::CompileOnly
    } else if expects_panic {
        DocMode::ShouldPanic
    } else {
        DocMode::Runs
    })
}

/// Inventories rustdoc blocks separately because syn treats doc contents as attributes.
fn scan_doctests(
    tests: &mut Vec<TestCase>,
    assertions: &mut Vec<AssertionSite>,
    diagnostics: &mut Vec<ScanDiagnostic>,
    input: ScanInput<'_>,
) {
    let mut active: Option<DocFence> = None;
    for (index, line) in input.lines.iter().enumerate() {
        let trimmed = line.trim_start();
        let comment = trimmed
            .strip_prefix("///")
            .or_else(|| trimmed.strip_prefix("//!"))
            .map(str::trim_start);
        let line_number = index.saturating_add(1);
        if let Some(fence) = &mut active {
            if comment.is_some_and(|comment| fence.closes(comment)) {
                if let Some(doc) = active.take().and_then(|fence| fence.test) {
                    finish_doctest(tests, assertions, diagnostics, &input, doc, line_number);
                }
            } else if let Some(doc) = &mut fence.test {
                if let Some(comment) = comment {
                    let code = rustdoc_code_line(comment);
                    // Replace the stripped prefix with whitespace so Syn's
                    // byte columns map back to the source file unchanged.
                    doc.body
                        .extend(repeat_n(' ', line.len().saturating_sub(code.len())));
                    doc.body.push_str(code);
                }
                doc.body.push('\n');
            }
        } else if let Some(comment) = comment
            && let Some((marker, width, info)) = DocFence::delimiter(comment)
        {
            let test = doctest_mode(info)
                .map(|mode| open_doctest(tests, assertions, &input, line_number, comment, mode));
            active = Some(DocFence {
                marker,
                width,
                test,
            });
        }
    }
    if let Some(doc) = active.and_then(|fence| fence.test) {
        diagnostics.push(ScanDiagnostic {
            kind: "unterminatedDoctest".to_owned(),
            path: input.path.to_owned(),
            line: tests.get(doc.index).map(|test| test.line.get()),
            detail: "The rustdoc code fence has no closing delimiter".to_owned(),
        });
        finish_doctest(
            tests,
            assertions,
            diagnostics,
            &input,
            doc,
            input.lines.len(),
        );
    }
}

/// Removes rustdoc's hidden-line marker or its escape, not Rust attributes.
fn rustdoc_code_line(comment: &str) -> &str {
    if comment == "#" {
        ""
    } else if comment.starts_with("##") {
        comment.strip_prefix('#').unwrap_or(comment)
    } else {
        comment.strip_prefix("# ").unwrap_or(comment)
    }
}

/// Registers ownership before collecting predicates from the fence body.
fn open_doctest(
    tests: &mut Vec<TestCase>,
    assertions: &mut Vec<AssertionSite>,
    input: &ScanInput<'_>,
    line: usize,
    comment: &str,
    mode: DocMode,
) -> DocTest {
    let index = tests.len();
    let test = TestCase {
        id: next_test_id(tests),
        name: format!("doctest at {line}"),
        kind: if mode == DocMode::Ignored {
            "ignoredDoctest"
        } else {
            "doctest"
        }
        .to_owned(),
        language: input.language.to_owned(),
        path: input.path.to_owned(),
        line: parser_line(line),
        end_line: line,
        direct_assertion_ids: Vec::new(),
        shared_assertion_ids: Vec::new(),
    };
    if mode == DocMode::ShouldPanic {
        let column = NonZeroUsize::MIN.saturating_add(
            source_line(input.lines, line)
                .map_or(0, |source| source.len().saturating_sub(comment.len())),
        );
        assertions.push(doctest_assertion(
            assertions,
            &test,
            input.file_sha256,
            "panicExpectation",
            test.line..=test.line,
            column,
            comment.to_owned(),
        ));
    }
    tests.push(test);
    // A block accepts statements as well as item declarations. Its opening
    // line stands in for the fence opener so body line offsets stay aligned.
    DocTest {
        index,
        mode,
        body: "{\n".to_owned(),
    }
}

/// Parses the projected source and emits only predicates belonging to the fence.
fn finish_doctest(
    tests: &mut [TestCase],
    assertions: &mut Vec<AssertionSite>,
    diagnostics: &mut Vec<ScanDiagnostic>,
    input: &ScanInput<'_>,
    mut doc: DocTest,
    end_line: usize,
) {
    let Some(test) = tests.get_mut(doc.index) else {
        return;
    };
    test.end_line = end_line;
    // Ignored rustdoc blocks remain in the test inventory for discovery parity,
    // but rustdoc never executes or tests their source body.
    if doc.mode == DocMode::Ignored {
        return;
    }
    doc.body.push('}');
    let block = match syn::parse_str::<Block>(&doc.body) {
        Ok(block) => block,
        Err(error) => {
            diagnostics.push(ScanDiagnostic {
                kind: "rustdocParseLimitation".to_owned(),
                path: test.path.clone(),
                line: Some(test.line.get()),
                detail: error.to_string(),
            });
            return;
        }
    };
    let mut visitor = DocPredicates {
        mode: doc.mode,
        in_const: false,
        spans: Vec::new(),
    };
    visitor.visit_block(&block);
    let projected: Vec<&str> = doc.body.lines().collect();
    for (kind, span) in visitor.spans {
        let start = span.start();
        let end = span.end();
        let line = test.line.saturating_add(start.line.saturating_sub(1));
        let end_line = test.line.saturating_add(end.line.saturating_sub(1));
        let column = NonZeroUsize::MIN.saturating_add(
            source_line(&projected, start.line).map_or(0, |text| byte_column(text, start)),
        );
        assertions.push(doctest_assertion(
            assertions,
            test,
            input.file_sha256,
            kind,
            line..=end_line,
            column,
            source_slice(&projected, start, end),
        ));
    }
}

/// Collects syntax-backed doctest predicates without registering nested tests.
struct DocPredicates {
    mode: DocMode,
    in_const: bool,
    spans: Vec<(&'static str, Span)>,
}

impl<'ast> Visit<'ast> for DocPredicates {
    fn visit_macro(&mut self, i: &'ast Macro) {
        let Some(kind) = i
            .path
            .segments
            .last()
            .and_then(|segment| rust_macro_kind(&segment.ident.to_string()))
        else {
            return;
        };
        let kind = if self.in_const && kind == "assertion" {
            "compileTimeAssertion"
        } else {
            kind
        };
        if (self.mode != DocMode::CompileOnly || kind == "compileTimeAssertion")
            && !(self.mode == DocMode::ShouldPanic && kind == "unexpectedBranch")
        {
            self.spans.push((kind, i.span()));
        }
    }

    fn visit_expr_method_call(&mut self, i: &'ast ExprMethodCall) {
        if i.method == "expect"
            && !is_pointer_fixture_access(&i.receiver)
            && let Some(span) = i.receiver.span().join(i.span())
        {
            self.spans.push(("fallibleValueCheck", span));
        }
        visit::visit_expr_method_call(self, i);
    }

    fn visit_item_const(&mut self, i: &'ast ItemConst) {
        let previous = replace(&mut self.in_const, true);
        visit::visit_item_const(self, i);
        self.in_const = previous;
    }

    fn visit_expr_const(&mut self, i: &'ast ExprConst) {
        let previous = replace(&mut self.in_const, true);
        visit::visit_expr_const(self, i);
        self.in_const = previous;
    }
}

/// Binds a predicate inside a rustdoc fence to the containing doctest.
fn doctest_assertion(
    assertions: &[AssertionSite],
    test: &TestCase,
    file_sha256: &str,
    kind: &str,
    lines: RangeInclusive<NonZeroUsize>,
    column: NonZeroUsize,
    expression: String,
) -> AssertionSite {
    AssertionSite {
        id: next_assertion_id(assertions),
        kind: kind.to_owned(),
        language: test.language.clone(),
        path: test.path.clone(),
        line: *lines.start(),
        end_line: lines.end().get(),
        column,
        expression,
        file_sha256: file_sha256.to_owned(),
        owner_line: test.line,
        owner_end_line: test.end_line,
        test_code: true,
        fixture_excluded: false,
        fixture_exclusion_reason: None,
        containing_test_id: Some(test.id.clone()),
        helper_name: None,
        calling_test_ids: Vec::new(),
        caller_evidence: Vec::new(),
        unresolved_caller_candidates: Vec::new(),
    }
}

/// Locates a likely macro name in text when the Rust file cannot be parsed.
fn first_macro_name(line: &str) -> Option<&str> {
    let prefix = line.get(..line.find('!')?)?.trim();
    let name = prefix
        .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
        .next_back()?
        .trim();
    (!name.is_empty()).then_some(name)
}

/// Uses narrow text patterns when no parser is available or valid syntax is absent.
fn scan_heuristically(
    tests: &mut Vec<TestCase>,
    assertions: &mut Vec<AssertionSite>,
    diagnostics: &mut Vec<ScanDiagnostic>,
    path: &str,
    language: &str,
    file_sha256: &str,
    text: &str,
) {
    let lines: Vec<&str> = text.lines().collect();
    let file_test_start = tests.len();
    let mut follows_test_attribute = false;
    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if matches!(language, "csharp" | "java" | "kotlin")
            && (trimmed.starts_with("[Fact")
                || trimmed.starts_with("[Theory")
                || trimmed.starts_with("@Test"))
        {
            follows_test_attribute = true;
            continue;
        }
        if let Some(name) = heuristic_test_name(language, line, follows_test_attribute) {
            tests.push(TestCase {
                id: next_test_id(tests),
                name,
                kind: "heuristic".to_owned(),
                language: language.to_owned(),
                path: path.to_owned(),
                line: NonZeroUsize::MIN.saturating_add(index),
                end_line: lines.len(),
                direct_assertion_ids: Vec::new(),
                shared_assertion_ids: Vec::new(),
            });
        }
        if !trimmed.is_empty() && !trimmed.starts_with('[') && !trimmed.starts_with('@') {
            follows_test_attribute = false;
        }
    }
    let test_indexes: Vec<usize> = (file_test_start..tests.len()).collect();
    for pair in test_indexes.windows(2) {
        let (Some(current), Some(next)) = (pair.first(), pair.get(1)) else {
            continue;
        };
        let Some(end_line) = tests
            .get(*next)
            .map(|test| test.line.get().saturating_sub(1))
        else {
            continue;
        };
        if let Some(test) = tests.get_mut(*current) {
            test.end_line = end_line;
        }
    }
    for (index, source_line) in lines.iter().enumerate() {
        let line = index.saturating_add(1);
        let test = test_indexes
            .iter()
            .rev()
            .filter_map(|candidate| tests.get(*candidate))
            .find(|test| line >= test.line.get() && line <= test.end_line);
        if let Some(kind) = heuristic_assertion_kind(language, source_line) {
            assertions.push(AssertionSite {
                id: next_assertion_id(assertions),
                kind: kind.to_owned(),
                language: language.to_owned(),
                path: path.to_owned(),
                line: NonZeroUsize::MIN.saturating_add(index),
                end_line: line,
                column: NonZeroUsize::MIN.saturating_add(
                    source_line
                        .len()
                        .saturating_sub(source_line.trim_start().len()),
                ),
                expression: source_line.trim().to_owned(),
                file_sha256: file_sha256.to_owned(),
                owner_line: test.map_or_else(|| parser_line(line), |test| test.line),
                owner_end_line: test.map_or(line, |test| test.end_line),
                test_code: true,
                fixture_excluded: false,
                fixture_exclusion_reason: None,
                containing_test_id: test.map(|test| test.id.clone()),
                helper_name: None,
                calling_test_ids: test.map(|test| test.id.clone()).into_iter().collect(),
                caller_evidence: Vec::new(),
                unresolved_caller_candidates: Vec::new(),
            });
        }
    }
    if language != "rust" {
        diagnostics.push(ScanDiagnostic {
            kind: "heuristicLanguage".to_owned(),
            path: path.to_owned(),
            line: None,
            detail: format!(
                "{language} tests use language-specific text patterns; custom macros and unusual layouts require manual evidence"
            ),
        });
    }
}

/// Recognizes common test declaration shapes without claiming complete language coverage.
fn heuristic_test_name(language: &str, line: &str, follows_test_attribute: bool) -> Option<String> {
    let trimmed = line.trim();
    match language {
        "rust" => trimmed
            .strip_prefix("#[test]")
            .map(|_| "attribute test".to_owned()),
        "python" => {
            let rest = trimmed
                .strip_prefix("async def ")
                .or_else(|| trimmed.strip_prefix("def "))?;
            let name = rest.split(['(', ':']).next()?;
            name.starts_with("test_").then(|| name.to_owned())
        }
        "go" => {
            let rest = trimmed.strip_prefix("func ")?;
            let name = rest.split('(').next()?;
            name.starts_with("Test").then(|| name.to_owned())
        }
        "csharp" => {
            if follows_test_attribute
                && (trimmed.starts_with("public ")
                    || trimmed.starts_with("internal ")
                    || trimmed.starts_with("private "))
            {
                let name = trimmed
                    .split_once('(')?
                    .0
                    .split_whitespace()
                    .last()?
                    .to_owned();
                (!name.is_empty()).then(|| name.clone())
            } else {
                None
            }
        }
        "java" | "kotlin" => {
            if follows_test_attribute {
                let name = trimmed.split('(').next()?.split_whitespace().last()?;
                (!name.is_empty()).then(|| name.to_owned())
            } else {
                None
            }
        }
        "javascript" | "typescript" => ["test(", "it(", "test.only(", "it.only("]
            .iter()
            .find(|prefix| trimmed.starts_with(*prefix))
            .map(|_| trimmed.to_owned()),
        "powershell" => trimmed.strip_prefix("It ").map(str::to_owned),
        _ => None,
    }
}

/// Recognizes only predicates that visibly assert, fail, or expect an exception.
fn heuristic_assertion_kind(language: &str, line: &str) -> Option<&'static str> {
    let trimmed = line.trim();
    match language {
        "python" => {
            if trimmed.starts_with("assert ") || trimmed.starts_with("self.assert") {
                Some("assertion")
            } else if trimmed.contains("pytest.raises(") || trimmed.contains("assertRaises") {
                Some("panicExpectation")
            } else {
                None
            }
        }
        "go" => (trimmed.contains(".Fatal(")
            || trimmed.contains(".Fatalf(")
            || trimmed.contains(".Error(")
            || trimmed.contains(".Errorf(")
            || trimmed.contains("assert.")
            || trimmed.contains("require."))
        .then_some("assertion"),
        "csharp" => {
            if trimmed.contains("Assert.Throws")
                || trimmed.contains("Assert.ThrowsAsync")
                || trimmed.contains("Throws.")
            {
                Some("panicExpectation")
            } else if trimmed.contains("Assert.") || trimmed.contains("Should()") {
                Some("assertion")
            } else {
                None
            }
        }
        "java" | "kotlin" => {
            if trimmed.contains("assertThrows")
                || trimmed.contains("expected =")
                || trimmed.contains("shouldThrow")
            {
                Some("panicExpectation")
            } else if trimmed.starts_with("assert ")
                || trimmed.contains("Assertions.")
                || trimmed.contains("Assert.")
            {
                Some("assertion")
            } else {
                None
            }
        }
        "javascript" | "typescript" => {
            if trimmed.contains(".toThrow")
                || trimmed.contains("assert.throws")
                || trimmed.contains("assert.rejects")
            {
                Some("panicExpectation")
            } else if trimmed.contains("expect(")
                || trimmed.starts_with("assert.")
                || trimmed.contains(" assert.")
            {
                Some("assertion")
            } else {
                None
            }
        }
        "powershell" => (trimmed.contains("Should ")
            || trimmed.contains("Should -")
            || trimmed.contains("ShouldThrow"))
        .then_some("assertion"),
        "rust" => {
            if let Some(name) = first_macro_name(trimmed) {
                rust_macro_kind(name)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Detects raw predicate markers independently from structured source inventory.
fn cross_check(
    diagnostics: &mut Vec<ScanDiagnostic>,
    path: &str,
    text: &str,
    assertion_count: usize,
) {
    let markers = text
        .lines()
        .filter(|line| {
            let lower = line.to_ascii_lowercase();
            lower.contains("assert")
                || lower.contains("should_panic")
                || lower.contains(".expect(")
                || lower.contains("pytest.raises")
                || lower.contains("assertthrows")
                || lower.contains("t.throw")
        })
        .count();
    diagnostics.push(ScanDiagnostic {
        kind: "sourceCrossCheck".to_owned(),
        path: path.to_owned(),
        line: None,
        detail: format!(
            "Independent marker lines: {markers}; inventoried assertion sites: {assertion_count}"
        ),
    });
}

/// Attaches surviving predicates to their owning and calling tests.
fn attach_assertions(tests: &mut [TestCase], assertions: &mut Vec<AssertionSite>) {
    // Test-owned callbacks remain visible even without a resolvable caller.
    assertions.retain(|assertion| {
        assertion.test_code
            || assertion.containing_test_id.is_some()
            || !assertion.calling_test_ids.is_empty()
            || !assertion.unresolved_caller_candidates.is_empty()
    });
    for assertion in assertions.iter() {
        if let Some(test_id) = &assertion.containing_test_id
            && let Some(test) = tests.iter_mut().find(|test| test.id == *test_id)
        {
            test.direct_assertion_ids.push(assertion.id.clone());
            continue;
        }
        for test_id in &assertion.calling_test_ids {
            if let Some(test) = tests.iter_mut().find(|test| test.id == *test_id) {
                test.shared_assertion_ids.push(assertion.id.clone());
            }
        }
    }
}

/// Replaces provisional candidate counts after unexpected production panics are removed.
fn refresh_cross_checks(diagnostics: &mut [ScanDiagnostic], assertions: &[AssertionSite]) {
    for diagnostic in diagnostics
        .iter_mut()
        .filter(|diagnostic| diagnostic.kind == "sourceCrossCheck")
    {
        let marker_summary = diagnostic
            .detail
            .split_once("; inventoried assertion sites:")
            .map_or(diagnostic.detail.as_str(), |(summary, _)| summary);
        let count = assertions
            .iter()
            .filter(|assertion| assertion.path == diagnostic.path)
            .count();
        diagnostic.detail = format!("{marker_summary}; inventoried assertion sites: {count}");
    }
}

/// Verifies that no source moved while the complete inventory was assembled.
fn verify_indexed_files(repository_root: &Path, files: &[IndexedFile]) -> Result<(), AppError> {
    for file in files {
        let bytes = read_source(&repository_root.join(Path::new(&file.path))).map_err(|error| {
            caused_by(
                "staleState",
                format!(
                    "Cannot reread audited file '{}' before publication",
                    file.path
                ),
                error,
            )
        })?;
        if bytes.is_none_or(|bytes| digest(&bytes) != file.sha256) {
            return Err(fail(
                "staleState",
                format!("Audited file changed during initialization: {}", file.path),
            ));
        }
    }
    Ok(())
}

/// Assigns a sequential test ID within this audit inventory.
fn next_test_id(tests: &[TestCase]) -> String {
    format!("T{:06}", tests.len().saturating_add(1))
}

/// Assigns a sequential assertion ID within this audit inventory.
fn next_assertion_id(assertions: &[AssertionSite]) -> String {
    format!("A{:06}", assertions.len().saturating_add(1))
}

#[cfg(test)]
mod fuzz;
