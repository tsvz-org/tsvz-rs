//! §13 field escaping, and the reserved marker-key pattern of §12.2.1.

use std::borrow::Cow;

/// Decode the §13.2 tokens of one field (already split on the raw delimiter, §13.4).
/// Unknown tokens and a bare `<` pass through literally; decoding is a single pass.
pub fn decode(field: &str, delimiter: u8) -> Cow<'_, str> {
    if memchr::memchr(b'<', field.as_bytes()).is_none() {
        return Cow::Borrowed(field);
    }
    let mut out = String::with_capacity(field.len());
    let mut rest = field;
    while let Some(i) = rest.find('<') {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let (len, ch) = if tail.starts_with("<sep>") {
            (5, delimiter as char)
        } else if tail.starts_with("<LF>") {
            (4, '\n')
        } else if tail.starts_with("<CR>") {
            (4, '\r')
        } else if tail.starts_with("<NUL>") {
            (5, '\0')
        } else if tail.starts_with("<lt>") {
            (4, '<')
        } else if tail.starts_with("<#>") {
            (3, '#')
        } else {
            (1, '<')
        };
        out.push(ch);
        rest = &tail[len..];
    }
    out.push_str(rest);
    Cow::Owned(out)
}

/// Append `value` encoded per §13.3 to `out`. With `key`, a leading `#` is written `<#>` (§8.3).
pub fn encode_into(out: &mut String, value: &str, delimiter: u8, key: bool) {
    let d = delimiter as char;
    let needs = |c: char| c == d || matches!(c, '\n' | '\r' | '\0' | '<');
    let mut rest = value;
    if key && rest.starts_with('#') {
        out.push_str("<#>");
        rest = &rest[1..];
    }
    if !rest.chars().any(needs) {
        out.push_str(rest);
        return;
    }
    for c in rest.chars() {
        if c == d {
            out.push_str("<sep>");
        } else {
            match c {
                '\n' => out.push_str("<LF>"),
                '\r' => out.push_str("<CR>"),
                '\0' => out.push_str("<NUL>"),
                '<' => out.push_str("<lt>"),
                _ => out.push(c),
            }
        }
    }
}

/// `value` encoded per §13.3.
pub fn encode(value: &str, delimiter: u8, key: bool) -> String {
    let mut out = String::with_capacity(value.len() + 8);
    encode_into(&mut out, value, delimiter, key);
    out
}

/// Whether a raw first field matches the reserved pattern `#_[A-Za-z0-9_-]+_#` of §12.2.1,
/// anchored to the whole field (case does not matter: the class already has both cases).
pub fn is_marker_key(field: &str) -> bool {
    let b = field.as_bytes();
    b.len() >= 5
        && b.starts_with(b"#_")
        && b.ends_with(b"_#")
        && b[2..b.len() - 2]
            .iter()
            .all(|&c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_token_decodes() {
        assert_eq!(
            decode("a<sep>b<LF>c<CR>d<NUL>e<lt>f<#>g", b'\t'),
            "a\tb\nc\rd\0e<f#g"
        );
        assert_eq!(decode("a<sep>b", b','), "a,b");
        assert_eq!(decode("a<sep>b", b'|'), "a|b");
        assert_eq!(decode("a<sep>b<NUL>c", 0), "a\0b\0c");
    }

    #[test]
    fn decoding_is_single_pass_and_case_sensitive() {
        assert_eq!(decode("<lt>sep>", b'\t'), "<sep>");
        assert_eq!(decode("<lt>LF>", b'\t'), "<LF>");
        assert_eq!(decode("x<TAB>y", b'\t'), "x<TAB>y");
        assert_eq!(decode("<lf><SEP>", b'\t'), "<lf><SEP>");
        assert_eq!(decode("a<b", b'\t'), "a<b");
        assert_eq!(decode("ends<", b'\t'), "ends<");
        assert_eq!(decode("plain", b'\t'), "plain");
        assert!(matches!(
            decode("plain", b'\t'),
            std::borrow::Cow::Borrowed(_)
        ));
    }

    #[test]
    fn encoding_inverts_decoding() {
        assert_eq!(
            encode("a\tb\nc\rd\0e<f#g", b'\t', false),
            "a<sep>b<LF>c<CR>d<NUL>e<lt>f#g"
        );
        assert_eq!(encode("#k", b'\t', true), "<#>k");
        assert_eq!(encode("#k", b'\t', false), "#k");
        assert_eq!(encode("k#", b'\t', true), "k#");
        assert_eq!(encode("a,b\tc", b',', false), "a<sep>b\tc");
        assert_eq!(encode("a\0b\tc", 0, false), "a<sep>b\tc");
        assert_eq!(encode("<sep>", b'\t', false), "<lt>sep>");
        for value in ["", "x", "#", "<<", "a\tb", "\r", "ü<LF>", "\0\n\r\t,|<#>"] {
            for d in [b'\t', b',', 0, b'|'] {
                assert_eq!(decode(&encode(value, d, true), d), value);
                assert_eq!(decode(&encode(value, d, false), d), value);
            }
        }
    }

    #[test]
    fn marker_key_pattern() {
        for key in [
            "#_defaults_#",
            "#_X-y_#",
            "#__ext__#",
            "#_CHECKSUM_CRC32_#",
            "#_9_#",
        ] {
            assert!(is_marker_key(key), "{key}");
        }
        for key in [
            "#__#", "#_a_", "#_a b_#", "#_a_# ", " #_a_#", "_a_#", "#foo", "#_ä_#", "",
        ] {
            assert!(!is_marker_key(key), "{key}");
        }
    }
}
