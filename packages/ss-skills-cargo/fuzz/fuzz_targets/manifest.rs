//! Coverage-guided checks for the source-preserving manifest parser.
#![no_main]

use libfuzzer_sys::fuzz_target;
use ss_skills_cargo::parse_fuzz_manifest;

// The arbitrary limit keeps each invocation small while allowing manifests
// containing several sections and multiline declarations.
const MAX_INPUT_BYTES: usize = 16 * 1024;

fuzz_target!(|input: &[u8]| {
    if input.len() > MAX_INPUT_BYTES {
        return;
    }
    let Ok(source) = std::str::from_utf8(input) else {
        return;
    };
    let Ok(dependencies) = parse_fuzz_manifest(source) else {
        return;
    };
    for dependency in dependencies {
        let line_start: usize = source
            .split_inclusive('\n')
            .take(dependency.line.saturating_sub(1))
            .map(str::len)
            .sum();
        let remaining = source.get(line_start..).expect("line is in the input");
        let first_line_end = remaining.find('\n').unwrap_or(remaining.len());
        let position = remaining.find(&dependency.raw).expect("raw is from source");
        assert!(position <= first_line_end);
        assert!(!dependency.raw.is_empty());
    }
});
