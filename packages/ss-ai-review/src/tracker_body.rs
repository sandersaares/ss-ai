//! Azure DevOps Markdown body encoding and read-back comparison.
//!
//! The caller remains responsible for title and Markdown-format verification,
//! binding a human decision to an item, and persisting that decision.

/// The result of comparing an approved body with a stored description.
#[derive(Debug, PartialEq)]
pub(crate) enum BodyVerdict {
    /// The stored representation is equivalent or was accepted by a person.
    Match,
    /// The stored body clearly differs, or an accepted rewrite has changed.
    Reject,
    /// A person must decide whether the stored description is acceptable.
    Adjudicate(BodyDeviation),
}

/// Evidence for a person deciding an unrecognized rewrite.
#[derive(Debug, PartialEq)]
pub(crate) struct BodyDeviation {
    /// Similarity if the bounded calculation could decide it.
    pub(crate) similarity: Option<f64>,
    /// First difference, measured in UTF-16 code units as in PowerShell.
    pub(crate) first_difference: usize,
}

/// The exact bodies a person accepted for a particular item.
///
/// The caller must associate this evidence with the item and authenticate the
/// decision before supplying it. The comparison never creates an acceptance.
#[derive(Debug)]
pub(crate) struct AcceptedRewrite<'a> {
    /// The body approved before publication.
    pub(crate) approved: &'a str,
    /// The exact stored body accepted by the person.
    pub(crate) stored: &'a str,
}

/// Encodes literal brackets without changing existing Markdown entities.
pub(crate) fn encode_ado_body(body: &str) -> String {
    let mut encoded = String::with_capacity(body.len());
    for character in body.chars() {
        match character {
            '<' => encoded.push_str("&lt;"),
            '>' => encoded.push_str("&gt;"),
            _ => encoded.push(character),
        }
    }
    encoded
}

/// Compares an approved body with the Markdown description read from ADO.
///
/// Call only after checking the title and that the stored format is Markdown.
/// A previous acceptance applies only to the same exact pair of bodies.
pub(crate) fn compare_ado_body(
    approved: &str,
    stored: &str,
    accepted: Option<&AcceptedRewrite<'_>>,
) -> BodyVerdict {
    let sent = encode_ado_body(approved);
    if sent == stored || normalize_lines(&sent) == normalize_lines(stored) {
        return BodyVerdict::Match;
    }

    let approved_normalized = normalize_lines(approved);
    let stored_decoded = normalize_lines(&decode_html(stored));
    // Raw brackets can be interpreted as HTML and disappear when rendered.
    if !stored.contains(['<', '>']) && approved_normalized == stored_decoded {
        return BodyVerdict::Match;
    }

    if let Some(accepted) = accepted {
        return if accepted.approved == approved && accepted.stored == stored {
            BodyVerdict::Match
        } else {
            BodyVerdict::Reject
        };
    }

    let deviation = measure_deviation(&approved_normalized, &stored_decoded);
    // The threshold rejects clear divergence; it never grants approval.
    // Ref: skills/ss-review/scripts/lib/TrackerPublication.ps1.
    if deviation
        .similarity
        .is_some_and(|similarity| similarity < 0.90)
    {
        BodyVerdict::Reject
    } else {
        BodyVerdict::Adjudicate(deviation)
    }
}

/// Exposes body verification to the offline fuzz harness.
///
/// The returned boolean reports automatic equivalence without a human decision.
#[doc(hidden)]
#[must_use]
pub fn fuzz_tracker_body(approved: &str, stored: &str) -> (String, bool) {
    (
        encode_ado_body(approved),
        compare_ado_body(approved, stored, None) == BodyVerdict::Match,
    )
}

/// Normalizes the line endings permitted by the Markdown storage pipeline.
fn normalize_lines(text: &str) -> String {
    let mut normalized = String::with_capacity(text.len());
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        if character == '\r' {
            // A CRLF pair is one line ending, not two.
            _ = characters.next_if_eq(&'\n');
            normalized.push('\n');
        } else {
            normalized.push(character);
        }
    }
    normalized
}

/// Decodes one layer of the HTML entities recognized by .NET `WebUtility`.
#[expect(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::string_slice,
    reason = "The searched ASCII delimiters are UTF-8 boundaries, and all offsets stay within the remaining string"
)]
fn decode_html(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut rest = text;
    let mut previous_len = None;
    while let Some(start) = rest.find('&') {
        // Every continuing iteration must consume input, even under mutation
        // and in optimized fuzz builds where debug assertions are disabled.
        assert!(previous_len.is_none_or(|length| rest.len() < length));
        previous_len = Some(rest.len());
        result.push_str(&rest[..start]);
        rest = &rest[start..];
        let Some(end) = rest[1..].find([';', '&']).map(|offset| offset + 1) else {
            break;
        };
        if rest.as_bytes()[end] == b';'
            && let Some(decoded) = decode_entity(&rest[1..end])
        {
            result.push(decoded);
            rest = &rest[end + 1..];
            continue;
        }
        result.push('&');
        rest = &rest[1..];
    }
    result.push_str(rest);
    result
}

/// Decodes an entity only when it names a Unicode scalar or HTML4 name.
fn decode_entity(entity: &str) -> Option<char> {
    if let Some(numeric) = entity.strip_prefix('#') {
        let (digits, radix) = match numeric.strip_prefix(['x', 'X']) {
            Some(hex) => (hex, 16),
            None => (numeric, 10),
        };
        if digits.is_empty()
            || !digits.bytes().all(|byte| match radix {
                16 => byte.is_ascii_hexdigit(),
                _ => byte.is_ascii_digit(),
            })
        {
            return None;
        }
        return u32::from_str_radix(digits, radix)
            .ok()
            .and_then(char::from_u32);
    }

    HTML_ENTITIES.split_ascii_whitespace().find_map(|entry| {
        let (name, value) = entry.split_once(':')?;
        (name == entity)
            .then(|| u32::from_str_radix(value, 16).ok().and_then(char::from_u32))
            .flatten()
    })
}

// HTML4 names plus apos, matching System.Net.WebUtility's named entities.
// Case is significant, and unknown names are left untouched.
const HTML_ENTITIES: &str = "\
AElig:c6 Aacute:c1 Acirc:c2 Agrave:c0 Alpha:391 Aring:c5 Atilde:c3 Auml:c4 Beta:392 Ccedil:c7 Chi:3a7 Dagger:2021 Delta:394 ETH:d0 Eacute:c9 Ecirc:ca Egrave:c8 Epsilon:395 Eta:397 Euml:cb Gamma:393 Iacute:cd Icirc:ce Igrave:cc Iota:399 Iuml:cf Kappa:39a Lambda:39b Mu:39c Ntilde:d1 Nu:39d OElig:152 Oacute:d3 Ocirc:d4 Ograve:d2 Omega:3a9 Omicron:39f Oslash:d8 Otilde:d5 Ouml:d6 Phi:3a6 Pi:3a0 Prime:2033 Psi:3a8 Rho:3a1 Scaron:160 Sigma:3a3 THORN:de Tau:3a4 Theta:398 Uacute:da Ucirc:db Ugrave:d9 Upsilon:3a5 Uuml:dc Xi:39e Yacute:dd Yuml:178 Zeta:396 \
aacute:e1 acirc:e2 acute:b4 aelig:e6 agrave:e0 alefsym:2135 alpha:3b1 amp:26 and:2227 ang:2220 apos:27 aring:e5 asymp:2248 atilde:e3 auml:e4 bdquo:201e beta:3b2 brvbar:a6 bull:2022 cap:2229 ccedil:e7 cedil:b8 cent:a2 chi:3c7 circ:2c6 clubs:2663 cong:2245 copy:a9 crarr:21b5 cup:222a curren:a4 dArr:21d3 dagger:2020 darr:2193 deg:b0 delta:3b4 diams:2666 divide:f7 eacute:e9 ecirc:ea egrave:e8 empty:2205 emsp:2003 ensp:2002 epsilon:3b5 equiv:2261 eta:3b7 eth:f0 euml:eb euro:20ac exist:2203 fnof:192 forall:2200 frac12:bd frac14:bc frac34:be frasl:2044 gamma:3b3 ge:2265 gt:3e hArr:21d4 harr:2194 hearts:2665 hellip:2026 iacute:ed icirc:ee iexcl:a1 igrave:ec image:2111 infin:221e int:222b iota:3b9 iquest:bf isin:2208 iuml:ef kappa:3ba lArr:21d0 lambda:3bb lang:2329 laquo:ab larr:2190 lceil:2308 ldquo:201c le:2264 lfloor:230a lowast:2217 loz:25ca lrm:200e lsaquo:2039 lsquo:2018 lt:3c macr:af mdash:2014 micro:b5 middot:b7 minus:2212 mu:3bc nabla:2207 nbsp:a0 ndash:2013 ne:2260 ni:220b not:ac notin:2209 nsub:2284 ntilde:f1 nu:3bd oacute:f3 ocirc:f4 oelig:153 ograve:f2 oline:203e omega:3c9 omicron:3bf oplus:2295 or:2228 ordf:aa ordm:ba oslash:f8 otilde:f5 otimes:2297 ouml:f6 para:b6 part:2202 permil:2030 perp:22a5 phi:3c6 pi:3c0 piv:3d6 plusmn:b1 pound:a3 prime:2032 prod:220f prop:221d psi:3c8 quot:22 rArr:21d2 radic:221a rang:232a raquo:bb rarr:2192 rceil:2309 rdquo:201d real:211c reg:ae rfloor:230b rho:3c1 rlm:200f rsaquo:203a rsquo:2019 sbquo:201a scaron:161 sdot:22c5 sect:a7 shy:ad sigma:3c3 sigmaf:3c2 sim:223c spades:2660 sub:2282 sube:2286 sum:2211 sup:2283 sup1:b9 sup2:b2 sup3:b3 supe:2287 szlig:df tau:3c4 there4:2234 theta:3b8 thetasym:3d1 thinsp:2009 thorn:fe tilde:2dc times:d7 trade:2122 uArr:21d1 uacute:fa uarr:2191 ucirc:fb ugrave:f9 uml:a8 upsih:3d2 upsilon:3c5 uuml:fc weierp:2118 xi:3be yacute:fd yen:a5 yuml:ff zeta:3b6 zwj:200d zwnj:200c";

/// Measures a bounded edit distance using PowerShell's UTF-16 string units.
#[expect(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::cast_precision_loss,
    reason = "Shared prefix and suffix lengths bound the slices and subtraction; floating-point ratios match PowerShell's similarity calculation"
)]
fn measure_deviation(sent: &str, stored: &str) -> BodyDeviation {
    let sent: Vec<u16> = sent.encode_utf16().collect();
    let stored: Vec<u16> = stored.encode_utf16().collect();
    let max = sent.len().max(stored.len());
    // Equal ends do not contribute to Levenshtein distance, so only the
    // differing spans need the quadratic calculation.
    // Ref: skills/ss-review/scripts/lib/TrackerPublication.ps1,
    // Measure-TrackerBodyDeviation.
    let prefix = sent.iter().zip(&stored).take_while(|(a, b)| a == b).count();
    let suffix = sent[prefix..]
        .iter()
        .rev()
        .zip(stored[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    if max == 0 {
        return BodyDeviation {
            similarity: Some(1.0),
            first_difference: prefix,
        };
    }
    let left = &sent[prefix..sent.len() - suffix];
    let right = &stored[prefix..stored.len() - suffix];
    // The length gap is a lower bound on distance, so this is a similarity
    // ceiling, not a measured score. Only a ceiling below the rejection
    // threshold settles the decision without calculating distance.
    let ceiling = 1.0 - left.len().abs_diff(right.len()) as f64 / max as f64;
    if ceiling < 0.90 {
        return BodyDeviation {
            similarity: Some(ceiling),
            first_difference: prefix,
        };
    }
    // Limit quadratic work; an unmeasured rewrite requires a human decision.
    // Ref: skills/ss-review/scripts/lib/TrackerPublication.ps1.
    if left
        .len()
        .saturating_add(1)
        .saturating_mul(right.len().saturating_add(1))
        > 4_000_000
    {
        return BodyDeviation {
            similarity: None,
            first_difference: prefix,
        };
    }
    let distance = edit_distance(left, right);
    BodyDeviation {
        similarity: Some(1.0 - distance as f64 / max as f64),
        first_difference: prefix,
    }
}

/// Computes Levenshtein distance of the differing UTF-16 spans.
///
/// Ref: skills/ss-review/scripts/lib/TrackerPublication.ps1,
/// Get-TrackerLevenshteinDistance.
#[expect(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    reason = "Both rows have one cell beyond the right span and each distance is bounded by the allocated span lengths"
)]
fn edit_distance(left: &[u16], right: &[u16]) -> usize {
    let mut previous: Vec<usize> = (0..=right.len()).collect();
    let mut current = vec![0; previous.len()];
    for (i, a) in left.iter().enumerate() {
        current[0] = i + 1;
        for (j, b) in right.iter().enumerate() {
            // Choose the least costly deletion, insertion, or substitution.
            current[j + 1] = (previous[j + 1] + 1)
                .min(current[j] + 1)
                .min(previous[j] + usize::from(a != b));
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[right.len()]
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn encoding_preserves_entities_and_unicode() {
        assert_eq!(
            encode_ado_body("Vec<T> &lt;x&gt; 🦀"),
            "Vec&lt;T&gt; &lt;x&gt; 🦀"
        );
        assert_eq!(encode_ado_body(""), "");
    }

    #[test]
    fn transformations_match_replacement_semantics() {
        // Include entity syntax, adjacent line endings, and multibyte text.
        let fragments = ["<", ">", "&lt;", "\r", "\n", "🦀"];
        let mut inputs = vec![String::new()];
        for _ in 0..4 {
            let next: Vec<_> = inputs
                .iter()
                .flat_map(|prefix| {
                    fragments
                        .iter()
                        .map(move |fragment| format!("{prefix}{fragment}"))
                })
                .collect();
            for input in &next {
                assert_eq!(
                    encode_ado_body(input),
                    input.replace('<', "&lt;").replace('>', "&gt;")
                );
                assert_eq!(
                    normalize_lines(input),
                    input.replace("\r\n", "\n").replace('\r', "\n")
                );
            }
            inputs = next;
        }
    }

    #[test]
    fn compares_representations_without_decoding_approved_entities() {
        assert_eq!(
            compare_ado_body("x &lt;\ny", "x &lt;\r\ny", None),
            BodyVerdict::Match
        );
        assert_eq!(
            compare_ado_body(
                "<x> &lt; ©\r\n",
                "&lt;x&gt; &amp;lt; &copy;&#13;&#10;",
                None
            ),
            BodyVerdict::Match
        );
        assert_ne!(compare_ado_body("<x>", "<x>", None), BodyVerdict::Match);
        assert_ne!(
            compare_ado_body("&lt;", "&amp;lt;", None),
            BodyVerdict::Reject
        );
    }

    #[test]
    fn decodes_one_layer_and_preserves_unknown_or_malformed_entities() {
        assert_eq!(
            decode_html("&amp;lt; &copy; &apos; &#65; &#x1F600;"),
            "&lt; © ' A 😀"
        );
        assert_eq!(decode_html("&Igrave; &lang; &zwnj;"), "Ì 〈 \u{200c}");
        assert_eq!(
            decode_html("&bad&copy; &#xD800; &#1114112; &LT; &#x+41;"),
            "&bad© &#xD800; &#1114112; &LT; &#x+41;"
        );
        assert_eq!(decode_html("&#65 &unfinished"), "&#65 &unfinished");
        assert_eq!(decode_html("&#13;&#10;"), "\r\n");
    }

    #[test]
    fn ambiguous_changes_require_human_decision() {
        let approved = "A sufficiently long finding that must retain its meaning.";
        let changed = "A sufficiently long finding that must retain its meanIng.";
        assert!(matches!(
            compare_ado_body(approved, changed, None),
            BodyVerdict::Adjudicate(_)
        ));
        assert_eq!(
            compare_ado_body(approved, "unrelated", None),
            BodyVerdict::Reject
        );
        let accepted = AcceptedRewrite {
            approved,
            stored: changed,
        };
        assert_eq!(
            compare_ado_body(approved, changed, Some(&accepted)),
            BodyVerdict::Match
        );
        assert_eq!(
            compare_ado_body(approved, "another rewrite", Some(&accepted)),
            BodyVerdict::Reject
        );
        assert_eq!(
            compare_ado_body("different approval", changed, Some(&accepted)),
            BodyVerdict::Reject
        );
        assert_eq!(
            compare_ado_body(approved, approved, Some(&accepted)),
            BodyVerdict::Match
        );
    }

    #[test]
    fn distance_matches_threshold_and_utf16_semantics() {
        let deviation = measure_deviation("commonXtail", "commonYtail");
        assert_eq!(deviation.first_difference, 6);
        assert!(deviation.similarity.unwrap() > 0.90);
        assert_eq!(
            measure_deviation("ABCDEFGHIJ", "KLMNOPQRS").similarity,
            Some(0.0)
        );
        assert!(matches!(
            compare_ado_body("abcdefghij", "abcdefghiX", None),
            BodyVerdict::Adjudicate(_)
        ));
        assert_eq!(
            compare_ado_body("abcdefghij", "abcdefghXY", None),
            BodyVerdict::Reject
        );
        assert_eq!(measure_deviation("😀a", "😀b").first_difference, 2);
        assert_eq!(measure_deviation("", "").similarity, Some(1.0));
    }

    #[test]
    fn oversized_divergent_bodies_do_not_match() {
        let approved = "a".repeat(2000);
        let stored = "b".repeat(2000);
        assert_ne!(
            compare_ado_body(&approved, &stored, None),
            BodyVerdict::Match
        );
    }
}
