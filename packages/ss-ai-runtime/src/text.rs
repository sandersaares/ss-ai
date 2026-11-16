/// Decodes BOM-tagged text consistently for overview capture and rendering.
///
/// UTF-8 is the fallback encoding. Malformed sequences and incomplete UTF-16
/// or UTF-32 units become replacement characters.
#[must_use]
pub fn decode_text_with_bom(bytes: &[u8]) -> String {
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
                char::from_u32(value).unwrap_or(char::REPLACEMENT_CHARACTER)
            })
            .collect::<String>();
        if !remainder.is_empty() {
            text.push(char::REPLACEMENT_CHARACTER);
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
            text.push(char::REPLACEMENT_CHARACTER);
        }
        text
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::decode_text_with_bom;

    #[test]
    fn decodes_bom_encodings_and_replaces_malformed_units() {
        assert_eq!(decode_text_with_bom(b"\xef\xbb\xbfhello"), "hello");
        assert_eq!(decode_text_with_bom(&[0xff, 0xfe, b'A', 0]), "A");
        assert_eq!(decode_text_with_bom(&[0xfe, 0xff, 0, b'A']), "A");
        assert_eq!(
            decode_text_with_bom(&[0xff, 0xfe, 0, 0, b'A', 0, 0, 0]),
            "A"
        );
        assert_eq!(
            decode_text_with_bom(&[0, 0, 0xfe, 0xff, 0, 0, 0, b'A']),
            "A"
        );
        assert_eq!(decode_text_with_bom(&[0xff, 0xfe, 0, 0xd8]), "\u{fffd}");
        assert_eq!(
            decode_text_with_bom(&[0xff, 0xfe, b'A', 0, b'B']),
            "A\u{fffd}"
        );
        assert_eq!(decode_text_with_bom(b"plain"), "plain");
    }
}
