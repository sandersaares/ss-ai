//! Deterministic source inventory with parser-backed Rust analysis.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use ohno::AppError;
use proc_macro2::LineColumn;
use sha2::{Digest, Sha256};
use ss_ai_runtime::{caused_by, fail};
use syn::spanned::Spanned;
use syn::visit::Visit;
use syn::{Block, ExprCall, ImplItemFn, ItemFn, Macro, Signature};

use crate::model::{
    AssertionSite, IndexEntry, IndexedFile, Inventory, RevisionKind, ScanDiagnostic, TestCase,
};

/// Prevents one generated or unexpected file from making an audit unbounded.
///
/// This cap accommodates ordinary source and generated documentation files
/// while preserving a predictable local-memory ceiling per file.
const MAX_FILE_BYTES: usize = 4 * 1024 * 1024;
/// Keeps oversized source lines usable in bounded search results.
const MAX_INDEX_EXCERPT_CHARACTERS: usize = 500;

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
    let mut test_calls: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut expected_panic_calls: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let files = collect_files(repository_root, scope_paths, &mut diagnostics)?;

    for path in files {
        let relative = relative_path(repository_root, &path)?;
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
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
        if bytes.len() > MAX_FILE_BYTES {
            diagnostics.push(ScanDiagnostic {
                kind: "oversizedFile".to_owned(),
                path: relative,
                line: None,
                detail: "The file exceeds the scan safety limit and was not analyzed".to_owned(),
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
        let language = language_for(&path);
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
        scan_file(
            &mut tests,
            &mut assertions,
            &mut diagnostics,
            ScanInput {
                path: &relative,
                language,
                file_sha256: &file_sha256,
                text: &text,
                test_calls: &mut test_calls,
                expected_panic_calls: &mut expected_panic_calls,
            },
        );
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
    link_shared_assertions(
        &mut tests,
        &mut assertions,
        &test_calls,
        &expected_panic_calls,
    );
    refresh_cross_checks(&mut diagnostics, &assertions);
    verify_indexed_files(repository_root, &indexed_files)?;

    let inventory = Inventory {
        schema_version: 1,
        audit_id: String::new(),
        repository_root: repository_root
            .to_str()
            .ok_or_else(|| fail("invalidInput", "Repository path must be UTF-8"))?
            .to_owned(),
        repository_revision: repository_revision.to_owned(),
        revision_kind,
        scope_paths: scope_paths.to_vec(),
        files: indexed_files,
        tests,
        assertions,
        diagnostics,
    };
    Ok((inventory, entries))
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
) -> Result<Vec<PathBuf>, AppError> {
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
        collect_path(&canonical, repository_root, &mut paths, diagnostics)?;
    }
    Ok(paths.into_iter().collect())
}

/// Walks a scope without following symbolic links outside or inside the repository.
fn collect_path(
    path: &Path,
    repository_root: &Path,
    paths: &mut BTreeSet<PathBuf>,
    diagnostics: &mut Vec<ScanDiagnostic>,
) -> Result<(), AppError> {
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
    if !metadata.is_dir() {
        return Ok(());
    }
    let mut children = fs::read_dir(path)?.collect::<Result<Vec<_>, _>>()?;
    children.sort_by_key(fs::DirEntry::file_name);
    for entry in children {
        let file_name = entry.file_name();
        if should_skip_directory(&file_name) {
            continue;
        }
        collect_path(&entry.path(), repository_root, paths, diagnostics)?;
    }
    Ok(())
}

/// Excludes generated state whose contents do not describe source contracts.
fn should_skip_directory(name: &OsStr) -> bool {
    matches!(
        name.to_str().unwrap_or_default(),
        ".git"
            | ".hg"
            | ".svn"
            | "target"
            | "node_modules"
            | ".venv"
            | "venv"
            | "bin"
            | "obj"
            | "dist"
            | "build"
            | "coverage"
            | ".idea"
            | ".vs"
            | ".ss-ai-test-audit"
    )
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
    for (offset, text) in lines.iter().enumerate() {
        if text.trim().is_empty() {
            continue;
        }
        entries.push(IndexEntry {
            path: path.to_owned(),
            line: offset.saturating_add(1),
            language: language.to_owned(),
            kind: index_kind(path, text),
            excerpt: truncate(text.trim(), MAX_INDEX_EXCERPT_CHARACTERS),
            repository_revision: repository_revision.to_owned(),
            file_sha256: file_sha256.to_owned(),
        });
    }
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
fn truncate(text: &str, max: usize) -> String {
    let mut result = text.chars().take(max).collect::<String>();
    if text.chars().count() > max {
        result.push_str("...");
    }
    result
}

/// Bundles one parsed file so scanner state stays separate from source identity.
struct ScanInput<'a> {
    path: &'a str,
    language: &'a str,
    file_sha256: &'a str,
    text: &'a str,
    test_calls: &'a mut BTreeMap<String, BTreeSet<String>>,
    expected_panic_calls: &'a mut BTreeMap<String, BTreeSet<String>>,
}

/// Selects parser-backed Rust scanning or explicit heuristic profiles.
fn scan_file(
    tests: &mut Vec<TestCase>,
    assertions: &mut Vec<AssertionSite>,
    diagnostics: &mut Vec<ScanDiagnostic>,
    input: ScanInput<'_>,
) {
    let ScanInput {
        path,
        language,
        file_sha256,
        text,
        test_calls,
        expected_panic_calls,
    } = input;
    if language == "rust" {
        match syn::parse_file(text) {
            Ok(file) => {
                let mut collector = RustCollector {
                    tests,
                    assertions,
                    path,
                    language,
                    file_sha256,
                    text,
                    current_test_id: None,
                    current_function: None,
                    current_function_is_test: false,
                    test_context: is_test_path(path),
                    test_calls,
                    expected_panic_calls,
                    expected_panic: false,
                };
                collector.visit_file(&file);
                scan_doctests(
                    tests,
                    assertions,
                    diagnostics,
                    path,
                    language,
                    file_sha256,
                    text,
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

/// Visits complete Rust syntax to distinguish test functions and shared macros.
struct RustCollector<'a> {
    tests: &'a mut Vec<TestCase>,
    assertions: &'a mut Vec<AssertionSite>,
    path: &'a str,
    language: &'a str,
    file_sha256: &'a str,
    text: &'a str,
    current_test_id: Option<String>,
    current_function: Option<String>,
    current_function_is_test: bool,
    test_context: bool,
    test_calls: &'a mut BTreeMap<String, BTreeSet<String>>,
    expected_panic_calls: &'a mut BTreeMap<String, BTreeSet<String>>,
    expected_panic: bool,
}

impl RustCollector<'_> {
    /// Adds a test and visits its body with direct-predicate ownership established.
    fn visit_function(
        &mut self,
        name: String,
        attributes: &[syn::Attribute],
        signature: &Signature,
        block: &Block,
    ) {
        let test = test_attributes(attributes);
        let function_is_test = self.test_context
            || test.is_some()
            || attributes.iter().any(attribute_enables_test_code);
        let expected_panic = attributes
            .iter()
            .any(|attribute| attribute.path().is_ident("should_panic"));
        let previous_test = self.current_test_id.take();
        let previous_function = self.current_function.take();
        let previous_function_is_test = self.current_function_is_test;
        let previous_expected = self.expected_panic;
        self.current_function = Some(name.clone());
        self.current_function_is_test = function_is_test;
        self.expected_panic = expected_panic;

        if let Some(kind) = test {
            let id = next_test_id(self.tests);
            let line = attributes
                .iter()
                .filter(|attribute| {
                    attribute.path().is_ident("should_panic")
                        || test_attributes(std::slice::from_ref(attribute)).is_some()
                })
                .map(|attribute| attribute.span().start().line)
                .min()
                .unwrap_or_else(|| signature.ident.span().start().line);
            self.tests.push(TestCase {
                id: id.clone(),
                name,
                kind: kind.to_owned(),
                language: self.language.to_owned(),
                path: self.path.to_owned(),
                line,
                end_line: block.span().end().line,
                direct_assertion_ids: Vec::new(),
                shared_assertion_ids: Vec::new(),
            });
            self.current_test_id = Some(id);
            if expected_panic
                && let Some(attribute) = attributes
                    .iter()
                    .find(|attribute| attribute.path().is_ident("should_panic"))
            {
                self.push_assertion(
                    "panicExpectation",
                    attribute.span().start().line,
                    attribute.span().end().line,
                    attribute.span().start().column.saturating_add(1),
                    source_slice(self.text, attribute.span().start(), attribute.span().end()),
                );
            }
        }

        syn::visit::visit_signature(self, signature);
        syn::visit::visit_block(self, block);
        self.current_test_id = previous_test;
        self.current_function = previous_function;
        self.current_function_is_test = previous_function_is_test;
        self.expected_panic = previous_expected;
    }

    /// Records a parser-confirmed source predicate with deterministic ownership.
    fn push_assertion(
        &mut self,
        kind: &str,
        line: usize,
        end_line: usize,
        column: usize,
        expression: String,
    ) {
        let mut containing_test_id = self.current_test_id.clone();
        if containing_test_id.is_none() && kind == "compileTimeAssertion" {
            let id = next_test_id(self.tests);
            self.tests.push(TestCase {
                id: id.clone(),
                name: format!("compile-time check at {line}"),
                kind: "compileTime".to_owned(),
                language: self.language.to_owned(),
                path: self.path.to_owned(),
                line,
                end_line,
                direct_assertion_ids: Vec::new(),
                shared_assertion_ids: Vec::new(),
            });
            containing_test_id = Some(id);
        }
        self.assertions.push(AssertionSite {
            id: next_assertion_id(self.assertions),
            kind: kind.to_owned(),
            language: self.language.to_owned(),
            path: self.path.to_owned(),
            line,
            end_line,
            column,
            expression,
            file_sha256: self.file_sha256.to_owned(),
            test_code: self.current_test_id.is_some() || self.current_function_is_test,
            containing_test_id,
            helper_name: if self.current_test_id.is_none() {
                self.current_function.clone()
            } else {
                None
            },
            calling_test_ids: Vec::new(),
        });
    }
}

impl<'ast> Visit<'ast> for RustCollector<'_> {
    fn visit_item_fn(&mut self, i: &'ast ItemFn) {
        self.visit_function(i.sig.ident.to_string(), &i.attrs, &i.sig, &i.block);
    }

    fn visit_impl_item_fn(&mut self, i: &'ast ImplItemFn) {
        self.visit_function(i.sig.ident.to_string(), &i.attrs, &i.sig, &i.block);
    }

    fn visit_item_mod(&mut self, i: &'ast syn::ItemMod) {
        let previous = self.test_context;
        if i.attrs.iter().any(attribute_enables_test_code) {
            self.test_context = true;
        }
        syn::visit::visit_item_mod(self, i);
        self.test_context = previous;
    }

    fn visit_item_impl(&mut self, i: &'ast syn::ItemImpl) {
        let previous = self.test_context;
        if i.attrs.iter().any(attribute_enables_test_code) {
            self.test_context = true;
        }
        syn::visit::visit_item_impl(self, i);
        self.test_context = previous;
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
        let kind = rust_macro_kind(&name);
        if let Some(kind) = kind {
            if kind == "unexpectedBranch"
                && (self.expected_panic
                    || self
                        .current_function
                        .as_deref()
                        .is_some_and(|name| name == "drop"))
            {
                return;
            }
            if kind == "compileTimeAssertion"
                || self.current_test_id.is_some()
                || self.current_function.is_some()
            {
                let start = i.span().start();
                let end = i.span().end();
                self.push_assertion(
                    kind,
                    start.line,
                    end.line,
                    start.column.saturating_add(1),
                    source_slice(self.text, start, end),
                );
            }
        }
    }

    fn visit_expr_call(&mut self, i: &'ast ExprCall) {
        let name = call_name(&i.func);
        if let (Some(test_id), Some(name)) = (&self.current_test_id, &name) {
            let calls = if self.expected_panic {
                &mut *self.expected_panic_calls
            } else {
                &mut *self.test_calls
            };
            calls
                .entry(test_id.clone())
                .or_default()
                .insert(name.clone());
        }
        let is_panic_expectation = name.as_deref().is_some_and(is_panic_expectation_call);
        if is_panic_expectation && (self.current_test_id.is_some() || self.current_function_is_test)
        {
            let start = i.span().start();
            let end = i.span().end();
            self.push_assertion(
                "panicExpectation",
                start.line,
                end.line,
                start.column.saturating_add(1),
                source_slice(self.text, start, end),
            );
            let previous_expected = self.expected_panic;
            self.expected_panic = true;
            syn::visit::visit_expr_call(self, i);
            self.expected_panic = previous_expected;
        } else {
            syn::visit::visit_expr_call(self, i);
        }
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
fn attribute_enables_test_code(attribute: &syn::Attribute) -> bool {
    if !attribute.path().is_ident("cfg") {
        return false;
    }
    let syn::Meta::List(list) = &attribute.meta else {
        return false;
    };
    list.tokens.to_string() == "test"
}

/// Resolves a direct function-call identifier without guessing method identity.
fn call_name(expression: &syn::Expr) -> Option<String> {
    let syn::Expr::Path(path) = expression else {
        return None;
    };
    path.path
        .segments
        .last()
        .map(|segment| segment.ident.to_string())
}

/// Identifies established test helpers that assert a closure unwinds.
fn is_panic_expectation_call(name: &str) -> bool {
    matches!(name, "assert_panics" | "assert_panics_with")
}

/// Classifies supported attributes without interpreting their test framework semantics.
fn test_attributes(attributes: &[syn::Attribute]) -> Option<&'static str> {
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
            | "const_assert"
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
fn source_slice(text: &str, start: LineColumn, end: LineColumn) -> String {
    let lines: Vec<&str> = text.lines().collect();
    if start.line == 0 || end.line == 0 || start.line > lines.len() || end.line > lines.len() {
        return String::new();
    }
    if start.line == end.line {
        return lines
            .get(start.line.saturating_sub(1))
            .map_or_else(String::new, |line| {
                line_slice(line, start.column, end.column)
            });
    }
    let mut output = Vec::new();
    if let Some(line) = lines.get(start.line.saturating_sub(1)) {
        output.push(line.get(start.column..).unwrap_or("").to_owned());
    }
    for line in lines
        .iter()
        .skip(start.line)
        .take(end.line.saturating_sub(start.line).saturating_sub(1))
    {
        output.push((*line).to_owned());
    }
    if let Some(line) = lines.get(end.line.saturating_sub(1)) {
        output.push(line_slice(line, 0, end.column));
    }
    output.join("\n").trim().to_owned()
}

/// Slices a source line at byte-safe proc-macro columns.
fn line_slice(line: &str, start: usize, end: usize) -> String {
    let start = line.floor_char_boundary(start.min(line.len()));
    let end = line.floor_char_boundary(end.min(line.len()));
    line.get(start..end).unwrap_or("").to_owned()
}

/// Inventories rustdoc blocks separately because syn treats doc contents as attributes.
fn scan_doctests(
    tests: &mut Vec<TestCase>,
    assertions: &mut Vec<AssertionSite>,
    diagnostics: &mut Vec<ScanDiagnostic>,
    path: &str,
    language: &str,
    file_sha256: &str,
    text: &str,
) {
    let lines: Vec<&str> = text.lines().collect();
    let mut active: Option<(usize, String, bool)> = None;
    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        let Some(comment) = trimmed
            .strip_prefix("///")
            .or_else(|| trimmed.strip_prefix("//!"))
        else {
            continue;
        };
        let comment = comment.trim_start();
        if comment.starts_with("```") {
            if let Some((start, id, _)) = active.take() {
                if let Some(test) = tests.iter_mut().find(|test| test.id == id) {
                    test.end_line = index.saturating_add(1);
                }
                if start == index {
                    diagnostics.push(ScanDiagnostic {
                        kind: "emptyDoctest".to_owned(),
                        path: path.to_owned(),
                        line: Some(index.saturating_add(1)),
                        detail: "The rustdoc code fence contains no executable lines".to_owned(),
                    });
                }
            } else {
                let fence = comment.trim_start_matches('`');
                if fence.is_empty()
                    || fence.starts_with("rust")
                    || fence.starts_with("no_run")
                    || fence.starts_with("compile_fail")
                    || fence.starts_with("should_panic")
                {
                    let expects_panic = fence.starts_with("should_panic");
                    let id = next_test_id(tests);
                    tests.push(TestCase {
                        id: id.clone(),
                        name: format!("doctest at {}", index.saturating_add(1)),
                        kind: "doctest".to_owned(),
                        language: language.to_owned(),
                        path: path.to_owned(),
                        line: index.saturating_add(1),
                        end_line: index.saturating_add(1),
                        direct_assertion_ids: Vec::new(),
                        shared_assertion_ids: Vec::new(),
                    });
                    if expects_panic {
                        assertions.push(AssertionSite {
                            id: next_assertion_id(assertions),
                            kind: "panicExpectation".to_owned(),
                            language: language.to_owned(),
                            path: path.to_owned(),
                            line: index.saturating_add(1),
                            end_line: index.saturating_add(1),
                            column: line.find(comment).unwrap_or_default().saturating_add(1),
                            expression: format!("```{fence}"),
                            file_sha256: file_sha256.to_owned(),
                            test_code: true,
                            containing_test_id: Some(id.clone()),
                            helper_name: None,
                            calling_test_ids: Vec::new(),
                        });
                    }
                    active = Some((index, id, expects_panic));
                }
            }
            continue;
        }
        let Some((_, id, expects_panic)) = &active else {
            continue;
        };
        let code = comment.trim_start_matches('#').trim_start();
        if let Some(name) = first_macro_name(code)
            && let Some(kind) = rust_macro_kind(&name)
        {
            if *expects_panic && kind == "unexpectedBranch" {
                continue;
            }
            assertions.push(AssertionSite {
                id: next_assertion_id(assertions),
                kind: kind.to_owned(),
                language: language.to_owned(),
                path: path.to_owned(),
                line: index.saturating_add(1),
                end_line: index.saturating_add(1),
                column: line.find(code).unwrap_or_default().saturating_add(1),
                expression: code.to_owned(),
                file_sha256: file_sha256.to_owned(),
                test_code: true,
                containing_test_id: Some(id.clone()),
                helper_name: None,
                calling_test_ids: Vec::new(),
            });
        }
    }
    if let Some((start, id, _)) = active
        && let Some(test) = tests.iter_mut().find(|test| test.id == id)
    {
        test.end_line = lines.len();
        diagnostics.push(ScanDiagnostic {
            kind: "unterminatedDoctest".to_owned(),
            path: path.to_owned(),
            line: Some(start.saturating_add(1)),
            detail: "The rustdoc code fence has no closing delimiter".to_owned(),
        });
    }
}

/// Extracts a simple macro identifier from one rustdoc source line.
fn first_macro_name(line: &str) -> Option<String> {
    let marker = line.find('!')?;
    let prefix = line.get(..marker)?.trim();
    let name = prefix
        .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
        .next_back()?
        .trim();
    (!name.is_empty()).then(|| name.to_owned())
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
                line: index.saturating_add(1),
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
        let Some(end_line) = tests.get(*next).map(|test| test.line.saturating_sub(1)) else {
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
            .find(|test| line >= test.line && line <= test.end_line)
            .map(|test| test.id.clone());
        if let Some(kind) = heuristic_assertion_kind(language, source_line) {
            assertions.push(AssertionSite {
                id: next_assertion_id(assertions),
                kind: kind.to_owned(),
                language: language.to_owned(),
                path: path.to_owned(),
                line,
                end_line: line,
                column: source_line
                    .len()
                    .saturating_sub(source_line.trim_start().len())
                    .saturating_add(1),
                expression: source_line.trim().to_owned(),
                file_sha256: file_sha256.to_owned(),
                test_code: true,
                containing_test_id: test.clone(),
                helper_name: None,
                calling_test_ids: test.into_iter().collect(),
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
                rust_macro_kind(&name)
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

/// Adds direct and helper relationships after all file scans are complete.
fn link_shared_assertions(
    tests: &mut [TestCase],
    assertions: &mut Vec<AssertionSite>,
    test_calls: &BTreeMap<String, BTreeSet<String>>,
    expected_panic_calls: &BTreeMap<String, BTreeSet<String>>,
) {
    for assertion in &mut *assertions {
        if let Some(test_id) = &assertion.containing_test_id
            && let Some(test) = tests.iter_mut().find(|test| test.id == *test_id)
        {
            test.direct_assertion_ids.push(assertion.id.clone());
            continue;
        }
        let Some(helper) = assertion.helper_name.as_deref() else {
            continue;
        };
        // `drop` is a trait callback as well as the prelude cleanup function, so
        // syntax cannot distinguish a callback invocation from an unrelated drop.
        if helper != "drop" {
            for test in tests.iter() {
                if test_calls
                    .get(&test.id)
                    .is_some_and(|calls| calls.contains(helper))
                {
                    assertion.calling_test_ids.push(test.id.clone());
                }
                if assertion.kind != "unexpectedBranch"
                    && expected_panic_calls
                        .get(&test.id)
                        .is_some_and(|calls| calls.contains(helper))
                {
                    assertion.calling_test_ids.push(test.id.clone());
                }
            }
        }
    }
    for assertion in &mut *assertions {
        assertion.calling_test_ids.sort();
        assertion.calling_test_ids.dedup();
    }

    // Production assertions are candidates until a test calls their free
    // helper. Method calls are deliberately unresolved, while test-owned
    // callbacks remain valid even when no static caller can be identified.
    assertions.retain(|assertion| {
        assertion.test_code
            || assertion.containing_test_id.is_some()
            || !assertion.calling_test_ids.is_empty()
    });
    for assertion in assertions.iter() {
        if assertion.containing_test_id.is_some() {
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
        let bytes = fs::read(repository_root.join(Path::new(&file.path))).map_err(|error| {
            caused_by(
                "staleState",
                format!(
                    "Cannot reread audited file '{}' before publication",
                    file.path
                ),
                error,
            )
        })?;
        if digest(&bytes) != file.sha256 {
            return Err(fail(
                "staleState",
                format!("Audited file changed during initialization: {}", file.path),
            ));
        }
    }
    Ok(())
}

/// Assigns globally stable IDs as records appear in source order.
fn next_test_id(tests: &[TestCase]) -> String {
    format!("T{:06}", tests.len().saturating_add(1))
}

/// Assigns globally stable IDs as source predicate sites appear.
fn next_assertion_id(assertions: &[AssertionSite]) -> String {
    format!("A{:06}", assertions.len().saturating_add(1))
}
