//! PowerShell-compatible JSON bytes used for capture fingerprints.

use std::fmt::{self, Write};

use ohno::AppError;
use serde::de::{Deserialize, Deserializer, MapAccess, Visitor};
use serde_json::{Map, Number, Value, value::RawValue};
use sha2::{Digest, Sha256};
use ss_ai_runtime::{caused_by, fail};

/// Reads JSON without discarding duplicate or ambiguous property names.
pub(crate) fn parse_text(text: &str) -> Result<Value, AppError> {
    parse_value(text, 0)
}

/// Retains every original object entry so duplicate keys cannot disappear in parsing.
struct ObjectEntries(Vec<(String, Box<RawValue>)>);

impl<'de> Deserialize<'de> for ObjectEntries {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ObjectVisitor;

        impl<'de> Visitor<'de> for ObjectVisitor {
            type Value = ObjectEntries;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a JSON object")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut entries = Vec::new();
                while let Some(entry) = map.next_entry()? {
                    entries.push(entry);
                }
                Ok(ObjectEntries(entries))
            }
        }

        deserializer.deserialize_map(ObjectVisitor)
    }
}

/// Materializes nested objects in source order without collapsing properties.
fn parse_value(text: &str, depth: usize) -> Result<Value, AppError> {
    // System.Text.Json's default maximum depth applies to capture input.
    if depth > 64 {
        return Err(fail("invalidInput", "JSON nesting exceeds capture depth"));
    }
    let text = text.trim_start_matches([' ', '\n', '\r', '\t']);
    if text.starts_with('{') {
        let ObjectEntries(raw) = serde_json::from_str(text)
            .map_err(|error| caused_by("invalidInput", "Invalid JSON", error))?;
        let mut entries: Vec<(String, Value)> = Vec::new();
        for (key, raw) in raw {
            let value = parse_value(raw.get(), depth.saturating_add(1))?;
            if equivalent_key(entries.iter().map(|(key, _)| key.as_str()), &key).is_some() {
                return Err(fail("invalidInput", "Duplicate or ambiguous JSON property"));
            }
            entries.push((key, value));
        }
        return Ok(Value::Object(entries.into_iter().collect::<Map<_, _>>()));
    }
    if text.starts_with('[') {
        let values: Vec<Box<RawValue>> = serde_json::from_str(text)
            .map_err(|error| caused_by("invalidInput", "Invalid JSON", error))?;
        return Ok(Value::Array(
            values
                .iter()
                .map(|value| parse_value(value.get(), depth.saturating_add(1)))
                .collect::<Result<_, _>>()?,
        ));
    }
    serde_json::from_str(text)
        .map_err(|error| caused_by("invalidInput", "Invalid JSON", error).into())
}

/// Finds exact duplicates and case-variant ASCII names.
pub(crate) fn equivalent_key<'a>(keys: impl Iterator<Item = &'a str>, name: &str) -> Option<usize> {
    for (index, key) in keys.enumerate() {
        if key == name || (key.is_ascii() && name.is_ascii() && key.eq_ignore_ascii_case(name)) {
            return Some(index);
        }
    }
    None
}

/// Serializes a JSON value like PowerShell's compressed JSON conversion.
///
/// Object order is significant. Numbers follow the capture reader's Int64 or
/// Double materialization, and strings retain their original contents.
///
/// # Errors
///
/// Rejects ambiguous property names and inputs that cannot be represented
/// faithfully in the PowerShell format.
pub fn powershell_json(value: &Value) -> Result<String, AppError> {
    let mut output = String::new();
    serialize(value, &mut output, 0)?;
    Ok(output)
}

/// Hashes the PowerShell-compatible compressed UTF-8 representation.
///
/// # Errors
///
/// Returns an error when the value cannot be serialized faithfully.
pub fn document_hash(value: &Value) -> Result<String, AppError> {
    let bytes = powershell_json(value)?;
    Ok(bytes_hash(bytes.as_bytes()))
}

/// Hashes exact input bytes using the workflow's uppercase SHA-256 form.
#[must_use]
pub fn bytes_hash(bytes: &[u8]) -> String {
    format!("{:X}", Sha256::digest(bytes))
}

/// Hashes the fixed snapshot envelope without copying the capture.
pub(crate) fn snapshot_hash(policy_version: &str, document: &Value) -> Result<String, AppError> {
    let mut output = String::from("{\"policyVersion\":");
    serialize_string(policy_version, &mut output);
    output.push_str(",\"document\":");
    // The document is one level below the envelope, just as in an owned Value.
    serialize(document, &mut output, 1)?;
    output.push('}');
    Ok(format!("{:X}", Sha256::digest(output.as_bytes())))
}

/// Formats values recursively without changing their insertion order.
fn serialize(value: &Value, output: &mut String, depth: usize) -> Result<(), AppError> {
    // PowerShell's explicit depth limit avoids silently hashing truncated data.
    if depth > 100 {
        return Err(fail(
            "invalidInput",
            "JSON nesting exceeds PowerShell depth",
        ));
    }
    match value {
        Value::Null => output.push_str("null"),
        Value::Bool(value) => output.push_str(if *value { "true" } else { "false" }),
        Value::Number(value) => output.push_str(&format_number(value)?),
        Value::String(value) => serialize_string(value, output),
        Value::Array(values) => {
            output.push('[');
            for (index, value) in values.iter().enumerate() {
                if index != 0 {
                    output.push(',');
                }
                serialize(value, output, depth.saturating_add(1))?;
            }
            output.push(']');
        }
        Value::Object(values) => {
            output.push('{');
            for (index, key) in values.keys().enumerate() {
                if equivalent_key(values.keys().take(index).map(String::as_str), key).is_some() {
                    // A Value may have already discarded later exact duplicates,
                    // so it cannot establish which casing was last in the input.
                    return Err(fail("invalidInput", "Ambiguous JSON property names"));
                }
            }
            for (index, (key, value)) in values.iter().enumerate() {
                if index != 0 {
                    output.push(',');
                }
                serialize_string(key, output);
                output.push(':');
                serialize(value, output, depth.saturating_add(1))?;
            }
            output.push('}');
        }
    }
    Ok(())
}

/// Matches Newtonsoft's default string escaping, including JavaScript separators.
fn serialize_string(value: &str, output: &mut String) {
    output.push('"');
    for character in value.chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\u{8}' => output.push_str("\\b"),
            '\t' => output.push_str("\\t"),
            '\n' => output.push_str("\\n"),
            '\u{c}' => output.push_str("\\f"),
            '\r' => output.push_str("\\r"),
            '\u{0}'..='\u{1f}' | '\u{85}' | '\u{2028}' | '\u{2029}' => {
                write!(output, "\\u{:04x}", u32::from(character))
                    .expect("writing to a string cannot fail");
            }
            _ => output.push(character),
        }
    }
    output.push('"');
}

/// Matches the invariant .NET formatting used by PowerShell's JSON writer.
fn format_number(value: &Number) -> Result<String, AppError> {
    if let Some(integer) = value.as_i64() {
        return Ok(integer.to_string());
    }
    let value = value
        .to_string()
        .parse::<f64>()
        .map_err(|error| caused_by("invalidInput", "Invalid JSON number", error))?;
    // Newtonsoft quotes non-finite Doubles instead of writing invalid JSON numbers.
    if value.is_infinite() {
        return Ok(if value.is_sign_negative() {
            "\"-Infinity\""
        } else {
            "\"Infinity\""
        }
        .to_owned());
    }
    // Newtonsoft JsonConvert.ToString(Double) uses invariant "R", then adds
    // ".0" only when neither a decimal point nor an exponent is present:
    // https://github.com/JamesNK/Newtonsoft.Json/blob/13.0.3/Src/Newtonsoft.Json/JsonConvert.cs
    //
    // .NET 10's Dragon4 fallback tests its ulong mantissa against
    // (1U << TNumber.DenormalMantissaBits). C# masks the uint shift count,
    // so Double's implicit bit never matches. Consequently normal powers of
    // two get a symmetric interval instead of a narrower lower interval.
    // Grisu3 normally succeeds first, so globally widening intervals would
    // be incorrect. The exceptional outputs below cover the entire finite
    // normal-power-of-two domain of that implementation, not nearby values.
    // They intentionally do NOT round-trip; correcting them changes hashes.
    // Source of the dispatch, margins, and preceding correct fast path:
    // https://github.com/dotnet/runtime/blob/v10.0.12/src/libraries/System.Private.CoreLib/src/System/Number.Formatting.cs
    // https://github.com/dotnet/runtime/blob/v10.0.12/src/libraries/System.Private.CoreLib/src/System/Number.Dragon4.cs
    // https://github.com/dotnet/runtime/blob/v10.0.12/src/libraries/System.Private.CoreLib/src/System/Number.Grisu3.cs
    let magnitude = value.to_bits() & !(1_u64 << 63);
    let compatibility = match magnitude {
        0x0410_0000_0000_0000 => Some("4.104536801298376E-289"),
        0x3e60_0000_0000_0000 => Some("2.980232238769531E-08"),
        _ => None,
    };
    if let Some(compatibility) = compatibility {
        let sign = if value.is_sign_negative() { "-" } else { "" };
        return Ok(format!("{sign}{compatibility}"));
    }
    // serde_json's shortest-round-trip formatter selects the even decimal on
    // an exact tie, as .NET does; Rust's Display can choose the other decimal.
    let shortest = Number::from_f64(value)
        .ok_or_else(|| fail("invalidInput", "Cannot format JSON number"))?
        .to_string();
    format_double(&shortest)
}

/// Applies .NET notation to shortest-round-trip decimal digits.
fn format_double(shortest: &str) -> Result<String, AppError> {
    let (sign, shortest) = shortest
        .strip_prefix('-')
        .map_or(("", shortest), |value| ("-", value));
    let (mantissa, exponent) = shortest.split_once(['e', 'E']).unwrap_or((shortest, "0"));
    let exponent: i32 = exponent
        .parse()
        .map_err(|error| caused_by("invalidInput", "Invalid exponent", error))?;
    let (integer, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let digits = format!("{integer}{fraction}");
    let leading = digits
        .len()
        .saturating_sub(digits.trim_start_matches('0').len());
    let digits = digits.trim_start_matches('0').trim_end_matches('0');
    if digits.is_empty() {
        return Ok(format!("{sign}0.0"));
    }
    let integer_length = i32::try_from(integer.len())
        .map_err(|error| caused_by("invalidInput", "Invalid mantissa", error))?;
    let leading = i32::try_from(leading)
        .map_err(|error| caused_by("invalidInput", "Invalid mantissa", error))?;
    let exponent = exponent
        .checked_add(integer_length)
        .and_then(|value| value.checked_sub(leading))
        .and_then(|value| value.checked_sub(1))
        .ok_or_else(|| fail("invalidInput", "Invalid decimal exponent"))?;

    // .NET's general round-trip format uses fixed notation in this exponent range.
    if !(-4..17).contains(&exponent) {
        let mut characters = digits.chars();
        let first = characters
            .next()
            .ok_or_else(|| fail("invalidInput", "Invalid mantissa"))?;
        let remaining = characters.as_str();
        let fractional = if remaining.is_empty() {
            String::new()
        } else {
            format!(".{remaining}")
        };
        return Ok(format!("{sign}{first}{fractional}E{exponent:+03}"));
    }

    let position = exponent
        .checked_add(1)
        .ok_or_else(|| fail("invalidInput", "Invalid decimal exponent"))?;
    if position <= 0 {
        let zeros = usize::try_from(position.unsigned_abs())
            .map_err(|error| caused_by("invalidInput", "Invalid exponent", error))?;
        return Ok(format!("{sign}0.{}{digits}", "0".repeat(zeros)));
    }
    let position = usize::try_from(position)
        .map_err(|error| caused_by("invalidInput", "Invalid exponent", error))?;
    if position >= digits.len() {
        return Ok(format!(
            "{sign}{digits}{}.0",
            "0".repeat(position.saturating_sub(digits.len()))
        ));
    }
    let left = digits
        .get(..position)
        .ok_or_else(|| fail("invalidInput", "Invalid mantissa"))?;
    let right = digits
        .get(position..)
        .ok_or_else(|| fail("invalidInput", "Invalid mantissa"))?;
    Ok(format!("{sign}{left}.{right}"))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use serde_json::{Value, json};
    use sha2::{Digest, Sha256};

    use super::{bytes_hash, document_hash, parse_text, powershell_json, snapshot_hash};

    #[test]
    fn byte_hash_uses_exact_bytes_and_uppercase_hex() {
        assert_eq!(
            bytes_hash(b"abc"),
            "BA7816BF8F01CFEA414140DE5DAE2223B00361A396177A9CB410FF61F20015AD"
        );
        assert_ne!(bytes_hash(b"abc"), bytes_hash(b"\xef\xbb\xbfabc"));
    }

    #[test]
    fn borrowed_snapshot_envelope_matches_owned_envelope() {
        let policy = "2\u{2028}\"";
        for document in [
            json!(null),
            json!({"title":"x","values":[-0.0,1e17,"\u{85}\n"],"nested":{"é":1,"id":2}}),
            json!([true, {"value": f64::from_bits(0x3e60_0000_0000_0000)}]),
        ] {
            let envelope = json!({"policyVersion": policy, "document": document});
            assert_eq!(
                snapshot_hash(policy, &document).unwrap(),
                document_hash(&envelope).unwrap()
            );
        }

        let ambiguous = json!({"outer":{"FOO":1,"bar":2,"foo":3}});
        let envelope = json!({"policyVersion": policy, "document": ambiguous});
        snapshot_hash(policy, &ambiguous).unwrap_err();
        document_hash(&envelope).unwrap_err();

        // The envelope contributes one nesting level to the depth limit.
        let mut document = Value::Null;
        for _ in 0..99 {
            document = Value::Array(vec![document]);
        }
        let envelope = json!({"policyVersion": policy, "document": document});
        assert_eq!(
            snapshot_hash(policy, &document).unwrap(),
            document_hash(&envelope).unwrap()
        );
        document = Value::Array(vec![document]);
        let envelope = json!({"policyVersion": policy, "document": document});
        snapshot_hash(policy, &document).unwrap_err();
        document_hash(&envelope).unwrap_err();
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "exhaustive binary64 boundary corpus; focused fixtures also cover these margins"
    )]
    fn normal_power_of_two_corpus_matches_powershell() {
        // Oracle: PowerShell 7.6.6 / .NET 10.0.12 ConvertTo-Json -Compress.
        // For each binary64 normal exponent field, in ascending order, first
        // construct the positive Double with an empty fraction and then its
        // negative counterpart using BitConverter.Int64BitsToDouble. Hash the
        // UTF-8 concatenation of "BITS=JSON\n", with uppercase, zero-padded
        // hexadecimal bits. This exhausts Dragon4's unequal-margin domain; it
        // is not a proof of arbitrary-f64 or arbitrary-runtime equivalence.
        let mut hash = Sha256::new();
        for exponent in 1_u64..=2046 {
            for sign in [0, 1_u64 << 63] {
                let bits = (exponent << 52) | sign;
                let serialized = powershell_json(&json!(f64::from_bits(bits))).unwrap();
                hash.update(format!("{bits:016X}={serialized}\n"));
            }
        }
        assert_eq!(
            format!("{:X}", hash.finalize()),
            "EA5F6725EF5357E1317B8C3FFA16214B71FAF9C90B64D2556C82693552A154E7"
        );
    }

    #[test]
    fn dotnet_dragon4_compatibility_preserves_capture_hashes() {
        // ConvertTo-Json -Compress over [ordered]@{value=$double}, with each
        // Double constructed by BitConverter.Int64BitsToDouble, is the oracle.
        // These are .NET 10 compatibility fixtures, not a shortest-format spec.
        for (bits, expected, positive_hash, negative_hash) in [
            (
                0x0410_0000_0000_0000,
                "4.104536801298376E-289",
                "710726755061364C4B5329555073121FE36B231E3240723A6002CEEBA35176BB",
                "87C62F0DC4A5923678B7DE8922279BA4241FEB0ED24DE8C4F0EA8B62758E48A9",
            ),
            (
                0x3e60_0000_0000_0000,
                "2.980232238769531E-08",
                "9F7EB58E72EB80B127E069DE00BCD1684A25FD18252D4E5B3AB21E2BD1CBCECA",
                "FAED01ABC8B3318F250D840963CFA065CD24898972F47EFF401EDBAAFE8A6F3C",
            ),
        ] {
            for (sign, sign_bit, hash) in
                [("", 0, positive_hash), ("-", 1_u64 << 63, negative_hash)]
            {
                let float = f64::from_bits(bits | sign_bit);
                let value = json!({"value": float});
                assert_eq!(
                    powershell_json(&value).unwrap(),
                    format!(r#"{{"value":{sign}{expected}}}"#)
                );
                assert_eq!(document_hash(&value).unwrap(), hash);
                // PowerShell really emits the predecessor's decimal, rather
                // than a spelling that round-trips to the original Double.
                assert_eq!(
                    format!("{sign}{expected}")
                        .parse::<f64>()
                        .unwrap()
                        .to_bits(),
                    bits.saturating_sub(1) | sign_bit
                );
            }
        }
    }

    #[test]
    fn compatibility_does_not_change_adjacent_doubles() {
        // The neighboring values share decimal prefixes but not the defective
        // mantissa; neither an epsilon comparison nor a decimal-prefix rule fits.
        for (bits, expected) in [
            (0x040f_ffff_ffff_ffff, "4.104536801298376E-289"),
            (0x0410_0000_0000_0001, "4.104536801298377E-289"),
            (0x3e5f_ffff_ffff_ffff, "2.980232238769531E-08"),
            (0x3e60_0000_0000_0001, "2.980232238769532E-08"),
        ] {
            for (sign, sign_bit) in [("", 0), ("-", 1_u64 << 63)] {
                let value = json!(f64::from_bits(bits | sign_bit));
                assert_eq!(
                    powershell_json(&value).unwrap(),
                    format!("{sign}{expected}")
                );
            }
        }
    }

    #[test]
    fn exponent_boundaries_and_extreme_doubles_match_powershell() {
        // References use invariant Double "R" plus Newtonsoft's decimal suffix.
        // Boundary neighbors ensure exponent choice uses the rounded digits.
        for (float, expected) in [
            (0.0, "0.0"),
            (-0.0, "-0.0"),
            (f64::from_bits(1), "5E-324"),
            (f64::MIN_POSITIVE, "2.2250738585072014E-308"),
            (
                f64::from_bits(0x000f_ffff_ffff_ffff),
                "2.225073858507201E-308",
            ),
            (f64::MAX, "1.7976931348623157E+308"),
            (1e-5, "1E-05"),
            (1e-4, "0.0001"),
            (1e16, "10000000000000000.0"),
            (1e17, "1E+17"),
            (
                f64::from_bits(1e-4_f64.to_bits().saturating_sub(1)),
                "9.999999999999999E-05",
            ),
            (
                f64::from_bits(1e17_f64.to_bits().saturating_sub(1)),
                "99999999999999980.0",
            ),
        ] {
            assert_eq!(powershell_json(&json!(float)).unwrap(), expected);
        }
    }

    #[test]
    fn midpoint_round_trip_matches_powershell() {
        // These exact binary midpoints distinguish even decimal tie-breaking.
        for (text, expected, hash) in [
            (
                r#"{"value":1491623381599489.25}"#,
                r#"{"value":1491623381599489.2}"#,
                "0C97367067EFDB50800D34AE70C10A772EA2BB8804D4304F33F4C3FEE488C2A6",
            ),
            (
                r#"{"value":-1491623381599489.25}"#,
                r#"{"value":-1491623381599489.2}"#,
                "66B00BB9073C2E4DDF80A3483D70250D21B390671B84616D8598FC1BD65D1738",
            ),
        ] {
            let value = parse_text(text).unwrap();
            assert_eq!(powershell_json(&value).unwrap(), expected);
            assert_eq!(document_hash(&value).unwrap(), hash);
        }
    }

    #[test]
    fn formats_powershell_strings_and_numbers() {
        let value = json!({"s":"\u{85}\u{2028}\n\"é", "n":[1.0,1e17,1e-5,-0.0]});
        assert_eq!(
            powershell_json(&value).unwrap(),
            r#"{"s":"\u0085\u2028\n\"é","n":[1.0,1E+17,1E-05,-0.0]}"#
        );
    }

    #[test]
    fn materializes_numeric_tokens_like_powershell() {
        let value = serde_json::from_str(r#"{"a":-0,"b":-0.0,"c":9223372036854775808,"d":1e17,"e":1e-5,"f":1e-4,"g":1e16,"s":"\u0085\u2028\u2029\u000b é"}"#).unwrap();
        assert_eq!(
            powershell_json(&value).unwrap(),
            r#"{"a":0,"b":-0.0,"c":9.223372036854776E+18,"d":1E+17,"e":1E-05,"f":0.0001,"g":10000000000000000.0,"s":"\u0085\u2028\u2029\u000b é"}"#
        );
        assert_eq!(
            document_hash(&value).unwrap(),
            "44C92DFA242DCCE7C5DAEB26CC252AE01BD42716A7F6BF949D158147CE6F9C12"
        );
        let overflow = serde_json::from_str("[1e400,-1e400]").unwrap();
        assert_eq!(
            powershell_json(&overflow).unwrap(),
            r#"["Infinity","-Infinity"]"#
        );
    }

    #[test]
    fn duplicate_properties_are_rejected_without_discarding_values() {
        parse_text(r#"{"foo":1,"foo":2}"#).unwrap_err();
        parse_text(r#"{"FOO":1,"bar":2,"foo":3}"#).unwrap_err();
        parse_text(r#"{"outer":{"field":1,"Field":2}}"#).unwrap_err();
        let direct = serde_json::from_str::<Value>(r#"{"FOO":1,"bar":2,"foo":3}"#).unwrap();
        powershell_json(&direct).unwrap_err();
    }

    #[test]
    fn unicode_properties_match_powershell_reference() {
        // The expected text and hash come from the PowerShell capture model.
        let text = r#"{"é":1,"ø":2,"Δ":"value","id":"stable"}"#;
        let value = parse_text(text).unwrap();
        assert_eq!(powershell_json(&value).unwrap(), text);
        assert_eq!(
            document_hash(&value).unwrap(),
            "FF9C95EF449C05A6E0C814B59F8505901272E60D7E86339B737549E6E3A18E65"
        );
        parse_text(r#"{"é":1,"é":2}"#).unwrap_err();
        parse_text(r#"{"name-é":1,"other-é":2}"#).unwrap();
    }
}
