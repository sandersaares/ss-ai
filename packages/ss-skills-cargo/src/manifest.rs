use std::fs;
use std::ops::Range;
use std::path::Path;

use toml_edit::{Document, Item, TableLike, Value};

use crate::inventory::{Dependency, ScanFailure, failure, relative_path};

/// Exposes the actual in-memory parser to the nonpublished fuzz harness.
#[cfg(feature = "fuzzing")]
#[doc(hidden)]
pub fn parse_fuzz_manifest(source: &str) -> Result<Vec<Dependency>, crate::InventoryError> {
    Ok(parse_manifest(source, "Cargo.toml".into())?.dependencies)
}

/// Parsed manifest information needed by discovery and classification.
#[derive(Debug, Clone)]
pub(crate) struct Manifest {
    pub(crate) relative_path: String,
    pub(crate) package: Option<String>,
    pub(crate) has_package: bool,
    pub(crate) has_workspace: bool,
    pub(crate) members: Vec<String>,
    pub(crate) exclude: Vec<String>,
    pub(crate) dependencies: Vec<Dependency>,
}

/// Reads and parses a manifest without changing it.
pub(crate) fn read_manifest(path: &Path, root: &Path) -> Result<Manifest, ScanFailure> {
    let text = fs::read_to_string(path)
        .map_err(|error| failure(format!("Cannot read manifest {}: {error}", path.display())))?;
    let relative = relative_path(path, root)?;
    parse_manifest(&text, relative)
        .map_err(|error| failure(format!("Cannot parse manifest {}: {error}", path.display())))
}

/// Interprets an immutable TOML document while retaining source spans.
fn parse_manifest(text: &str, relative_path: String) -> Result<Manifest, ScanFailure> {
    let document =
        Document::parse(text).map_err(|error| failure(format!("Invalid TOML: {error}")))?;
    let table = document.as_table();
    let package_table = optional_table(table.get("package"), "package")?;
    let workspace_table = optional_table(table.get("workspace"), "workspace")?;
    let package = package_table
        .map(|table| optional_string(table.get("name"), "package.name"))
        .transpose()?
        .flatten();
    let members = workspace_table
        .map(|table| string_array(table.get("members"), "workspace.members"))
        .transpose()?
        .flatten()
        .unwrap_or_default();
    let exclude = workspace_table
        .map(|table| string_array(table.get("exclude"), "workspace.exclude"))
        .transpose()?
        .flatten()
        .unwrap_or_default();
    let mut dependencies = Vec::new();
    for (key, section) in dependency_sections() {
        collect_section(
            table.get(key),
            section,
            None,
            text,
            &relative_path,
            package.as_deref(),
            &mut dependencies,
        )?;
    }
    if let Some(workspace) = workspace_table {
        collect_section(
            workspace.get("dependencies"),
            "workspace.dependencies",
            None,
            text,
            &relative_path,
            package.as_deref(),
            &mut dependencies,
        )?;
    }
    if let Some(targets) = optional_table(table.get("target"), "target")? {
        for (target, item) in targets.iter() {
            let target_table = optional_table(Some(item), "target entry")?
                .ok_or_else(|| failure(format!("Invalid target {target}")))?;
            for (key, section) in dependency_sections() {
                collect_section(
                    target_table.get(key),
                    section,
                    Some(target),
                    text,
                    &relative_path,
                    package.as_deref(),
                    &mut dependencies,
                )?;
            }
        }
    }
    dependencies.sort_by_key(|item| item.line);
    Ok(Manifest {
        relative_path,
        package,
        has_package: package_table.is_some(),
        has_workspace: workspace_table.is_some(),
        members,
        exclude,
        dependencies,
    })
}

/// Includes Cargo's historical underscore spellings under the same section name.
fn dependency_sections() -> [(&'static str, &'static str); 5] {
    [
        ("dependencies", "dependencies"),
        ("dev-dependencies", "dev-dependencies"),
        ("dev_dependencies", "dev-dependencies"),
        ("build-dependencies", "build-dependencies"),
        ("build_dependencies", "build-dependencies"),
    ]
}

/// Collects one record for each parsed dependency key.
fn collect_section(
    item: Option<&Item>,
    section: &str,
    target: Option<&str>,
    source: &str,
    manifest: &str,
    package: Option<&str>,
    output: &mut Vec<Dependency>,
) -> Result<(), ScanFailure> {
    let Some(table) = optional_table(item, section)? else {
        return Ok(());
    };
    for (name, value) in table.iter() {
        let key = table
            .key(name)
            .and_then(toml_edit::Key::span)
            .ok_or_else(|| failure(format!("Missing source location for {name}")))?;
        let (style, attrs, span) = match value {
            Item::Value(Value::String(_)) => {
                let span = value
                    .span()
                    .ok_or_else(|| failure(format!("Missing value span for {name}")))?;
                ("bare-string", None, key.start..span.end)
            }
            Item::Value(Value::InlineTable(inline)) => {
                let span = value
                    .span()
                    .ok_or_else(|| failure(format!("Missing value span for {name}")))?;
                let attrs: &dyn TableLike = inline;
                ("inline-table", Some(attrs), key.start..span.end)
            }
            Item::Table(subtable) => {
                let attrs: &dyn TableLike = subtable;
                let span = table_source_span(source, subtable, key)?;
                let style = if subtable.is_dotted() {
                    "dotted-key"
                } else {
                    "sub-table"
                };
                (style, Some(attrs), span)
            }
            _ => return Err(failure(format!("Invalid dependency declaration {name}"))),
        };
        let raw = source
            .get(span.clone())
            .ok_or_else(|| failure(format!("Invalid source span for {name}")))?
            .to_owned();
        let line = source
            .get(..span.start)
            .ok_or_else(|| failure(format!("Invalid source offset for {name}")))?
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count()
            .saturating_add(1);
        let version = if let Some(attrs) = attrs {
            optional_string(attrs.get("version"), "version")?
        } else {
            value.as_str().map(str::to_owned)
        };
        let path = attrs
            .map(|table| optional_string(table.get("path"), "path"))
            .transpose()?
            .flatten();
        let workspace = attrs
            .map(|table| optional_bool(table.get("workspace"), "workspace"))
            .transpose()?
            .flatten();
        let default_features = attrs
            .map(|table| optional_bool(table.get("default-features"), "default-features"))
            .transpose()?
            .flatten();
        let features = attrs
            .map(|table| string_array(table.get("features"), "features"))
            .transpose()?
            .flatten();
        output.push(Dependency {
            manifest: manifest.to_owned(),
            package: package.map(str::to_owned),
            section: section.to_owned(),
            target: target.map(str::to_owned),
            name: name.to_owned(),
            style: style.to_owned(),
            version,
            path,
            workspace,
            default_features,
            features,
            same_workspace: false,
            resolved_manifest: None,
            raw,
            line,
        });
    }
    Ok(())
}

/// Derives the complete table span from the header and parsed value spans.
fn table_source_span(
    source: &str,
    table: &toml_edit::Table,
    key: Range<usize>,
) -> Result<Range<usize>, ScanFailure> {
    let header = table.span();
    let mut start = if table.is_dotted() {
        line_start(source, key.start)?
    } else {
        header.as_ref().map_or(key.start, |span| span.start)
    };
    let mut end = header.map_or(key.end, |span| span.end);
    for (_, value) in table {
        let span = value
            .span()
            .ok_or_else(|| failure("Missing dependency attribute span".into()))?;
        if table.is_dotted() {
            start = start.min(line_start(source, span.start)?);
        }
        end = end.max(span.end);
    }
    Ok(start..end)
}

/// Finds the start of the source line containing an offset.
fn line_start(source: &str, offset: usize) -> Result<usize, ScanFailure> {
    let prefix = source
        .get(..offset)
        .ok_or_else(|| failure("Invalid source offset".into()))?;
    Ok(prefix
        .rfind('\n')
        .map_or(0, |index| index.saturating_add(1)))
}

/// Rejects malformed tables instead of treating them as absent.
fn optional_table<'a>(
    item: Option<&'a Item>,
    name: &str,
) -> Result<Option<&'a dyn TableLike>, ScanFailure> {
    item.map(|item| {
        item.as_table_like()
            .ok_or_else(|| failure(format!("{name} must be a table")))
    })
    .transpose()
}

/// Reads an optional TOML string with strict type checking.
fn optional_string(item: Option<&Item>, name: &str) -> Result<Option<String>, ScanFailure> {
    item.map(|item| {
        item.as_str()
            .map(str::to_owned)
            .ok_or_else(|| failure(format!("{name} must be a string")))
    })
    .transpose()
}

/// Reads an optional TOML boolean with strict type checking.
fn optional_bool(item: Option<&Item>, name: &str) -> Result<Option<bool>, ScanFailure> {
    item.map(|item| {
        item.as_bool()
            .ok_or_else(|| failure(format!("{name} must be a boolean")))
    })
    .transpose()
}

/// Reads an optional array of strings without coercing malformed elements.
fn string_array(item: Option<&Item>, name: &str) -> Result<Option<Vec<String>>, ScanFailure> {
    item.map(|item| {
        let array = item
            .as_array()
            .ok_or_else(|| failure(format!("{name} must be an array")))?;
        array
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| failure(format!("{name} must contain strings")))
            })
            .collect()
    })
    .transpose()
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[expect(
    clippy::indexing_slicing,
    reason = "The tests use fixed, asserted record layouts."
)]
mod tests {
    use super::parse_manifest;

    #[test]
    fn groups_interleaved_dotted_keys_and_preserves_source() {
        let input =
            "[dependencies]\na.version = \"1\"\nb = \"2\"\na.features = [\n \"x\", # kept\n]\n";
        let parsed = parse_manifest(input, "Cargo.toml".into()).unwrap();
        assert_eq!(parsed.dependencies.len(), 2);
        let a = &parsed.dependencies[0];
        assert_eq!(a.style, "dotted-key");
        assert_eq!(a.line, 2);
        assert_eq!(a.features.as_deref(), Some(&["x".into()][..]));
        assert!(a.raw.contains("b = \"2\""));
        assert!(a.raw.contains("# kept"));
    }

    #[test]
    fn accepts_multiline_inline_tables_and_build_sections() {
        let input = "[build-dependencies]\na = {\n version = \"1\", # kept\n features = [],\n}\n[target.'cfg(unix)'.build-dependencies]\nb = \"2\"\n";
        let parsed = parse_manifest(input, "Cargo.toml".into()).unwrap();
        assert_eq!(parsed.dependencies.len(), 2);
        assert_eq!(
            parsed.dependencies[0].raw,
            "a = {\n version = \"1\", # kept\n features = [],\n}"
        );
        assert_eq!(parsed.dependencies[0].features, Some(vec![]));
        assert_eq!(parsed.dependencies[1].target.as_deref(), Some("cfg(unix)"));
    }

    #[test]
    fn rejects_invalid_attribute_types() {
        parse_manifest(
            "[dependencies]\na = { features = [1] }",
            "Cargo.toml".into(),
        )
        .unwrap_err();
    }

    #[test]
    fn normalizes_legacy_dependency_section_names() {
        let parsed = parse_manifest(
            "[dev_dependencies]\na = '1'\n[target.'cfg(unix)'.build_dependencies]\nb = '2'\n",
            "Cargo.toml".into(),
        )
        .unwrap();
        assert_eq!(parsed.dependencies[0].section, "dev-dependencies");
        assert_eq!(parsed.dependencies[1].section, "build-dependencies");
        assert_eq!(parsed.dependencies[1].target.as_deref(), Some("cfg(unix)"));
    }

    #[cfg(feature = "fuzzing")]
    #[test]
    fn fuzz_adapter_uses_the_same_parser_and_preserves_spans() {
        let input = include_str!("../fuzz/seeds/dependencies.toml");
        let dependencies = super::parse_fuzz_manifest(input).unwrap();
        assert_eq!(dependencies.len(), 7);
        for dependency in dependencies {
            let line_start: usize = input
                .split_inclusive('\n')
                .take(dependency.line.saturating_sub(1))
                .map(str::len)
                .sum();
            let tail = input.get(line_start..).unwrap();
            assert!(tail.find(&dependency.raw).unwrap() <= tail.find('\n').unwrap_or(tail.len()));
        }
        super::parse_fuzz_manifest(include_str!("../fuzz/seeds/malformed.toml")).unwrap_err();
    }

    #[test]
    fn bounded_parser_inputs_make_progress() {
        // Bound both generated input length and operations. Each generated
        // input uses a different deterministic state, with no external clock.
        let alphabet = b"abc.[]=,{}\"' \n#01\\";
        for seed in 0_u64..512 {
            let mut state = seed + 1;
            let mut input = String::from("[dependencies]\n");
            for _ in 0..128 {
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                let index = (state >> 32) as usize % alphabet.len();
                input.push(char::from(alphabet[index]));
            }
            let _result = parse_manifest(&input, "Cargo.toml".into());
        }
    }
}
