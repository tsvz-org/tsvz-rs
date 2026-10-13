//! File names: variants and delimiters (§5), codec suffixes (§16.2), numbered parts (§17.2, Appendix A).

use std::path::Path;

/// A compression codec of §16.2.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Codec {
    Gzip,
    Bzip2,
    Xz,
    Zstd,
}

impl Codec {
    /// The codec a file-name suffix (without its dot, any ASCII case) names.
    pub fn from_suffix(suffix: &str) -> Option<Codec> {
        match suffix.to_ascii_lowercase().as_str() {
            "gz" | "gzip" => Some(Codec::Gzip),
            "bz2" | "bzip2" => Some(Codec::Bzip2),
            "xz" | "lzma" => Some(Codec::Xz),
            "zst" | "zstd" => Some(Codec::Zstd),
            _ => None,
        }
    }

    /// The name `parts` prints (§20.2).
    pub fn name(self) -> &'static str {
        match self {
            Codec::Gzip => "gzip",
            Codec::Bzip2 => "bzip2",
            Codec::Xz => "xz",
            Codec::Zstd => "zstd",
        }
    }

    /// The bytes every member of this codec begins with (§16.6).
    pub fn magic(self) -> &'static [u8] {
        match self {
            Codec::Gzip => b"\x1f\x8b\x08",
            Codec::Bzip2 => b"BZh",
            Codec::Xz => b"\xfd7zXZ\x00",
            Codec::Zstd => b"\x28\xb5\x2f\xfd",
        }
    }

    /// Whether this build reads and writes the codec: gzip always (§16.5), the others by feature.
    pub fn supported(self) -> bool {
        match self {
            Codec::Gzip => true,
            Codec::Bzip2 => cfg!(feature = "bzip2"),
            Codec::Xz => cfg!(feature = "xz"),
            Codec::Zstd => cfg!(feature = "zstd"),
        }
    }
}

/// A store's variant: its field delimiter, and whether it is a strict (`…z`) variant (§5.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Format {
    pub delimiter: u8,
    pub strict: bool,
}

fn variant(ext: &str) -> Option<Format> {
    let (delimiter, strict) = match ext.to_ascii_lowercase().as_str() {
        "tsvz" => (b'\t', true),
        "csvz" => (b',', true),
        "nsvz" => (0, true),
        "psvz" => (b'|', true),
        "tsv" => (b'\t', false),
        "csv" => (b',', false),
        "nsv" => (0, false),
        "psv" => (b'|', false),
        _ => return None,
    };
    Some(Format { delimiter, strict })
}

/// Python's `os.path.splitext`: the root and the extension (without its dot) of the last
/// path component. A component that is only dots before its last dot has no extension.
fn split_ext(name: &str) -> (&str, Option<&str>) {
    let start = name.rfind('/').map_or(0, |i| i + 1);
    let base = &name[start..];
    match base.rfind('.') {
        Some(dot) if base[..dot].bytes().any(|b| b != b'.') => {
            (&name[..start + dot], Some(&base[dot + 1..]))
        }
        _ => (name, None),
    }
}

/// (the name without its codec suffix, the codec) when the last extension is a codec suffix of
/// §16.2; otherwise (the whole name, None). The suffix is outermost (§16.2) and stripped first (§16.3).
pub fn split_codec(name: &str) -> (&str, Option<Codec>) {
    if let (root, Some(ext)) = split_ext(name) {
        if let Some(codec) = Codec::from_suffix(ext) {
            return (root, Some(codec));
        }
    }
    (name, None)
}

/// The format of a store or part path, from its variant extension (§5.1): the codec suffix
/// and a `.rotated` component are skipped, and so is one ordinal (§17.2). None when the path
/// has no variant extension or is not UTF-8.
pub fn format_of(path: &Path) -> Option<Format> {
    let name = path.to_str()?;
    let mut base = split_codec(name).0;
    if let Some(stripped) = base.strip_suffix(".rotated") {
        base = stripped;
    }
    let (root, ext) = split_ext(base);
    let ext = ext?;
    if let Some(format) = variant(ext) {
        return Some(format);
    }
    // Only an ordinal may follow the variant (§17.2): `s.tsvz.1f` is a part, while
    // `notes.tsvz.txt` or a `s.tsvz.damaged-…` backup is no store at all.
    if ext.is_empty() || !ext.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    split_ext(root).1.and_then(variant)
}

/// Whether a path names one numbered part of a store, `<store>.<ext>.<ordinal>[.rotated][.<codec>]`
/// (§17.2), rather than a store (§20.1.3).
pub fn names_a_part(path: &Path) -> bool {
    let Some(name) = path.to_str() else {
        return false;
    };
    let mut base = split_codec(name).0;
    let rotated = base.strip_suffix(".rotated");
    if let Some(stripped) = rotated {
        base = stripped;
    }
    let (root, ext) = split_ext(base);
    let numbered = ext.is_some_and(|e| !e.is_empty() && e.bytes().all(|b| b.is_ascii_hexdigit()));
    numbered && split_ext(root).1.and_then(variant).is_some()
}

/// A numbered part's name after `<store>.<ext>.`: `<ordinal>[.rotated][.<codec>]` (Appendix A).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Numbered {
    pub ordinal: String,
    pub rotated: bool,
    pub codec: Option<Codec>,
}

/// Parse what follows `<store>.<ext>.` in a directory entry; None when it is not a part name
/// (for example a pointer file `….serve` or a backup `….damaged-…`).
pub fn parse_numbered(rest: &str) -> Option<Numbered> {
    let hex_end = rest
        .bytes()
        .position(|b| !b.is_ascii_hexdigit())
        .unwrap_or(rest.len());
    if hex_end == 0 {
        return None;
    }
    let mut tail = &rest[hex_end..];
    let rotated = tail.starts_with(".rotated");
    if rotated {
        tail = &tail[".rotated".len()..];
    }
    let codec = if tail.is_empty() {
        None
    } else {
        let suffix = tail.strip_prefix('.')?;
        if suffix.is_empty() || !suffix.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return None;
        }
        Some(Codec::from_suffix(suffix)?)
    };
    Some(Numbered {
        ordinal: rest[..hex_end].to_string(),
        rotated,
        codec,
    })
}

/// A sort key that orders ordinals by their hexadecimal value, of any length, without
/// overflow (§17.3): leading zeros dropped, then (digit count, lowercase digits).
pub fn ordinal_key(ordinal: &str) -> (usize, String) {
    let digits = ordinal.trim_start_matches('0').to_ascii_lowercase();
    (digits.len(), digits)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn codec_suffixes_any_case() {
        assert_eq!(Codec::from_suffix("gz"), Some(Codec::Gzip));
        assert_eq!(Codec::from_suffix("GZIP"), Some(Codec::Gzip));
        assert_eq!(Codec::from_suffix("bz2"), Some(Codec::Bzip2));
        assert_eq!(Codec::from_suffix("bzip2"), Some(Codec::Bzip2));
        assert_eq!(Codec::from_suffix("lzma"), Some(Codec::Xz));
        assert_eq!(Codec::from_suffix("Zst"), Some(Codec::Zstd));
        assert_eq!(Codec::from_suffix("zip"), None);
        assert_eq!(Codec::Gzip.name(), "gzip");
        assert_eq!(Codec::Zstd.magic(), b"\x28\xb5\x2f\xfd");
        assert!(Codec::Gzip.supported());
    }

    #[test]
    fn split_codec_strips_only_a_codec_suffix() {
        assert_eq!(split_codec("a/s.tsvz.gz"), ("a/s.tsvz", Some(Codec::Gzip)));
        assert_eq!(
            split_codec("s.tsvz.10.rotated.XZ"),
            ("s.tsvz.10.rotated", Some(Codec::Xz))
        );
        assert_eq!(split_codec("s.tsvz"), ("s.tsvz", None));
        assert_eq!(split_codec("dir.gz/s.tsvz"), ("dir.gz/s.tsvz", None));
        assert_eq!(split_codec(".gz"), (".gz", None));
    }

    #[test]
    fn format_from_the_variant_extension() {
        let f = |p: &str| format_of(Path::new(p));
        assert_eq!(
            f("s.tsvz"),
            Some(Format {
                delimiter: b'\t',
                strict: true
            })
        );
        assert_eq!(
            f("s.CSVZ"),
            Some(Format {
                delimiter: b',',
                strict: true
            })
        );
        assert_eq!(
            f("s.nsvz.gz"),
            Some(Format {
                delimiter: 0,
                strict: true
            })
        );
        assert_eq!(
            f("s.psvz.1f.rotated.zst"),
            Some(Format {
                delimiter: b'|',
                strict: true
            })
        );
        assert_eq!(
            f("dir/s.tsv"),
            Some(Format {
                delimiter: b'\t',
                strict: false
            })
        );
        assert_eq!(
            f("s.csv.0190c3a1"),
            Some(Format {
                delimiter: b',',
                strict: false
            })
        );
        assert_eq!(f("s.txt"), None);
        assert_eq!(f("tsvz"), None);
    }

    #[test]
    fn paths_that_name_one_part() {
        for p in [
            "s.tsvz.1",
            "dir/s.csvz.0190c3a1.gz",
            "s.tsvz.B.rotated",
            "s.tsv.2.xz",
        ] {
            assert!(names_a_part(Path::new(p)), "{p}");
        }
        for p in [
            "s.tsvz",
            "s.tsvz.gz",
            "s.tsvz.damaged-20261012T101010",
            "x.1",
            "s.tsvz.1x",
        ] {
            assert!(!names_a_part(Path::new(p)), "{p}");
        }
    }

    #[test]
    fn only_an_ordinal_may_follow_the_variant() {
        let f = |p: &str| format_of(Path::new(p));
        assert_eq!(f("notes.tsvz.txt"), None);
        assert_eq!(f("s.tsvz.damaged-20261012T101010"), None);
        assert_eq!(f("s.tsvz.damaged-20261012T101010.1"), None);
        assert_eq!(f("s.tsvz.1f"), Some(Format { delimiter: b'\t', strict: true }));
    }

    #[test]
    fn numbered_part_names() {
        assert_eq!(
            parse_numbered("0190c3a1"),
            Some(Numbered {
                ordinal: "0190c3a1".into(),
                rotated: false,
                codec: None
            })
        );
        assert_eq!(
            parse_numbered("B.rotated.gz"),
            Some(Numbered {
                ordinal: "B".into(),
                rotated: true,
                codec: Some(Codec::Gzip)
            })
        );
        assert_eq!(parse_numbered("1.Zstd").unwrap().codec, Some(Codec::Zstd));
        assert_eq!(parse_numbered("1.serve"), None);
        assert_eq!(parse_numbered("1.damaged-20261012T101010"), None);
        assert_eq!(parse_numbered("serve"), None);
        assert_eq!(parse_numbered(""), None);
        assert_eq!(parse_numbered("1.gz.x"), None);
    }

    #[test]
    fn ordinals_compare_as_hex_integers() {
        let mut ords = vec![
            "10",
            "f",
            "002",
            "1",
            "B",
            "a",
            "0",
            "00000000000000000000000000000000ff",
        ];
        ords.sort_by_key(|o| ordinal_key(o));
        assert_eq!(
            ords,
            vec![
                "0",
                "1",
                "002",
                "a",
                "B",
                "f",
                "10",
                "00000000000000000000000000000000ff"
            ]
        );
    }
}
