//! Exercises parser-backed inventory through the audit initialization boundary.
#![expect(
    clippy::indexing_slicing,
    reason = "The regression tests select named fields from their own JSON fixtures."
)]

use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use jiff::Timestamp;
use ohno::AppError;
use serde_json::Value;
use ss_ai_runtime::{Clock, Process, ProcessOutput};
use ss_ai_test_audit::initialize;
use tempfile::tempdir_in;

/// Keeps audit initialization independent of wall time and retries.
struct FixedClock;

impl Clock for FixedClock {
    fn now(&self) -> Timestamp {
        Timestamp::UNIX_EPOCH
    }

    fn wait(&self, _: Duration) -> Result<(), AppError> {
        panic!("An isolated scanner fixture should have no lock contention")
    }
}

/// Gives each fixture a Git root distinct from the enclosing checkout.
struct FixtureGit;

impl Process for FixtureGit {
    fn execute(&self, _: &str, _: &[String], _: &[u8]) -> io::Result<ProcessOutput> {
        panic!("Scanner fixtures must not launch processes")
    }

    fn isolated_git(&self, directory: &Path, arguments: &[String]) -> io::Result<ProcessOutput> {
        assert_eq!(arguments.first().map(String::as_str), Some("rev-parse"));
        let output = match arguments.get(1).map(String::as_str) {
            Some("--show-toplevel") => directory.to_str().unwrap(),
            Some("HEAD") => "0123456789abcdef0123456789abcdef01234567",
            _ => panic!("Unexpected Git query"),
        };
        Ok(ProcessOutput::new(
            true,
            output.as_bytes().to_vec(),
            Vec::new(),
        ))
    }
}

/// Reads the persisted inventory before disposable build fixtures are removed.
fn scan(populate: impl FnOnce(&Path)) -> Value {
    let fixture = tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let repository = fixture.path().join("repository");
    let state = fixture.path().join("state");
    write(&repository, ".git", "gitdir: ../fixture-git");
    populate(&repository);
    initialize(&repository, &[], &state, &FixtureGit, &FixedClock).unwrap();
    serde_json::from_slice(&fs::read(state.join("inventory.json")).unwrap()).unwrap()
}

/// Adds a source or evidence file to the selected repository.
fn write(repository: &Path, path: &str, source: &str) {
    let path = repository.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, source).unwrap();
}

#[test]
fn narrow_assertion_scope_indexes_repository_contract_evidence() {
    let fixture = tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let repository = fixture.path().join("repository");
    let state = fixture.path().join("state");
    write(&repository, ".git", "gitdir: ../fixture-git");
    write(
        &repository,
        "docs/public.md",
        "# Public contract\n\n`value` returns one.\n",
    );
    write(
        &repository,
        "tests/check.rs",
        "#[test]\nfn checks_value() { assert_eq!(1, 1); }\n",
    );
    initialize(
        &repository,
        &[PathBuf::from("tests")],
        &state,
        &FixtureGit,
        &FixedClock,
    )
    .unwrap();
    let inventory: Value =
        serde_json::from_slice(&fs::read(state.join("inventory.json")).unwrap()).unwrap();
    assert_eq!(inventory["scopePaths"], serde_json::json!(["tests"]));
    assert_eq!(rows(&inventory, "tests").len(), 1);
    assert!(
        rows(&inventory, "files")
            .iter()
            .any(|file| file["path"] == "docs/public.md")
    );
    let index = fs::read_to_string(state.join("search-index.jsonl")).unwrap();
    assert!(index.contains("value` returns one"));
}

/// Accesses a collection in the emitted inventory by its public field name.
fn rows<'a>(inventory: &'a Value, field: &str) -> &'a [Value] {
    inventory.get(field).unwrap().as_array().unwrap()
}

/// Finds a predicate by the expression it contributes to the inventory.
fn assertion<'a>(inventory: &'a Value, expression: &str) -> &'a Value {
    rows(inventory, "assertions")
        .iter()
        .find(|row| row["expression"] == expression)
        .unwrap_or_else(|| panic!("Missing assertion: {expression}"))
}

#[test]
fn links_callers_to_the_resolved_helper_not_its_terminal_name() {
    let inventory = scan(|repository| {
        write(
            repository,
            "src/lib.rs",
            "
#[cfg(test)]
mod first {
    pub(crate) fn check() { assert!(first_contract()); }

    #[test]
    fn bare() { check(); }

    #[test]
    fn shadowed() { let check = || {}; check(); }

    mod nested {
        #[test]
        fn parent() { super::check(); }
    }
}

#[cfg(test)]
mod second {
    pub(crate) fn check() { assert!(second_contract()); }
}

#[test]
fn qualified() { self::second::check(); }

#[test]
fn from_root() { crate::first::check(); }

#[test]
fn unrelated() { external_crate::check(); }
",
        );
        write(
            repository,
            "src/other.rs",
            "#[cfg(test)] fn check() { assert!(separate_file()); }",
        );
    });

    let callers = |expression| {
        rows(assertion(&inventory, expression), "callingTestIds")
            .iter()
            .map(|id| {
                rows(&inventory, "tests")
                    .iter()
                    .find(|test| test["id"] == *id)
                    .unwrap()["name"]
                    .as_str()
                    .unwrap()
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        callers("assert!(first_contract())"),
        ["bare", "parent", "from_root"]
    );
    assert_eq!(callers("assert!(second_contract())"), ["qualified"]);
    assert!(callers("assert!(separate_file())").is_empty());
}

#[test]
fn compile_checks_require_the_matching_bounded_function() {
    let inventory = scan(|repository| {
        write(
            repository,
            "src/lib.rs",
            "
mod bounded { pub(crate) fn check<T: Copy>() {} }
mod unbounded { pub(crate) fn check<T>() {} }
mod ambiguous {
    #[cfg(unix)] pub(crate) fn check<T: Copy>() {}
    #[cfg(not(unix))] pub(crate) fn check<T>() {}
}

#[test]
fn instantiate() {
    bounded::check::<u8>();
    unbounded::check::<u8>();
    ambiguous::check::<u8>();
    external_crate::check::<u8>();
    fn check<T: Copy>() {}
    check::<u8>();
}
",
        );
    });

    let sites = rows(&inventory, "assertions");
    assert_eq!(sites.len(), 2);
    for expression in ["bounded::check::<u8>()", "check::<u8>()"] {
        assert_eq!(
            assertion(&inventory, expression)["kind"],
            "compileTimeAssertion"
        );
    }
}

#[test]
fn cfg_diagnostics_follow_ast_owners_and_keep_same_line_sites_distinct() {
    let inventory = scan(|repository| {
        write(
            repository,
            "src/lib.rs",
            r#"
#[cfg(feature = "outer")]
mod nested {
    #[cfg(unix)]
    fn check() {
        #[cfg(debug_assertions)] static_assertions::const_assert!(a()); static_assertions::const_assert!(b());
    }
}

#[cfg(test)]
mod tests {
    fn check<T: Copy>() {}
    #[test]
    fn compile() {
        #[cfg_attr(unix, cfg(debug_assertions))]
        check::<u8>();
        static_assertions::const_assert!(ungated());
    }
}
"#,
        );
    });
    let conditional = rows(&inventory, "diagnostics")
        .iter()
        .filter(|row| row["kind"] == "conditionalAssertion")
        .collect::<Vec<_>>();
    for (expression, gates) in [
        (
            "static_assertions::const_assert!(a())",
            r#"#[cfg(feature = "outer")] #[cfg(unix)] #[cfg(debug_assertions)]"#,
        ),
        (
            "static_assertions::const_assert!(b())",
            r#"#[cfg(feature = "outer")] #[cfg(unix)]"#,
        ),
        ("check::<u8>()", "#[cfg_attr(unix, cfg(debug_assertions))]"),
    ] {
        let site = assertion(&inventory, expression);
        let id = site["id"].as_str().unwrap();
        let expected = format!("Assertion '{id}' is conditional on {gates}");
        assert!(
            conditional
                .iter()
                .any(|row| row["line"] == site["line"] && row["detail"] == expected),
            "{expected}"
        );
    }
    assert_eq!(conditional.len(), 3);
}

#[test]
fn detects_static_assertion_families_and_evaluated_const_blocks() {
    let inventory = scan(|repository| {
        write(
            repository,
            "src/lib.rs",
            "
static_assertions::const_assert!(true);
static_assertions::const_assert_eq!(1, 1);
static_assertions::const_assert_ne!(1, 2);
static_assertions::assert_type_eq_all!(u8, u8);
static_assertions::assert_eq_size!(u8, i8);
const _: () = { assert!(true); };
struct Value;
impl Value { const CHECK: () = assert!(1 < 2); }

#[test]
fn inline_const() {
    let _ = const { assert!(2 < 3); };
    assert!(runtime());
}
",
        );
    });
    for expression in [
        "static_assertions::const_assert!(true)",
        "static_assertions::const_assert_eq!(1, 1)",
        "static_assertions::const_assert_ne!(1, 2)",
        "static_assertions::assert_type_eq_all!(u8, u8)",
        "static_assertions::assert_eq_size!(u8, i8)",
        "assert!(true)",
        "assert!(1 < 2)",
        "assert!(2 < 3)",
    ] {
        assert_eq!(
            assertion(&inventory, expression)["kind"],
            "compileTimeAssertion"
        );
    }
    assert_eq!(
        assertion(&inventory, "assert!(runtime())")["kind"],
        "assertion"
    );
}

#[test]
fn retains_expect_candidates_without_evidence_of_fixture_access() {
    let inventory = scan(|repository| {
        write(
            repository,
            "src/lib.rs",
            r#"
#[test]
fn fixture_and_api_access() {
    let reader = api();
    let mut buffer = [];
    reader.read(&mut buffer).expect("API read");
    reader.read().expect("zero-argument API read");
    reader.as_ref().expect("API reference");
    reader.join().expect("API join");
    unsafe { reader.as_ref() }.expect("ambiguous unsafe access");

    let lock = std::sync::RwLock::new(0);
    lock.read().expect("fixture lock");
    let worker = std::thread::spawn(|| {});
    worker.join().expect("fixture thread");
    unsafe { (&raw const buffer).as_ref() }.expect("fixture pointer");
}
"#,
        );
    });
    let sites = rows(&inventory, "assertions");
    assert_eq!(sites.len(), 5);
    for expression in [
        r#"reader.read(&mut buffer).expect("API read")"#,
        r#"reader.read().expect("zero-argument API read")"#,
        r#"reader.as_ref().expect("API reference")"#,
        r#"reader.join().expect("API join")"#,
        r#"unsafe { reader.as_ref() }.expect("ambiguous unsafe access")"#,
    ] {
        assert_eq!(
            assertion(&inventory, expression)["kind"],
            "fallibleValueCheck"
        );
    }
}

#[test]
fn rustdoc_flags_are_order_independent_and_ignored_fences_are_skipped() {
    let inventory = scan(|repository| {
        write(
            repository,
            "src/lib.rs",
            r#"
//! ```rust,ignore
//! assert!(ignored_first());
//! ```
//!
//! ````ignore,rust
//! ```rust
//! assert!(ignored_second());
//! ```
//! ````
//!
//! ```rust,should_panic
//! # panic!("expected");
//! ```
//!
//! ```edition2024, should_panic, rust
//! panic!("also expected");
//! ```
//!
//! ```no_run,rust
//! assert!(checked());
//! ```
"#,
        );
    });
    assert_eq!(rows(&inventory, "tests").len(), 3);
    assert_eq!(rows(&inventory, "assertions").len(), 3);
    for marker in ["```rust,should_panic", "```edition2024, should_panic, rust"] {
        assert_eq!(assertion(&inventory, marker)["kind"], "panicExpectation");
    }
    assert_eq!(
        assertion(&inventory, "assert!(checked());")["kind"],
        "assertion"
    );
}

#[test]
fn scans_source_bin_directories_without_indexing_build_outputs() {
    let inventory = scan(|repository| {
        write(repository, "Cargo.toml", "[workspace]");
        write(repository, "src/mixed.csproj", "<Project />");
        write(
            repository,
            "src/bin/cli.rs",
            "#[test] fn cli() { assert!(cli_contract()); }",
        );
        write(
            repository,
            "crates/app/src/bin/tool/main.rs",
            "#[test] fn tool() { assert!(tool_contract()); }",
        );
        write(
            repository,
            "src/target/module.rs",
            "#[test] fn module() { assert!(not_build_output()); }",
        );
        write(
            repository,
            "target/debug/generated.rs",
            "#[test] fn generated() {}",
        );
        write(repository, "dotnet/example.csproj", "<Project />");
        write(repository, "dotnet/bin/Generated.cs", "generated");
    });
    for expression in [
        "assert!(cli_contract())",
        "assert!(tool_contract())",
        "assert!(not_build_output())",
    ] {
        assert_eq!(assertion(&inventory, expression)["kind"], "assertion");
    }
    assert!(!rows(&inventory, "files").iter().any(|file| {
        let path = file["path"].as_str().unwrap();
        matches!(
            path,
            ".git" | "target/debug/generated.rs" | "dotnet/bin/Generated.cs"
        )
    }));
}

#[test]
fn preserves_utf8_spans_and_byte_columns_for_rust_assertions() {
    let source = r#"
#[test]
fn unicode() {
    let café = "🦀"; assert_eq!(café, "🦀");
    let é = "é"; assert_eq!(
        é, "é");
}
"#;
    let bom_source = "\u{feff}#[test] fn bom() { assert!(true); }";
    let inventory = scan(|repository| {
        write(repository, "src/lib.rs", source);
        write(repository, "src/bom.rs", bom_source);
    });
    for (expression, source_line) in [
        (
            r#"assert_eq!(café, "🦀")"#,
            r#"    let café = "🦀"; assert_eq!(café, "🦀");"#,
        ),
        (
            "assert_eq!(\n        é, \"é\")",
            r#"    let é = "é"; assert_eq!("#,
        ),
    ] {
        let site = assertion(&inventory, expression);
        let byte_column = source_line.find("assert_eq!").unwrap().saturating_add(1);
        assert_eq!(site["column"], byte_column);
    }
    let bom_column = bom_source.find("assert!").unwrap().saturating_add(1);
    assert_eq!(assertion(&inventory, "assert!(true)")["column"], bom_column);
}

#[test]
fn accepts_the_file_limit_and_excludes_larger_inputs() {
    // Whitespace exercises the 4 MiB read cap without creating a huge index.
    const LIMIT: usize = 4 * 1024 * 1024;
    let inventory = scan(|repository| {
        fs::write(repository.join("limit.md"), vec![b' '; LIMIT]).unwrap();
        File::create(repository.join("over.md"))
            .unwrap()
            .set_len(LIMIT.saturating_add(1) as u64)
            .unwrap();
    });

    assert!(
        rows(&inventory, "files")
            .iter()
            .any(|row| { row["path"] == "limit.md" && row["byteCount"] == LIMIT })
    );
    assert!(
        !rows(&inventory, "files")
            .iter()
            .any(|row| row["path"] == "over.md")
    );
    assert!(
        rows(&inventory, "diagnostics")
            .iter()
            .any(|row| { row["path"] == "over.md" && row["kind"] == "oversizedFile" })
    );
}
