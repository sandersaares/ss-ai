//! Deterministic source inventory with parser-backed Rust analysis.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};

use ohno::AppError;
use proc_macro2::{Ident, LineColumn, Span};
use sha2::{Digest, Sha256};
use ss_ai_runtime::{caused_by, fail};
use syn::spanned::Spanned;
use syn::visit::Visit;
use syn::{
    Attribute, Block, Expr, ExprCall, ExprMethodCall, ImplItemFn, Item, ItemFn, Macro, Signature,
    Stmt, UseTree, visit,
};

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
    let mut assertion_diagnostics = Vec::new();
    let assertion_files: BTreeSet<PathBuf> =
        collect_files(repository_root, scope_paths, &mut assertion_diagnostics)?
            .into_iter()
            .collect();
    let _ = assertion_diagnostics;
    // The whole repository supplies public-contract evidence. The selected
    // scope controls tests and assertions only, so narrow audits can still cite
    // docs and declarations outside the asserted source tree.
    let files = collect_files(repository_root, &[".".to_owned()], &mut diagnostics)?;
    for path in files {
        let relative = relative_path(repository_root, &path)?;
        let bytes = match read_source(&path) {
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
        if assertion_files.contains(&path) {
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
    attach_assertions(&mut tests, &mut assertions);
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
    if !metadata.is_dir() || should_skip_directory(path, repository_root)? {
        return Ok(());
    }
    let mut children = fs::read_dir(path)?.collect::<Result<Vec<_>, _>>()?;
    children.sort_by_key(fs::DirEntry::file_name);
    for entry in children {
        collect_path(&entry.path(), repository_root, paths, diagnostics)?;
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
                    calls: BTreeMap::new(),
                    helper_assertions: Vec::new(),
                };
                collector.visit_file(&file);
                collector.link_helper_assertions();
                scan_doctests(
                    tests,
                    assertions,
                    diagnostics,
                    path,
                    language,
                    file_sha256,
                    lines,
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
    calls: BTreeMap<FunctionId, BTreeMap<String, bool>>,
    helper_assertions: Vec<(usize, FunctionId)>,
}

/// Separates diagnostic display names from the identities used for linking.
#[derive(Default)]
struct FunctionContext {
    test_id: Option<String>,
    name: Option<String>,
    helper: Option<FunctionId>,
    is_test: bool,
    expects_panic: bool,
    // Function items cannot capture variables from an enclosing function.
    first_binding_scope: usize,
}

/// Identifies a declaration by its position within the current parsed file.
type FunctionId = (usize, usize);

/// Holds block-local declarations and the bindings seen before the current use.
#[derive(Default)]
struct BlockScope<'ast> {
    items: Vec<&'ast Item>,
    bindings: BTreeMap<String, Option<FixtureKind>>,
}

/// Identifies receivers tied to standard synchronization or thread fixtures.
#[derive(Clone, Copy)]
enum FixtureKind {
    Mutex,
    RwLock,
    JoinHandle,
}

impl<'ast> RustCollector<'_, 'ast> {
    /// Adds a test and visits its body with direct-predicate ownership established.
    fn visit_function(
        &mut self,
        attributes: &'ast [Attribute],
        signature: &'ast Signature,
        block: &'ast Block,
        helper: Option<FunctionId>,
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
        let previous = std::mem::replace(
            &mut self.functions,
            FunctionContext {
                name: Some(name.clone()),
                helper,
                is_test: function_is_test,
                expects_panic: panic_attribute.is_some(),
                first_binding_scope: self.blocks.len(),
                ..FunctionContext::default()
            },
        );
        let previous_const = std::mem::replace(&mut self.in_const, false);

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
            self.functions.test_id = Some(id);
            if let Some(attribute) = panic_attribute {
                self.push_span("panicExpectation", attribute.span());
            }
        }

        let mut parameters = BlockScope::default();
        for input in &signature.inputs {
            if let syn::FnArg::Typed(argument) = input {
                record_bindings(&mut parameters.bindings, &argument.pat);
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
        let mut containing_test_id = self.functions.test_id.clone();
        let id = next_assertion_id(self.assertions);
        if kind == "compileTimeAssertion" && !self.cfg.is_empty() {
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
                line,
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
            line,
            end_line,
            column: source_line(self.lines, line)
                .map_or(0, |text| byte_column(text, start))
                .saturating_add(1),
            expression: source_slice(self.lines, start, end),
            file_sha256: self.file_sha256.to_owned(),
            test_code: self.functions.is_test,
            containing_test_id,
            helper_name: if self.functions.test_id.is_none() {
                self.functions.name.clone()
            } else {
                None
            },
            calling_test_ids: Vec::new(),
        });
    }

    /// Applies calls after visiting the whole file, regardless of declaration order.
    fn link_helper_assertions(&mut self) {
        for &(index, helper) in &self.helper_assertions {
            if let (Some(assertion), Some(callers)) =
                (self.assertions.get_mut(index), self.calls.get(&helper))
            {
                assertion.calling_test_ids.extend(
                    callers
                        .iter()
                        .filter(|&(_, normal_call)| {
                            *normal_call || assertion.kind != "unexpectedBranch"
                        })
                        .map(|(test_id, _)| test_id.clone()),
                );
            }
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
        let previous = std::mem::replace(&mut self.in_const, true);
        visit(self);
        self.in_const = previous;
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

    fn visit_impl_item(&mut self, i: &'ast syn::ImplItem) {
        self.with_attributes(|this| visit::visit_impl_item(this, i));
    }

    fn visit_trait_item(&mut self, i: &'ast syn::TraitItem) {
        self.with_attributes(|this| visit::visit_trait_item(this, i));
    }

    fn visit_expr(&mut self, i: &'ast Expr) {
        self.with_attributes(|this| visit::visit_expr(this, i));
    }

    fn visit_stmt_macro(&mut self, i: &'ast syn::StmtMacro) {
        self.with_attributes(|this| visit::visit_stmt_macro(this, i));
    }

    fn visit_arm(&mut self, i: &'ast syn::Arm) {
        let mut bindings = BlockScope::default();
        record_bindings(&mut bindings.bindings, &i.pat);
        self.blocks.push(bindings);
        self.with_attributes(|this| visit::visit_arm(this, i));
        self.blocks.pop();
    }

    fn visit_variant(&mut self, i: &'ast syn::Variant) {
        self.with_attributes(|this| visit::visit_variant(this, i));
    }

    fn visit_field(&mut self, i: &'ast syn::Field) {
        self.with_attributes(|this| visit::visit_field(this, i));
    }

    fn visit_attribute(&mut self, i: &'ast Attribute) {
        // The audit already selects `cfg(test)` code; other configurations
        // restrict which builds exercise the check within that selection.
        if (i.path().is_ident("cfg") || i.path().is_ident("cfg_attr"))
            && !attribute_enables_test_code(i)
        {
            self.cfg
                .push(source_slice(self.lines, i.span().start(), i.span().end()));
        }
    }

    fn visit_item_fn(&mut self, i: &'ast ItemFn) {
        self.visit_function(&i.attrs, &i.sig, &i.block, Some(function_id(i)));
    }

    fn visit_impl_item_fn(&mut self, i: &'ast ImplItemFn) {
        // An associated function needs type resolution; a terminal method name
        // cannot establish this identity.
        self.visit_function(&i.attrs, &i.sig, &i.block, None);
    }

    fn visit_item_mod(&mut self, i: &'ast syn::ItemMod) {
        let previous = self.test_context;
        self.test_context |= i.attrs.iter().any(attribute_enables_test_code);
        if let Some((_, items)) = &i.content {
            self.modules.push(items);
            let blocks = std::mem::take(&mut self.blocks);
            let functions = std::mem::take(&mut self.functions);
            let in_const = std::mem::replace(&mut self.in_const, false);
            visit::visit_item_mod(self, i);
            self.in_const = in_const;
            self.functions = functions;
            self.blocks = blocks;
            self.modules.pop();
        }
        self.test_context = previous;
    }

    fn visit_item_impl(&mut self, i: &'ast syn::ItemImpl) {
        let previous = self.test_context;
        self.test_context |= i.attrs.iter().any(attribute_enables_test_code);
        visit::visit_item_impl(self, i);
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

    fn visit_local(&mut self, i: &'ast syn::Local) {
        self.with_attributes(|this| visit::visit_local(this, i));
        let fixture = self
            .functions
            .is_test
            .then(|| self.local_fixture(i))
            .flatten();
        if let Some(block) = self.blocks.last_mut() {
            record_bindings(&mut block.bindings, &i.pat);
            if let Some((name, kind)) = fixture {
                block.bindings.insert(name, Some(kind));
            }
        }
    }

    fn visit_expr_closure(&mut self, i: &'ast syn::ExprClosure) {
        let mut parameters = BlockScope::default();
        for input in &i.inputs {
            record_bindings(&mut parameters.bindings, input);
        }
        self.blocks.push(parameters);
        // A closure body is deferred even if the closure is constructed in a const.
        let previous = std::mem::replace(&mut self.in_const, false);
        visit::visit_expr_closure(self, i);
        self.in_const = previous;
        self.blocks.pop();
    }

    fn visit_item_const(&mut self, i: &'ast syn::ItemConst) {
        self.in_constant(|this| visit::visit_item_const(this, i));
    }

    fn visit_item_static(&mut self, i: &'ast syn::ItemStatic) {
        self.in_constant(|this| visit::visit_item_static(this, i));
    }

    fn visit_impl_item_const(&mut self, i: &'ast syn::ImplItemConst) {
        self.in_constant(|this| visit::visit_impl_item_const(this, i));
    }

    fn visit_trait_item_const(&mut self, i: &'ast syn::TraitItemConst) {
        self.in_constant(|this| visit::visit_trait_item_const(this, i));
    }

    fn visit_expr_const(&mut self, i: &'ast syn::ExprConst) {
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
        if kind == "unexpectedBranch"
            && (self.functions.expects_panic || self.functions.name.as_deref() == Some("drop"))
        {
            return;
        }
        if kind == "assertion" && self.in_const {
            kind = "compileTimeAssertion";
        }
        if kind == "compileTimeAssertion" || self.functions.name.is_some() {
            self.push_span(kind, i.span());
        }
    }

    fn visit_expr_call(&mut self, i: &'ast ExprCall) {
        for attribute in &i.attrs {
            self.visit_attribute(attribute);
        }
        if self.functions.is_test
            && let Some(function) = self.resolve_function(&i.func)
        {
            if let Some(test_id) = &self.functions.test_id {
                // A regular call is enough to link even if this caller also
                // invokes the helper inside a panic expectation.
                *self
                    .calls
                    .entry(function_id(function))
                    .or_default()
                    .entry(test_id.clone())
                    .or_default() |= !self.functions.expects_panic;
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
        if i.method == "expect" && self.functions.is_test && !self.is_fixture_access(&i.receiver) {
            self.push_range(
                "fallibleValueCheck",
                i.receiver.span().start(),
                i.span().end(),
            );
        }
        self.visit_expr(&i.receiver);
        if let Some(arguments) = &i.turbofish {
            self.visit_angle_bracketed_generic_arguments(arguments);
        }
        for argument in &i.args {
            self.visit_expr(argument);
        }
    }
}

impl<'ast> RustCollector<'_, 'ast> {
    /// Visits call arguments while suppressing deliberately triggered expected panics.
    fn visit_call_children(&mut self, i: &'ast ExprCall) {
        let previous = self.functions.expects_panic;
        if self.functions.is_test && is_panic_expectation_call(&i.func) {
            self.push_range("panicExpectation", i.func.span().start(), i.span().end());
            self.functions.expects_panic = true;
        }
        self.visit_expr(&i.func);
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
        if path.qself.is_some() || path.path.leading_colon.is_some() {
            return None;
        }
        let names = path
            .path
            .segments
            .iter()
            .map(|segment| segment.ident.to_string())
            .collect::<Vec<_>>();
        let names = names.iter().map(String::as_str).collect::<Vec<_>>();
        let (&first, remaining) = names.split_first()?;
        let current = self.modules.last()?;
        let target = match first {
            "crate" if is_crate_root(self.path) => walk_items(self.modules.first()?, remaining),
            "crate" | "Self" => None,
            "self" => walk_items(current, remaining),
            "super" => {
                let parents = remaining
                    .iter()
                    .take_while(|&&name| name == "super")
                    .count();
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
        remaining: &[&str],
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
    fn local_fixture(&self, local: &syn::Local) -> Option<(String, FixtureKind)> {
        let (pattern, kind) = match &local.pat {
            syn::Pat::Type(typed) => {
                let syn::Type::Path(path) = typed.ty.as_ref() else {
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
                (
                    typed.pat.as_ref(),
                    self.fixture_type(&names, path.path.leading_colon.is_some())?,
                )
            }
            pattern => (
                pattern,
                self.fixture_constructor(&local.init.as_ref()?.expr)?,
            ),
        };
        let syn::Pat::Ident(ident) = pattern else {
            return None;
        };
        (ident.by_ref.is_none() && ident.subpat.is_none()).then(|| (ident.ident.to_string(), kind))
    }

    /// Recognizes standard constructors without trusting a receiver's variable name.
    fn fixture_constructor(&self, expression: &Expr) -> Option<FixtureKind> {
        let Expr::Call(call) = unparenthesized(expression) else {
            return None;
        };
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
            match fixture_import(block.items.iter().copied(), namespace, &terminal) {
                Some(imported) => return imported.then_some(kind),
                None => continue,
            }
        }
        fixture_import(module.iter(), namespace, &terminal)
            .unwrap_or(false)
            .then_some(kind)
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
        let fixture = if let Expr::Path(path) = unparenthesized(&call.receiver)
            && let Some(name) = path.path.get_ident()
        {
            self.blocks
                .iter()
                .skip(self.functions.first_binding_scope)
                .rev()
                .find_map(|block| block.bindings.get(&name.to_string()).copied())
                .flatten()
        } else {
            self.fixture_constructor(&call.receiver)
        };
        matches!(
            (fixture, call.method.to_string().as_str()),
            (Some(FixtureKind::Mutex), "lock" | "try_lock")
                | (
                    Some(FixtureKind::RwLock),
                    "read" | "try_read" | "write" | "try_write"
                )
                | (Some(FixtureKind::JoinHandle), "join")
        )
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
    let syn::Meta::List(list) = &attribute.meta else {
        return false;
    };
    list.tokens.to_string() == "test"
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
fn walk_items<'ast>(items: &'ast [Item], names: &[&str]) -> Option<&'ast Item> {
    let (first, remaining) = names.split_first()?;
    walk_child(unique_named_item(items.iter(), first).ok()??, remaining)
}

/// Requires every nonterminal component to be an inline module declaration.
fn walk_child<'ast>(item: &'ast Item, remaining: &[&str]) -> Option<&'ast Item> {
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
) -> Option<bool> {
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
    if !matches!(unique_named_item(items.clone(), name), Ok(None)) {
        return Some(false);
    }
    let mut imports = items.filter(|item| import_could_supply(item, name));
    let Item::Use(import) = imports.next()? else {
        return None;
    };
    Some(imports.next().is_none() && imports_path(&import.tree, &["std", namespace, name]))
}

/// Marks pattern bindings as shadows until a fixture constructor proves their type.
fn record_bindings(bindings: &mut BTreeMap<String, Option<FixtureKind>>, pattern: &syn::Pat) {
    struct Bindings<'a>(&'a mut BTreeMap<String, Option<FixtureKind>>);

    impl<'ast> Visit<'ast> for Bindings<'_> {
        fn visit_pat_ident(&mut self, i: &'ast syn::PatIdent) {
            self.0.insert(i.ident.to_string(), None);
            visit::visit_pat_ident(self, i);
        }
    }
    Bindings(bindings).visit_pat(pattern);
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
        Expr::Cast(cast) => matches!(cast.ty.as_ref(), syn::Type::Ptr(_)),
        // A literal wrapped directly in `Some` cannot probe an API result.
        Expr::Call(constructor) => {
            matches!(constructor.func.as_ref(), Expr::Path(path) if path.path.is_ident("Some"))
                && matches!(constructor.args.first(), Some(Expr::Lit(_)))
        }
        _ => false,
    }
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
                        syn::WherePredicate::Type(predicate)
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
    std::iter::once(first)
        .chain(middle.iter().copied())
        .chain(std::iter::once(last))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned()
}

/// Looks up a line using the parser's one-based line numbering.
fn source_line<'a>(lines: &[&'a str], line: usize) -> Option<&'a str> {
    lines.get(line.checked_sub(1)?).copied()
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
    expects_panic: bool,
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
fn doctest_expects_panic(info: &str) -> Option<bool> {
    let mut expects_panic = false;
    for flag in info
        .split(|character: char| character == ',' || character.is_whitespace())
        .filter(|flag| !flag.is_empty())
    {
        match flag {
            "should_panic" => expects_panic = true,
            "rust" | "compile_fail" | "no_run" | "test_harness" | "edition2015" | "edition2018"
            | "edition2021" | "edition2024" => {}
            // Includes `ignore` flags and other language identifiers; both
            // are tracked as fences but do not contribute test sites.
            _ => return None,
        }
    }
    Some(expects_panic)
}

/// Inventories rustdoc blocks separately because syn treats doc contents as attributes.
fn scan_doctests(
    tests: &mut Vec<TestCase>,
    assertions: &mut Vec<AssertionSite>,
    diagnostics: &mut Vec<ScanDiagnostic>,
    path: &str,
    language: &str,
    file_sha256: &str,
    lines: &[&str],
) {
    let mut active: Option<DocFence> = None;
    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        let Some(comment) = trimmed
            .strip_prefix("///")
            .or_else(|| trimmed.strip_prefix("//!"))
        else {
            continue;
        };
        let comment = comment.trim_start();
        let line_number = index.saturating_add(1);
        if let Some(fence) = &active {
            if fence.closes(comment) {
                if let Some(test) = fence.test.as_ref().and_then(|doc| tests.get_mut(doc.index)) {
                    test.end_line = line_number;
                }
                active = None;
            } else if let Some(doc) = &fence.test
                && let Some(test) = tests.get(doc.index)
            {
                let code = comment.trim_start_matches('#').trim_start();
                if let Some(kind) = first_macro_name(code).as_deref().and_then(rust_macro_kind)
                    && !(doc.expects_panic && kind == "unexpectedBranch")
                {
                    assertions.push(doctest_assertion(
                        assertions,
                        test,
                        file_sha256,
                        kind,
                        line_number,
                        line,
                        code,
                    ));
                }
            }
        } else if let Some((marker, width, info)) = DocFence::delimiter(comment) {
            let test = doctest_expects_panic(info).map(|expects_panic| {
                let index = tests.len();
                let test = TestCase {
                    id: next_test_id(tests),
                    name: format!("doctest at {line_number}"),
                    kind: "doctest".to_owned(),
                    language: language.to_owned(),
                    path: path.to_owned(),
                    line: line_number,
                    end_line: line_number,
                    direct_assertion_ids: Vec::new(),
                    shared_assertion_ids: Vec::new(),
                };
                if expects_panic {
                    assertions.push(doctest_assertion(
                        assertions,
                        &test,
                        file_sha256,
                        "panicExpectation",
                        line_number,
                        line,
                        comment,
                    ));
                }
                tests.push(test);
                DocTest {
                    index,
                    expects_panic,
                }
            });
            active = Some(DocFence {
                marker,
                width,
                test,
            });
        }
    }
    if let Some(doc) = active.and_then(|fence| fence.test)
        && let Some(test) = tests.get_mut(doc.index)
    {
        test.end_line = lines.len();
        diagnostics.push(ScanDiagnostic {
            kind: "unterminatedDoctest".to_owned(),
            path: path.to_owned(),
            line: Some(test.line),
            detail: "The rustdoc code fence has no closing delimiter".to_owned(),
        });
    }
}

/// Binds a predicate inside a rustdoc fence to the containing doctest.
fn doctest_assertion(
    assertions: &[AssertionSite],
    test: &TestCase,
    file_sha256: &str,
    kind: &str,
    line: usize,
    source: &str,
    expression: &str,
) -> AssertionSite {
    AssertionSite {
        id: next_assertion_id(assertions),
        kind: kind.to_owned(),
        language: test.language.clone(),
        path: test.path.clone(),
        line,
        end_line: line,
        column: source
            .len()
            .saturating_sub(expression.len())
            .saturating_add(1),
        expression: expression.to_owned(),
        file_sha256: file_sha256.to_owned(),
        test_code: true,
        containing_test_id: Some(test.id.clone()),
        helper_name: None,
        calling_test_ids: Vec::new(),
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

/// Assigns globally stable IDs as records appear in source order.
fn next_test_id(tests: &[TestCase]) -> String {
    format!("T{:06}", tests.len().saturating_add(1))
}

/// Assigns globally stable IDs as source predicate sites appear.
fn next_assertion_id(assertions: &[AssertionSite]) -> String {
    format!("A{:06}", assertions.len().saturating_add(1))
}
