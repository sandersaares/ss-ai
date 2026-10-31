//! Validation and rendering of agent-authored focus-area maps.
#![allow(
    clippy::indexing_slicing,
    reason = "Validated source spans, aligned map arrays, and matching tables bound these indexes"
)]
#![allow(
    clippy::arithmetic_side_effects,
    reason = "Counters and fence widths are bounded by allocated inputs and retry limits"
)]

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};
use std::time::Duration;

use ohno::AppError;
use serde::Deserialize;
use serde_json::{Value, json};
use ss_ai_review_context::{bytes_hash, powershell_json};
use ss_ai_runtime::{Clock, FileTransaction, LockMode, ResourceLock, caused_by, fail, read_bytes};

use crate::focus_area_expansion::{Expansion, ReferenceContextLink};
use crate::focus_area_source::{SourceSnapshot, Span, decode_text, source_path};
use crate::workflow_plan::parse_json;

/// A map whose source spans and structural constraints have been checked.
#[derive(Debug)]
pub struct ValidatedMap {
    value: Value,
    map: Map,
}

/// Agent-assigned category and chapter data.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Map {
    schema_version: u32,
    categories: Vec<Category>,
    chapters: Vec<Chapter>,
}

/// A named group of chapters.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Category {
    id: String,
    title: String,
}

/// A disjoint chapter with its source and alternative contexts.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Chapter {
    id: String,
    category_id: String,
    content: Span,
    sources: Vec<Span>,
    applicability: Vec<Applicability>,
}

/// One agent-authored alternative rather than an inferred intersection.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Applicability {
    effective_scope: String,
    #[serde(default)]
    path_scope: Option<PathScope>,
    declarations: Vec<Span>,
    origins: Vec<String>,
    uncertainties: Vec<Uncertainty>,
}

/// A conservative path-only proof supplied by the agent.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PathScope {
    base: String,
    include: Vec<String>,
}

/// A limitation in a scope judgment and its supporting source spans.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Uncertainty {
    reason: String,
    evidence: Vec<Span>,
}

/// Stores selected lines by their source tree and exact normalized path.
type LineCoverage = HashMap<String, HashMap<String, HashSet<usize>>>;

impl ValidatedMap {
    /// Validates a complete or partial worker map against one source snapshot.
    ///
    /// # Errors
    ///
    /// Returns an error for schema drift, invalid spans, or incomplete coverage.
    pub fn parse(bytes: &[u8], sources: &SourceSnapshot, complete: bool) -> Result<Self, AppError> {
        let text = decode_text(bytes)?;
        let value = parse_json(text.as_bytes())?;
        Self::from_value(value, sources, complete)
    }

    /// Keeps the original JSON so optional metadata and authored ordering survive.
    fn from_value(
        value: Value,
        sources: &SourceSnapshot,
        complete: bool,
    ) -> Result<Self, AppError> {
        let map: Map = serde_json::from_value(value.clone())
            .map_err(|error| caused_by("invalidInput", "Invalid focus-area map", error))?;
        if map.schema_version != 1 || map.categories.is_empty() || map.chapters.is_empty() {
            return Err(fail("invalidInput", "Invalid focus-area map envelope"));
        }
        let mut categories = HashMap::new();
        for category in &map.categories {
            if !valid_id(&category.id)
                || category.title.trim().is_empty()
                || category.title.contains(['\r', '\n'])
                || categories.insert(category.id.as_str(), 0_usize).is_some()
            {
                return Err(fail("invalidInput", "Invalid or duplicate category"));
            }
        }
        let mut ids = HashSet::new();
        let mut content = LineCoverage::new();
        let mut coverage = LineCoverage::new();
        for chapter in &map.chapters {
            if !valid_id(&chapter.id) || !ids.insert(&chapter.id) {
                return Err(fail("invalidInput", "Invalid or duplicate chapter"));
            }
            let count = categories
                .get_mut(chapter.category_id.as_str())
                .ok_or_else(|| fail("invalidInput", "Unknown chapter category"))?;
            *count += 1;
            if chapter.content.root != "instruction-set-raw" {
                return Err(fail(
                    "invalidInput",
                    "Chapter content must use the raw source",
                ));
            }
            cover(&chapter.content, sources, &mut coverage, Some(&mut content))?;
            if chapter.sources.is_empty() || chapter.applicability.is_empty() {
                return Err(fail(
                    "invalidInput",
                    "Chapter requires sources and applicability",
                ));
            }
            for source in &chapter.sources {
                sources.span(source)?;
            }
            for context in &chapter.applicability {
                if context.effective_scope.trim().is_empty()
                    || context
                        .origins
                        .iter()
                        .any(|origin| origin.trim().is_empty())
                {
                    return Err(fail("invalidInput", "Invalid applicability prose"));
                }
                if let Some(scope) = &context.path_scope {
                    validate_path_scope(scope)?;
                    if context.declarations.is_empty() || !context.uncertainties.is_empty() {
                        return Err(fail(
                            "invalidInput",
                            "Path-only scope requires declarations without uncertainty",
                        ));
                    }
                }
                for span in &context.declarations {
                    cover(span, sources, &mut coverage, None)?;
                }
                for uncertainty in &context.uncertainties {
                    if uncertainty.reason.trim().is_empty() || uncertainty.evidence.is_empty() {
                        return Err(fail(
                            "invalidInput",
                            "Uncertainty requires a reason and evidence",
                        ));
                    }
                    for span in &uncertainty.evidence {
                        cover(span, sources, &mut coverage, None)?;
                    }
                }
            }
        }
        if categories.values().any(|count| *count == 0) {
            return Err(fail("invalidInput", "Category has no chapters"));
        }
        // The workflow bounds reviewer dispatch rather than partial worker maps.
        if complete && categories.len() > 128 {
            return Err(fail("invalidInput", "Too many focus-area categories"));
        }
        if complete {
            for (root, path, lines) in sources
                .files()
                .filter(|(root, _, _)| *root == "instruction-set-raw")
            {
                let covered = coverage.get(root).and_then(|files| files.get(path));
                for (index, line) in lines.iter().enumerate() {
                    if !line.trim().is_empty()
                        && !covered.is_some_and(|lines| lines.contains(&(index + 1)))
                    {
                        return Err(fail(
                            "invalidInput",
                            format!("Uncovered raw input line {path}:{}", index + 1),
                        ));
                    }
                }
            }
        }
        Ok(Self { value, map })
    }

    /// Returns the validated original map for downstream authoring.
    #[must_use]
    pub fn value(&self) -> &Value {
        &self.value
    }

    /// Checks reference-chain evidence against each claimed output chapter.
    ///
    /// This checks the recorded origins and declarations, not the meaning of
    /// effective-scope prose or the correctness of an agent's path judgment.
    ///
    /// # Errors
    ///
    /// Returns an error when distinct reference chains lack distinct matching
    /// applicability alternatives.
    pub fn validate_reference_alternatives(&self, expansion: &Expansion) -> Result<(), AppError> {
        for chapter in &self.map.chapters {
            let Some(chains) = expansion.output_chains(&chapter.content.path)? else {
                continue;
            };
            if !chains_have_distinct_alternatives(&chains, &chapter.applicability)? {
                return Err(fail(
                    "invalidInput",
                    format!(
                        "Chapter '{}' lacks distinct applicability alternatives for its reference chains",
                        chapter.id
                    ),
                ));
            }
        }
        Ok(())
    }

    /// Renders the catalog and digest-bound reviewer documents.
    ///
    /// # Errors
    ///
    /// Returns an error if serialization or source lookup fails.
    pub fn render(&self, sources: &SourceSnapshot) -> Result<BTreeMap<String, String>, AppError> {
        let mut documents = BTreeMap::new();
        let mut catalog = vec!["# Guideline categories".to_owned()];
        for (index, category) in self.map.categories.iter().enumerate() {
            let heading = format!("# {}", category.title);
            catalog.push(format!("{heading}\n\nCategory: {}", category.id));
            let mut sections = vec![heading];
            let mut chapter_values = Vec::new();
            for (chapter_index, chapter) in self.map.chapters.iter().enumerate() {
                if chapter.category_id != category.id {
                    continue;
                }
                let lines = sources.span(&chapter.content)?;
                let metadata = format_metadata(chapter, sources)?;
                catalog.push(format!(
                    "## {}\n\nContent: {}\n\n{metadata}",
                    chapter.id,
                    label(&chapter.content)?
                ));
                let heading_lines = if atx_heading(&lines[0]) {
                    1
                } else if lines.get(1).is_some_and(|line| setext_heading(line)) {
                    2
                } else {
                    0
                };
                if heading_lines == 0 {
                    sections.push(format!("{metadata}\n\n{}", lines.join("\n")));
                } else {
                    sections.push(format!(
                        "{}\n\n{metadata}\n\n{}",
                        lines[..heading_lines].join("\n"),
                        lines[heading_lines..].join("\n")
                    ));
                }
                chapter_values.push(self.value["chapters"][chapter_index].clone());
            }
            let content = normalize_newlines(&format!("{}\n", sections.join("\n\n")));
            let metadata = json!({"schemaVersion": 1, "contentSha256": bytes_hash(content.as_bytes()),
                "map": {"schemaVersion": 1, "categories": [self.value["categories"][index].clone()], "chapters": chapter_values}});
            let encoded = base64(powershell_json(&metadata)?.as_bytes());
            let path = format!("focus-areas/focus-{}.md", category.id);
            documents.insert(
                path,
                format!("<!-- ss-review applicability {encoded} -->\n{content}"),
            );
        }
        documents.insert(
            "instruction-set-categories.md".to_owned(),
            normalize_newlines(&format!("{}\n", catalog.join("\n\n"))),
        );
        Ok(documents)
    }
}

/// Validates the lower-case identifiers used in generated filenames.
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.split('-').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        })
}

/// Matches chains to distinct alternatives without merging inherited contexts.
fn chains_have_distinct_alternatives(
    chains: &[Vec<ReferenceContextLink>],
    alternatives: &[Applicability],
) -> Result<bool, AppError> {
    let mut candidates = Vec::with_capacity(chains.len());
    let mut distinct = Vec::with_capacity(chains.len());
    for chain in chains {
        if distinct.contains(&chain.as_slice()) {
            continue;
        }
        distinct.push(chain.as_slice());
        let mut matching = Vec::new();
        for (index, alternative) in alternatives.iter().enumerate() {
            if chain_matches_alternative(chain, alternative)? {
                matching.push(index);
            }
        }
        candidates.push(matching);
    }

    // The bipartite graph connects chains to their compatible alternatives.
    // `assigned` maps alternatives to chains; `parents` maps a displaced chain
    // to the preceding chain and their contested alternative. Reversing a path
    // to a free alternative adds a pairing without losing earlier pairings.
    // See https://en.wikipedia.org/wiki/Maximum_bipartite_matching#Algorithms
    let mut assigned: Vec<Option<usize>> = vec![None; alternatives.len()];
    let mut parents = vec![None; candidates.len()];
    let mut visited = vec![false; candidates.len()];
    let mut queue = Vec::with_capacity(candidates.len());
    for first in 0..candidates.len() {
        parents.fill(None);
        visited.fill(false);
        queue.clear();
        queue.push(first);
        visited[first] = true;
        let mut free = None;
        // Each chain can enter the queue at most once.
        for next in 0..candidates.len() {
            let Some(&chain) = queue.get(next) else {
                break;
            };
            for &alternative in &candidates[chain] {
                if let Some(previous) = assigned[alternative] {
                    if !visited[previous] {
                        visited[previous] = true;
                        parents[previous] = Some((chain, alternative));
                        queue.push(previous);
                    }
                } else {
                    free = Some((chain, alternative));
                    break;
                }
            }
            if free.is_some() {
                break;
            }
        }
        let Some((mut chain, mut alternative)) = free else {
            return Ok(false);
        };
        // The predecessor path cannot contain more chains than the graph.
        for _ in 0..candidates.len() {
            assigned[alternative] = Some(chain);
            let Some((parent, previous_alternative)) = parents[chain] else {
                break;
            };
            chain = parent;
            alternative = previous_alternative;
        }
        debug_assert!(
            parents[chain].is_none(),
            "Matching predecessor path is cyclic"
        );
        #[cfg(debug_assertions)]
        {
            for processed in 0..=first {
                debug_assert_eq!(
                    assigned
                        .iter()
                        .filter(|&&value| value == Some(processed))
                        .count(),
                    1,
                    "Each processed chain must have one alternative"
                );
            }
            for (alternative, chain) in assigned.iter().enumerate() {
                if let Some(chain) = chain {
                    debug_assert!(candidates[*chain].contains(&alternative));
                }
            }
        }
    }
    Ok(true)
}

/// Checks ordered canonical origins and inherited declaration coverage.
fn chain_matches_alternative(
    chain: &[ReferenceContextLink],
    alternative: &Applicability,
) -> Result<bool, AppError> {
    let mut origins = alternative.origins.iter();
    for link in chain {
        if !origins.any(|origin| same_origin(origin, &link.uri)) {
            return Ok(false);
        }
        for declaration in &link.declarations {
            if !declarations_cover(declaration, &alternative.declarations)? {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

/// Allows alternate separators for local locations while keeping URI spelling exact.
fn same_origin(origin: &str, canonical: &str) -> bool {
    origin == canonical
        || (!origin.contains("://")
            && !canonical.contains("://")
            && origin.replace('\\', "/") == canonical.replace('\\', "/"))
}

/// Accepts exact, enclosing, or split spans covering the original declaration.
fn declarations_cover(required: &Span, declarations: &[Span]) -> Result<bool, AppError> {
    let required_path = source_path(&required.path)?;
    let mut covered = required.start_line.saturating_sub(1);
    // Each successful extension uses a declaration with a greater end line,
    // so the number of declarations bounds the number of extensions.
    for _ in declarations {
        if covered >= required.end_line {
            return Ok(true);
        }
        let mut next = covered;
        for declaration in declarations {
            if declaration.root == required.root
                && source_path(&declaration.path)? == required_path
                && declaration.start_line <= covered.saturating_add(1)
            {
                next = next.max(declaration.end_line);
            }
        }
        if next == covered {
            return Ok(false);
        }
        covered = next;
    }
    Ok(covered >= required.end_line)
}

/// Checks the supported glob language without making scope judgments.
fn validate_path_scope(scope: &PathScope) -> Result<(), AppError> {
    let valid_base = scope.base == "."
        || scope.base.split('/').all(|part| {
            !matches!(part, "" | "." | "..")
                && part
                    .bytes()
                    .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, b'_' | b'.' | b' ' | b'-'))
        });
    let mut unique = HashSet::new();
    if !valid_base || scope.include.is_empty() {
        return Err(fail("invalidInput", "Invalid path scope"));
    }
    for pattern in &scope.include {
        if !unique.insert(pattern)
            || pattern.split('/').any(|part| {
                matches!(part, "" | "." | "..")
                    || (part.contains("**") && part != "**")
                    || !part.bytes().all(|ch| {
                        ch.is_ascii_alphanumeric()
                            || matches!(ch, b'_' | b'.' | b'*' | b'?' | b' ' | b'-')
                    })
            })
        {
            return Err(fail("invalidInput", "Unsupported path-scope pattern"));
        }
    }
    Ok(())
}

/// Accumulates context coverage separately from non-overlapping chapter content.
fn cover(
    span: &Span,
    sources: &SourceSnapshot,
    coverage: &mut LineCoverage,
    content: Option<&mut LineCoverage>,
) -> Result<(), AppError> {
    sources.span(span)?;
    let path = source_path(&span.path)?;
    let first = usize::try_from(span.start_line)
        .map_err(|error| caused_by("invalidInput", "Invalid line", error))?;
    let last = usize::try_from(span.end_line)
        .map_err(|error| caused_by("invalidInput", "Invalid line", error))?;
    let covered = coverage
        .entry(span.root.clone())
        .or_default()
        .entry(path.clone())
        .or_default();
    let mut content = content.map(|content| {
        content
            .entry(span.root.clone())
            .or_default()
            .entry(path)
            .or_default()
    });
    for line in first..=last {
        if let Some(content) = content.as_deref_mut()
            && !content.insert(line)
        {
            return Err(fail("invalidInput", "Overlapping chapter content"));
        }
        covered.insert(line);
    }
    Ok(())
}

/// Identifies an exact source selection in the generated metadata.
fn label(span: &Span) -> Result<String, AppError> {
    Ok(format!(
        "{}\\{}:{}-{}",
        span.root,
        source_path(&span.path)?,
        span.start_line,
        span.end_line
    ))
}

/// Quotes verbatim source text without letting its backticks close the fence.
fn quote(text: &str) -> String {
    let mut run = 0;
    let mut longest = 0;
    for ch in text.chars() {
        if ch == '`' {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    let fence = "`".repeat(3.max(longest + 1));
    format!("{fence}\n{text}\n{fence}")
}

/// Renders all source and applicability context adjacent to its chapter.
fn format_metadata(chapter: &Chapter, sources: &SourceSnapshot) -> Result<String, AppError> {
    let mut parts = vec!["<!-- ss-review scope metadata -->".to_owned()];
    for source in &chapter.sources {
        let lines = sources.span(source)?;
        let heading = lines
            .iter()
            .find(|line| !line.trim().is_empty())
            .ok_or_else(|| fail("invalidInput", "Source span is blank"))?;
        parts.push(format!("Source: {}", label(source)?));
        parts.push(quote(heading));
    }
    for (index, context) in chapter.applicability.iter().enumerate() {
        parts.push(format!("Applicability alternative {}:", index + 1));
        parts.push(format!("Effective scope: {}", context.effective_scope));
        if context.declarations.is_empty() {
            parts.push("Scope declarations: None declared.".to_owned());
        } else {
            parts.push("Scope declarations:".to_owned());
            for span in &context.declarations {
                parts.push(label(span)?);
                parts.push(quote(&sources.span(span)?.join("\n")));
            }
        }
        if context.origins.is_empty() {
            parts.push("Reference origins: None (direct guidance).".to_owned());
        } else {
            parts.push(format!(
                "Reference origins:\n{}",
                context.origins.join("\n")
            ));
        }
        if context.uncertainties.is_empty() {
            parts.push("Scope uncertainty: None recorded.".to_owned());
        } else {
            for uncertainty in &context.uncertainties {
                parts.push(format!("Scope uncertainty: {}", uncertainty.reason));
                for span in &uncertainty.evidence {
                    parts.push(label(span)?);
                    parts.push(quote(&sources.span(span)?.join("\n")));
                }
            }
        }
    }
    parts.push("<!-- end ss-review scope metadata -->".to_owned());
    Ok(parts.join("\n\n"))
}

/// Recognizes an ATX heading only at the beginning of the selected span.
fn atx_heading(line: &str) -> bool {
    let count = line.bytes().take_while(|byte| *byte == b'#').count();
    (1..=6).contains(&count)
        && line
            .as_bytes()
            .get(count)
            .is_some_and(|ch| matches!(ch, b' ' | b'\t'))
}

/// Recognizes the underline of a setext heading.
fn setext_heading(line: &str) -> bool {
    let line = line.trim_matches([' ', '\t']);
    !line.is_empty() && (line.bytes().all(|ch| ch == b'=') || line.bytes().all(|ch| ch == b'-'))
}

/// Normalizes all generated document line endings.
fn normalize_newlines(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// Encodes applicability metadata in the standard padded base64 alphabet.
fn base64(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::new();
    for chunk in bytes.chunks(3) {
        let a = chunk[0];
        let b = chunk.get(1).copied().unwrap_or(0);
        let c = chunk.get(2).copied().unwrap_or(0);
        result.push(char::from(DIGITS[usize::from(a >> 2)]));
        result.push(char::from(DIGITS[usize::from(((a & 3) << 4) | (b >> 4))]));
        result.push(if chunk.len() > 1 {
            char::from(DIGITS[usize::from(((b & 15) << 2) | (c >> 6))])
        } else {
            '='
        });
        result.push(if chunk.len() > 2 {
            char::from(DIGITS[usize::from(c & 63)])
        } else {
            '='
        });
    }
    result
}

/// A typed map authoring operation.
#[derive(Debug)]
#[expect(
    clippy::exhaustive_enums,
    reason = "The variants define the supported operations"
)]
pub enum MapOperation {
    /// Merge a complete worker map without replacing conflicting chapters.
    Append(Vec<u8>),
    /// Replace named chapters from a validated worker map.
    Replace(Vec<u8>),
    /// Move all chapters of a category into a named destination category.
    Regroup {
        /// Existing category.
        category_id: String,
        /// Destination category.
        new_category_id: String,
        /// Destination category title.
        title: String,
    },
}

/// Validates a worker map while retaining the coordinated source generation.
///
/// # Errors
///
/// Returns an error for invalid sources, missing maps, or invalid map contents.
pub fn validate(
    session: &Path,
    map_path: &Path,
    clock: &dyn Clock,
    complete: bool,
) -> Result<Value, AppError> {
    let _source_lock = ResourceLock::open(
        &session.join("focus-area-cache-operation"),
        LockMode::Shared,
        clock,
    )?;
    let sources = SourceSnapshot::read(session, clock)?;
    let bytes = read_bytes(map_path, clock)?
        .ok_or_else(|| fail("invalidInput", "Focus-area map is missing"))?;
    let map = ValidatedMap::parse(&bytes, &sources, complete)?;
    Ok(
        json!({"status": "Valid", "categoryCount": map.map.categories.len(), "chapterCount": map.map.chapters.len()}),
    )
}

/// Authors a map under its record lock without overwriting conflicting work.
///
/// # Errors
///
/// Returns an error for invalid input, conflicting contributions, or I/O errors.
pub fn update(
    session: &Path,
    path: &Path,
    operation: MapOperation,
    clock: &dyn Clock,
) -> Result<Value, AppError> {
    reject_reserved_map_path(session, path)?;
    let _source_lock = ResourceLock::open(
        &session.join("focus-area-cache-operation"),
        LockMode::Shared,
        clock,
    )?;
    let transaction = FileTransaction::open(path, clock)?;
    let sources = SourceSnapshot::read(session, clock)?;
    let mut document = if let Some(bytes) = transaction.read_bytes()? {
        ValidatedMap::parse(&bytes, &sources, false)?.value
    } else {
        json!({"schemaVersion": 1, "categories": [], "chapters": []})
    };
    let replace = matches!(&operation, MapOperation::Replace(_));
    match operation {
        MapOperation::Append(bytes) | MapOperation::Replace(bytes) => {
            let incoming = ValidatedMap::parse(&bytes, &sources, false)?.value;
            merge_map(&mut document, &incoming, replace)?;
        }
        MapOperation::Regroup {
            category_id,
            new_category_id,
            title,
        } => regroup(&mut document, &category_id, &new_category_id, &title)?,
    }
    let validated = ValidatedMap::from_value(document, &sources, false)?;
    let result = json!({"status": "Recorded", "categoryCount": validated.map.categories.len(),
            "chapterCount": validated.map.chapters.len()});
    transaction.write_bytes(powershell_json(&validated.value)?.as_bytes())?;
    Ok(result)
}

/// Prevents recursive locks and map writes into another protected resource.
fn reject_reserved_map_path(session: &Path, path: &Path) -> Result<(), AppError> {
    if !session.is_absolute() || !path.is_absolute() {
        return Err(fail(
            "invalidInput",
            "Session and map paths must be absolute",
        ));
    }
    let session = fs::canonicalize(session)
        .map_err(|error| caused_by("io", "Cannot resolve session directory", error))?;
    let parent = path
        .parent()
        .ok_or_else(|| fail("invalidInput", "Map path has no parent"))?;
    let mut existing = parent;
    while !existing.exists() {
        existing = existing
            .parent()
            .ok_or_else(|| fail("invalidInput", "Map path has no existing ancestor"))?;
    }
    let existing = fs::canonicalize(existing)
        .map_err(|error| caused_by("io", "Cannot resolve map directory", error))?;
    for protected in [
        "instruction-set-source",
        "instruction-set-raw",
        "focus-areas",
    ] {
        let directory = session.join(protected);
        if fs::canonicalize(directory).is_ok_and(|directory| existing.starts_with(directory)) {
            return Err(fail(
                "invalidInput",
                "Map path is inside a protected resource",
            ));
        }
    }
    if existing == session && parent.exists() {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        if matches!(
            name,
            "focus-area-cache-operation"
                | "focus-area-cache-operation.lock"
                | "focus-areas"
                | "focus-areas.lock"
                | "focus-area-expansion-status.json"
                | "focus-area-expansion-status.json.lock"
        ) {
            return Err(fail(
                "invalidInput",
                "Map path aliases a protected resource",
            ));
        }
    }
    Ok(())
}

/// Merges only exact chapter retries and requires explicit replacements.
fn merge_map(document: &mut Value, incoming: &Value, replace: bool) -> Result<(), AppError> {
    let categories = incoming["categories"]
        .as_array()
        .ok_or_else(|| fail("invalidInput", "Missing categories"))?;
    for category in categories {
        let current = document["categories"]
            .as_array_mut()
            .ok_or_else(|| fail("invalidInput", "Missing categories"))?;
        if let Some(existing) = current
            .iter()
            .find(|existing| existing["id"] == category["id"])
        {
            if existing["title"] != category["title"] {
                return Err(fail("invalidInput", "Conflicting category title"));
            }
        } else {
            current.push(category.clone());
        }
    }
    let chapters = incoming["chapters"]
        .as_array()
        .ok_or_else(|| fail("invalidInput", "Missing chapters"))?;
    for chapter in chapters {
        let current = document["chapters"]
            .as_array_mut()
            .ok_or_else(|| fail("invalidInput", "Missing chapters"))?;
        if let Some(existing) = current
            .iter_mut()
            .find(|existing| existing["id"] == chapter["id"])
        {
            if replace {
                *existing = chapter.clone();
            } else if existing != chapter {
                return Err(fail("invalidInput", "Conflicting chapter"));
            }
        } else if replace {
            return Err(fail("invalidInput", "Cannot replace an unknown chapter"));
        } else {
            current.push(chapter.clone());
        }
    }
    let used: HashSet<_> = document["chapters"]
        .as_array()
        .ok_or_else(|| fail("invalidInput", "Missing chapters"))?
        .iter()
        .filter_map(|chapter| chapter["categoryId"].as_str().map(str::to_owned))
        .collect();
    document["categories"]
        .as_array_mut()
        .ok_or_else(|| fail("invalidInput", "Missing categories"))?
        .retain(|category| category["id"].as_str().is_some_and(|id| used.contains(id)));
    Ok(())
}

/// Changes only category assignments and preserves all authored provenance.
fn regroup(document: &mut Value, old: &str, new: &str, title: &str) -> Result<(), AppError> {
    if !valid_id(old) || !valid_id(new) || title.trim().is_empty() {
        return Err(fail("invalidInput", "Invalid category regrouping"));
    }
    let categories = document["categories"]
        .as_array_mut()
        .ok_or_else(|| fail("invalidInput", "Missing categories"))?;
    if !categories.iter().any(|category| category["id"] == old) {
        return Err(fail("invalidInput", "Unknown source category"));
    }
    categories.retain(|category| category["id"] != old && category["id"] != new);
    categories.push(json!({"id": new, "title": title}));
    for chapter in document["chapters"]
        .as_array_mut()
        .ok_or_else(|| fail("invalidInput", "Missing chapters"))?
    {
        if chapter["categoryId"] == old {
            chapter["categoryId"] = json!(new);
        }
    }
    Ok(())
}

/// Assembles a complete map and publishes its paired artifacts under their lock.
///
/// # Errors
///
/// Returns an error for invalid or unfinished inputs and for publication failures.
pub fn assemble(session: &Path, map_path: &Path, clock: &dyn Clock) -> Result<Value, AppError> {
    let _source_lock = ResourceLock::open(
        &session.join("focus-area-cache-operation"),
        LockMode::Shared,
        clock,
    )?;
    let _artifact_lock =
        ResourceLock::open(&session.join("focus-areas"), LockMode::Exclusive, clock)?;
    let sources = SourceSnapshot::read(session, clock)?;
    let expansion = read_bytes(&session.join("focus-area-expansion-status.json"), clock)?
        .ok_or_else(|| fail("invalidInput", "Expansion manifest is missing"))?;
    let expansion = Expansion::parse(&expansion, &sources, true)?;
    let map = read_bytes(map_path, clock)?
        .ok_or_else(|| fail("invalidInput", "Focus-area map is missing"))?;
    let map = ValidatedMap::parse(&map, &sources, true)?;
    map.validate_reference_alternatives(&expansion)?;
    let documents = map.render(&sources)?;
    publish_documents(session, &documents, clock)?;
    Ok(
        json!({"status": "Assembled", "categoryCount": map.map.categories.len(), "chapterCount": map.map.chapters.len()}),
    )
}

/// Ensures an existing artifact tree cannot redirect installation through links.
fn reject_links(path: &Path) -> Result<(), AppError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(caused_by("io", "Cannot inspect artifacts", error).into()),
    };
    if metadata.file_type().is_symlink() {
        return Err(fail("invalidInput", "Artifacts must not contain links"));
    }
    if metadata.is_dir() {
        for entry in fs::read_dir(path)
            .map_err(|error| caused_by("io", "Cannot inspect artifacts", error))?
        {
            let entry =
                entry.map_err(|error| caused_by("io", "Cannot inspect artifact entry", error))?;
            reject_links(&entry.path())?;
        }
    }
    Ok(())
}

/// Allocates a collision-safe sibling directory without deleting another writer's work.
#[expect(
    clippy::create_dir,
    reason = "An existing staging directory must not be reused"
)]
fn staging_directory(session: &Path) -> Result<PathBuf, AppError> {
    // A finite search prevents abandoned staging directories from causing an unbounded loop.
    for index in 0..10_000_u32 {
        let path = session.join(format!(
            ".focus-area-assembly-{}-{index}",
            std::process::id()
        ));
        match fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(caused_by("io", "Cannot create assembly staging", error).into());
            }
        }
    }
    Err(fail("io", "Cannot allocate assembly staging directory"))
}

/// Supplies file operations to the artifact transaction.
trait AssemblyStorage {
    fn validate(&self, path: &Path) -> Result<(), AppError>;
    fn create_staging(&self, session: &Path) -> Result<PathBuf, AppError>;
    fn create_focus_directory(&self, path: &Path) -> Result<(), AppError>;
    fn write(&self, path: &Path, text: &str) -> Result<(), AppError>;
    fn exists(&self, path: &Path) -> bool;
    fn rename(&self, from: &Path, to: &Path) -> Result<(), AppError>;
    fn remove_artifact(&self, path: &Path) -> Result<(), AppError>;
    fn remove_staging(&self, path: &Path) -> Result<(), AppError>;
}

/// Performs assembly effects on the filesystem with the runtime's wait policy.
struct FileAssemblyStorage<'a> {
    clock: &'a dyn Clock,
}

impl AssemblyStorage for FileAssemblyStorage<'_> {
    fn validate(&self, path: &Path) -> Result<(), AppError> {
        reject_links(path)
    }

    fn create_staging(&self, session: &Path) -> Result<PathBuf, AppError> {
        staging_directory(session)
    }

    #[expect(
        clippy::create_dir,
        reason = "The staging subdirectory must be newly created"
    )]
    fn create_focus_directory(&self, path: &Path) -> Result<(), AppError> {
        fs::create_dir(path)
            .map_err(|error| caused_by("io", "Cannot stage focus areas", error).into())
    }

    fn write(&self, path: &Path, text: &str) -> Result<(), AppError> {
        fs::write(path, text.as_bytes())
            .map_err(|error| caused_by("io", "Cannot stage focus document", error).into())
    }

    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }

    fn rename(&self, from: &Path, to: &Path) -> Result<(), AppError> {
        retry_contention(self.clock, || fs::rename(from, to))
    }

    fn remove_artifact(&self, path: &Path) -> Result<(), AppError> {
        retry_contention(self.clock, || {
            if path.is_dir() {
                fs::remove_dir_all(path)
            } else {
                fs::remove_file(path)
            }
        })
    }

    fn remove_staging(&self, path: &Path) -> Result<(), AppError> {
        fs::remove_dir_all(path)
            .map_err(|error| caused_by("io", "Cannot remove assembly staging", error).into())
    }
}

/// Publishes through the same transaction used by simulated filesystem tests.
fn publish_documents(
    session: &Path,
    documents: &BTreeMap<String, String>,
    clock: &dyn Clock,
) -> Result<(), AppError> {
    publish_with_storage(session, documents, &FileAssemblyStorage { clock })
}

/// Installs both staged outputs and preserves backups when rollback fails.
fn publish_with_storage(
    session: &Path,
    documents: &BTreeMap<String, String>,
    storage: &impl AssemblyStorage,
) -> Result<(), AppError> {
    let artifacts = ["focus-areas", "instruction-set-categories.md"];
    for artifact in artifacts {
        storage.validate(&session.join(artifact))?;
    }
    let staging = storage.create_staging(session)?;
    let mut rollback_failed = false;
    let result = (|| {
        storage.create_focus_directory(&staging.join("focus-areas"))?;
        for (path, text) in documents {
            storage.write(&staging.join(path), text)?;
        }
        let mut backed_up = Vec::new();
        let mut installed = Vec::new();
        for artifact in artifacts {
            let destination = session.join(artifact);
            let backup = staging.join(format!("backup-{artifact}"));
            let install = (|| {
                if storage.exists(&destination) {
                    storage.rename(&destination, &backup)?;
                    backed_up.push(artifact);
                }
                storage.rename(&staging.join(artifact), &destination)?;
                installed.push(artifact);
                Ok::<(), AppError>(())
            })();
            if let Err(error) = install {
                for installed_artifact in installed.iter().rev() {
                    let path = session.join(installed_artifact);
                    let removal = storage.remove_artifact(&path);
                    if let Err(rollback) = removal {
                        rollback_failed = true;
                        return Err(fail(
                            "io",
                            format!(
                                "Assembly failed: {error}; rollback failed: {rollback}; backups remain at {}",
                                staging.display()
                            ),
                        ));
                    }
                }
                for backed_up_artifact in backed_up.iter().rev() {
                    if let Err(rollback) = storage.rename(
                        &staging.join(format!("backup-{backed_up_artifact}")),
                        &session.join(backed_up_artifact),
                    ) {
                        rollback_failed = true;
                        return Err(fail(
                            "io",
                            format!(
                                "Assembly failed: {error}; rollback failed: {rollback}; backups remain at {}",
                                staging.display()
                            ),
                        ));
                    }
                }
                return Err(error);
            }
        }
        Ok(())
    })();
    if !rollback_failed && let Err(cleanup) = storage.remove_staging(&staging) {
        return Err(match result {
            Ok(()) => cleanup,
            Err(error) => fail(
                "io",
                format!("Assembly failed: {error}; staging cleanup failed: {cleanup}"),
            ),
        });
    }
    result
}

/// Retries only sharing conflicts while retaining the artifact transaction lock.
pub(crate) fn retry_contention(
    clock: &dyn Clock,
    mut operation: impl FnMut() -> io::Result<()>,
) -> Result<(), AppError> {
    // Match the runtime's bounded contention budget for coordinated filesystem work.
    const ATTEMPTS: u32 = 6_000;
    const INTERVAL: Duration = Duration::from_millis(50);
    for attempt in 0..ATTEMPTS {
        match operation() {
            Ok(()) => return Ok(()),
            Err(error) => {
                let contention = error.kind() == ErrorKind::WouldBlock
                    || cfg!(windows) && matches!(error.raw_os_error(), Some(32 | 33));
                if !contention || attempt + 1 == ATTEMPTS {
                    return Err(caused_by("io", "Cannot publish focus artifacts", error).into());
                }
                clock.wait(INTERVAL)?;
            }
        }
    }
    Err(fail("io", "Focus artifact contention retry limit reached"))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[allow(
    clippy::assertions_on_result_states,
    reason = "Boolean result checks make scenario matrices concise"
)]
mod tests {
    use std::cell::Cell;
    use std::cell::RefCell;
    use std::collections::BTreeMap;
    use std::io::{self, ErrorKind};
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use jiff::Timestamp;
    use ohno::AppError;
    use serde_json::{Value, json};
    use ss_ai_runtime::Clock;

    use super::{
        AssemblyStorage, ValidatedMap, base64, merge_map, publish_with_storage, quote, regroup,
        retry_contention,
    };
    use crate::focus_area_expansion::Expansion;
    use crate::focus_area_source::SourceSnapshot;

    /// Simulates files and directories while injecting failures at named effects.
    #[derive(Default)]
    struct SimulatedStorage {
        entries: RefCell<BTreeMap<PathBuf, String>>,
        failures: RefCell<Vec<(&'static str, PathBuf)>>,
    }

    impl SimulatedStorage {
        fn initial() -> Self {
            let storage = Self::default();
            let mut entries = storage.entries.borrow_mut();
            entries.insert(PathBuf::from("/session/focus-areas"), "directory".into());
            entries.insert(
                PathBuf::from("/session/focus-areas/old.md"),
                "old focus".into(),
            );
            entries.insert(
                PathBuf::from("/session/instruction-set-categories.md"),
                "old catalog".into(),
            );
            drop(entries);
            storage
        }

        fn fail_once(&self, operation: &'static str, path: &str) {
            self.failures
                .borrow_mut()
                .push((operation, PathBuf::from(path)));
        }

        fn perform(&self, operation: &str, path: &Path) -> Result<(), AppError> {
            let mut failures = self.failures.borrow_mut();
            if let Some(index) = failures
                .iter()
                .position(|(name, candidate)| *name == operation && candidate == path)
            {
                failures.remove(index);
                return Err(ss_ai_runtime::fail(
                    "io",
                    format!("Injected {operation} failure"),
                ));
            }
            Ok(())
        }

        fn remove_tree(&self, path: &Path) {
            self.entries
                .borrow_mut()
                .retain(|entry, _| !entry.starts_with(path));
        }
    }

    impl AssemblyStorage for SimulatedStorage {
        fn validate(&self, path: &Path) -> Result<(), AppError> {
            self.perform("validate", path)
        }

        fn create_staging(&self, session: &Path) -> Result<PathBuf, AppError> {
            let path = session.join(".focus-area-assembly-test");
            self.perform("create-staging", &path)?;
            self.entries
                .borrow_mut()
                .insert(path.clone(), "directory".into());
            Ok(path)
        }

        fn create_focus_directory(&self, path: &Path) -> Result<(), AppError> {
            self.perform("create-directory", path)?;
            self.entries
                .borrow_mut()
                .insert(path.to_owned(), "directory".into());
            Ok(())
        }

        fn write(&self, path: &Path, text: &str) -> Result<(), AppError> {
            self.perform("write", path)?;
            self.entries
                .borrow_mut()
                .insert(path.to_owned(), text.to_owned());
            Ok(())
        }

        fn exists(&self, path: &Path) -> bool {
            self.entries.borrow().contains_key(path)
        }

        fn rename(&self, from: &Path, to: &Path) -> Result<(), AppError> {
            self.perform("rename", from)?;
            let mut entries = self.entries.borrow_mut();
            let paths = entries
                .keys()
                .filter(|path| path.starts_with(from))
                .cloned()
                .collect::<Vec<_>>();
            if paths.is_empty() || entries.contains_key(to) {
                return Err(ss_ai_runtime::fail("io", "Invalid simulated rename"));
            }
            for path in paths {
                let relative = path.strip_prefix(from).unwrap();
                let value = entries.remove(&path).unwrap();
                entries.insert(to.join(relative), value);
            }
            Ok(())
        }

        fn remove_artifact(&self, path: &Path) -> Result<(), AppError> {
            self.perform("remove-artifact", path)?;
            self.remove_tree(path);
            Ok(())
        }

        fn remove_staging(&self, path: &Path) -> Result<(), AppError> {
            self.perform("remove-staging", path)?;
            self.remove_tree(path);
            Ok(())
        }
    }

    /// Supplies a complete, staged output pair for installation tests.
    fn documents() -> BTreeMap<String, String> {
        BTreeMap::from([
            ("focus-areas/new.md".into(), "new focus".into()),
            ("instruction-set-categories.md".into(), "new catalog".into()),
        ])
    }

    /// Checks that neither previous artifact was lost or modified.
    fn assert_previous(storage: &SimulatedStorage) {
        let entries = storage.entries.borrow();
        assert_eq!(
            entries
                .get(Path::new("/session/focus-areas/old.md"))
                .unwrap(),
            "old focus"
        );
        assert_eq!(
            entries
                .get(Path::new("/session/instruction-set-categories.md"))
                .unwrap(),
            "old catalog"
        );
    }

    #[test]
    fn staging_failures_leave_previous_artifacts_untouched() {
        let staging = "/session/.focus-area-assembly-test";
        for (operation, path) in [
            ("validate", "/session/focus-areas"),
            ("create-staging", staging),
            (
                "create-directory",
                "/session/.focus-area-assembly-test/focus-areas",
            ),
            (
                "write",
                "/session/.focus-area-assembly-test/focus-areas/new.md",
            ),
            (
                "write",
                "/session/.focus-area-assembly-test/instruction-set-categories.md",
            ),
        ] {
            let storage = SimulatedStorage::initial();
            storage.fail_once(operation, path);
            publish_with_storage(Path::new("/session"), &documents(), &storage).unwrap_err();
            assert_previous(&storage);
            assert!(!storage.exists(Path::new(staging)));
        }
    }

    #[test]
    fn partial_swap_failures_restore_both_previous_artifacts() {
        for failed_source in [
            "/session/focus-areas",
            "/session/.focus-area-assembly-test/focus-areas",
            "/session/instruction-set-categories.md",
            "/session/.focus-area-assembly-test/instruction-set-categories.md",
        ] {
            let storage = SimulatedStorage::initial();
            storage.fail_once("rename", failed_source);
            publish_with_storage(Path::new("/session"), &documents(), &storage).unwrap_err();
            assert_previous(&storage);
            assert!(!storage.exists(Path::new("/session/.focus-area-assembly-test")));
        }
    }

    #[test]
    fn rollback_failure_retains_backups_and_reports_their_location() {
        let storage = SimulatedStorage::initial();
        storage.fail_once(
            "rename",
            "/session/.focus-area-assembly-test/instruction-set-categories.md",
        );
        storage.fail_once("remove-artifact", "/session/focus-areas");
        let error =
            publish_with_storage(Path::new("/session"), &documents(), &storage).unwrap_err();
        let staging = Path::new("/session").join(".focus-area-assembly-test");
        assert!(error.to_string().contains(&staging.display().to_string()));
        let entries = storage.entries.borrow();
        assert_eq!(
            entries
                .get(Path::new(
                    "/session/.focus-area-assembly-test/backup-focus-areas/old.md"
                ))
                .unwrap(),
            "old focus"
        );
        assert_eq!(
            entries
                .get(Path::new(
                    "/session/.focus-area-assembly-test/backup-instruction-set-categories.md"
                ))
                .unwrap(),
            "old catalog"
        );
    }

    #[test]
    fn failed_backup_restoration_keeps_unrestored_data_in_staging() {
        let storage = SimulatedStorage::initial();
        storage.fail_once(
            "rename",
            "/session/.focus-area-assembly-test/instruction-set-categories.md",
        );
        storage.fail_once(
            "rename",
            "/session/.focus-area-assembly-test/backup-focus-areas",
        );
        let error =
            publish_with_storage(Path::new("/session"), &documents(), &storage).unwrap_err();
        let staging = Path::new("/session").join(".focus-area-assembly-test");
        assert!(error.to_string().contains(&staging.display().to_string()));
        let entries = storage.entries.borrow();
        assert_eq!(
            entries
                .get(Path::new("/session/instruction-set-categories.md"))
                .unwrap(),
            "old catalog"
        );
        assert_eq!(
            entries
                .get(Path::new(
                    "/session/.focus-area-assembly-test/backup-focus-areas/old.md"
                ))
                .unwrap(),
            "old focus"
        );
    }

    #[test]
    fn staging_cleanup_failure_reports_error_and_preserves_backup() {
        let storage = SimulatedStorage::initial();
        storage.fail_once("remove-staging", "/session/.focus-area-assembly-test");
        publish_with_storage(Path::new("/session"), &documents(), &storage).unwrap_err();
        let entries = storage.entries.borrow();
        assert_eq!(
            entries
                .get(Path::new("/session/focus-areas/new.md"))
                .unwrap(),
            "new focus"
        );
        assert_eq!(
            entries
                .get(Path::new(
                    "/session/.focus-area-assembly-test/backup-focus-areas/old.md"
                ))
                .unwrap(),
            "old focus"
        );
    }

    #[test]
    fn successful_swap_replaces_both_artifacts_and_cleans_staging() {
        let storage = SimulatedStorage::initial();
        publish_with_storage(Path::new("/session"), &documents(), &storage).unwrap();
        let entries = storage.entries.borrow();
        assert_eq!(
            entries
                .get(Path::new("/session/focus-areas/new.md"))
                .unwrap(),
            "new focus"
        );
        assert_eq!(
            entries
                .get(Path::new("/session/instruction-set-categories.md"))
                .unwrap(),
            "new catalog"
        );
        assert!(!entries.contains_key(Path::new("/session/focus-areas/old.md")));
        assert!(!entries.contains_key(Path::new("/session/.focus-area-assembly-test")));
    }

    fn fixture() -> (SourceSnapshot, Value) {
        let files = ["instruction-set-source", "instruction-set-raw"]
            .into_iter()
            .map(|root| {
                (
                    (root.to_owned(), "guide.md".to_owned()),
                    vec![
                        "Scope: scripts".to_owned(),
                        String::new(),
                        "# Rule".to_owned(),
                        "Keep it.".to_owned(),
                    ],
                )
            })
            .collect::<BTreeMap<_, _>>();
        let span = |start, end| json!({"root":"instruction-set-raw","path":"guide.md","startLine":start,"endLine":end});
        let map = json!({"schemaVersion":1,"categories":[{"id":"scripts","title":"Scripts"}],
                "chapters":[{"id":"rule","categoryId":"scripts","content":span(3,4),
                    "sources":[span(3,4)],"applicability":[{"effectiveScope":"Scripts",
                        "pathScope":{"base":".","include":["**/*.ps1"]},
                        "declarations":[span(1,1)],"origins":[],"uncertainties":[]}]}]});
        (SourceSnapshot::from_files(files), map)
    }

    #[test]
    fn complete_coverage_renders_bound_metadata_without_inventing_scope() {
        let (sources, value) = fixture();
        let map = ValidatedMap::from_value(value.clone(), &sources, true).unwrap();
        let docs = map.render(&sources).unwrap();
        let rendered = &docs["focus-areas/focus-scripts.md"];
        assert!(rendered.starts_with("<!-- ss-review applicability "));
        assert!(rendered.contains("# Rule\n\n<!-- ss-review scope metadata -->"));
        assert!(rendered.contains("Scope: scripts"));
        assert!(rendered.contains("Keep it."));
        assert_eq!(map.value(), &value);
        assert_eq!(base64(b"abcde"), "YWJjZGU=");
        assert_eq!(quote("```"), "````\n```\n````");
    }

    #[test]
    fn declaration_quotation_preserves_embedded_markdown_fences() {
        let text = "Scope example:\n```powershell\nWrite-Output '$u'\n```";
        let files = ["instruction-set-source", "instruction-set-raw"]
            .into_iter()
            .map(|root| {
                (
                    (root.to_owned(), "guide.md".to_owned()),
                    text.lines().map(str::to_owned).collect(),
                )
            })
            .collect();
        let sources = SourceSnapshot::from_files(files);
        let span = |root| json!({"root":root,"path":"guide.md","startLine":1,"endLine":4});
        let map = json!({"schemaVersion":1,"categories":[{"id":"rules","title":"Rules"}],
            "chapters":[{"id":"rule","categoryId":"rules","content":span("instruction-set-raw"),
                "sources":[span("instruction-set-source")],"applicability":[{
                    "effectiveScope":"Agent judgment","declarations":[span("instruction-set-raw")],
                    "origins":[],"uncertainties":[]}]}]});
        let map = ValidatedMap::from_value(map, &sources, true).unwrap();
        let documents = map.render(&sources).unwrap();
        assert!(documents["focus-areas/focus-rules.md"].contains(&format!("````\n{text}\n````")));
    }

    #[test]
    fn rejects_missing_coverage_overlap_and_uncertain_path_exclusions() {
        let (sources, mut value) = fixture();
        value["chapters"][0]["applicability"][0]["declarations"] = json!([]);
        value["chapters"][0]["applicability"][0]["pathScope"] = Value::Null;
        assert!(ValidatedMap::from_value(value.clone(), &sources, false).is_ok());
        assert!(ValidatedMap::from_value(value.clone(), &sources, true).is_err());
        value["chapters"][0]["applicability"][0]["pathScope"] =
            json!({"base":".","include":["**/*.ps1"]});
        assert!(ValidatedMap::from_value(value.clone(), &sources, false).is_err());
        let (_, mut value) = fixture();
        let mut second = value["chapters"][0].clone();
        second["id"] = json!("other");
        value["chapters"].as_array_mut().unwrap().push(second);
        assert!(ValidatedMap::from_value(value, &sources, false).is_err());
    }

    #[test]
    fn coverage_counts_raw_uncertainty_evidence_but_not_source_attribution() {
        let (sources, mut value) = fixture();
        let context = &mut value["chapters"][0]["applicability"][0];
        context["declarations"] = json!([]);
        context["pathScope"] = Value::Null;
        context["uncertainties"] = json!([{"reason":"Uncertain","evidence":[{
            "root":"instruction-set-raw","path":"guide.md","startLine":1,"endLine":1
        }]}]);
        assert!(ValidatedMap::from_value(value.clone(), &sources, true).is_ok());
        value["chapters"][0]["applicability"][0]["uncertainties"][0]["evidence"][0]["root"] =
            json!("instruction-set-source");
        value["chapters"][0]["sources"]
            .as_array_mut()
            .unwrap()
            .push(json!({
                "root":"instruction-set-raw","path":"guide.md","startLine":1,"endLine":1
            }));
        assert!(ValidatedMap::from_value(value, &sources, true).is_err());
    }

    #[test]
    fn rejects_the_legacy_schema_drift_cases() {
        let (sources, original) = fixture();
        let mut cases = Vec::new();
        let mut value = original.clone();
        value["chapters"][0]["applicability"][0]
            .as_object_mut()
            .unwrap()
            .remove("uncertainties");
        cases.push(value);
        let mut value = original.clone();
        value["chapters"][0]["applicability"][0]["uncertainties"] = Value::Null;
        cases.push(value);
        let mut value = original.clone();
        value["chapters"][0]["content"]["sourceLineRange"] = json!([3, 4]);
        cases.push(value);
        let mut value = original.clone();
        value["chapters"][0]["applicability"][0]["declarationContextVerbatim"] = json!("Scope");
        cases.push(value);
        let mut value = original.clone();
        value["schemaVersion"] = json!(2);
        cases.push(value);
        let mut value = original.clone();
        value["chapters"][0]["sources"] = value["chapters"][0]["sources"][0].clone();
        cases.push(value);
        let mut value = original.clone();
        value["chapters"][0]["applicability"][0]["pathScope"] = Value::Null;
        value["chapters"][0]["applicability"][0]["uncertainties"] =
            json!([{"reason":"Unknown","evidence":[]}]);
        cases.push(value);
        let mut value = original;
        value["chapters"][0]["content"]["startLine"] = json!(3.5);
        cases.push(value);
        for value in cases {
            assert!(ValidatedMap::from_value(value, &sources, false).is_err());
        }
    }

    #[test]
    fn rejects_the_legacy_source_and_assignment_errors() {
        let (sources, original) = fixture();
        let mut cases = Vec::new();
        let mut value = original.clone();
        let category = value["categories"][0].clone();
        value["categories"].as_array_mut().unwrap().push(category);
        cases.push(value);
        let mut value = original.clone();
        value["categories"][0]["title"] = json!(" ");
        cases.push(value);
        let mut value = original.clone();
        value["chapters"][0]["categoryId"] = json!("missing");
        cases.push(value);
        let mut value = original.clone();
        let duplicate = value["chapters"][0].clone();
        value["chapters"].as_array_mut().unwrap().push(duplicate);
        cases.push(value);
        let mut value = original.clone();
        let mut overlap = value["chapters"][0].clone();
        overlap["id"] = json!("other");
        value["chapters"].as_array_mut().unwrap().push(overlap);
        cases.push(value);
        for (field, invalid) in [
            ("endLine", json!(99)),
            ("endLine", json!(2)),
            ("root", json!("instruction-set-source")),
            ("path", json!("../guide.md")),
            ("path", json!("C:\\guide.md")),
            ("path", json!("missing.md")),
            ("path", json!("GUIDE.md")),
        ] {
            let mut value = original.clone();
            value["chapters"][0]["content"][field] = invalid;
            cases.push(value);
        }
        for value in cases {
            assert!(ValidatedMap::from_value(value, &sources, false).is_err());
        }
    }

    #[test]
    fn category_limit_applies_only_to_complete_maps() {
        for count in [128_usize, 129] {
            let lines = (1..=count)
                .map(|index| format!("# Rule {index}"))
                .collect::<Vec<_>>();
            let files = ["instruction-set-source", "instruction-set-raw"]
                .into_iter()
                .map(|root| ((root.to_owned(), "many.md".to_owned()), lines.clone()))
                .collect();
            let sources = SourceSnapshot::from_files(files);
            let mut categories = Vec::new();
            let mut chapters = Vec::new();
            for index in 1..=count {
                let id = format!("category-{index}");
                let span =
                    |root| json!({"root":root,"path":"many.md","startLine":index,"endLine":index});
                categories.push(json!({"id":id,"title":format!("Category {index}")}));
                chapters.push(json!({"id":format!("chapter-{index}"),"categoryId":id,
                    "content":span("instruction-set-raw"),"sources":[span("instruction-set-source")],
                    "applicability":[{"effectiveScope":"All","declarations":[],"origins":[],"uncertainties":[]}]}));
            }
            let value = json!({"schemaVersion":1,"categories":categories,"chapters":chapters});
            assert!(ValidatedMap::from_value(value.clone(), &sources, false).is_ok());
            assert_eq!(
                ValidatedMap::from_value(value, &sources, true).is_ok(),
                count == 128
            );
        }
    }

    #[test]
    fn authoring_keeps_chapters_and_scope_when_regrouping() {
        let (_, mut map) = fixture();
        let incoming = map.clone();
        merge_map(&mut map, &incoming, false).unwrap();
        assert_eq!(map["chapters"].as_array().unwrap().len(), 1);
        regroup(&mut map, "scripts", "review", "Review").unwrap();
        assert_eq!(map["chapters"][0]["categoryId"], "review");
        assert_eq!(
            map["chapters"][0]["applicability"],
            incoming["chapters"][0]["applicability"]
        );
    }

    #[test]
    fn retries_only_contention_without_repeating_unrelated_failures() {
        struct TestClock(Cell<usize>);
        impl Clock for TestClock {
            fn now(&self) -> Timestamp {
                Timestamp::MIN
            }
            fn wait(&self, _: Duration) -> Result<(), AppError> {
                self.0.set(self.0.get() + 1);
                Ok(())
            }
        }
        let clock = TestClock(Cell::new(0));
        let attempts = Cell::new(0);
        retry_contention(&clock, || {
            attempts.set(attempts.get() + 1);
            if attempts.get() == 1 {
                Err(io::Error::from(ErrorKind::WouldBlock))
            } else {
                Ok(())
            }
        })
        .unwrap();
        assert_eq!(clock.0.get(), 1);
        let result = retry_contention(&clock, || Err(io::Error::from(ErrorKind::PermissionDenied)));
        assert!(result.is_err());
        assert_eq!(clock.0.get(), 1);
    }

    #[test]
    fn late_referrers_require_distinct_complete_descendant_alternatives() {
        let files = [
            (
                "instruction-set-source",
                "source.md",
                vec!["First scope", "Second scope"],
            ),
            (
                "instruction-set-raw",
                "parent.md",
                vec!["# Parent", "Child link"],
            ),
            ("instruction-set-raw", "child.md", vec!["# Child"]),
        ]
        .into_iter()
        .map(|(root, path, lines)| {
            (
                (root.to_owned(), path.to_owned()),
                lines.into_iter().map(str::to_owned).collect(),
            )
        })
        .collect();
        let sources = SourceSnapshot::from_files(files);
        let span = |root, path, start, end| json!({"root":root,"path":path,"startLine":start,"endLine":end});
        let first = span("instruction-set-source", "source.md", 1, 1);
        let second = span("instruction-set-source", "source.md", 2, 2);
        let referrer = |line, declaration| {
            json!({
                "parentId":null,"source":span("instruction-set-source","source.md",line,line),
                "scope":format!("scope {line}"),"declarations":[declaration]
            })
        };
        let manifest = json!({"schemaVersion":1,"state":"Complete","references":[
            {"id":"parent","uri":"https://example.invalid/parent","state":"Complete",
                "outputs":["parent.md"],"error":null,
                "referrers":[referrer(1,first.clone()),referrer(2,second.clone())]},
            {"id":"child","uri":"https://example.invalid/child","state":"Complete",
                "outputs":["child.md"],"error":null,"referrers":[{
                    "parentId":"parent","source":span("instruction-set-raw","parent.md",2,2),
                    "scope":"Nested","declarations":[]}]}
        ]});
        let expansion =
            Expansion::parse(&serde_json::to_vec(&manifest).unwrap(), &sources, true).unwrap();
        let alternative = |origins, declaration| {
            json!({
                "effectiveScope":"Agent-authored judgment","declarations":[declaration],
                "origins":origins,"uncertainties":[],"pathScope":null
            })
        };
        let parent_origins = json!(["https://example.invalid/parent"]);
        let child_origins = json!([
            "https://example.invalid/parent",
            "https://example.invalid/child"
        ]);
        let parent_contexts = vec![
            alternative(parent_origins.clone(), first.clone()),
            alternative(parent_origins, second.clone()),
        ];
        let mut map = json!({"schemaVersion":1,"categories":[{"id":"rules","title":"Rules"}],
        "chapters":[
            {"id":"parent","categoryId":"rules","content":span("instruction-set-raw","parent.md",1,2),
                "sources":[span("instruction-set-raw","parent.md",1,2)],"applicability":parent_contexts},
            {"id":"child","categoryId":"rules","content":span("instruction-set-raw","child.md",1,1),
                "sources":[span("instruction-set-raw","child.md",1,1)],
                "applicability":[alternative(child_origins.clone(),first)]}
        ]});
        let parsed = ValidatedMap::from_value(map.clone(), &sources, true).unwrap();
        assert!(parsed.validate_reference_alternatives(&expansion).is_err());
        map["chapters"][1]["applicability"]
            .as_array_mut()
            .unwrap()
            .push(alternative(child_origins, second));
        ValidatedMap::from_value(map.clone(), &sources, true)
            .unwrap()
            .validate_reference_alternatives(&expansion)
            .unwrap();
        map["chapters"][1]["applicability"][1]["origins"] = json!([
            "https://example.invalid/child",
            "https://example.invalid/parent"
        ]);
        assert!(
            ValidatedMap::from_value(map, &sources, true)
                .unwrap()
                .validate_reference_alternatives(&expansion)
                .is_err()
        );
    }

    #[test]
    fn matching_reassigns_overlapping_evidence_and_keeps_uncertainty() {
        let files = [
            (
                "instruction-set-source",
                "source.md",
                vec!["Scope A", "Scope B"],
            ),
            ("instruction-set-raw", "target.md", vec!["# Guidance"]),
        ]
        .into_iter()
        .map(|(root, path, lines)| {
            (
                (root.to_owned(), path.to_owned()),
                lines.into_iter().map(str::to_owned).collect(),
            )
        })
        .collect();
        let sources = SourceSnapshot::from_files(files);
        let span = |root, path, start, end| json!({"root":root,"path":path,"startLine":start,"endLine":end});
        let first = span("instruction-set-source", "source.md", 1, 1);
        let both = span("instruction-set-source", "source.md", 1, 2);
        let manifest = json!({"schemaVersion":1,"state":"Complete","references":[{
        "id":"ref","uri":"same\\target.md","state":"Complete","outputs":["target.md"],"error":null,
        "referrers":[
            {"parentId":null,"source":first,"scope":"A","declarations":[first]},
            {"parentId":null,"source":both,"scope":"B","declarations":[both]}
        ]}]});
        let expansion =
            Expansion::parse(&serde_json::to_vec(&manifest).unwrap(), &sources, true).unwrap();
        let applicability = |declarations, uncertainty| {
            json!({
                "effectiveScope":"Agent judgment","origins":["same/target.md"],
                "declarations":declarations,"uncertainties":uncertainty,"pathScope":null
            })
        };
        let map = json!({"schemaVersion":1,"categories":[{"id":"rules","title":"Rules"}],
        "chapters":[{"id":"target","categoryId":"rules",
            "content":span("instruction-set-raw","target.md",1,1),
            "sources":[span("instruction-set-raw","target.md",1,1)],
            "applicability":[
                applicability(json!([both]),json!([])),
                applicability(json!([first]),json!([{"reason":"Uncertain","evidence":[first]}]))
            ]}]});
        let validated = ValidatedMap::from_value(map, &sources, true).unwrap();
        validated
            .validate_reference_alternatives(&expansion)
            .unwrap();
        let rendered = validated.render(&sources).unwrap();
        assert!(rendered["focus-areas/focus-rules.md"].contains("Scope uncertainty: Uncertain"));
    }

    #[test]
    fn indistinguishable_origins_still_require_separate_alternatives() {
        let files = [
            (
                "instruction-set-source",
                "source.md",
                vec!["First", "Second"],
            ),
            ("instruction-set-raw", "target.md", vec!["# Guidance"]),
        ]
        .into_iter()
        .map(|(root, path, lines)| {
            (
                (root.to_owned(), path.to_owned()),
                lines.into_iter().map(str::to_owned).collect(),
            )
        })
        .collect();
        let sources = SourceSnapshot::from_files(files);
        let span =
            |root, path, line| json!({"root":root,"path":path,"startLine":line,"endLine":line});
        let manifest = json!({"schemaVersion":1,"state":"Complete","references":[{
        "id":"ref","uri":"https://example.invalid/target","state":"Complete",
        "outputs":["target.md"],"error":null,"referrers":[
            {"parentId":null,"source":span("instruction-set-source","source.md",1),
                "scope":"First","declarations":[]},
            {"parentId":null,"source":span("instruction-set-source","source.md",2),
                "scope":"Second","declarations":[]}
        ]}]});
        let expansion =
            Expansion::parse(&serde_json::to_vec(&manifest).unwrap(), &sources, true).unwrap();
        let alternative = json!({"effectiveScope":"Agent judgment","declarations":[],
            "origins":["https://example.invalid/target"],"uncertainties":[],"pathScope":null});
        let mut map = json!({"schemaVersion":1,"categories":[{"id":"rules","title":"Rules"}],
            "chapters":[{"id":"target","categoryId":"rules",
                "content":span("instruction-set-raw","target.md",1),
                "sources":[span("instruction-set-raw","target.md",1)],
                "applicability":[alternative]}]});
        assert!(
            ValidatedMap::from_value(map.clone(), &sources, true)
                .unwrap()
                .validate_reference_alternatives(&expansion)
                .is_err()
        );
        let mut repeated = manifest;
        repeated["references"][0]["referrers"][1] =
            repeated["references"][0]["referrers"][0].clone();
        let repeated =
            Expansion::parse(&serde_json::to_vec(&repeated).unwrap(), &sources, true).unwrap();
        ValidatedMap::from_value(map.clone(), &sources, true)
            .unwrap()
            .validate_reference_alternatives(&repeated)
            .unwrap();
        map["chapters"][0]["applicability"]
            .as_array_mut()
            .unwrap()
            .push(alternative);
        ValidatedMap::from_value(map, &sources, true)
            .unwrap()
            .validate_reference_alternatives(&expansion)
            .unwrap();
    }
}
