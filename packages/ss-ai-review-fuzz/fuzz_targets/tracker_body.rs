//! Exercises ADO entity parsing and body comparison without provider effects.
#![no_main]

use libfuzzer_sys::fuzz_target;
use ss_ai_review::fuzz_tracker_body;

// Include complete, incomplete, numeric, named and nested entities so the
// decoder sees structured input even when arbitrary bytes contain no '&'.
const FRAGMENTS: [&str; 16] = [
    "&amp;",
    "&lt;",
    "&#13;",
    "&#x1F600;",
    "&copy;",
    "&unknown;",
    "&",
    "&#xD800;",
    "&amp;lt;",
    "<T>",
    ">",
    "\r\n",
    "🦀",
    "&#0;",
    "&#x+41;",
    ";",
];

/// Produces the whole-body entity encoding accepted on ADO readback.
fn encode_all(text: &str) -> String {
    let mut encoded = String::new();
    for character in text.chars() {
        match character {
            '&' => encoded.push_str("&amp;"),
            '<' => encoded.push_str("&lt;"),
            '>' => encoded.push_str("&gt;"),
            '"' => encoded.push_str("&quot;"),
            '\'' => encoded.push_str("&#39;"),
            _ => encoded.push(character),
        }
    }
    encoded
}

fuzz_target!(|data: &[u8]| {
    // Keep comparisons and parser scans small enough for repeated fuzzing.
    let data = data.get(..data.len().min(512)).unwrap();
    let split = data.len().checked_div(2).unwrap();
    let approved = String::from_utf8_lossy(data.get(..split).unwrap());
    let stored = String::from_utf8_lossy(data.get(split..).unwrap());
    let (sent, equivalent) = fuzz_tracker_body(&approved, &stored);
    assert_eq!(
        (sent.clone(), equivalent),
        fuzz_tracker_body(&approved, &stored),
        "comparison must be deterministic"
    );
    assert!(fuzz_tracker_body(&approved, &sent).1);

    // The representational transformations are the only automatic equivalences.
    let reended = sent
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .replace('\n', "\r\n");
    assert!(fuzz_tracker_body(&approved, &reended).1);
    assert!(fuzz_tracker_body(&approved, &encode_all(&approved)).1);

    let mut structured = String::new();
    for byte in data.iter().take(32) {
        let index = usize::from(*byte).checked_rem(FRAGMENTS.len()).unwrap();
        structured.push_str(FRAGMENTS.get(index).unwrap());
    }
    let encoded = encode_all(&structured);
    assert!(fuzz_tracker_body(&structured, &encoded).1);
    _ = fuzz_tracker_body(&structured, &stored);

    // Disjoint alphabets cannot become equal through line or entity decoding.
    let lower: String = data
        .iter()
        .take(128)
        .map(|byte| char::from(b'a'.checked_add(byte.checked_rem(26).unwrap()).unwrap()))
        .collect();
    let upper: String = data
        .iter()
        .take(128)
        .map(|byte| char::from(b'A'.checked_add(byte.checked_rem(26).unwrap()).unwrap()))
        .collect();
    let lower = format!("a{lower}");
    let upper = format!("A{upper}");
    assert!(!fuzz_tracker_body(&lower, &upper).1);
});
