//! Markdown report rendering with isolated, checked diagram images.
//!
//! Report presentation and HTML report rendering in
//! `skills/ss-review/docs/{design,implementation}.md` explain the static,
//! offline presentation and source-preserving transformations.

use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

use base64::{Engine, engine::general_purpose::STANDARD};
use ohno::AppError;
use quick_xml::{
    Reader, XmlVersion,
    escape::unescape,
    events::{BytesStart, Event},
};
use regex::Regex;
use regex::bytes::Regex as BytesRegex;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use ss_ai_runtime::{Clock, FileTransaction, Process, ResourceLock, caused_by, fail, read_bytes};
use tempfile::Builder;
use url::Url;

use crate::report_tools::{ReportTools, assert_ready};

/// Identifies the published report and the exact input and output bytes.
#[derive(Debug)]
#[non_exhaustive]
pub struct ReportResult {
    /// Absolute path of the Markdown input.
    pub source_path: PathBuf,
    /// SHA-256 of the decoded Markdown encoded as UTF-8.
    pub source_hash: String,
    /// Absolute path of the published HTML.
    pub html_path: PathBuf,
    /// File URI of the published HTML.
    pub html_uri: String,
    /// SHA-256 of the published HTML bytes.
    pub html_hash: String,
    /// Number of rendered Mermaid diagrams.
    pub diagram_count: usize,
}

/// Renders and atomically publishes a standalone HTML reading view.
///
/// The caller retains the tools resource lock from discovery until this returns.
///
/// # Errors
///
/// Returns an error if input, tools, rendering, validation or publication fails.
#[expect(
    clippy::too_many_arguments,
    reason = "Rendering needs separate paths, tool ownership and injected process and clock"
)]
pub fn write_report(
    source: &Path,
    output: &Path,
    skill_root: &Path,
    title: &str,
    tools: &ReportTools,
    _guard: &ResourceLock,
    process: &dyn Process,
    clock: &dyn Clock,
) -> Result<ReportResult, AppError> {
    assert_ready(tools, false)?;
    let source_path = absolute(source)?;
    let html_path = absolute(output)?;
    let html_uri = Url::from_file_path(&html_path)
        .map_err(|()| invalid("Cannot construct report file URI"))?
        .to_string();
    let transaction = FileTransaction::open(&html_path, clock)?;
    reject_alias(&source_path, &html_path)?;
    let bytes =
        read_bytes(&source_path, clock)?.ok_or_else(|| invalid("Report Markdown is missing"))?;
    let markdown = decode_markdown(&bytes);
    if markdown.trim().is_empty() {
        return Err(invalid("Report Markdown is empty"));
    }
    let source_hash = hash(markdown.as_bytes());
    let parent = html_path
        .parent()
        .ok_or_else(|| invalid("Output requires a parent"))?;
    let scratch = Builder::new()
        .prefix(".report-")
        .tempdir_in(parent)
        .map_err(|error| caused_by("io", "Cannot create report workspace", error))?;
    let ast = run(
        process,
        &tools.pandoc,
        &["--from=gfm-raw_html".into(), "--to=json".into()],
        markdown.as_bytes(),
    )?;
    let mut ast: Value = serde_json::from_slice(&ast)
        .map_err(|error| caused_by("invalidInput", "Pandoc returned malformed JSON", error))?;
    let object = ast
        .as_object_mut()
        .ok_or_else(|| invalid("Pandoc returned an invalid document"))?;
    if !object.get("blocks").is_some_and(Value::is_array)
        || !object
            .get("pandoc-api-version")
            .is_some_and(Value::is_array)
    {
        return Err(invalid("Pandoc returned an invalid document"));
    }
    object.insert("meta".into(), json!({}));
    let mut renderer = DiagramRenderer {
        process,
        tools,
        directory: scratch.path(),
        source_hash: &source_hash,
        count: 0,
    };
    transform(&mut ast, &mut renderer)?;
    let template = absolute(&skill_root.join("templates").join("report.html"))?;
    let arguments = vec![
        "--from=json".into(),
        "--to=html5".into(),
        "--standalone".into(),
        "--toc".into(),
        "--wrap=none".into(),
        "--syntax-highlighting=pygments".into(),
        "--template".into(),
        path_text(&template)?,
        "--metadata".into(),
        format!("title={title}"),
    ];
    let input = serde_json::to_vec(&ast)
        .map_err(|error| caused_by("invalidInput", "Cannot encode Pandoc document", error))?;
    let html = run(process, &tools.pandoc, &arguments, &input)?;
    let html_text = std::str::from_utf8(&html)
        .map_err(|error| caused_by("invalidInput", "Pandoc returned non-UTF-8 HTML", error))?;
    validate_html(html_text)?;
    // Check aliases again after rendering, before replacing the output.
    reject_alias(&source_path, &html_path)?;
    transaction.write_bytes(&html)?;
    let published = transaction
        .read_bytes()?
        .ok_or_else(|| invalid("Published report is missing"))?;
    let diagram_count = renderer.count;
    Ok(ReportResult {
        source_path,
        source_hash,
        html_path,
        html_uri,
        html_hash: hash(&published),
        diagram_count,
    })
}

/// Runs the production pipeline for readiness, including the browser renderer.
///
/// # Errors
///
/// Returns an error unless a diagram is rendered and embedded.
pub fn probe(
    directory: &Path,
    skill_root: &Path,
    tools: &ReportTools,
    guard: &ResourceLock,
    process: &dyn Process,
    clock: &dyn Clock,
) -> Result<(), AppError> {
    fs::create_dir_all(directory)
        .map_err(|error| caused_by("io", "Cannot create probe directory", error))?;
    let directory = Builder::new()
        .prefix(".report-check-")
        .tempdir_in(directory)
        .map_err(|error| caused_by("io", "Cannot create probe workspace", error))?;
    let source = directory.path().join("check.md");
    let output = directory.path().join("check.html");
    fs::write(&source, "# Renderer prerequisite check\n\n```mermaid\nflowchart LR\n    A[Prerequisites] --> B[Report]\n```\n")
        .map_err(|error| caused_by("io", "Cannot write report probe", error))?;
    let result = write_report(
        &source,
        &output,
        skill_root,
        "Review report",
        tools,
        guard,
        process,
        clock,
    )?;
    let html = read_bytes(&output, clock)?.ok_or_else(|| invalid("Probe output is missing"))?;
    if result.diagram_count != 1
        || !html
            .windows(b"data:image/svg+xml;base64,".len())
            .any(|part| part == b"data:image/svg+xml;base64,")
    {
        return Err(invalid("Report readiness probe did not embed a diagram"));
    }
    Ok(())
}

/// Holds deterministic diagram state for one source document.
struct DiagramRenderer<'a> {
    process: &'a dyn Process,
    tools: &'a ReportTools,
    directory: &'a Path,
    source_hash: &'a str,
    count: usize,
}

impl DiagramRenderer<'_> {
    /// Renders through the installed Mermaid CLI with the report's strict settings.
    fn render(&mut self, source: &str) -> Result<String, AppError> {
        static FORBIDDEN: OnceLock<Result<Regex, regex::Error>> = OnceLock::new();
        let forbidden = FORBIDDEN.get_or_init(|| Regex::new(r"(?i-u)%%\{|\A\s*---|\b(?:img|icon)\s*:|(?:https?|file|data):|<\s*(?:img|iframe|script|link)\b"))
            .as_ref().map_err(|error| caused_by("invalidInput", "Cannot compile diagram check", error.clone()))?;
        if forbidden.is_match(source) {
            return Err(invalid(
                "Report diagrams must not contain directives or external resources",
            ));
        }
        self.count = self
            .count
            .checked_add(1)
            .ok_or_else(|| invalid("Too many diagrams"))?;
        let input = self.directory.join(format!("diagram-{}.mmd", self.count));
        let output = self.directory.join(format!("diagram-{}.svg", self.count));
        let config = self.directory.join("mermaid.json");
        // The source identity and ordinal give each diagram a repeatable seed.
        let settings = json!({
            "securityLevel": "strict", "htmlLabels": false, "flowchart": {"htmlLabels": false},
            "fontFamily": "Arial, sans-serif", "deterministicIds": true,
            "deterministicIDSeed": format!("{}-{}", self.source_hash, self.count), "theme": "neutral"
        });
        fs::write(&input, source)
            .map_err(|error| caused_by("io", "Cannot write diagram source", error))?;
        fs::write(
            &config,
            serde_json::to_vec(&settings).map_err(|error| {
                caused_by("invalidInput", "Cannot encode diagram settings", error)
            })?,
        )
        .map_err(|error| caused_by("io", "Cannot write diagram settings", error))?;
        let node = self
            .tools
            .node
            .as_ref()
            .ok_or_else(|| invalid("Node is unavailable"))?;
        let arguments = vec![
            path_text(&self.tools.mermaid)?,
            "--input".into(),
            path_text(&input)?,
            "--output".into(),
            path_text(&output)?,
            "--configFile".into(),
            path_text(&config)?,
            "--quiet".into(),
        ];
        run(self.process, node, &arguments, &[])?;
        let svg = fs::read_to_string(output)
            .map_err(|error| caused_by("io", "Cannot read rendered SVG", error))?;
        diagram_html(source, &svg)
    }
}

/// Converts unsupported AST nodes without interpreting Markdown as HTML.
fn transform(node: &mut Value, renderer: &mut DiagramRenderer<'_>) -> Result<(), AppError> {
    if let Some(object) = node.as_object_mut() {
        let kind = object.get("t").and_then(Value::as_str).unwrap_or_default();
        let is_code = kind == "CodeBlock";
        let is_image = kind == "Image";
        let is_link = kind == "Link";
        let is_raw_inline = kind == "RawInline";
        let is_raw_block = kind == "RawBlock";
        if is_code {
            let content = object
                .get("c")
                .and_then(Value::as_array)
                .ok_or_else(|| invalid("Malformed code block"))?;
            let classes = content
                .first()
                .and_then(Value::as_array)
                .and_then(|v| v.get(1))
                .and_then(Value::as_array)
                .ok_or_else(|| invalid("Malformed code block attributes"))?;
            if classes
                .iter()
                .any(|value| value.as_str() == Some("mermaid"))
            {
                let source = content
                    .get(1)
                    .and_then(Value::as_str)
                    .ok_or_else(|| invalid("Malformed diagram source"))?;
                let html = renderer.render(source)?;
                object.insert("t".into(), json!("RawBlock"));
                object.insert("c".into(), json!(["html", html]));
                return Ok(());
            }
        }
        if is_image {
            return Err(invalid(
                "Image references are not supported in review reports",
            ));
        }
        if is_link {
            let content = object
                .get("c")
                .and_then(Value::as_array)
                .ok_or_else(|| invalid("Malformed link"))?;
            let target = content
                .get(2)
                .and_then(Value::as_array)
                .and_then(|v| v.first())
                .and_then(Value::as_str)
                .ok_or_else(|| invalid("Malformed link destination"))?;
            if !target.starts_with('#') {
                match Url::parse(target) {
                    Ok(url) if matches!(url.scheme(), "http" | "https") => {}
                    Ok(_) => return Err(invalid("Report links must use HTTP or HTTPS")),
                    Err(url::ParseError::RelativeUrlWithoutBase) => {
                        content
                            .get(1)
                            .and_then(Value::as_array)
                            .ok_or_else(|| invalid("Malformed link label"))?;
                        let content = object
                            .get_mut("c")
                            .and_then(Value::as_array_mut)
                            .ok_or_else(|| invalid("Malformed link"))?;
                        let mut label = content
                            .get_mut(1)
                            .and_then(Value::as_array_mut)
                            .map(std::mem::take)
                            .ok_or_else(|| invalid("Malformed link label"))?;
                        let target = content
                            .get_mut(2)
                            .and_then(Value::as_array_mut)
                            .and_then(|value| value.first_mut())
                            .map(Value::take)
                            .ok_or_else(|| invalid("Malformed link destination"))?;
                        label.extend([
                            json!({"t":"Space"}),
                            json!({"t":"Str","c":"("}),
                            json!({"t":"Code","c":[["",[],[]], target]}),
                            json!({"t":"Str","c":")"}),
                        ]);
                        object.insert("t".into(), json!("Span"));
                        object.insert("c".into(), json!([["", [], []], label]));
                    }
                    Err(_) => return Err(invalid("Malformed report link")),
                }
            }
        }
        if is_raw_inline || is_raw_block {
            let raw = object
                .get_mut("c")
                .and_then(Value::as_array_mut)
                .and_then(|v| v.get_mut(1))
                .filter(|value| value.is_string())
                .map(Value::take)
                .ok_or_else(|| invalid("Malformed raw node"))?;
            if is_raw_inline {
                object.insert("t".into(), json!("Str"));
                object.insert("c".into(), json!(raw));
            } else {
                object.insert("t".into(), json!("CodeBlock"));
                object.insert("c".into(), json!([["", [], []], raw]));
            }
            return Ok(());
        }
        for value in object.values_mut() {
            transform(value, renderer)?;
        }
    } else if let Some(values) = node.as_array_mut() {
        for value in values {
            transform(value, renderer)?;
        }
    }
    Ok(())
}

/// Checks the SVG before embedding it as an inert image and preserving its source.
fn diagram_html(source: &str, svg: &str) -> Result<String, AppError> {
    static EXTERNAL: OnceLock<Result<BytesRegex, regex::Error>> = OnceLock::new();
    // XML entities must be resolved before inspecting CSS, or encoded URLs can evade checks.
    let decoded =
        unescape(svg).map_err(|error| caused_by("invalidInput", "Invalid SVG entity", error))?;
    if decoded.contains('\0') {
        return Err(invalid("SVG contains unsupported escaped content"));
    }
    let external = EXTERNAL
        .get_or_init(|| {
            BytesRegex::new(r#"(?i-u)@import|url\(\s*(?:["']([^"']*)["']|([^)\s]+))\s*\)"#)
        })
        .as_ref()
        .map_err(|error| caused_by("invalidInput", "Cannot compile SVG check", error.clone()))?;
    for capture in external.captures_iter(decoded.as_bytes()) {
        let destination = capture.get(1).or_else(|| capture.get(2));
        if destination.is_none_or(|value| !value.as_bytes().starts_with(b"#")) {
            return Err(invalid("SVG references an external style resource"));
        }
    }
    let mut reader = Reader::from_str(svg);
    // Expanding empty elements gives every start tag a matching end event.
    reader.config_mut().expand_empty_elements = true;
    let mut depth = 0_usize;
    let mut root_seen = false;
    let mut desc_depth = None;
    let mut desc_seen = false;
    let mut style_depth = None;
    let mut description = String::new();
    let mut dimensions = String::new();
    loop {
        match reader
            .read_event()
            .map_err(|error| caused_by("invalidInput", "Malformed SVG", error))?
        {
            Event::Start(element) => {
                let name = element.local_name().as_ref().to_owned();
                if depth == 0 {
                    if root_seen || name != "svg" {
                        return Err(invalid("Mermaid did not produce one SVG document"));
                    }
                    root_seen = true;
                }
                if matches!(
                    name.to_ascii_lowercase().as_str(),
                    "script"
                        | "foreignobject"
                        | "image"
                        | "iframe"
                        | "animate"
                        | "animatemotion"
                        | "animatetransform"
                        | "set"
                        | "discard"
                ) {
                    return Err(invalid("SVG contains unsupported active content"));
                }
                inspect_svg_attributes(&element, depth == 0, &mut dimensions)?;
                if name == "desc" && !desc_seen {
                    desc_seen = true;
                    desc_depth = Some(depth);
                }
                if name.eq_ignore_ascii_case("style") && style_depth.is_none() {
                    style_depth = Some(depth);
                }
                depth = depth
                    .checked_add(1)
                    .ok_or_else(|| invalid("SVG nesting is too deep"))?;
            }
            Event::End(_) => {
                depth = depth
                    .checked_sub(1)
                    .ok_or_else(|| invalid("Malformed SVG nesting"))?;
                if desc_depth == Some(depth) {
                    desc_depth = None;
                }
                if style_depth == Some(depth) {
                    style_depth = None;
                }
            }
            Event::Text(text) => {
                let text = text.xml10_content();
                if depth == 0 && !text.trim().is_empty() {
                    return Err(invalid("Text outside SVG root"));
                }
                if style_depth.is_some() && text.contains('\\') {
                    return Err(invalid("SVG style contains unsupported escapes"));
                }
                if desc_depth.is_some() {
                    description.push_str(&text);
                }
            }
            Event::CData(text) => {
                let text = text.xml10_content();
                if depth == 0 {
                    return Err(invalid("Text outside SVG root"));
                }
                if style_depth.is_some() && text.contains('\\') {
                    return Err(invalid("SVG style contains unsupported escapes"));
                }
                if desc_depth.is_some() {
                    description.push_str(&text);
                }
            }
            Event::GeneralRef(reference) => {
                if depth == 0 {
                    return Err(invalid("Text outside SVG root"));
                }
                if desc_depth.is_some() || style_depth.is_some() {
                    let reference = reference.xml10_content();
                    let reference = format!("&{reference};");
                    let reference = unescape(&reference).map_err(|error| {
                        caused_by("invalidInput", "Invalid SVG reference", error)
                    })?;
                    if style_depth.is_some() && reference.contains('\\') {
                        return Err(invalid("SVG style contains unsupported escapes"));
                    }
                    if desc_depth.is_some() {
                        description.push_str(&reference);
                    }
                }
            }
            Event::DocType(_) | Event::PI(_) => {
                return Err(invalid(
                    "SVG declarations and processing instructions are unsupported",
                ));
            }
            Event::Empty(_) => return Err(invalid("Malformed SVG document")),
            Event::Eof => break,
            Event::Decl(_) | Event::Comment(_) => {}
        }
    }
    if !root_seen || depth != 0 {
        return Err(invalid("Malformed SVG document"));
    }
    let alt = if desc_seen {
        &description
    } else {
        "Mermaid diagram"
    };
    Ok(format!(
        "<figure class=\"diagram\"><div class=\"diagram-scroll\"><img alt=\"{}\"{} src=\"data:image/svg+xml;base64,{}\"></div><details><summary>Diagram source</summary><pre><code>{}</code></pre></details></figure>",
        html_escape(alt),
        dimensions,
        STANDARD.encode(svg.as_bytes()),
        html_escape(source)
    ))
}

/// Validates SVG attributes and captures the renderer's intrinsic dimensions.
fn inspect_svg_attributes(
    element: &BytesStart<'_>,
    root: bool,
    dimensions: &mut String,
) -> Result<(), AppError> {
    for attribute in element.attributes() {
        let attribute = attribute
            .map_err(|error| caused_by("invalidInput", "Malformed SVG attribute", error))?;
        let name = attribute.key.local_name().as_ref().to_ascii_lowercase();
        let value = attribute
            .normalized_value(XmlVersion::Implicit1_0)
            .map_err(|error| caused_by("invalidInput", "Invalid SVG attribute", error))?;
        if value.contains('\\')
            || attribute.key.as_ref().eq_ignore_ascii_case("xml:base")
            || name.starts_with("on")
            || (matches!(name.as_str(), "href" | "src") && !value.starts_with('#'))
        {
            return Err(invalid(
                "SVG contains active or externally referenced content",
            ));
        }
        if root && name == "viewbox" && !value.is_empty() {
            let mut coordinates = value
                .split(|c: char| c.is_whitespace() || c == ',')
                .filter(|part| !part.is_empty());
            let (Some(_), Some(_), Some(width), Some(height), None) = (
                coordinates.next(),
                coordinates.next(),
                coordinates.next(),
                coordinates.next(),
                coordinates.next(),
            ) else {
                return Err(invalid("SVG viewBox is malformed"));
            };
            let width = width
                .parse::<f64>()
                .ok()
                .ok_or_else(|| invalid("Invalid SVG width"))?;
            let height = height
                .parse::<f64>()
                .ok()
                .ok_or_else(|| invalid("Invalid SVG height"))?;
            if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
                return Err(invalid("SVG dimensions must be finite and positive"));
            }
            *dimensions = format!(
                " width=\"{:.0}\" height=\"{:.0}\"",
                width.ceil(),
                height.ceil()
            );
        }
    }
    Ok(())
}

/// Ensures Pandoc produced the standalone template and no executable markup.
fn validate_html(html: &str) -> Result<(), AppError> {
    static ACTIVE: OnceLock<Result<BytesRegex, regex::Error>> = OnceLock::new();
    let active = ACTIVE.get_or_init(|| BytesRegex::new(r#"(?is-u)<script\b|<[a-z][^>]*\son[a-z]+\s*=|<[a-z][^>]*\s(?:src|href)\s*=\s*["'](?:javascript:|file:)"#))
        .as_ref().map_err(|error| caused_by("invalidInput", "Cannot compile HTML check", error.clone()))?;
    if !html.contains("<!DOCTYPE html>")
        || !html.contains(concat!(
            "<meta http-equiv=\"Content-Security-Policy\" content=\"",
            "default-src 'none'; img-src data:; style-src 'unsafe-inline'; ",
            "base-uri 'none'; form-action 'none'\">"
        ))
        || active.is_match(html.as_bytes())
    {
        return Err(invalid("Pandoc did not produce a safe standalone report"));
    }
    Ok(())
}

/// Rejects paths that identify the canonical Markdown file, including hard links.
fn reject_alias(source: &Path, output: &Path) -> Result<(), AppError> {
    let source_identity = fs::canonicalize(source)
        .map_err(|error| caused_by("io", "Cannot resolve report source", error))?;
    let output_identity = match fs::canonicalize(output) {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let parent = output
                .parent()
                .ok_or_else(|| invalid("Output requires a parent"))?;
            let name = output
                .file_name()
                .ok_or_else(|| invalid("Output requires a filename"))?;
            fs::canonicalize(parent)
                .map_err(|error| caused_by("io", "Cannot resolve output directory", error))?
                .join(name)
        }
        Err(error) => return Err(caused_by("io", "Cannot resolve output path", error).into()),
    };
    if source_identity == output_identity || same_file(source, output)? {
        return Err(invalid(
            "HTML output must not replace the canonical Markdown file",
        ));
    }
    Ok(())
}

/// Detects hard-linked inputs using filesystem identity.
fn same_file(source: &Path, output: &Path) -> Result<bool, AppError> {
    match fs::metadata(output) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(caused_by("io", "Cannot inspect report output", error).into()),
    }
    same_file::is_same_file(source, output)
        .map_err(|error| caused_by("io", "Cannot compare report file identities", error).into())
}

/// Runs a renderer and preserves its failure instead of interpreting stale output.
fn run(
    process: &dyn Process,
    program: &Path,
    arguments: &[String],
    input: &[u8],
) -> Result<Vec<u8>, AppError> {
    let output = process
        .execute(&path_text(program)?, arguments, input)
        .map_err(|error| caused_by("io", "Cannot launch report renderer", error))?;
    if !output.success {
        return Err(fail(
            "renderer",
            format!(
                "Report renderer failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ),
        ));
    }
    Ok(output.stdout)
}

/// Represents paths as explicit renderer arguments without lossy conversion.
fn path_text(path: &Path) -> Result<String, AppError> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| invalid("Report path is not UTF-8"))
}

/// Resolves the caller's path without requiring an existing output.
fn absolute(path: &Path) -> Result<PathBuf, AppError> {
    let path = std::path::absolute(path)
        .map_err(|error| caused_by("io", "Cannot resolve report path", error))?;
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                result.pop();
            }
            other => result.push(other.as_os_str()),
        }
    }
    Ok(result)
}

/// Matches the text reader's BOM detection before computing the source hash.
fn decode_markdown(bytes: &[u8]) -> String {
    if let Some((bytes, little_endian)) = bytes
        .strip_prefix(&[0xff, 0xfe, 0, 0])
        .map(|bytes| (bytes, true))
        .or_else(|| {
            bytes
                .strip_prefix(&[0, 0, 0xfe, 0xff])
                .map(|bytes| (bytes, false))
        })
    {
        let (chunks, remainder) = bytes.as_chunks::<4>();
        let mut text = chunks
            .iter()
            .map(|&quad| {
                let value = if little_endian {
                    u32::from_le_bytes(quad)
                } else {
                    u32::from_be_bytes(quad)
                };
                char::from_u32(value).unwrap_or('\u{fffd}')
            })
            .collect::<String>();
        if !remainder.is_empty() {
            text.push('\u{fffd}');
        }
        return text;
    }
    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
    if let Some((bytes, little_endian)) = bytes
        .strip_prefix(&[0xff, 0xfe])
        .map(|bytes| (bytes, true))
        .or_else(|| {
            bytes
                .strip_prefix(&[0xfe, 0xff])
                .map(|bytes| (bytes, false))
        })
    {
        let (chunks, remainder) = bytes.as_chunks::<2>();
        let units = chunks.iter().map(|&pair| {
            if little_endian {
                u16::from_le_bytes(pair)
            } else {
                u16::from_be_bytes(pair)
            }
        });
        let mut text = char::decode_utf16(units)
            .map(|result| result.unwrap_or(char::REPLACEMENT_CHARACTER))
            .collect::<String>();
        if !remainder.is_empty() {
            text.push('\u{fffd}');
        }
        text
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    }
}

/// Produces the same lowercase digest used by the report record.
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Escapes text for both HTML content and quoted attributes.
fn html_escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            other => escaped.push(other),
        }
    }
    escaped
}

/// Uses the workflow's common input error classification.
fn invalid(message: impl Into<String>) -> AppError {
    fail("invalidInput", message.into())
}
