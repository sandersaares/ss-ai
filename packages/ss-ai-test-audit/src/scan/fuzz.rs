//! Exercises the fence state machine with bounded combinations of Rustdoc input.

use std::fmt::Write as _;

use super::{ScanInput, scan_doctests};

/// Mixes independent fence choices and checks ownership without reading files.
#[test]
fn fuzz_doctest_fences_with_bounded_comment_sequences() {
    for flags in [
        "rust",
        "text",
        "rust,ignore",
        "should_panic,rust",
        "no_run,rust",
    ] {
        for (marker, other) in [('`', '~'), ('~', '`')] {
            for width in [3_usize, 4, 5] {
                for ending in ["matching", "wider", "narrower", "wrong", "no close"] {
                    let opening = marker.to_string().repeat(width);
                    let mut source = format!(
                        "//! assert!(outside_before());\n//! {opening}{flags}\n\
                         //! assert!(inside_once());\n//! panic!(\"expected\");\n"
                    );
                    let closed = matches!(ending, "matching" | "wider");
                    if ending != "no close" {
                        let delimiter = match ending {
                            "matching" => opening,
                            "wider" => marker.to_string().repeat(width + 1),
                            "narrower" => marker.to_string().repeat(width - 1),
                            "wrong" => other.to_string().repeat(width),
                            _ => unreachable!(),
                        };
                        writeln!(source, "//! {delimiter}").unwrap();
                    }
                    if closed {
                        source.push_str("//! assert!(outside_after());\n//! ```rust\n//! assert!(second_fence());\n//! ```\n");
                    }
                    // This grammar cannot generate more lines or bytes than
                    // these fixture budgets; check them before invoking the parser.
                    let lines: Vec<_> = source.lines().collect();
                    assert!(source.len() <= 256 && lines.len() <= 10);
                    let (mut tests, mut assertions, mut diagnostics) =
                        (Vec::new(), Vec::new(), Vec::new());
                    scan_doctests(
                        &mut tests,
                        &mut assertions,
                        &mut diagnostics,
                        ScanInput {
                            path: "lib.rs",
                            language: "rust",
                            file_sha256: "source",
                            text: &source,
                            lines: &lines,
                        },
                    );
                    let recognized = flags != "text";
                    assert_eq!(tests.len(), usize::from(recognized) + usize::from(closed));
                    for pair in tests.windows(2) {
                        let [earlier, later] = pair else {
                            unreachable!()
                        };
                        assert!(earlier.end_line < later.line.get());
                    }
                    for test in &tests {
                        assert!(test.line.get() <= test.end_line && test.end_line <= lines.len());
                    }
                    for site in &assertions {
                        let owner = tests
                            .iter()
                            .find(|test| Some(&test.id) == site.containing_test_id.as_ref())
                            .unwrap();
                        assert!(
                            owner.line <= site.line
                                && site.line.get() <= site.end_line
                                && site.end_line <= owner.end_line
                        );
                        assert!(!site.expression.contains("outside_"));
                    }
                    let contains =
                        |expression| assertions.iter().any(|site| site.expression == expression);
                    assert_eq!(contains("assert!(second_fence())"), closed);
                    if closed || ending == "no close" {
                        assert_eq!(
                            contains("assert!(inside_once())"),
                            recognized && !matches!(flags, "no_run,rust" | "rust,ignore")
                        );
                        assert_eq!(contains("panic!(\"expected\")"), flags == "rust");
                    }
                    assert_eq!(
                        diagnostics
                            .iter()
                            .any(|item| item.kind == "unterminatedDoctest"),
                        recognized && !closed
                    );
                }
            }
        }
    }
}
