//! Exercises overview heading and fence transitions without publication effects.
#![no_main]

use std::fmt::Write;
use std::path::Path;

use libfuzzer_sys::fuzz_target;
use ss_ai_review::overview_text_is_publishable;

/// Required sections form the contract for an accepted overview.
const HEADINGS: [&str; 9] = [
    "Mission",
    "Substance",
    "Design changes",
    "Significant changes",
    "Structure",
    "Mechanism",
    "Completeness assessment",
    "Related history",
    "Related work",
];

/// Builds the required sections, optionally omitting one to test rejection.
fn sections(omit: Option<usize>) -> String {
    let mut text = String::new();
    for (index, heading) in HEADINGS.iter().enumerate() {
        if omit != Some(index) {
            writeln!(text, "## {heading}\nContent").unwrap();
        }
    }
    text
}

fuzz_target!(|data: &[u8]| {
    let output = Path::new("/__overview_fuzz_output__");
    let skill = Path::new("/__overview_fuzz_skill__");
    let validate = |text: &str| overview_text_is_publishable(text, output, skill);
    let mut text = sections(None);

    // Cap generated blocks and marker widths so every invocation remains small,
    // while each byte selects a fence transition and a hidden heading.
    for &byte in data.iter().take(16) {
        let marker = if byte & 1 == 0 { '`' } else { '~' };
        let other = if marker == '`' { '~' } else { '`' };
        let width = 4 + usize::from(byte % 5);
        let indentation = " ".repeat(usize::from((byte >> 1) % 4));
        let fence = marker.to_string().repeat(width);
        let other_fence = other.to_string().repeat(width);
        let short_fence = marker.to_string().repeat(3);
        let heading_index = usize::from(byte).checked_rem(HEADINGS.len()).unwrap();
        let heading = HEADINGS.get(heading_index).unwrap();
        let newline = match (byte >> 3) % 3 {
            0 => "\n",
            1 => "\r",
            _ => "\r\n",
        };
        write!(
            text,
            "{indentation}{fence}{newline}{indentation}{short_fence}{newline}{indentation}{other_fence}{newline}## {heading}{newline}{indentation}{fence}{newline}"
        )
        .unwrap();
    }

    assert!(
        validate(&text),
        "fenced headings must not count as sections"
    );
    text.push_str("## Mission\nContent\n");
    assert!(!validate(&text), "an unfenced duplicate must be rejected");

    let omitted = usize::from(data.first().copied().unwrap_or_default())
        .checked_rem(HEADINGS.len())
        .unwrap();
    assert!(
        !validate(&sections(Some(omitted))),
        "every required heading must be present"
    );

    // Arbitrary malformed input exercises rejection and decoding without
    // imposing an acceptance oracle on unrelated path and placeholder gates.
    let bytes = data.get(..data.len().min(1024)).unwrap();
    let arbitrary = String::from_utf8_lossy(bytes);
    _ = validate(&arbitrary);
});
