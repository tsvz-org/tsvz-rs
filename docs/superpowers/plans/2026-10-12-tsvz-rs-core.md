# tsvz-rs core crate and CLI Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A Rust crate `tsvz` (library + `tsvz` binary) that reads and writes tsvz-spec-v1 stores, passes the reader, writer and cli classes of the TSVZ conformance suite with every codec and digest claimed, and beats TSVZ.py on every tsvzbench CLI workload.

**Architecture:** Layered engine: `codec` splits compressed parts into members, `frame` turns bytes into committed lines, `replay` runs the §7 pipeline as a state machine into a `Sink`, and `store` is the ordered map of live rows with scopes and incremental refresh. `write`, `lock` and `maint` append and rewrite parts with the same locks as TSVZ.py. `cli` is the §20 command line on top.

**Tech Stack:** Rust 2024 (stable, MSRV 1.85); `memchr`, `hashbrown`, `libc`, `crc32fast`, `crc32c`, `sha2`, `blake3`, `flate2` (zlib-rs), `bzip2` (libbz2-rs-sys), `liblzma` (static), `zstd` (static); `tempfile` for tests; Python 3 for the conformance suite, the differential harness and the width-table generator.

**Spec:** `docs/superpowers/specs/2026-10-12-tsvz-rs-core-design.md` (this repo). The format spec is `../TSVZ/tsvz-spec-v1.md`; the reference implementation `../TSVZ/TSVZ.py`; the conformance runner `../TSVZ/tsvzconform.py`; the benchmark `../TSVZ/tsvzbench.py`.

## Global Constraints

- Repository `cas/tsvz-rs` (this directory); the TSVZ repo is checked out beside it at `../TSVZ`.
- One crate named `tsvz`, version `0.1.0`, `edition = "2024"`, `rust-version = "1.85"`, `license = "GPL-3.0-or-later"`.
- Features: `cli`, `bzip2`, `xz`, `zstd`, all default. gzip and the four digests are always built.
- The binary is named `tsvz` and needs the `cli` feature; `-V` prints `tsvz-rs 0.1.0`.
- Platforms: Linux and macOS. Unix-only APIs (`std::os::unix`, `libc`) are fine.
- No argument-parsing crate; no `chrono`; no `unicode-width`.
- Where `../TSVZ/tsvz-spec-v1.md` is silent, behave as `../TSVZ/TSVZ.py` does and add the choice to `docs/silent-choices.md`.
- Locks: an in-process mutex per canonical path, then an exclusive whole-file `F_OFD_SETLKW` lock on Linux (fallback to classic `F_SETLKW` on `EINVAL`), classic `F_SETLKW` on macOS. Readers take no lock.
- No panics on any input file or argument: errors become `Error`, warnings, or exit statuses.
- Benchmarks run with a work directory on `/mnt/gen5` (f2fs), never `/tmp` (tmpfs) or `/mnt/klein` (mergerfs).
- Every commit message ends with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

## Review Focus

Inputs and conditions the spec implies but the conformance suite does not exercise, most likely to bite first. Each has a test in the task named.

1. **Very long lines and large files** — a single 64 MiB line, or a line split across thousands of reads, is framed once in linear time and read correctly (test in Task 7).
2. **Awkward paths** — a store path with spaces, non-ASCII characters, or a leading `-` (given after `--`) works; a non-UTF-8 argument exits 2 with a message, never a panic (tests in Task 10).
3. **Stdout closed early** — `tsvz read big.tsvz | head -1` ends without a panic message on stderr (test in Task 10).
4. **Degenerate parts** — a 0-byte `.gz` part, a file of only a BOM, only `\r\n`, only zero bytes, or only an unterminated line reads as an empty store without error (tests in Task 9).
5. **Unreadable store paths** — a store path that is a directory, or a part the user may not read, exits 1 with a message naming the path, never a panic (tests in Task 10).

---

## File Structure

```
tsvz-rs/
  Cargo.toml                  crate, features, dependencies, release profile          (Task 1, 10)
  .gitignore                                                                          (Task 1)
  src/lib.rs                  module list and re-exports                               (every task)
  src/error.rs                Error, Warning, WarnFn                                   (Task 1)
  src/name.rs                 Codec, Format, split_codec, format_of, parse_numbered    (Task 1)
  src/escape.rs               §13 decode/encode, is_marker_key                        (Task 2)
  src/parts.rs                Part, Stamp, list, active                                (Task 3)
  src/digest.rs               Algo, Accumulator                                        (Task 4)
  src/codec.rs                member decoding, damaged-member rules, items, encoding   (Task 5, 6)
  src/frame.rs                Line, Framer                                             (Task 7)
  src/replay.rs               Markers, Binding, Cells, Sink, Replay                    (Task 8)
  src/store.rs                Options, Scope, Store, Row, PartInfo, refresh            (Task 9, 16)
  src/lock.rs                 the write lock shared with TSVZ.py                       (Task 11)
  src/write.rs                Record, encode_record, Writer                            (Task 11, 15)
  src/maint.rs                clear, scrub, Outcome                                    (Task 13, 15)
  src/main.rs                 the binary                                               (Task 10)
  src/cli/mod.rs              entry point, context, dispatch                           (Task 10, 12, 13)
  src/cli/args.rs             §20.1 argument parser                                    (Task 10)
  src/cli/output.rs           records output                                           (Task 10, 14)
  src/cli/read.rs             read, get, has, len, keys, verify, parts                 (Task 10)
  src/cli/write.rs            set, append, delete, bulk input, pop, popitem, setdefault (Task 12)
  src/cli/table.rs            table layout and control-character escaping              (Task 14)
  src/cli/width_table.rs      generated character widths                               (Task 14)
  tools/gen_width.py          generates width_table.rs from Python's unicodedata       (Task 14)
  tests/cli.rs                end-to-end tests of the binary                           (Task 10, 12, 13, 14, 15)
  tests/interop.rs            locking and reading together with TSVZ.py                (Task 17)
  tests/robust.rs             random-bytes robustness on stable Rust                   (Task 20)
  fuzz/                       cargo-fuzz targets                                       (Task 20)
  difftest/difftest.py        differential harness against TSVZ.py                     (Task 19)
  docs/silent-choices.md      behaviour the spec leaves open                           (Task 18)
  bench/RESULTS.md            tsvzbench numbers                                        (Task 21)
  README.md                                                                            (Task 22)
  .github/workflows/ci.yml, release.yml                                                (Task 22)
```

Dependency direction: `name` ← `escape`, `parts`, `digest`, `codec` ← `frame` ← `replay` ← `store` ← `write`, `maint` ← `cli`.

---

### Task 1: Crate skeleton, errors and file names

**Files:**
- Create: `Cargo.toml`, `.gitignore`, `src/lib.rs`, `src/error.rs`, `src/name.rs`

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `tsvz::Error` (enum: `Missing(PathBuf)`, `Io { path: PathBuf, source: std::io::Error }`, `UnsupportedCodec { path: PathBuf, codec: &'static str }`, `BadPath(PathBuf)`, `Refused(String)`), `Error::io(path: &Path, source: io::Error) -> Error`.
  - `tsvz::Warning { part: PathBuf, offset: Option<u64>, message: String }`, `tsvz::WarnFn = Arc<dyn Fn(&Warning) + Send + Sync>`.
  - `tsvz::name::Codec` (`Gzip, Bzip2, Xz, Zstd`) with `from_suffix(&str) -> Option<Codec>`, `name(self) -> &'static str`, `magic(self) -> &'static [u8]`, `supported(self) -> bool`.
  - `tsvz::name::Format { delimiter: u8, strict: bool }`.
  - `name::split_codec(&str) -> (&str, Option<Codec>)`, `name::format_of(&Path) -> Option<Format>`, `name::Numbered { ordinal: String, rotated: bool, codec: Option<Codec> }`, `name::parse_numbered(&str) -> Option<Numbered>`, `name::ordinal_key(&str) -> (usize, String)`.

- [ ] **Step 1: Write the crate files**

`Cargo.toml`:

```toml
[package]
name = "tsvz"
version = "0.1.0"
edition = "2024"
rust-version = "1.85"
license = "GPL-3.0-or-later"
description = "Fast tool and library for tsvz-spec-v1 key-value stores: append-only TSV/CSV/NSV/PSV logs"
repository = "https://github.com/yufei-pan/tsvz-rs"
authors = ["Yufei Pan <pan@zopyr.us>"]
readme = "README.md"
keywords = ["tsv", "key-value", "append-only", "log", "cli"]
categories = ["command-line-utilities", "database", "parser-implementations"]

[features]
default = ["cli", "bzip2", "xz", "zstd"]
cli = []
bzip2 = ["dep:bzip2"]
xz = ["dep:liblzma"]
zstd = ["dep:zstd"]

[dependencies]
memchr = "2.8"
hashbrown = "0.17"
libc = "0.2"
crc32fast = "1.5"
crc32c = "0.6"
sha2 = "0.11"
blake3 = { version = "1.8", features = ["pure"] }
flate2 = { version = "1.1", default-features = false, features = ["zlib-rs"] }
bzip2 = { version = "0.6", optional = true }
liblzma = { version = "0.4", features = ["static"], optional = true }
zstd = { version = "0.14", optional = true }

[dev-dependencies]
tempfile = "3"

[profile.release]
lto = "fat"
codegen-units = 1
panic = "abort"
```

`.gitignore`:

```
/target
/fuzz/target
/fuzz/corpus
/fuzz/artifacts
```

`src/lib.rs`:

```rust
//! tsvz: read and write tsvz-spec-v1 stores, append-only key-value logs in
//! TSV, CSV, NSV and PSV files (see `tsvz-spec-v1.md` in the TSVZ repository).

mod error;
pub mod name;

pub use error::{Error, WarnFn, Warning};
```

`src/error.rs`:

```rust
//! Errors and warnings.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Why an operation on a store failed.
#[derive(Debug)]
pub enum Error {
    /// The store has no part (§20.2.2).
    Missing(PathBuf),
    /// Reading or writing a file failed.
    Io { path: PathBuf, source: io::Error },
    /// A part is compressed with a codec this build does not include (§16.5).
    UnsupportedCodec { path: PathBuf, codec: &'static str },
    /// The path names no TSVZ variant, or is not valid UTF-8.
    BadPath(PathBuf),
    /// The operation was refused; nothing was written. The text says why.
    Refused(String),
}

impl Error {
    pub fn io(path: &Path, source: io::Error) -> Error {
        Error::Io { path: path.to_path_buf(), source }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Missing(path) => write!(f, "{}: no such store", path.display()),
            Error::Io { path, source } => write!(f, "{}: {}", path.display(), source),
            Error::UnsupportedCodec { path, codec } => {
                write!(f, "{}: this tsvz cannot read {} parts", path.display(), codec)
            }
            Error::BadPath(path) => write!(
                f,
                "{}: not a TSVZ store path (it needs a .tsvz, .csvz, .nsvz, .psvz, .tsv, .csv, .nsv or .psv extension, and must be UTF-8)",
                path.display()
            ),
            Error::Refused(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// A problem a reader or writer tolerated, and what it did about it (§4.10).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Warning {
    pub part: PathBuf,
    pub offset: Option<u64>,
    pub message: String,
}

/// Where warnings go.
pub type WarnFn = Arc<dyn Fn(&Warning) + Send + Sync>;
```

- [ ] **Step 2: Write the failing tests for `name`**

Create `src/name.rs` with only the tests first:

```rust
//! File names: variants and delimiters (§5), codec suffixes (§16.2), numbered parts (§17.2, Appendix A).

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
        assert_eq!(split_codec("s.tsvz.10.rotated.XZ"), ("s.tsvz.10.rotated", Some(Codec::Xz)));
        assert_eq!(split_codec("s.tsvz"), ("s.tsvz", None));
        assert_eq!(split_codec("dir.gz/s.tsvz"), ("dir.gz/s.tsvz", None));
        assert_eq!(split_codec(".gz"), (".gz", None));
    }

    #[test]
    fn format_from_the_variant_extension() {
        let f = |p: &str| format_of(Path::new(p));
        assert_eq!(f("s.tsvz"), Some(Format { delimiter: b'\t', strict: true }));
        assert_eq!(f("s.CSVZ"), Some(Format { delimiter: b',', strict: true }));
        assert_eq!(f("s.nsvz.gz"), Some(Format { delimiter: 0, strict: true }));
        assert_eq!(f("s.psvz.1f.rotated.zst"), Some(Format { delimiter: b'|', strict: true }));
        assert_eq!(f("dir/s.tsv"), Some(Format { delimiter: b'\t', strict: false }));
        assert_eq!(f("s.csv.0190c3a1"), Some(Format { delimiter: b',', strict: false }));
        assert_eq!(f("s.txt"), None);
        assert_eq!(f("tsvz"), None);
    }

    #[test]
    fn numbered_part_names() {
        assert_eq!(
            parse_numbered("0190c3a1"),
            Some(Numbered { ordinal: "0190c3a1".into(), rotated: false, codec: None })
        );
        assert_eq!(
            parse_numbered("B.rotated.gz"),
            Some(Numbered { ordinal: "B".into(), rotated: true, codec: Some(Codec::Gzip) })
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
        let mut ords = vec!["10", "f", "002", "1", "B", "a", "0", "00000000000000000000000000000000ff"];
        ords.sort_by_key(|o| ordinal_key(o));
        assert_eq!(ords, vec!["0", "1", "002", "a", "B", "f", "10", "00000000000000000000000000000000ff"]);
    }
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test --lib name`
Expected: compile errors: `Codec`, `split_codec`, `format_of`, `parse_numbered`, `Numbered`, `ordinal_key` not found.

- [ ] **Step 4: Implement `name`**

Add above the tests in `src/name.rs`:

```rust
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
        Some(dot) if base[..dot].bytes().any(|b| b != b'.') => (&name[..start + dot], Some(&base[dot + 1..])),
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
    for _ in 0..2 {
        let (root, ext) = split_ext(base);
        let ext = ext?;
        if let Some(format) = variant(ext) {
            return Some(format);
        }
        base = root;
    }
    None
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
    let hex_end = rest.bytes().position(|b| !b.is_ascii_hexdigit()).unwrap_or(rest.len());
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
    Some(Numbered { ordinal: rest[..hex_end].to_string(), rotated, codec })
}

/// A sort key that orders ordinals by their hexadecimal value, of any length, without
/// overflow (§17.3): leading zeros dropped, then (digit count, lowercase digits).
pub fn ordinal_key(ordinal: &str) -> (usize, String) {
    let digits = ordinal.trim_start_matches('0').to_ascii_lowercase();
    (digits.len(), digits)
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test --lib name`
Expected: 5 passed.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml .gitignore src/lib.rs src/error.rs src/name.rs Cargo.lock
git commit -m "Crate skeleton: errors, warnings, and file names (variants, codec suffixes, numbered parts)." -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 2: Field escaping and marker keys

**Files:**
- Create: `src/escape.rs`
- Modify: `src/lib.rs` (add `pub mod escape;`)

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `escape::decode(field: &str, delimiter: u8) -> Cow<'_, str>`
  - `escape::encode_into(out: &mut String, value: &str, delimiter: u8, key: bool)`
  - `escape::encode(value: &str, delimiter: u8, key: bool) -> String`
  - `escape::is_marker_key(field: &str) -> bool` (the §12.2.1 pattern, anchored)

- [ ] **Step 1: Write the failing tests**

`src/escape.rs`:

```rust
//! §13 field escaping, and the reserved marker-key pattern of §12.2.1.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_token_decodes() {
        assert_eq!(decode("a<sep>b<LF>c<CR>d<NUL>e<lt>f<#>g", b'\t'), "a\tb\nc\rd\0e<f#g");
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
        assert!(matches!(decode("plain", b'\t'), std::borrow::Cow::Borrowed(_)));
    }

    #[test]
    fn encoding_inverts_decoding() {
        assert_eq!(encode("a\tb\nc\rd\0e<f#g", b'\t', false), "a<sep>b<LF>c<CR>d<NUL>e<lt>f#g");
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
        for key in ["#_defaults_#", "#_X-y_#", "#__ext__#", "#_CHECKSUM_CRC32_#", "#_9_#"] {
            assert!(is_marker_key(key), "{key}");
        }
        for key in ["#__#", "#_a_", "#_a b_#", "#_a_# ", " #_a_#", "_a_#", "#foo", "#_ä_#", ""] {
            assert!(!is_marker_key(key), "{key}");
        }
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib escape` (after adding `pub mod escape;` to `src/lib.rs`)
Expected: compile errors: `decode`, `encode`, `is_marker_key` not found.

- [ ] **Step 3: Implement**

Add above the tests in `src/escape.rs`:

```rust
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
        && b[2..b.len() - 2].iter().all(|&c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
}
```

Note: `#__#` has an empty middle once both `_` are taken by the anchors, so `b.len() >= 5` rejects it, as the regex `+` requires one class character between `#_` and `_#`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib escape`
Expected: 4 passed.

- [ ] **Step 5: Commit**

```bash
git add src/escape.rs src/lib.rs
git commit -m "Field escaping (§13) and the reserved marker-key pattern (§12.2.1)." -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 3: A store's parts

**Files:**
- Create: `src/parts.rs`
- Modify: `src/lib.rs` (add `pub mod parts;`)

**Interfaces:**
- Consumes: `name::{split_codec, parse_numbered, ordinal_key, Codec}`, `Error`.
- Produces:
  - `parts::Part { path: PathBuf, ordinal: Option<String>, codec: Option<Codec> }`
  - `parts::list(store: &Path) -> Result<Vec<Part>, Error>` — replay order, `.rotated` skipped.
  - `parts::active(store: &Path) -> Result<Part, Error>` — the last part, or the store path itself as a part with `ordinal: None` when there is none.
  - `parts::Stamp { ino: u64, size: u64, mtime: i128 }` with `Stamp::of(&std::fs::Metadata) -> Stamp` (what identifies a part's bytes between a read and a rewrite).

- [ ] **Step 1: Write the failing tests**

`src/parts.rs`:

```rust
//! A store's parts in replay order (§17).

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn names(parts: &[Part]) -> Vec<String> {
        parts.iter().map(|p| p.path.file_name().unwrap().to_str().unwrap().to_string()).collect()
    }

    #[test]
    fn unnumbered_file_first_then_hex_order() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["s.tsvz", "s.tsvz.10", "s.tsvz.f", "s.tsvz.002", "s.tsvz.B", "s.tsvz.1"] {
            fs::write(dir.path().join(name), "").unwrap();
        }
        let parts = list(&dir.path().join("s.tsvz")).unwrap();
        assert_eq!(names(&parts), ["s.tsvz", "s.tsvz.1", "s.tsvz.002", "s.tsvz.B", "s.tsvz.f", "s.tsvz.10"]);
        assert_eq!(parts[0].ordinal, None);
        assert_eq!(parts[2].ordinal.as_deref(), Some("002"));
    }

    #[test]
    fn rotated_unknown_and_other_files_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        for name in [
            "s.tsvz.1", "s.tsvz.2.rotated", "s.tsvz.3.rotated.gz", "s.tsvz.4.GZ", "s.tsvz.5.zip",
            "s.tsvz.serve", "s.tsvz.damaged-20261012T000000", "s.tsvzx.6", "t.tsvz.7", "s.csvz.8",
        ] {
            fs::write(dir.path().join(name), "").unwrap();
        }
        let parts = list(&dir.path().join("s.tsvz")).unwrap();
        assert_eq!(names(&parts), ["s.tsvz.1", "s.tsvz.4.GZ"]);
        assert_eq!(parts[1].codec, Some(Codec::Gzip));
    }

    #[test]
    fn compressed_store_path_and_its_parts() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["s.tsvz.gz", "s.tsvz.1", "s.tsvz.2.xz"] {
            fs::write(dir.path().join(name), "").unwrap();
        }
        let parts = list(&dir.path().join("s.tsvz.gz")).unwrap();
        assert_eq!(names(&parts), ["s.tsvz.gz", "s.tsvz.1", "s.tsvz.2.xz"]);
        assert_eq!(parts[0].codec, Some(Codec::Gzip));
        assert_eq!(parts[2].codec, Some(Codec::Xz));
    }

    #[test]
    fn missing_store_and_active_part() {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("s.tsvz");
        assert!(list(&store).unwrap().is_empty());
        assert_eq!(active(&store).unwrap().path, store);
        fs::write(dir.path().join("s.tsvz.a"), "").unwrap();
        fs::write(dir.path().join("s.tsvz.9"), "").unwrap();
        assert_eq!(active(&store).unwrap().path, dir.path().join("s.tsvz.a"));
    }

    #[test]
    fn relative_store_path_keeps_relative_part_paths() {
        let parts = list(std::path::Path::new("no-such-dir-xyz/s.tsvz")).unwrap();
        assert!(parts.is_empty());
    }

    #[test]
    fn stamp_changes_with_content() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.tsvz");
        fs::write(&p, "a\tx\n").unwrap();
        let one = Stamp::of(&fs::metadata(&p).unwrap());
        fs::write(&p, "a\tx\nb\ty\n").unwrap();
        let two = Stamp::of(&fs::metadata(&p).unwrap());
        assert_eq!(one.ino, two.ino);
        assert_ne!(one, two);
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib parts`
Expected: compile errors: `list`, `active`, `Part`, `Stamp` not found.

- [ ] **Step 3: Implement**

Add above the tests in `src/parts.rs`:

```rust
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use crate::name::{self, Codec};
use crate::Error;

/// One part of a store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Part {
    pub path: PathBuf,
    /// The ordinal as written in the file name; None for the unnumbered part 0 (§17.1).
    pub ordinal: Option<String>,
    pub codec: Option<Codec>,
}

/// What identifies a part's bytes between a read and a later check: inode, size, mtime (ns).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stamp {
    pub ino: u64,
    pub size: u64,
    pub mtime: i128,
}

impl Stamp {
    pub fn of(meta: &std::fs::Metadata) -> Stamp {
        Stamp {
            ino: meta.ino(),
            size: meta.size(),
            mtime: meta.mtime() as i128 * 1_000_000_000 + meta.mtime_nsec() as i128,
        }
    }
}

/// The parts of the store at `store`, in replay order (§17.3): the unnumbered file when it
/// exists, then the numbered parts `<store-without-codec>.<ordinal>[.<codec>]` by ordinal
/// value. `.rotated` parts and unknown codec suffixes are skipped (§19.5). A missing
/// directory gives no parts.
pub fn list(store: &Path) -> Result<Vec<Part>, Error> {
    let text = store.to_str().ok_or_else(|| Error::BadPath(store.to_path_buf()))?;
    let (base, codec) = name::split_codec(text);
    let base = Path::new(base);
    let file_name = base
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| Error::BadPath(store.to_path_buf()))?;
    let prefix = format!("{file_name}.");
    let parent = base.parent().filter(|p| !p.as_os_str().is_empty());
    let mut numbered = Vec::new();
    if let Ok(entries) = std::fs::read_dir(parent.unwrap_or(Path::new("."))) {
        for entry in entries.flatten() {
            let entry_name = entry.file_name();
            let Some(entry_name) = entry_name.to_str() else { continue };
            let Some(rest) = entry_name.strip_prefix(&prefix) else { continue };
            let Some(n) = name::parse_numbered(rest) else { continue };
            if n.rotated {
                continue;
            }
            numbered.push((name::ordinal_key(&n.ordinal), entry_name.to_string(), n));
        }
    }
    numbered.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
    let mut parts = Vec::with_capacity(numbered.len() + 1);
    if store.is_file() {
        parts.push(Part { path: store.to_path_buf(), ordinal: None, codec });
    }
    for (_, entry_name, n) in numbered {
        let path = match parent {
            Some(dir) => dir.join(&entry_name),
            None => PathBuf::from(&entry_name),
        };
        parts.push(Part { path, ordinal: Some(n.ordinal), codec: n.codec });
    }
    Ok(parts)
}

/// The part appends go to: the store's last part in replay order, or the store path itself
/// when the store has no part yet (§17.7, §20.2.2).
pub fn active(store: &Path) -> Result<Part, Error> {
    let mut parts = list(store)?;
    Ok(match parts.pop() {
        Some(last) => last,
        None => {
            let text = store.to_str().ok_or_else(|| Error::BadPath(store.to_path_buf()))?;
            Part { path: store.to_path_buf(), ordinal: None, codec: name::split_codec(text).1 }
        }
    })
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib parts`
Expected: 6 passed.

- [ ] **Step 5: Commit**

```bash
git add src/parts.rs src/lib.rs
git commit -m "Parts: list a store's parts in hex ordinal order, skip .rotated, find the active part." -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---
### Task 4: Digests

**Files:**
- Create: `src/digest.rs`
- Modify: `src/lib.rs` (add `pub mod digest;`)

**Interfaces:**
- Consumes: nothing.
- Produces: `digest::Algo` (`Crc32, Crc32c, Sha256, Blake3`) with `from_name(&str) -> Option<Algo>` (any ASCII case) and `name(self) -> &'static str`; `digest::Accumulator` with `new(Algo)`, `update(&mut self, &[u8])`, `hex(&self) -> String` (non-consuming, lowercase).

- [ ] **Step 1: Write the failing tests**

`src/digest.rs`:

```rust
//! The digest algorithms of §15.2.1.

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(algo: Algo, parts: &[&[u8]]) -> String {
        let mut acc = Accumulator::new(algo);
        for p in parts {
            acc.update(p);
        }
        acc.hex()
    }

    #[test]
    fn known_vectors() {
        assert_eq!(digest(Algo::Crc32, &[b"123456789"]), "cbf43926");
        assert_eq!(digest(Algo::Crc32c, &[b"123456789"]), "e3069283");
        assert_eq!(digest(Algo::Sha256, &[b"abc"]), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(digest(Algo::Blake3, &[b"abc"]), "6437b3ac38465133ffb63b75273a8db548c558465d79db03fd359c6cd5bd9d85");
        assert_eq!(digest(Algo::Crc32, &[]), "00000000");
    }

    #[test]
    fn incremental_equals_whole_and_hex_does_not_consume() {
        for algo in [Algo::Crc32, Algo::Crc32c, Algo::Sha256, Algo::Blake3] {
            assert_eq!(digest(algo, &[b"a\tx\n", b"b\ty\n"]), digest(algo, &[b"a\tx\nb\ty\n"]));
            let mut acc = Accumulator::new(algo);
            acc.update(b"a");
            let first = acc.hex();
            assert_eq!(first, acc.hex());
            acc.update(b"b");
            assert_ne!(first, acc.hex());
        }
    }

    #[test]
    fn names() {
        assert_eq!(Algo::from_name("CRC32C"), Some(Algo::Crc32c));
        assert_eq!(Algo::from_name("sha256"), Some(Algo::Sha256));
        assert_eq!(Algo::from_name("md5"), None);
        assert_eq!(Algo::Blake3.name(), "blake3");
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib digest`
Expected: compile errors: `Algo`, `Accumulator` not found.

- [ ] **Step 3: Implement**

Add above the tests:

```rust
use sha2::Digest;

/// A digest algorithm named by `#_checksum_<algo>_#`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Algo {
    Crc32,
    Crc32c,
    Sha256,
    Blake3,
}

impl Algo {
    /// The algorithm an `<algo>` name (any ASCII case) names; None for names this crate does not implement.
    pub fn from_name(name: &str) -> Option<Algo> {
        match name.to_ascii_lowercase().as_str() {
            "crc32" => Some(Algo::Crc32),
            "crc32c" => Some(Algo::Crc32c),
            "sha256" => Some(Algo::Sha256),
            "blake3" => Some(Algo::Blake3),
            _ => None,
        }
    }

    /// The lowercase name `verify` prints (§20.2).
    pub fn name(self) -> &'static str {
        match self {
            Algo::Crc32 => "crc32",
            Algo::Crc32c => "crc32c",
            Algo::Sha256 => "sha256",
            Algo::Blake3 => "blake3",
        }
    }
}

/// A running digest over a segment (§15.4).
#[derive(Clone)]
pub enum Accumulator {
    Crc32(crc32fast::Hasher),
    Crc32c(u32),
    Sha256(sha2::Sha256),
    Blake3(Box<blake3::Hasher>),
}

impl Accumulator {
    pub fn new(algo: Algo) -> Accumulator {
        match algo {
            Algo::Crc32 => Accumulator::Crc32(crc32fast::Hasher::new()),
            Algo::Crc32c => Accumulator::Crc32c(0),
            Algo::Sha256 => Accumulator::Sha256(sha2::Sha256::new()),
            Algo::Blake3 => Accumulator::Blake3(Box::new(blake3::Hasher::new())),
        }
    }

    pub fn update(&mut self, data: &[u8]) {
        match self {
            Accumulator::Crc32(h) => h.update(data),
            Accumulator::Crc32c(c) => *c = crc32c::crc32c_append(*c, data),
            Accumulator::Sha256(h) => h.update(data),
            Accumulator::Blake3(h) => {
                h.update(data);
            }
        }
    }

    /// The digest so far as §15.2.1 writes it, in lowercase hexadecimal.
    pub fn hex(&self) -> String {
        match self {
            Accumulator::Crc32(h) => format!("{:08x}", h.clone().finalize()),
            Accumulator::Crc32c(c) => format!("{c:08x}"),
            Accumulator::Sha256(h) => h.clone().finalize().iter().map(|b| format!("{b:02x}")).collect(),
            Accumulator::Blake3(h) => h.finalize().to_hex().to_string(),
        }
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib digest`
Expected: 3 passed.

- [ ] **Step 5: Commit**

```bash
git add src/digest.rs src/lib.rs
git commit -m "Digests: crc32, crc32c, sha256 and blake3 accumulators (§15.2.1)." -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 5: Decoding compressed members (§16.6)

**Files:**
- Create: `src/codec.rs`
- Modify: `src/lib.rs` (add `pub mod codec;`)

**Interfaces:**
- Consumes: `name::Codec`.
- Produces:
  - `codec::Member` (`Complete { out: Vec<u8>, end: usize }`, `Short`, `Bad`); `codec::member(Codec, &[u8]) -> Member`.
  - `codec::Alone` (`Short(Vec<u8>)`, `Corrupt(Vec<u8>)`); `codec::decode_alone(Codec, &[u8]) -> Alone`.
  - `codec::gzip_header_len(&[u8]) -> Option<usize>`, `codec::gzip_keep(region: &[u8]) -> usize`.
  - `codec::Item` (`Data(Vec<u8>)`, `CutShort { offset: usize, out: Vec<u8> }`, `Corrupt { offset: usize, out: Vec<u8> }`, `Break`); `codec::items(Codec, &[u8]) -> Vec<Item>`.

The runner's oracle in `../TSVZ/tsvzconform.py` (`_member`, `_gzip_header_length`, `_gzip_unpad`, `_decode_alone`, `decompress_items`) is the reference: each function below mirrors one there. Read them before starting.

- [ ] **Step 1: Write the failing tests**

`src/codec.rs`:

```rust
//! Compressed parts (§16): decoding members, the damaged-member rules of §16.6, and (Task 6) encoding.

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// A member built by the codec crates' own encoders, so decoding is tested independently of Task 6.
    fn fixture(codec: Codec, data: &[u8]) -> Vec<u8> {
        match codec {
            Codec::Gzip => {
                let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
                e.write_all(data).unwrap();
                e.finish().unwrap()
            }
            #[cfg(feature = "bzip2")]
            Codec::Bzip2 => {
                let mut e = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::default());
                e.write_all(data).unwrap();
                e.finish().unwrap()
            }
            #[cfg(feature = "xz")]
            Codec::Xz => {
                let mut e = liblzma::write::XzEncoder::new(Vec::new(), 6);
                e.write_all(data).unwrap();
                e.finish().unwrap()
            }
            #[cfg(feature = "zstd")]
            Codec::Zstd => {
                let mut c = zstd::bulk::Compressor::new(3).unwrap();
                c.set_parameter(zstd::zstd_safe::CParameter::ChecksumFlag(true)).unwrap();
                c.compress(data).unwrap()
            }
            #[allow(unreachable_patterns)]
            _ => unreachable!("tests only use codecs this build has"),
        }
    }

    fn codecs() -> Vec<Codec> {
        [Codec::Gzip, Codec::Bzip2, Codec::Xz, Codec::Zstd].into_iter().filter(|c| c.supported()).collect()
    }

    #[test]
    fn complete_short_and_bad_members() {
        for codec in codecs() {
            let data = b"a\tx\nb\ty\n".repeat(50);
            let mut two = fixture(codec, &data);
            let first = two.len();
            two.extend(fixture(codec, b"c\tz\n"));
            assert_eq!(member(codec, &two), Member::Complete { out: data.clone(), end: first }, "{codec:?}");
            assert_eq!(member(codec, &two[..first - 3]), Member::Short, "{codec:?}");
            let mut bad = fixture(codec, &data);
            let n = bad.len();
            bad[n / 2] ^= 0xff;
            assert_eq!(member(codec, &bad), Member::Bad, "{codec:?}");
            assert!(matches!(decode_alone(codec, &bad), Alone::Corrupt(_)), "{codec:?}");
            assert!(matches!(decode_alone(codec, &two[..first - 3]), Alone::Short(_)), "{codec:?}");
            assert_eq!(member(codec, b"garbage!garbage!"), Member::Bad, "{codec:?}");
        }
    }

    #[test]
    fn a_failed_gzip_check_is_corrupt_with_its_output() {
        let mut g = fixture(Codec::Gzip, b"hello\n");
        let n = g.len();
        g[n - 8] ^= 1; // the stored CRC-32
        assert_eq!(member(Codec::Gzip, &g), Member::Bad);
        assert_eq!(decode_alone(Codec::Gzip, &g), Alone::Corrupt(b"hello\n".to_vec()));
        let mut l = fixture(Codec::Gzip, b"hello\n");
        l[n - 4] ^= 1; // the stored length
        assert_eq!(member(Codec::Gzip, &l), Member::Bad);
    }

    #[cfg(feature = "zstd")]
    #[test]
    fn a_failed_zstd_checksum_is_bad() {
        let mut z = fixture(Codec::Zstd, b"hello world hello world\n");
        let n = z.len();
        z[n - 1] ^= 1;
        assert_eq!(member(Codec::Zstd, &z), Member::Bad);
    }

    #[test]
    fn gzip_header_lengths() {
        assert_eq!(gzip_header_len(b"\x1f\x8b\x08\x00\0\0\0\0\0\xff"), Some(10));
        assert_eq!(gzip_header_len(b"\x1f\x8b\x08\x08\0\0\0\0\0\xffname\0rest"), Some(15));
        assert_eq!(gzip_header_len(b"\x1f\x8b\x08\x08\0\0\0\0\0\xffname"), None);
        assert_eq!(gzip_header_len(b"\x1f\x8b\x08\x04\0\0\0\0\0\xff\x02\0ab"), Some(14));
        assert_eq!(gzip_header_len(b"\x1f\x8b\x09\x00\0\0\0\0\0\xff"), None);
    }

    #[test]
    fn items_of_a_healthy_multi_member_part() {
        for codec in codecs() {
            let mut part = fixture(codec, b"a\tx\n");
            part.extend(fixture(codec, b"b\ty\n"));
            assert_eq!(items(codec, &part), vec![Item::Data(b"a\tx\n".to_vec()), Item::Data(b"b\ty\n".to_vec())], "{codec:?}");
        }
    }

    #[test]
    fn zero_padding_between_gzip_members_is_skipped() {
        let mut part = vec![0u8; 7];
        part.extend(fixture(Codec::Gzip, b"a\tx\n"));
        part.extend([0u8; 100]);
        part.extend(fixture(Codec::Gzip, b"b\ty\n"));
        part.extend([0u8; 3]);
        assert_eq!(items(Codec::Gzip, &part), vec![Item::Data(b"a\tx\n".to_vec()), Item::Data(b"b\ty\n".to_vec())]);
    }

    #[test]
    fn a_cut_short_member_before_another() {
        for codec in codecs() {
            let first = fixture(codec, &b"k\tvalue\n".repeat(200));
            let mut part = first[..first.len() / 2].to_vec();
            part.extend(fixture(codec, b"z\t1\n"));
            let got = items(codec, &part);
            assert_eq!(got.len(), 3, "{codec:?}: {got:?}");
            assert!(matches!(&got[0], Item::CutShort { offset: 0, .. }), "{codec:?}");
            assert_eq!(got[1], Item::Break);
            assert_eq!(got[2], Item::Data(b"z\t1\n".to_vec()));
        }
    }

    #[test]
    fn a_corrupt_member_in_the_middle() {
        for codec in codecs() {
            let a = fixture(codec, b"a\tx\n");
            let mut b = fixture(codec, &b"b\ty\n".repeat(100));
            let n = b.len();
            // gzip: break the stored CRC-32, a sure check failure; the others: a byte in the middle.
            if codec == Codec::Gzip { b[n - 8] ^= 1 } else { b[n / 2] ^= 0xff }
            let mut part = a.clone();
            part.extend(&b);
            part.extend(fixture(codec, b"c\tz\n"));
            let got = items(codec, &part);
            assert_eq!(got[0], Item::Data(b"a\tx\n".to_vec()), "{codec:?}");
            assert!(matches!(&got[1], Item::Corrupt { offset, .. } if *offset == a.len()), "{codec:?}: {got:?}");
            assert_eq!(got[2], Item::Break, "{codec:?}");
            assert_eq!(got[3], Item::Data(b"c\tz\n".to_vec()), "{codec:?}");
        }
    }

    #[test]
    fn gzip_padding_after_an_unfinished_header_is_not_its_own() {
        // The header's own zero bytes run into the padding: its end lies past the last non-zero byte.
        let mut region = b"\x1f\x8b\x08".to_vec();
        region.extend([0u8; 40]);
        assert_eq!(gzip_keep(&region), 3);
        assert_eq!(gzip_keep(b"\x1f\x8b\x08\x00\x01"), 5); // no trailing zeros: all of it
    }

    #[test]
    fn gzip_zeros_that_complete_a_begun_trailer_are_its_own() {
        let full = fixture(Codec::Gzip, b"hello\n");
        let cut = full.len() - 5; // the deflate data and 3 trailer bytes
        assert!(full[cut - 1] != 0, "fixture precondition: the last kept byte is not zero");
        let mut region = full[..cut].to_vec();
        region.extend([0u8; 10]);
        assert_eq!(gzip_keep(&region), cut + 5);
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib codec`
Expected: compile errors: `member`, `decode_alone`, `items`, `gzip_keep`, `gzip_header_len`, `Member`, `Alone`, `Item` not found.

- [ ] **Step 3: Implement decoding**

Add above the tests in `src/codec.rs`:

```rust
use crate::name::Codec;

/// How the member at the start of some bytes decoded (§16.6 rule 1).
#[derive(Debug, PartialEq, Eq)]
pub enum Member {
    /// Decoded to its end and passed its check; `end` is its length in bytes.
    Complete { out: Vec<u8>, end: usize },
    /// The bytes ended before the member did.
    Short,
    /// The bytes are not a valid member.
    Bad,
}

/// A damaged member's bytes decoded on their own (§16.6 rule 2).
#[derive(Debug, PartialEq, Eq)]
pub enum Alone {
    /// They merely end before the member does (or decode whole): the output is used.
    Short(Vec<u8>),
    /// Decoding failed: the output decoded before the failure, for the corruption policy.
    Corrupt(Vec<u8>),
}

/// A compressed part as the framer takes it (§16.6).
#[derive(Debug, PartialEq, Eq)]
pub enum Item {
    /// A complete member's output.
    Data(Vec<u8>),
    /// A member cut short: its output is used, with a warning.
    CutShort { offset: usize, out: Vec<u8> },
    /// A corrupt member: the policy in force decides what happens to `out` (§4.10).
    Corrupt { offset: usize, out: Vec<u8> },
    /// A damaged member ended here: the unfinished line before this point is dropped.
    Break,
}

/// The length of the RFC 1952 header at the start of `data`, or None when it is not all
/// there or the bytes are not a gzip deflate member.
pub fn gzip_header_len(data: &[u8]) -> Option<usize> {
    if data.len() < 10 || &data[..3] != b"\x1f\x8b\x08" {
        return None;
    }
    let flags = data[3];
    let mut pos = 10usize;
    if flags & 4 != 0 {
        if data.len() < pos + 2 {
            return None;
        }
        pos += 2 + data[pos] as usize + 256 * data[pos + 1] as usize;
    }
    for flag in [8u8, 16] {
        if flags & flag != 0 {
            let rest = data.get(pos..)?;
            pos += memchr::memchr(0, rest)? + 1;
        }
    }
    if flags & 2 != 0 {
        pos += 2;
    }
    (pos <= data.len()).then_some(pos)
}

struct GzipDecoder {
    header: Vec<u8>,
    /// The raw inflater once the header is complete, and whether its deflate stream ended.
    inflate: Option<(flate2::Decompress, bool)>,
    trailer: Vec<u8>,
    crc: crc32fast::Hasher,
    size: u32,
    done: bool,
}

/// An incremental decoder for one member.
enum Decoder {
    Gzip(Box<GzipDecoder>),
    #[cfg(feature = "bzip2")]
    Bzip2(bzip2::Decompress),
    #[cfg(feature = "xz")]
    Xz(liblzma::stream::Stream),
    #[cfg(feature = "zstd")]
    Zstd(zstd::zstd_safe::DCtx<'static>),
    /// A codec this build does not include: every byte is a data error. `Store::open` refuses
    /// such parts before decoding (§16.5), so this is only a safe default.
    Unsupported,
}

impl Decoder {
    fn new(codec: Codec) -> Decoder {
        match codec {
            Codec::Gzip => Decoder::Gzip(Box::new(GzipDecoder {
                header: Vec::new(),
                inflate: None,
                trailer: Vec::new(),
                crc: crc32fast::Hasher::new(),
                size: 0,
                done: false,
            })),
            #[cfg(feature = "bzip2")]
            Codec::Bzip2 => Decoder::Bzip2(bzip2::Decompress::new(false)),
            #[cfg(feature = "xz")]
            Codec::Xz => match liblzma::stream::Stream::new_auto_decoder(u64::MAX, 0) {
                Ok(s) => Decoder::Xz(s),
                Err(_) => Decoder::Unsupported,
            },
            #[cfg(feature = "zstd")]
            Codec::Zstd => Decoder::Zstd(zstd::zstd_safe::DCtx::create()),
            #[allow(unreachable_patterns)]
            _ => Decoder::Unsupported,
        }
    }

    /// Feed `input`, appending output to `out`. Ok((bytes consumed, member ended and checked))
    /// or Err(()) on a data error. Output decoded before an error stays in `out`.
    fn feed(&mut self, input: &[u8], out: &mut Vec<u8>) -> Result<(usize, bool), ()> {
        match self {
            Decoder::Gzip(g) => g.feed(input, out),
            #[cfg(feature = "bzip2")]
            Decoder::Bzip2(d) => {
                let before = d.total_in();
                loop {
                    out.reserve(64 * 1024);
                    let status = d.decompress_vec(&input[(d.total_in() - before) as usize..], out).map_err(|_| ())?;
                    let used = (d.total_in() - before) as usize;
                    if status == bzip2::Status::StreamEnd {
                        return Ok((used, true));
                    }
                    if used >= input.len() && out.len() < out.capacity() {
                        return Ok((used, false));
                    }
                }
            }
            #[cfg(feature = "xz")]
            Decoder::Xz(s) => {
                let before = s.total_in();
                loop {
                    out.reserve(64 * 1024);
                    let used = (s.total_in() - before) as usize;
                    let status = s.process_vec(&input[used..], out, liblzma::stream::Action::Run).map_err(|_| ())?;
                    let used = (s.total_in() - before) as usize;
                    if status == liblzma::stream::Status::StreamEnd {
                        return Ok((used, true));
                    }
                    if used >= input.len() && out.len() < out.capacity() {
                        return Ok((used, false));
                    }
                }
            }
            #[cfg(feature = "zstd")]
            Decoder::Zstd(d) => {
                let mut src = zstd::zstd_safe::InBuffer::around(input);
                loop {
                    out.reserve(64 * 1024);
                    let start = out.len();
                    let hint = {
                        let mut dst = zstd::zstd_safe::OutBuffer::around_pos(out, start);
                        d.decompress_stream(&mut dst, &mut src).map_err(|_| ())?
                    };
                    if hint == 0 {
                        return Ok((src.pos(), true));
                    }
                    if src.pos() >= input.len() && out.len() < out.capacity() {
                        return Ok((src.pos(), false));
                    }
                }
            }
            Decoder::Unsupported => Err(()),
        }
    }
}

impl GzipDecoder {
    fn feed(&mut self, input: &[u8], out: &mut Vec<u8>) -> Result<(usize, bool), ()> {
        let mut used = 0;
        if self.done {
            return Ok((0, true));
        }
        if self.inflate.is_none() {
            while gzip_header_len(&self.header).is_none() && used < input.len() {
                self.header.push(input[used]);
                used += 1;
                let n = self.header.len();
                if n <= 3 && self.header[..] != b"\x1f\x8b\x08"[..n] {
                    return Err(());
                }
                if n > 1 << 20 {
                    return Err(()); // an absurd header: not a member a writer made
                }
            }
            if gzip_header_len(&self.header).is_none() {
                return Ok((used, false));
            }
            self.inflate = Some((flate2::Decompress::new(false), false));
        }
        let (z, ended) = self.inflate.as_mut().expect("set above");
        while !*ended && used < input.len() {
            let before_in = z.total_in();
            let before_out = out.len();
            out.reserve(64 * 1024);
            let status = z.decompress_vec(&input[used..], out, flate2::FlushDecompress::None).map_err(|_| ())?;
            used += (z.total_in() - before_in) as usize;
            self.crc.update(&out[before_out..]);
            self.size = self.size.wrapping_add((out.len() - before_out) as u32);
            if status == flate2::Status::StreamEnd {
                *ended = true;
            } else if z.total_in() == before_in && out.len() == before_out {
                break;
            }
        }
        if !*ended {
            return Ok((used, false));
        }
        while self.trailer.len() < 8 && used < input.len() {
            self.trailer.push(input[used]);
            used += 1;
        }
        if self.trailer.len() < 8 {
            return Ok((used, false));
        }
        let crc = u32::from_le_bytes(self.trailer[..4].try_into().expect("4 bytes"));
        let size = u32::from_le_bytes(self.trailer[4..].try_into().expect("4 bytes"));
        if crc != self.crc.clone().finalize() || size != self.size {
            return Err(());
        }
        self.done = true;
        Ok((used, true))
    }
}

/// Decode the member at the start of `data` (§16.6 rule 1).
pub fn member(codec: Codec, data: &[u8]) -> Member {
    let mut d = Decoder::new(codec);
    let mut out = Vec::new();
    match d.feed(data, &mut out) {
        Err(()) => Member::Bad,
        Ok((end, true)) => Member::Complete { out, end },
        Ok((_, false)) => Member::Short,
    }
}

/// Decode a damaged member's bytes on their own (§16.6 rule 2), one byte at a time so that
/// nothing decoded before a failure is lost with the call that fails.
pub fn decode_alone(codec: Codec, data: &[u8]) -> Alone {
    let mut d = Decoder::new(codec);
    let mut out = Vec::new();
    for i in 0..data.len() {
        match d.feed(&data[i..i + 1], &mut out) {
            Err(()) => return Alone::Corrupt(out),
            Ok((_, true)) => return Alone::Short(out),
            Ok(_) => {}
        }
    }
    Alone::Short(out)
}

/// Feed `input` to a raw inflater and discard the output: Ok(Some(bytes consumed)) when the
/// deflate stream ended within `input`, Ok(None) when it needs more, Err on a data error.
fn inflate_to_end(z: &mut flate2::Decompress, input: &[u8], scratch: &mut [u8]) -> Result<Option<usize>, ()> {
    let mut pos = 0;
    loop {
        let (in0, out0) = (z.total_in(), z.total_out());
        let status = z.decompress(&input[pos..], scratch, flate2::FlushDecompress::None).map_err(|_| ())?;
        pos += (z.total_in() - in0) as usize;
        if status == flate2::Status::StreamEnd {
            return Ok(Some(pos));
        }
        let produced = (z.total_out() - out0) as usize;
        if (pos >= input.len() && produced < scratch.len()) || (z.total_in() == in0 && produced == 0) {
            return Ok(None);
        }
    }
}

/// How many bytes of a damaged gzip member's region belong to it (§16.6 rule 2): zero bytes
/// ending the region are padding, except those its compressed data needs to reach its end
/// and those that complete an 8-byte trailer it has begun. Mirrors `_gzip_unpad` in the runner.
pub fn gzip_keep(region: &[u8]) -> usize {
    let stripped = region.iter().rposition(|&b| b != 0).map_or(0, |i| i + 1);
    let zeros = region.len() - stripped;
    if zeros == 0 {
        return region.len();
    }
    let start = match gzip_header_len(region) {
        Some(start) if start <= stripped => start,
        _ => return stripped,
    };
    let mut z = flate2::Decompress::new(false);
    let mut scratch = vec![0u8; 1 << 16];
    match inflate_to_end(&mut z, &region[start..stripped], &mut scratch) {
        Err(()) => stripped,
        Ok(Some(used)) => {
            let begun = stripped - start - used; // trailer bytes before the zeros
            if begun > 0 && begun < 8 { stripped + (8 - begun).min(zeros) } else { stripped }
        }
        Ok(None) => {
            // Fed only zeros, deflate data in a Huffman block ends within a few hundred bytes, fails or never
            // ends; in a stored block it ends only if the block is final, after the rest of its NLEN (both
            // bytes zero only when LEN is 65535) and its data: at most 2 + 65535 zeros.
            let zeros = zeros.min(2 + 65535);
            let pad = vec![0u8; 1 << 16];
            let mut fed = 0;
            while fed < zeros {
                let n = (zeros - fed).min(pad.len());
                match inflate_to_end(&mut z, &pad[..n], &mut scratch) {
                    Err(()) => return stripped,
                    Ok(Some(used)) => return stripped + fed + used,
                    Ok(None) => fed += n,
                }
            }
            stripped
        }
    }
}

/// A compressed part's bytes as items for the framer (§16.6). Mirrors `decompress_items`.
pub fn items(codec: Codec, data: &[u8]) -> Vec<Item> {
    let magic = codec.magic();
    let mut items = Vec::new();
    let mut pos = 0;
    while pos < data.len() {
        if codec == Codec::Gzip && data[pos] == 0 {
            pos += data[pos..].iter().take_while(|&&b| b == 0).count(); // padding before a member
            continue;
        }
        if let Member::Complete { out, end } = member(codec, &data[pos..]) {
            items.push(Item::Data(out));
            pos += end.max(1);
            continue;
        }
        // A damaged member's bytes run to the next member start, complete or not (rule 2).
        let following = memchr::memmem::find(&data[pos + 1..], magic).map(|i| pos + 1 + i);
        let mut region = &data[pos..following.unwrap_or(data.len())];
        if codec == Codec::Gzip {
            region = &region[..gzip_keep(region)];
        }
        items.push(match decode_alone(codec, region) {
            Alone::Short(out) => Item::CutShort { offset: pos, out },
            Alone::Corrupt(out) => Item::Corrupt { offset: pos, out },
        });
        match following {
            Some(next) => {
                items.push(Item::Break);
                pos = next;
            }
            None => break,
        }
    }
    items
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib codec`
Expected: 10 passed. Also run `cargo test --lib codec --no-default-features`: 9 pass (the zstd-only test is compiled out, and the other tests skip the codecs the build lacks).

- [ ] **Step 5: Commit**

```bash
git add src/codec.rs src/lib.rs
git commit -m "Codec: decode members of gzip, bzip2, xz and zstd parts; judge damaged members by §16.6." -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 6: Encoding compressed members (§16.4, §16.7, §16.8)

**Files:**
- Modify: `src/codec.rs`

**Interfaces:**
- Consumes: `member` (Task 5) in its tests.
- Produces: `codec::encode_member(Codec, data: &[u8]) -> Option<Vec<u8>>` (None when the build lacks the codec); `codec::encode_members(Codec, payload: &[u8], member_size: usize) -> Option<Vec<u8>>` (members of at most `member_size` uncompressed bytes, ending at record boundaries; one longer record is a member of its own).

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module in `src/codec.rs`:

```rust
    #[test]
    fn encoded_members_decode_back() {
        for codec in codecs() {
            let data = b"a\tx\nb\ty\n";
            let m = encode_member(codec, data).unwrap();
            assert_eq!(member(codec, &m), Member::Complete { out: data.to_vec(), end: m.len() }, "{codec:?}");
        }
    }

    #[test]
    fn gzip_members_have_a_fixed_header() {
        let m = encode_member(Codec::Gzip, b"x\n").unwrap();
        assert_eq!(&m[..10], b"\x1f\x8b\x08\x00\x00\x00\x00\x00\x00\xff");
    }

    #[cfg(feature = "zstd")]
    #[test]
    fn zstd_frames_carry_a_checksum() {
        let m = encode_member(Codec::Zstd, b"x\n").unwrap();
        assert_ne!(m[4] & 0x04, 0, "frame header descriptor has the content checksum flag");
    }

    #[test]
    fn members_split_at_record_boundaries() {
        let payload: Vec<u8> = (0..100).flat_map(|i| format!("k{i:03}\t{}\n", "v".repeat(20)).into_bytes()).collect();
        for codec in codecs() {
            let all = encode_members(codec, &payload, 300).unwrap();
            let got = items(codec, &all);
            let outs: Vec<&Vec<u8>> = got.iter().map(|i| match i { Item::Data(d) => d, other => panic!("{other:?}") }).collect();
            assert!(outs.len() > 5, "{codec:?}");
            assert!(outs.iter().all(|o| o.len() <= 300 && o.ends_with(b"\n")), "{codec:?}");
            assert_eq!(outs.into_iter().flatten().copied().collect::<Vec<u8>>(), payload, "{codec:?}");
        }
    }

    #[test]
    fn a_record_longer_than_the_size_is_its_own_member() {
        let payload = [b"short\n".to_vec(), vec![b'x'; 1000], b"\nend\n".to_vec()].concat();
        let all = encode_members(Codec::Gzip, &payload, 100).unwrap();
        let got = items(Codec::Gzip, &all);
        assert_eq!(got.len(), 3);
        assert_eq!(got[1], Item::Data([vec![b'x'; 1000], b"\n".to_vec()].concat()));
    }

    #[test]
    fn empty_payload_is_no_member() {
        assert_eq!(encode_members(Codec::Gzip, b"", 100).unwrap(), Vec::<u8>::new());
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib codec`
Expected: compile errors: `encode_member`, `encode_members` not found.

- [ ] **Step 3: Implement**

Add to `src/codec.rs` (above the tests):

```rust
/// One member holding `data`, or None when this build does not include the codec.
/// gzip members have a fixed header (no name, mtime 0, OS 255); zstd frames carry the content
/// checksum (§16.8).
pub fn encode_member(codec: Codec, data: &[u8]) -> Option<Vec<u8>> {
    use std::io::Write;
    match codec {
        Codec::Gzip => {
            let mut out = b"\x1f\x8b\x08\x00\x00\x00\x00\x00\x00\xff".to_vec();
            let mut e = flate2::write::DeflateEncoder::new(out.split_off(10), flate2::Compression::default());
            e.write_all(data).ok()?;
            out.extend(e.finish().ok()?);
            out.extend(crc32fast::hash(data).to_le_bytes());
            out.extend((data.len() as u32).to_le_bytes());
            Some(out)
        }
        #[cfg(feature = "bzip2")]
        Codec::Bzip2 => {
            let mut e = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::default());
            e.write_all(data).ok()?;
            e.finish().ok()
        }
        #[cfg(feature = "xz")]
        Codec::Xz => {
            let mut e = liblzma::write::XzEncoder::new(Vec::new(), 6);
            e.write_all(data).ok()?;
            e.finish().ok()
        }
        #[cfg(feature = "zstd")]
        Codec::Zstd => {
            let mut c = zstd::bulk::Compressor::new(3).ok()?;
            c.set_parameter(zstd::zstd_safe::CParameter::ChecksumFlag(true)).ok()?;
            c.compress(data).ok()
        }
        #[allow(unreachable_patterns)]
        _ => None,
    }
}

/// `payload` (whole records ending in `\n`) as members of at most `member_size` uncompressed
/// bytes each, ending at record boundaries; a single longer record is a member of its own
/// (§16.7). None when this build does not include the codec.
pub fn encode_members(codec: Codec, payload: &[u8], member_size: usize) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut start = 0;
    while start < payload.len() {
        let mut end = start;
        loop {
            let next = memchr::memchr(b'\n', &payload[end..]).map_or(payload.len(), |i| end + i + 1);
            if end > start && next - start > member_size {
                break;
            }
            end = next;
            if end >= payload.len() {
                break;
            }
        }
        out.extend(encode_member(codec, &payload[start..end])?);
        start = end;
    }
    Some(out)
}
```

Note on the gzip header line: `out.split_off(10)` returns the empty tail as the encoder's buffer, so `out` keeps exactly the 10 header bytes.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib codec`
Expected: all codec tests pass (16 with default features).

- [ ] **Step 5: Commit**

```bash
git add src/codec.rs
git commit -m "Codec: encode members, split at record boundaries by member size; zstd frames carry a checksum." -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 7: Framing committed lines (§4, §7.1)

**Files:**
- Create: `src/frame.rs`
- Modify: `src/lib.rs` (add `pub mod frame;`)

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `frame::Line<'a> { raw: &'a [u8] /* includes the "\n" */, number: u64 /* 1-based in the part */, offset: u64 /* where the line starts in the part's byte stream */ }` with `Line::text(&self) -> &'a [u8]` (no `\n`, no one `\r` before it).
  - `frame::Framer` with `Framer::new()`, `Framer::resume(offset: u64, number: u64)`, `push(&mut self, data: &[u8], on_line: &mut dyn FnMut(&Line<'_>) -> bool) -> bool` (false once `on_line` returned false), `brk(&mut self)`, `tail(&self) -> &[u8]`, `committed(&self) -> u64` (offset just after the last committed line), `number(&self) -> u64`.

- [ ] **Step 1: Write the failing tests**

`src/frame.rs`:

```rust
//! Committed lines of a part (§4.2-§4.7, §7.1).

#[cfg(test)]
mod tests {
    use super::*;

    /// (raw, text, number, offset) of each line.
    type Framed = Vec<(Vec<u8>, Vec<u8>, u64, u64)>;

    fn frame(chunks: &[&[u8]]) -> (Framed, Framer) {
        let mut f = Framer::new();
        let mut lines = Vec::new();
        for c in chunks {
            f.push(c, &mut |l| {
                lines.push((l.raw.to_vec(), l.text().to_vec(), l.number, l.offset));
                true
            });
        }
        (lines, f)
    }

    #[test]
    fn lines_crlf_and_the_tail() {
        let (lines, f) = frame(&[b"a\tx\nb\ty\r\nc\r\r\npartial"]);
        let texts: Vec<&[u8]> = lines.iter().map(|l| l.1.as_slice()).collect();
        assert_eq!(texts, vec![&b"a\tx"[..], b"b\ty", b"c\r"]);
        assert_eq!(lines[1].0, b"b\ty\r\n");
        assert_eq!(lines.iter().map(|l| (l.2, l.3)).collect::<Vec<_>>(), vec![(1, 0), (2, 4), (3, 9)]);
        assert_eq!(f.tail(), b"partial");
        assert_eq!(f.committed(), 13);
        assert_eq!(f.number(), 3);
    }

    #[test]
    fn a_line_split_across_pushes() {
        let (lines, _) = frame(&[b"ab", b"c\td", b"", b"ef\ng", b"h\n"]);
        assert_eq!(lines.iter().map(|l| l.0.clone()).collect::<Vec<_>>(), vec![b"abc\tdef\n".to_vec(), b"gh\n".to_vec()]);
        assert_eq!(lines[1].3, 8);
    }

    #[test]
    fn one_bom_is_removed_at_the_start_only() {
        let (lines, _) = frame(&[b"\xef\xbb\xbfa\tx\n\xef\xbb\xbfb\ty\n"]);
        assert_eq!(lines[0].1, b"a\tx");
        assert_eq!(lines[0].3, 3);
        assert_eq!(lines[1].1, b"\xef\xbb\xbfb\ty");
        let (split, _) = frame(&[b"\xef", b"\xbb", b"\xbfk\tv\n"]);
        assert_eq!(split[0].1, b"k\tv");
        let (only, f) = frame(&[b"\xef\xbb\xbf"]);
        assert!(only.is_empty() && f.tail().is_empty());
    }

    #[test]
    fn a_break_drops_the_unfinished_line_and_decides_the_bom() {
        let mut f = Framer::new();
        let mut got = Vec::new();
        f.push(b"a\tx\nunfin", &mut |l| { got.push(l.text().to_vec()); true });
        f.brk();
        f.push(b"\xef\xbb\xbfb\ty\n", &mut |l| { got.push(l.text().to_vec()); true });
        assert_eq!(got, vec![b"a\tx".to_vec(), b"\xef\xbb\xbfb\ty".to_vec()]);
        let mut g = Framer::new();
        g.push(b"\xef", &mut |_| true);
        g.brk();
        let mut after = Vec::new();
        g.push(b"\xbb\xbfz\n", &mut |l| { after.push(l.text().to_vec()); true });
        assert_eq!(after, vec![b"\xbb\xbfz".to_vec()]);
    }

    #[test]
    fn stopping_early() {
        let mut f = Framer::new();
        let mut n = 0;
        assert!(!f.push(b"a\nb\nc\n", &mut |_| { n += 1; n < 2 }));
        assert_eq!(n, 2);
    }

    #[test]
    fn resume_continues_numbers_and_offsets() {
        let mut f = Framer::resume(100, 7);
        let mut got = Vec::new();
        f.push(b"\xef\xbb\xbfk\tv\n", &mut |l| { got.push((l.text().to_vec(), l.number, l.offset)); true });
        assert_eq!(got, vec![(b"\xef\xbb\xbfk\tv".to_vec(), 8, 100)]);
        assert_eq!(f.committed(), 107);
    }

    #[test]
    fn a_huge_line_in_small_pieces_is_linear() {
        let piece = vec![b'x'; 16 * 1024];
        let mut f = Framer::new();
        let started = std::time::Instant::now();
        let mut lens = Vec::new();
        for _ in 0..4096 {
            f.push(&piece, &mut |_| true); // 64 MiB without a newline
        }
        f.push(b"\n", &mut |l| { lens.push(l.raw.len()); true });
        assert_eq!(lens, vec![64 * 1024 * 1024 + 1]);
        assert!(started.elapsed().as_secs() < 10, "framing a 64 MiB line took {:?}", started.elapsed());
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib frame`
Expected: compile errors: `Framer`, `Line` not found.

- [ ] **Step 3: Implement**

Add above the tests:

```rust
const BOM: &[u8] = b"\xef\xbb\xbf";

/// One committed line of a part.
#[derive(Clone, Copy, Debug)]
pub struct Line<'a> {
    /// The line's bytes including its terminating `\n` (and a `\r` before it): what digests cover (§15.4).
    pub raw: &'a [u8],
    /// 1-based line number in the part; a byte-order mark does not change it (§4.7).
    pub number: u64,
    /// Where the line starts in the part's byte stream (decompressed for a compressed part).
    pub offset: u64,
}

impl<'a> Line<'a> {
    /// The record without its `\n` and without one `\r` before it (§4.2, §7.1).
    pub fn text(&self) -> &'a [u8] {
        let body = &self.raw[..self.raw.len() - 1];
        body.strip_suffix(b"\r").unwrap_or(body)
    }
}

/// Turns a part's bytes into committed lines: one BOM removed at the start (§4.7), lines
/// ended by `\n`, bytes after the last `\n` held as the uncommitted tail (§4.3).
pub struct Framer {
    pending: Vec<u8>,
    bom_decided: bool,
    number: u64,
    consumed: u64,
    committed: u64,
}

impl Default for Framer {
    fn default() -> Self {
        Framer::new()
    }
}

impl Framer {
    /// A framer at the start of a part.
    pub fn new() -> Framer {
        Framer { pending: Vec::new(), bom_decided: false, number: 0, consumed: 0, committed: 0 }
    }

    /// A framer continuing a part at `offset`, just after committed line number `number`.
    pub fn resume(offset: u64, number: u64) -> Framer {
        Framer { pending: Vec::new(), bom_decided: offset > 0, number, consumed: offset, committed: offset }
    }

    /// Feed the next bytes of the part. `on_line` gets each committed line; returning false
    /// stops the framing, and `push` then returns false.
    pub fn push(&mut self, data: &[u8], on_line: &mut dyn FnMut(&Line<'_>) -> bool) -> bool {
        if self.bom_decided {
            return self.push_decided(data, on_line);
        }
        // The mark is decided once three bytes or a newline are in, as the runner's oracle does.
        self.pending.extend_from_slice(data);
        self.consumed += data.len() as u64;
        if self.pending.len() < 3 && memchr::memchr(b'\n', &self.pending).is_none() {
            return true;
        }
        self.bom_decided = true;
        let mut held = std::mem::take(&mut self.pending);
        self.consumed -= held.len() as u64;
        if held.starts_with(BOM) {
            held.drain(..3);
            self.consumed += 3;
            self.committed += 3;
        }
        self.push_decided(&held, on_line)
    }

    fn push_decided(&mut self, data: &[u8], on_line: &mut dyn FnMut(&Line<'_>) -> bool) -> bool {
        self.consumed += data.len() as u64;
        let mut rest = data;
        if !self.pending.is_empty() {
            let Some(i) = memchr::memchr(b'\n', rest) else {
                self.pending.extend_from_slice(rest);
                return true;
            };
            let mut raw = std::mem::take(&mut self.pending);
            raw.extend_from_slice(&rest[..=i]);
            rest = &rest[i + 1..];
            if !self.emit(&raw, on_line) {
                return false;
            }
        }
        while let Some(i) = memchr::memchr(b'\n', rest) {
            let (line, tail) = rest.split_at(i + 1);
            if !self.emit(line, on_line) {
                return false;
            }
            rest = tail;
        }
        self.pending.extend_from_slice(rest);
        true
    }

    fn emit(&mut self, raw: &[u8], on_line: &mut dyn FnMut(&Line<'_>) -> bool) -> bool {
        self.number += 1;
        let line = Line { raw, number: self.number, offset: self.committed };
        self.committed += raw.len() as u64;
        on_line(&line)
    }

    /// A damaged member ended here (§16.6): drop the unfinished line; a later BOM is data.
    pub fn brk(&mut self) {
        self.pending.clear();
        self.bom_decided = true;
        self.committed = self.consumed;
    }

    /// The uncommitted bytes after the last `\n` so far (§4.3).
    pub fn tail(&self) -> &[u8] {
        &self.pending
    }

    /// The offset just after the last committed line.
    pub fn committed(&self) -> u64 {
        self.committed
    }

    /// The number of the last committed line.
    pub fn number(&self) -> u64 {
        self.number
    }
}
```

Note on the BOM path: `pending` is moved to `held` and replayed through `push_decided`, which re-adds `held.len()` to `consumed`; subtracting it first keeps `consumed` exact.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib frame --release` (release: the 64 MiB test is slow in debug builds) and `cargo test --lib frame`
Expected: 7 passed in both.

- [ ] **Step 5: Commit**

```bash
git add src/frame.rs src/lib.rs
git commit -m "Framer: committed lines with BOM, CRLF, tail and damaged-member breaks (§4, §7.1)." -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---
### Task 8: Replay — the §7 pipeline

**Files:**
- Create: `src/replay.rs`
- Modify: `src/lib.rs` (add `pub mod replay;`)

**Interfaces:**
- Consumes: `frame::Line`, `escape::{decode, is_marker_key}`, `digest::{Algo, Accumulator}`, `name::Format`, `WarnFn`, `Warning`.
- Produces:
  - `replay::DEFAULT_MEMBER_SIZE: u64 = 1048576`.
  - `replay::Policy` (`Recover, Skip, Stop`) with `name(self) -> &'static str`.
  - `replay::Markers { version: u64, defaults: Arc<Vec<String>>, strip: bool, fill_empty: bool, return_missing: bool, on_corruption: Policy, rotate: &'static str, write_ack: &'static str, member_size: u64 }` with `Default` (1, empty, true, false, true, Recover, "keep", "memory", 1048576). `defaults[0]` is value column 1.
  - `replay::Binding { defaults: Arc<Vec<String>>, fill_empty: bool }`.
  - `replay::Cells` with `push(&mut self, &str)`, `len()`, `is_empty()`, `get(j) -> &str`, `iter()`, `from_values(impl IntoIterator<Item: AsRef<str>>)`; `replay::EMPTY_CELLS: Cells` (a static).
  - `replay::Mismatch { part: PathBuf, line: u64, algo: &'static str, expected: String, computed: String }`.
  - `replay::DamageKind` (`ZeroFill, CorruptMember`), `replay::Damage { part: PathBuf, offset: u64, kind: DamageKind }`.
  - `replay::Sink` trait: `wants_cells(&self, key: &str) -> bool` (default true), `put(&mut self, key: &str, cells: Cells, binding: &Arc<Binding>)`, `delete(&mut self, key: &str)`.
  - `replay::Replay` with `new(Format, verify: bool, Option<WarnFn>)`, `begin_part(&mut self, &Path, first: bool)`, `line(&mut self, &Line<'_>, &mut dyn Sink) -> bool` (false: a `stop` policy ended the read), `cut_short(&mut self, offset: usize)`, `corrupt(&mut self, offset: usize, decoded: usize) -> Policy`, `tail(&mut self, bytes: usize)`, `binding(&mut self) -> Arc<Binding>`, `format(&self) -> Format`; public fields `markers: Markers`, `mismatches: Vec<Mismatch>`, `stopped: Option<Damage>`, `damaged: bool`, `header: Option<String>`.

- [ ] **Step 1: Write the failing tests**

`src/replay.rs`:

```rust
//! The reading pipeline of §7 as a state machine: classification, markers (§12), integrity
//! (§15), zero fill (§4.9), tombstones (§9), stripping (§10), escaping (§13) and the binding
//! of rows to the defaults in force where they appear (§14.4).

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::Framer;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Rows(Vec<(String, Vec<String>, Arc<Binding>)>);

    impl Sink for Rows {
        fn put(&mut self, key: &str, cells: Cells, binding: &Arc<Binding>) {
            let cells: Vec<String> = cells.iter().map(str::to_string).collect();
            match self.0.iter_mut().find(|r| r.0 == key) {
                Some(r) => {
                    r.1 = cells;
                    r.2 = binding.clone();
                }
                None => self.0.push((key.to_string(), cells, binding.clone())),
            }
        }
        fn delete(&mut self, key: &str) {
            self.0.retain(|r| r.0 != key);
        }
    }

    const TSVZ: Format = Format { delimiter: b'\t', strict: true };

    fn run(format: Format, verify: bool, data: &[u8]) -> (Replay, Rows, Vec<String>) {
        let warnings = Arc::new(Mutex::new(Vec::new()));
        let sink = warnings.clone();
        let warn: WarnFn = Arc::new(move |w: &Warning| sink.lock().unwrap().push(w.message.clone()));
        let mut replay = Replay::new(format, verify, Some(warn));
        replay.begin_part(Path::new("s.tsvz"), true);
        let mut rows = Rows::default();
        let mut framer = Framer::new();
        framer.push(data, &mut |l| replay.line(l, &mut rows));
        let w = warnings.lock().unwrap().clone();
        (replay, rows, w)
    }

    fn table(rows: &Rows) -> Vec<(String, Vec<String>)> {
        rows.0.iter().map(|r| (r.0.clone(), r.1.clone())).collect()
    }

    fn row(k: &str, cells: &[&str]) -> (String, Vec<String>) {
        (k.to_string(), cells.iter().map(|c| c.to_string()).collect())
    }

    #[test]
    fn last_wins_and_tombstones() {
        let (_, rows, _) = run(TSVZ, false, b"a\tx\nb\ty\na\tz\nb\nc\t\t\nd\n");
        assert_eq!(table(&rows), vec![row("a", &["z"]), row("c", &["", ""])]);
    }

    #[test]
    fn stripping_escaping_and_empty_keys() {
        let (_, rows, _) = run(TSVZ, false, b"a  \tx <LF>\n \t1\n\tz\n<#>h\tv<sep>w\n");
        assert_eq!(table(&rows), vec![row("a", &["x \n"]), row("#h", &["v\tw"])]);
        let (_, off, _) = run(TSVZ, false, b"#_strip_trailing_whites_#\tfalse\na \tx \n \t1\n");
        assert_eq!(table(&off), vec![row("a ", &["x "]), row(" ", &["1"])]);
    }

    #[test]
    fn markers_set_reset_and_ignore_invalid_values() {
        let (r, _, w) = run(TSVZ, false, b"#_strip_trailing_whites_#\tmaybe\n#_FILL_EMPTY_WITH_DEFAULT_#\tYES\n#_version_#\t0\n#_member_size_#\t+5\n#_on_corruption_#\tSkip\n#_return_defaults_when_missing_#\n");
        assert!(r.markers.strip);
        assert!(r.markers.fill_empty);
        assert_eq!(r.markers.version, 1);
        assert_eq!(r.markers.member_size, DEFAULT_MEMBER_SIZE);
        assert_eq!(r.markers.on_corruption, Policy::Skip);
        assert!(r.markers.return_missing);
        assert_eq!(w.len(), 3, "{w:?}");
    }

    #[test]
    fn defaults_are_decoded_not_stripped_and_rows_bind_where_they_appear() {
        let (r, rows, _) = run(TSVZ, false, b"#_defaults_#\td<sep>e \tf\na\t1\nb\t2\n#_defaults_#\tg\nc\t3\n");
        assert_eq!(*r.markers.defaults, vec!["g".to_string()]);
        assert_eq!(*rows.0[0].2.defaults, vec!["d\te ".to_string(), "f".to_string()]);
        assert!(Arc::ptr_eq(&rows.0[0].2, &rows.0[1].2));
        assert!(!Arc::ptr_eq(&rows.0[0].2, &rows.0[2].2));
    }

    #[test]
    fn comments_unknown_markers_and_the_header() {
        let (r, rows, _) = run(TSVZ, false, b"#name\tage\n#__ext__#\tx\n#_frobnicate_#\ty\n #k\tv\n#_defaults_# \tz\n");
        assert_eq!(r.header.as_deref(), Some("#name\tage"));
        assert_eq!(table(&rows), vec![row(" #k", &["v"])]);
        assert!(r.markers.defaults.is_empty(), "a marker key with a trailing space is a comment");
        let (m, _, _) = run(TSVZ, false, b"#_version_#\t1\n#c\n");
        assert_eq!(m.header, None);
    }

    #[test]
    fn zero_fill_under_each_policy() {
        let (_, rec, w) = run(TSVZ, false, b"a\t1\n\0\0b\t2\nc\t3\n");
        assert_eq!(table(&rec), vec![row("a", &["1"]), row("b", &["2"]), row("c", &["3"])]);
        assert_eq!(w.len(), 1);
        let (_, skip, _) = run(TSVZ, false, b"#_on_corruption_#\tskip\na\t1\n\0\0b\t2\nc\t3\n");
        assert_eq!(table(&skip), vec![row("a", &["1"]), row("c", &["3"])]);
        let (r, stop, _) = run(TSVZ, false, b"#_on_corruption_#\tstop\na\t1\n\0\0b\t2\nc\t3\n");
        assert_eq!(table(&stop), vec![row("a", &["1"])]);
        assert_eq!(r.stopped.as_ref().map(|d| (d.offset, d.kind)), Some((27, DamageKind::ZeroFill)));
        let (_, nsv, _) = run(Format { delimiter: 0, strict: true }, false, b"a\0x\n\0y\n");
        assert_eq!(table(&nsv), vec![row("a", &["x"])]);
    }

    #[test]
    fn invalid_utf8_becomes_replacement_characters() {
        let (_, rows, w) = run(TSVZ, false, b"a\t\xff\xfeok\xe2\x82\n");
        assert_eq!(table(&rows), vec![row("a", &["\u{fffd}\u{fffd}ok\u{fffd}"])]);
        assert_eq!(w.len(), 1);
    }

    #[test]
    fn verify_segments() {
        let seg = b"a\tx\n#_checksum_sha256_#\n";
        let crc = format!("{:08x}", crc32fast::hash(seg));
        let good = format!("#_checksum_crc32_#\tignored\n{}#_CHECKSUM_CRC32_#\t{}\n", std::str::from_utf8(seg).unwrap(), crc.to_uppercase());
        let (r, rows, _) = run(TSVZ, true, good.as_bytes());
        assert!(r.mismatches.is_empty(), "{:?}", r.mismatches);
        assert_eq!(table(&rows), vec![row("a", &["x"])]);
        let bad = b"#_checksum_crc32_#\na\tx\n#_checksum_crc32_#\tDEADBEEF\n#_checksum_crc32_#\t\n#_checksum_crc32_#\t11111111\n";
        let (r, _, _) = run(TSVZ, true, bad);
        assert_eq!(r.mismatches.len(), 2);
        assert_eq!((r.mismatches[0].line, r.mismatches[0].algo, r.mismatches[0].expected.as_str()), (3, "crc32", "deadbeef"));
        assert_eq!(r.mismatches[0].computed, format!("{:08x}", crc32fast::hash(b"a\tx\n")));
        assert_eq!(r.mismatches[1].line, 5);
        let (off, _, _) = run(TSVZ, false, bad);
        assert!(off.mismatches.is_empty());
    }

    #[test]
    fn loose_lines_are_split_raw() {
        let (r, rows, _) = run(Format { delimiter: b',', strict: false }, false, b"#h,dr\nk,x<lt>,y \n#_defaults_#,d\nb\nk2, \n");
        assert_eq!(r.header.as_deref(), Some("#h,dr"));
        assert!(r.markers.defaults.is_empty());
        assert_eq!(table(&rows), vec![row("k", &["x<lt>", "y "]), row("k2", &[" "])]);
    }

    #[test]
    fn keys_only_sinks_get_no_cells() {
        struct Keys(Vec<(String, usize)>);
        impl Sink for Keys {
            fn wants_cells(&self, _: &str) -> bool { false }
            fn put(&mut self, key: &str, cells: Cells, _: &Arc<Binding>) { self.0.push((key.into(), cells.len())); }
            fn delete(&mut self, _: &str) {}
        }
        let mut replay = Replay::new(TSVZ, false, None);
        replay.begin_part(Path::new("s.tsvz"), true);
        let mut keys = Keys(Vec::new());
        Framer::new().push(b"a\tx\ty\n", &mut |l| replay.line(l, &mut keys));
        assert_eq!(keys.0, vec![("a".to_string(), 0)]);
    }

    #[test]
    fn corrupt_member_policy() {
        let mut r = Replay::new(TSVZ, false, None);
        r.begin_part(Path::new("s.tsvz.gz"), true);
        assert_eq!(r.corrupt(10, 4), Policy::Recover);
        assert!(r.damaged && r.stopped.is_none());
        r.markers.on_corruption = Policy::Stop;
        assert_eq!(r.corrupt(99, 0), Policy::Stop);
        assert_eq!(r.stopped.as_ref().map(|d| (d.offset, d.kind)), Some((99, DamageKind::CorruptMember)));
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib replay`
Expected: compile errors: `Replay`, `Sink`, `Cells`, `Binding`, … not found.

- [ ] **Step 3: Implement**

Add above the tests in `src/replay.rs`:

```rust
use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::digest::{Accumulator, Algo};
use crate::escape::{self, is_marker_key};
use crate::frame::Line;
use crate::name::Format;
use crate::{WarnFn, Warning};

/// `#_member_size_#`'s built-in default (§16.7).
pub const DEFAULT_MEMBER_SIZE: u64 = 1 << 20;

/// What a reader does with damaged data (§4.10).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Policy {
    Recover,
    Skip,
    Stop,
}

impl Policy {
    pub fn name(self) -> &'static str {
        match self {
            Policy::Recover => "recover",
            Policy::Skip => "skip",
            Policy::Stop => "stop",
        }
    }
}

/// The marker state of §12.4, the checksum markers aside.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Markers {
    pub version: u64,
    /// The defaults of value columns 1, 2, … at index 0, 1, … (§14.1).
    pub defaults: Arc<Vec<String>>,
    pub strip: bool,
    pub fill_empty: bool,
    pub return_missing: bool,
    pub on_corruption: Policy,
    pub rotate: &'static str,
    pub write_ack: &'static str,
    pub member_size: u64,
}

impl Default for Markers {
    fn default() -> Self {
        Markers {
            version: 1,
            defaults: Arc::new(Vec::new()),
            strip: true,
            fill_empty: false,
            return_missing: true,
            on_corruption: Policy::Recover,
            rotate: "keep",
            write_ack: "memory",
            member_size: DEFAULT_MEMBER_SIZE,
        }
    }
}

/// The defaults and fill-empty setting in force at a row's position (§12.4.4, §14.4).
#[derive(Debug, PartialEq, Eq)]
pub struct Binding {
    pub defaults: Arc<Vec<String>>,
    pub fill_empty: bool,
}

/// A row's decoded value cells, in one buffer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Cells {
    text: String,
    ends: Vec<usize>,
}

/// No cells: what a missing key's row resolves from (§14.5).
pub static EMPTY_CELLS: Cells = Cells { text: String::new(), ends: Vec::new() };

impl Cells {
    pub fn push(&mut self, cell: &str) {
        self.text.push_str(cell);
        self.ends.push(self.text.len());
    }

    pub fn len(&self) -> usize {
        self.ends.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ends.is_empty()
    }

    /// Cell `j` (value column `j + 1`).
    pub fn get(&self, j: usize) -> &str {
        let start = if j == 0 { 0 } else { self.ends[j - 1] };
        &self.text[start..self.ends[j]]
    }

    pub fn iter(&self) -> impl Iterator<Item = &str> + '_ {
        (0..self.len()).map(move |j| self.get(j))
    }

    pub fn from_values<S: AsRef<str>>(values: impl IntoIterator<Item = S>) -> Cells {
        let mut cells = Cells::default();
        for v in values {
            cells.push(v.as_ref());
        }
        cells
    }
}

/// A segment whose digest does not match its closing marker (§15.3, §20.2 `verify`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mismatch {
    pub part: PathBuf,
    /// The 1-based line, within `part`, of the marker that closes the segment.
    pub line: u64,
    pub algo: &'static str,
    pub expected: String,
    pub computed: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DamageKind {
    ZeroFill,
    CorruptMember,
}

/// Where a `stop` policy ended the read (§4.10).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Damage {
    pub part: PathBuf,
    pub offset: u64,
    pub kind: DamageKind,
}

/// Where replayed rows go.
pub trait Sink {
    /// Whether the cells of `key`'s rows are needed; when false they are not decoded.
    fn wants_cells(&self, _key: &str) -> bool {
        true
    }
    fn put(&mut self, key: &str, cells: Cells, binding: &Arc<Binding>);
    fn delete(&mut self, key: &str);
}

/// The §7 pipeline over the committed lines of a store's parts, in order.
pub struct Replay {
    format: Format,
    pub markers: Markers,
    binding: Option<Arc<Binding>>,
    verify: bool,
    armed: Vec<(Algo, Accumulator)>,
    pub mismatches: Vec<Mismatch>,
    pub stopped: Option<Damage>,
    /// The read met zero fill, or a member that was cut short or corrupt.
    pub damaged: bool,
    /// The first committed line of the first part, when it is a comment (§19.2.3c).
    pub header: Option<String>,
    first_line: bool,
    part: PathBuf,
    warn: Option<WarnFn>,
}

fn split_first(text: &str, delimiter: u8) -> (&str, Option<&str>) {
    match memchr::memchr(delimiter, text.as_bytes()) {
        Some(i) => (&text[..i], Some(&text[i + 1..])),
        None => (text, None),
    }
}

fn trim_ws(s: &str) -> &str {
    s.trim_end_matches([' ', '\t'])
}

/// Set a boolean marker from its lowercased value; false when the value is invalid (§12.4.1).
fn set_bool(slot: &mut bool, default: bool, lower: &str) -> bool {
    *slot = match lower {
        "" => default,
        "true" | "yes" | "on" | "1" => true,
        "false" | "no" | "off" | "0" => false,
        _ => return false,
    };
    true
}

/// An integer marker value: ASCII digits only, at least 1 (§12.4.1).
fn positive(value: &str) -> Option<u64> {
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    value.parse::<u64>().ok().filter(|&n| n >= 1)
}

impl Replay {
    pub fn new(format: Format, verify: bool, warn: Option<WarnFn>) -> Replay {
        Replay {
            format,
            markers: Markers::default(),
            binding: None,
            verify,
            armed: Vec::new(),
            mismatches: Vec::new(),
            stopped: None,
            damaged: false,
            header: None,
            first_line: false,
            part: PathBuf::new(),
            warn,
        }
    }

    pub fn format(&self) -> Format {
        self.format
    }

    /// Start a part. `first` marks the store's first part, whose first line may be its header.
    pub fn begin_part(&mut self, path: &Path, first: bool) {
        self.part = path.to_path_buf();
        self.first_line = first;
    }

    fn warn(&self, offset: Option<u64>, message: String) {
        if let Some(w) = &self.warn {
            w(&Warning { part: self.part.clone(), offset, message });
        }
    }

    /// The binding a row takes at the current position.
    pub fn binding(&mut self) -> Arc<Binding> {
        let markers = &self.markers;
        self.binding
            .get_or_insert_with(|| Arc::new(Binding { defaults: markers.defaults.clone(), fill_empty: markers.fill_empty }))
            .clone()
    }

    /// Apply one committed line (§7). Returns false when a `stop` policy ended the read.
    pub fn line(&mut self, line: &Line<'_>, sink: &mut dyn Sink) -> bool {
        let first = std::mem::replace(&mut self.first_line, false);
        let mut body = line.text();
        if self.format.strict && self.format.delimiter != 0 {
            if let Some(z) = memchr::memrchr(0, body) {
                self.damaged = true;
                let n = line.number;
                match self.markers.on_corruption {
                    Policy::Stop => {
                        self.stopped = Some(Damage { part: self.part.clone(), offset: line.offset, kind: DamageKind::ZeroFill });
                        self.warn(Some(line.offset), format!("line {n} holds zero bytes (zero-fill damage); reading stopped there (#_on_corruption_# stop)"));
                        return false;
                    }
                    Policy::Skip => {
                        self.feed(line.raw, None); // digests cover the line as written (§4.9)
                        self.warn(Some(line.offset), format!("line {n} holds zero bytes (zero-fill damage); skipped it"));
                        return true;
                    }
                    Policy::Recover => {
                        self.warn(Some(line.offset), format!("line {n} holds zero bytes (zero-fill damage); read only what follows the last one"));
                        body = &body[z + 1..];
                    }
                }
            }
        }
        let text = String::from_utf8_lossy(body);
        if matches!(text, Cow::Owned(_)) {
            self.warn(Some(line.offset), format!("line {} is not valid UTF-8; replaced the bad bytes with U+FFFD", line.number));
        }
        if self.format.strict {
            self.strict_line(&text, line, first, sink);
        } else {
            self.loose_line(&text, first, sink);
        }
        true
    }

    /// Feed a line's raw bytes to every armed digest but `except` (§7.4).
    fn feed(&mut self, raw: &[u8], except: Option<Algo>) {
        for (algo, acc) in &mut self.armed {
            if Some(*algo) != except {
                acc.update(raw);
            }
        }
    }

    fn strict_line(&mut self, text: &str, line: &Line<'_>, first: bool, sink: &mut dyn Sink) {
        let (key_raw, rest) = split_first(text, self.format.delimiter);
        if key_raw.starts_with('#') {
            if is_marker_key(key_raw) {
                let lower = key_raw.to_ascii_lowercase();
                if let Some(name) = lower.strip_prefix("#_checksum_").and_then(|n| n.strip_suffix("_#")) {
                    let algo = Algo::from_name(name);
                    self.feed(line.raw, algo);
                    if let (Some(algo), true) = (algo, self.verify) {
                        self.checksum(algo, rest, line.number);
                    }
                    return;
                }
                self.feed(line.raw, None);
                self.marker(&lower, rest, line);
                return;
            }
            self.feed(line.raw, None);
            if first {
                self.header = Some(text.to_string());
            }
            return;
        }
        self.feed(line.raw, None);
        self.data(key_raw, rest, sink);
    }

    fn checksum(&mut self, algo: Algo, rest: Option<&str>, number: u64) {
        let d = self.format.delimiter;
        let value = rest.map(|r| escape::decode(split_first(r, d).0, d).into_owned()).unwrap_or_default();
        match self.armed.iter().position(|(a, _)| *a == algo) {
            None => self.armed.push((algo, Accumulator::new(algo))), // the first marker arms; its value is ignored (§15.3)
            Some(i) => {
                let computed = self.armed[i].1.hex();
                if !value.is_empty() && !value.eq_ignore_ascii_case(&computed) {
                    self.mismatches.push(Mismatch {
                        part: self.part.clone(),
                        line: number,
                        algo: algo.name(),
                        expected: value.to_ascii_lowercase(),
                        computed,
                    });
                }
                self.armed[i].1 = Accumulator::new(algo);
            }
        }
    }

    fn marker(&mut self, key: &str, rest: Option<&str>, line: &Line<'_>) {
        let d = self.format.delimiter;
        let mut fields = rest.into_iter().flat_map(move |r| r.split(d as char));
        if key == "#_defaults_#" {
            self.markers.defaults = Arc::new(fields.map(|f| escape::decode(f, d).into_owned()).collect());
            self.binding = None;
            return;
        }
        let value = fields.next().map(|f| escape::decode(f, d)).unwrap_or(Cow::Borrowed(""));
        let value: &str = &value;
        let lower = value.to_ascii_lowercase();
        let m = &mut self.markers;
        let valid = match key {
            "#_version_#" => match (value, positive(value)) {
                ("", _) => {
                    m.version = 1;
                    true
                }
                (_, Some(n)) => {
                    m.version = n;
                    true
                }
                _ => false,
            },
            "#_strip_trailing_whites_#" => set_bool(&mut m.strip, true, &lower),
            "#_fill_empty_with_default_#" => {
                let ok = set_bool(&mut m.fill_empty, false, &lower);
                self.binding = None;
                ok
            }
            "#_return_defaults_when_missing_#" => set_bool(&mut m.return_missing, true, &lower),
            "#_on_corruption_#" => {
                m.on_corruption = match lower.as_str() {
                    "" | "recover" => Policy::Recover,
                    "skip" => Policy::Skip,
                    "stop" => Policy::Stop,
                    _ => m.on_corruption,
                };
                matches!(lower.as_str(), "" | "recover" | "skip" | "stop")
            }
            "#_rotate_#" => {
                m.rotate = match lower.as_str() {
                    "" | "keep" => "keep",
                    "rename" => "rename",
                    "delete" => "delete",
                    _ => m.rotate,
                };
                matches!(lower.as_str(), "" | "keep" | "rename" | "delete")
            }
            "#_write_ack_#" => {
                m.write_ack = match lower.as_str() {
                    "" | "memory" => "memory",
                    "disk" => "disk",
                    _ => m.write_ack,
                };
                matches!(lower.as_str(), "" | "memory" | "disk")
            }
            "#_member_size_#" => match (value, positive(value)) {
                ("", _) => {
                    m.member_size = DEFAULT_MEMBER_SIZE;
                    true
                }
                (_, Some(n)) => {
                    m.member_size = n;
                    true
                }
                _ => false,
            },
            _ => true, // a marker version 1 does not define: no effect (§12.3)
        };
        if !valid {
            self.warn(Some(line.offset), format!("line {}: {} has an invalid value {:?}; ignored", line.number, key, value));
        }
    }

    fn data(&mut self, key_raw: &str, rest: Option<&str>, sink: &mut dyn Sink) {
        let d = self.format.delimiter;
        let strip = self.markers.strip;
        let key = escape::decode(if strip { trim_ws(key_raw) } else { key_raw }, d);
        if key.is_empty() {
            return; // §7.7, §8.4
        }
        let Some(rest) = rest else {
            sink.delete(&key); // a tombstone (§7.6, §7.8)
            return;
        };
        let binding = self.binding();
        let mut cells = Cells::default();
        if sink.wants_cells(&key) {
            for field in rest.split(d as char) {
                cells.push(&escape::decode(if strip { trim_ws(field) } else { field }, d));
            }
        }
        sink.put(&key, cells, &binding);
    }

    /// A loose file: lines split on the delimiter, no markers, escaping or stripping (§5.2).
    fn loose_line(&mut self, text: &str, first: bool, sink: &mut dyn Sink) {
        if text.starts_with('#') {
            if first {
                self.header = Some(text.to_string());
            }
            return;
        }
        let (key, rest) = split_first(text, self.format.delimiter);
        if key.is_empty() {
            return;
        }
        let Some(rest) = rest else {
            sink.delete(key);
            return;
        };
        let binding = self.binding();
        let mut cells = Cells::default();
        if sink.wants_cells(key) {
            for field in rest.split(self.format.delimiter as char) {
                cells.push(field);
            }
        }
        sink.put(key, cells, &binding);
    }

    /// A member was cut short (§16.6): its whole lines are read.
    pub fn cut_short(&mut self, offset: usize) {
        self.damaged = true;
        self.warn(Some(offset as u64), format!("the member at byte {offset} is cut short; its whole lines are read"));
    }

    /// A corrupt member (§16.6): what the policy in force says to do with its output (§4.10).
    pub fn corrupt(&mut self, offset: usize, decoded: usize) -> Policy {
        self.damaged = true;
        let policy = self.markers.on_corruption;
        let message = match policy {
            Policy::Recover => format!("used {decoded} bytes the corrupt member at byte {offset} decoded before it failed; they are not verified"),
            Policy::Skip => format!("skipped the corrupt member at byte {offset}"),
            Policy::Stop => {
                self.stopped = Some(Damage { part: self.part.clone(), offset: offset as u64, kind: DamageKind::CorruptMember });
                format!("the member at byte {offset} is corrupt; reading stopped there (#_on_corruption_# stop)")
            }
        };
        self.warn(Some(offset as u64), message);
        policy
    }

    /// Bytes after a part's last newline were ignored (§4.3).
    pub fn tail(&mut self, bytes: usize) {
        if bytes > 0 {
            self.warn(None, format!("ignored {bytes} uncommitted bytes after the last newline"));
        }
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib replay`
Expected: 11 passed.

- [ ] **Step 5: Commit**

```bash
git add src/replay.rs src/lib.rs
git commit -m "Replay: the §7 pipeline — markers, digests, zero fill, tombstones, stripping, bindings, loose files." -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 9: The store

**Files:**
- Create: `src/store.rs`
- Modify: `src/lib.rs` (add `pub mod store;` and `pub use store::{Options, PartInfo, Row, Scope, Store};`)

**Interfaces:**
- Consumes: `parts::{list, Stamp}`, `codec::{items, Item}`, `frame::Framer`, `replay::*`, `name::{format_of, Format, Codec}`, `Error`, `WarnFn`.
- Produces:
  - `store::Scope` (`All` (default), `KeysOnly`, `Keys(Vec<String>)`).
  - `store::Options { scope: Scope, verify: bool, delimiter: Option<u8>, warnings: Option<WarnFn> }` (`Clone`, `Default`). `delimiter` applies to loose files only.
  - `store::PartInfo { index: usize, ordinal: Option<String>, path: PathBuf, codec: Option<Codec>, active: bool }`.
  - `store::Row<'a>` (`Copy`): `len()`, `is_empty()`, `cell(j) -> &'a str`, `cells(self) -> impl Iterator<Item = &'a str>`, `to_vec()`, `written()`.
  - `store::Store`: `open(path, Options) -> Result<Store, Error>`, `path()`, `format()`, `len()`, `is_empty()`, `get(&str) -> Option<Row>`, `lookup_key<'k>(&self, &'k str) -> &'k str`, `missing() -> Option<Row>`, `iter() -> impl Iterator<Item = (&str, Row)>`, `first()`, `last()`, `markers() -> &Markers`, `stopped() -> Option<&Damage>`, `damaged() -> bool`, `mismatches() -> &[Mismatch]`, `header() -> Option<&str>`, `parts() -> &[PartInfo]`, `options() -> &Options`.
  - (Used by Task 16) private `Follow` state recorded during `load`.

- [ ] **Step 1: Write the failing tests**

`src/store.rs`:

```rust
//! A store: the ordered map of live rows after replaying its parts (§3, §14, §17).

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(dir: &Path, name: &str, data: &[u8]) -> PathBuf {
        let p = dir.join(name);
        fs::write(&p, data).unwrap();
        p
    }

    fn rows(store: &Store) -> Vec<Vec<String>> {
        store.iter().map(|(k, r)| std::iter::once(k.to_string()).chain(r.to_vec()).collect()).collect()
    }

    fn v(cells: &[&str]) -> Vec<String> {
        cells.iter().map(|c| c.to_string()).collect()
    }

    #[test]
    fn order_last_wins_and_rewrite_after_delete_moves_to_the_end() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(dir.path(), "s.tsvz", b"a\t1\nb\t2\na\nc\t3\na\t4\nb\t5\n");
        let s = Store::open(&p, Options::default()).unwrap();
        assert_eq!(rows(&s), vec![v(&["b", "5"]), v(&["c", "3"]), v(&["a", "4"])]);
        assert_eq!(s.len(), 3);
        assert_eq!(s.first().unwrap().0, "b");
        assert_eq!(s.last().unwrap().0, "a");
    }

    #[test]
    fn defaults_resolve_per_binding_and_missing_keys() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(dir.path(), "s.tsvz", b"#_defaults_#\tguest\t0\nalice\tAlice\t30\nbob\tBob\ncarol\t\t25\n#_defaults_#\tnew\nbob\tBob\n");
        let s = Store::open(&p, Options::default()).unwrap();
        assert_eq!(s.get("carol").unwrap().to_vec(), v(&["", "25"]));
        assert_eq!(s.get("bob").unwrap().to_vec(), v(&["Bob"]));
        assert_eq!(s.missing().unwrap().to_vec(), v(&["new"]));
        assert!(s.get("dave").is_none());
        let q = write(dir.path(), "q.tsvz", b"#_fill_empty_with_default_#\ttrue\n#_defaults_#\td1\td2\na\t\tq\n#_return_defaults_when_missing_#\tfalse\n");
        let s = Store::open(&q, Options::default()).unwrap();
        assert_eq!(s.get("a").unwrap().to_vec(), v(&["d1", "q"]));
        assert!(s.missing().is_none());
    }

    #[test]
    fn multipart_markers_span_parts_and_lookup_key_strips() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "s.tsvz", b"#_defaults_#\td\nk\tfrom-0\n");
        write(dir.path(), "s.tsvz.10", b"k\tfrom-10\n");
        write(dir.path(), "s.tsvz.f", b"k\tfrom-f\nz\n");
        let s = Store::open(dir.path().join("s.tsvz"), Options::default()).unwrap();
        assert_eq!(rows(&s), vec![v(&["k", "from-10"])]);
        assert_eq!(s.parts().iter().map(|p| (p.index, p.active)).collect::<Vec<_>>(), vec![(0, false), (1, false), (2, true)]);
        assert_eq!(s.lookup_key("k \t"), "k");
    }

    #[test]
    fn compressed_parts_and_the_corruption_policy() {
        let dir = tempfile::tempdir().unwrap();
        let mut bad = crate::codec::encode_member(crate::name::Codec::Gzip, b"b\t2\nc\t3\n").unwrap();
        let n = bad.len();
        bad[n - 8] ^= 1; // CRC mismatch: a corrupt member whose output is still decoded
        for (policy, want) in [("recover", vec!["a", "b", "c", "d"]), ("skip", vec!["a", "d"]), ("stop", vec!["a"])] {
            let mut part = crate::codec::encode_member(crate::name::Codec::Gzip, format!("#_on_corruption_#\t{policy}\na\t1\n").as_bytes()).unwrap();
            part.extend(&bad);
            part.extend(crate::codec::encode_member(crate::name::Codec::Gzip, b"d\t4\n").unwrap());
            let p = write(dir.path(), &format!("{policy}.tsvz.gz"), &part);
            let s = Store::open(&p, Options::default()).unwrap();
            assert_eq!(s.iter().map(|(k, _)| k).collect::<Vec<_>>(), want, "{policy}");
            assert_eq!(s.stopped().is_some(), policy == "stop");
            assert!(s.damaged());
        }
    }

    #[test]
    fn scopes() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(dir.path(), "s.tsvz", b"a\tx\ty\nb\tz\nc\tw\n");
        let keys = Store::open(&p, Options { scope: Scope::KeysOnly, ..Options::default() }).unwrap();
        assert_eq!(keys.iter().map(|(k, r)| (k, r.written())).collect::<Vec<_>>(), vec![("a", 0), ("b", 0), ("c", 0)]);
        let some = Store::open(&p, Options { scope: Scope::Keys(vec!["b \t".into(), "zz".into()]), ..Options::default() }).unwrap();
        assert_eq!(some.len(), 1);
        assert_eq!(some.get(some.lookup_key("b \t")).unwrap().to_vec(), v(&["z"]));
    }

    #[test]
    fn verify_reports_mismatches_with_their_part() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "s.tsvz", b"#_checksum_crc32_#\na\tx\n");
        write(dir.path(), "s.tsvz.1", b"b\ty\n#_checksum_crc32_#\t00000000\n");
        let s = Store::open(dir.path().join("s.tsvz"), Options { verify: true, ..Options::default() }).unwrap();
        let m = &s.mismatches()[0];
        assert_eq!((m.part.clone(), m.line, m.computed.as_str()), (dir.path().join("s.tsvz.1"), 2, "c1ec8234"));
    }

    #[test]
    fn header_comment() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(dir.path(), "s.tsvz", b"#key\tval\na\tx\n");
        assert_eq!(Store::open(&p, Options::default()).unwrap().header(), Some("#key\tval"));
    }

    #[test]
    fn degenerate_parts_are_empty_stores() {
        let dir = tempfile::tempdir().unwrap();
        for (name, data) in [
            ("gz.tsvz.gz", &b""[..]),
            ("bom.tsvz", b"\xef\xbb\xbf"),
            ("crlf.tsvz", b"\r\n"),
            ("zeros.tsvz", b"\0\0\0\0"),
            ("zeroline.tsvz", b"\0\0\n"),
            ("tail.tsvz", b"no newline"),
            ("empty.tsvz", b""),
        ] {
            let p = write(dir.path(), name, data);
            let s = Store::open(&p, Options::default()).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(s.len(), 0, "{name}");
            assert!(s.stopped().is_none(), "{name}");
        }
    }

    #[test]
    fn missing_and_bad_paths() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(Store::open(dir.path().join("none.tsvz"), Options::default()), Err(Error::Missing(_))));
        assert!(matches!(Store::open(dir.path().join("x.txt"), Options::default()), Err(Error::BadPath(_))));
    }

    #[test]
    fn loose_delimiter_override() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(dir.path(), "s.tsv", b"a;x\n");
        let s = Store::open(&p, Options { delimiter: Some(b';'), ..Options::default() }).unwrap();
        assert_eq!(rows(&s), vec![v(&["a", "x"])]);
        let strict = write(dir.path(), "s.tsvz", b"a;x\tb\n");
        let s = Store::open(&strict, Options { delimiter: Some(b';'), ..Options::default() }).unwrap();
        assert_eq!(rows(&s), vec![v(&["a;x", "b"])]);
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib store`
Expected: compile errors: `Store`, `Options`, `Scope` not found.

- [ ] **Step 3: Implement**

Add above the tests in `src/store.rs`:

```rust
use std::collections::HashSet;
use std::fs::File;
use std::io::Read;
use std::os::unix::fs::{FileExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use hashbrown::HashMap;

use crate::codec::{self, Item};
use crate::frame::Framer;
use crate::name::{self, Codec, Format};
use crate::parts::{self, Stamp};
use crate::replay::{Binding, Cells, Damage, Markers, Mismatch, Policy, Replay, Sink, EMPTY_CELLS};
use crate::{Error, WarnFn};

/// What a store keeps of each row.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Scope {
    /// Every row with its cells.
    #[default]
    All,
    /// Keys and their order, no cells (`len`, `keys`, `has`).
    KeysOnly,
    /// Only these keys (as requested, and with trailing space and tab removed), with cells (`get`).
    Keys(Vec<String>),
}

/// How to open a store.
#[derive(Clone, Default)]
pub struct Options {
    pub scope: Scope,
    /// Compute digests and record mismatches (§15); off for plain reads.
    pub verify: bool,
    /// The delimiter of a loose file (`-d`); ignored for strict variants (§5.1).
    pub delimiter: Option<u8>,
    pub warnings: Option<WarnFn>,
}

/// One part as `parts` lists it (§20.2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PartInfo {
    pub index: usize,
    pub ordinal: Option<String>,
    pub path: PathBuf,
    pub codec: Option<Codec>,
    pub active: bool,
}

/// A live key's resolved row (§14.2, §14.3), or a missing key's defaults (§14.5).
#[derive(Clone, Copy)]
pub struct Row<'a> {
    cells: &'a Cells,
    defaults: &'a [String],
    fill: bool,
}

impl<'a> Row<'a> {
    pub fn len(&self) -> usize {
        self.cells.len().max(self.defaults.len())
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Value column `j + 1`, resolved.
    pub fn cell(&self, j: usize) -> &'a str {
        if j < self.cells.len() {
            let c = self.cells.get(j);
            if c.is_empty() && self.fill {
                if let Some(d) = self.defaults.get(j) {
                    return d;
                }
            }
            c
        } else {
            self.defaults.get(j).map_or("", |d| d.as_str())
        }
    }

    pub fn cells(self) -> impl Iterator<Item = &'a str> {
        (0..self.len()).map(move |j| self.cell(j))
    }

    pub fn to_vec(&self) -> Vec<String> {
        self.cells().map(str::to_string).collect()
    }

    /// How many cells the row was written with (0 for a missing key, or under `Scope::KeysOnly`).
    pub fn written(&self) -> usize {
        self.cells.len()
    }
}

struct Slot {
    key: Box<str>,
    cells: Cells,
    binding: Arc<Binding>,
}

fn row_of(slot: &Slot) -> Row<'_> {
    Row { cells: &slot.cells, defaults: &slot.binding.defaults, fill: slot.binding.fill_empty }
}

/// Live rows in first-appearance order (§3.4): slots plus an index; freed slots are compacted.
#[derive(Default)]
struct Table {
    slots: Vec<Option<Slot>>,
    index: HashMap<Box<str>, usize>,
    live: usize,
    cells: bool,
    wanted: Option<HashSet<String>>,
}

impl Table {
    fn new(scope: &Scope) -> Table {
        let wanted = match scope {
            Scope::Keys(keys) => Some(keys.iter().flat_map(|k| [k.clone(), k.trim_end_matches([' ', '\t']).to_string()]).collect()),
            _ => None,
        };
        Table { cells: !matches!(scope, Scope::KeysOnly), wanted, ..Table::default() }
    }

    fn compact(&mut self) {
        if self.slots.len() < 1024 || self.slots.len() - self.live <= self.live {
            return;
        }
        self.slots.retain(Option::is_some);
        for (i, slot) in self.slots.iter().enumerate() {
            if let Some(slot) = slot {
                if let Some(at) = self.index.get_mut(&*slot.key) {
                    *at = i;
                }
            }
        }
    }
}

impl Sink for Table {
    fn wants_cells(&self, key: &str) -> bool {
        self.cells && self.wanted.as_ref().is_none_or(|w| w.contains(key))
    }

    fn put(&mut self, key: &str, cells: Cells, binding: &Arc<Binding>) {
        if let Some(w) = &self.wanted {
            if !w.contains(key) {
                return;
            }
        }
        match self.index.get(key) {
            Some(&i) => {
                if let Some(slot) = self.slots[i].as_mut() {
                    slot.cells = cells;
                    slot.binding = binding.clone();
                }
            }
            None => {
                self.index.insert(key.into(), self.slots.len());
                self.slots.push(Some(Slot { key: key.into(), cells, binding: binding.clone() }));
                self.live += 1;
            }
        }
    }

    fn delete(&mut self, key: &str) {
        if let Some(i) = self.index.remove(key) {
            self.slots[i] = None;
            self.live -= 1;
            self.compact();
        }
    }
}

/// How many bytes before the last part's read offset are kept to detect an in-place rewrite.
pub(crate) const CHECK_BYTES: u64 = 4096;
/// A directory modified this recently may change again within one timestamp tick.
pub(crate) const RACY_NS: i128 = 2_000_000_000;

/// What `refresh` (Task 16) needs to follow the files.
#[derive(Clone, Debug, Default)]
#[allow(dead_code)] // the directory fields are read by `refresh` (Task 16), which removes this line
pub(crate) struct Follow {
    pub dir_mtime: i128,
    pub dir_racy: bool,
    pub stamps: Vec<Option<Stamp>>,
    /// Offset just after the last committed line of the last part (plain parts only).
    pub offset: u64,
    pub line: u64,
    pub tail: Vec<u8>,
    /// Up to CHECK_BYTES bytes just before `offset`.
    pub check: Vec<u8>,
}

pub(crate) fn dir_mtime(store: &Path) -> i128 {
    let dir = store.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    std::fs::metadata(dir).map_or(0, |m| m.mtime() as i128 * 1_000_000_000 + m.mtime_nsec() as i128)
}

pub(crate) fn racy(mtime: i128) -> bool {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos() as i128);
    now - mtime < RACY_NS
}

fn read_check(file: &File, offset: u64) -> Vec<u8> {
    let start = offset.saturating_sub(CHECK_BYTES);
    let mut buf = vec![0u8; (offset - start) as usize];
    match file.read_exact_at(&mut buf, start) {
        Ok(()) => buf,
        Err(_) => Vec::new(),
    }
}

/// A store read from its files.
pub struct Store {
    path: PathBuf,
    format: Format,
    options: Options,
    table: Table,
    replay: Replay,
    parts: Vec<PartInfo>,
    pub(crate) follow: Follow,
}

impl Store {
    /// Read the store at `path` (the unnumbered file's path, §20.1.3).
    pub fn open(path: impl AsRef<Path>, options: Options) -> Result<Store, Error> {
        let path = path.as_ref().to_path_buf();
        let mut format = name::format_of(&path).ok_or_else(|| Error::BadPath(path.clone()))?;
        if let (false, Some(d)) = (format.strict, options.delimiter) {
            format.delimiter = d;
        }
        let mut store = Store {
            replay: Replay::new(format, options.verify, options.warnings.clone()),
            table: Table::new(&options.scope),
            path,
            format,
            options,
            parts: Vec::new(),
            follow: Follow::default(),
        };
        store.load()?;
        Ok(store)
    }

    fn load(&mut self) -> Result<(), Error> {
        let list = parts::list(&self.path)?;
        if list.is_empty() {
            return Err(Error::Missing(self.path.clone()));
        }
        if let Some(part) = list.iter().find(|p| p.codec.is_some_and(|c| !c.supported())) {
            return Err(Error::UnsupportedCodec { path: part.path.clone(), codec: part.codec.map_or("", Codec::name) });
        }
        let dir = dir_mtime(&self.path);
        self.follow = Follow { dir_mtime: dir, dir_racy: racy(dir), ..Follow::default() };
        let last = list.len() - 1;
        for (i, part) in list.iter().enumerate() {
            self.replay.begin_part(&part.path, i == 0);
            let mut file = File::open(&part.path).map_err(|e| Error::io(&part.path, e))?;
            let meta = file.metadata().map_err(|e| Error::io(&part.path, e))?;
            if meta.is_dir() {
                return Err(Error::io(&part.path, std::io::Error::from(std::io::ErrorKind::IsADirectory)));
            }
            self.follow.stamps.push(Some(Stamp::of(&meta)));
            let (replay, table) = (&mut self.replay, &mut self.table);
            let mut framer = Framer::new();
            match part.codec {
                Some(codec) => {
                    let mut data = Vec::new();
                    file.read_to_end(&mut data).map_err(|e| Error::io(&part.path, e))?;
                    for item in codec::items(codec, &data) {
                        let go = match item {
                            Item::Data(out) => framer.push(&out, &mut |l| replay.line(l, table)),
                            Item::CutShort { offset, out } => {
                                replay.cut_short(offset);
                                framer.push(&out, &mut |l| replay.line(l, table))
                            }
                            Item::Corrupt { offset, out } => match replay.corrupt(offset, out.len()) {
                                Policy::Recover => framer.push(&out, &mut |l| replay.line(l, table)),
                                Policy::Skip => true,
                                Policy::Stop => false,
                            },
                            Item::Break => {
                                framer.brk();
                                true
                            }
                        };
                        if !go {
                            break;
                        }
                    }
                }
                None => {
                    let mut buf = vec![0u8; 1 << 20];
                    loop {
                        let n = match file.read(&mut buf) {
                            Ok(0) => break,
                            Ok(n) => n,
                            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                            Err(e) => return Err(Error::io(&part.path, e)),
                        };
                        if !framer.push(&buf[..n], &mut |l| replay.line(l, table)) {
                            break;
                        }
                    }
                }
            }
            if self.replay.stopped.is_some() {
                break; // a stop policy ends the read of the whole store (§4.10)
            }
            self.replay.tail(framer.tail().len());
            if i == last && part.codec.is_none() {
                self.follow.offset = framer.committed();
                self.follow.line = framer.number();
                self.follow.tail = framer.tail().to_vec();
                self.follow.check = read_check(&file, framer.committed());
            }
        }
        self.parts = list
            .into_iter()
            .enumerate()
            .map(|(i, p)| PartInfo { index: i, ordinal: p.ordinal, path: p.path, codec: p.codec, active: i == last })
            .collect();
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn format(&self) -> Format {
        self.format
    }

    pub fn options(&self) -> &Options {
        &self.options
    }

    pub fn len(&self) -> usize {
        self.table.live
    }

    pub fn is_empty(&self) -> bool {
        self.table.live == 0
    }

    pub fn markers(&self) -> &Markers {
        &self.replay.markers
    }

    /// A requested key as §7.7 resolves a key: trailing space and tab removed while the final
    /// stripping is on (§20.2 `get`).
    pub fn lookup_key<'k>(&self, requested: &'k str) -> &'k str {
        if self.format.strict && self.replay.markers.strip {
            requested.trim_end_matches([' ', '\t'])
        } else {
            requested
        }
    }

    pub fn get(&self, key: &str) -> Option<Row<'_>> {
        let i = *self.table.index.get(key)?;
        self.table.slots[i].as_ref().map(row_of)
    }

    /// What a read of a missing key returns (§14.5): the current defaults, or None (not-found).
    pub fn missing(&self) -> Option<Row<'_>> {
        if self.format.strict && !self.replay.markers.return_missing {
            return None;
        }
        Some(Row { cells: &EMPTY_CELLS, defaults: &self.replay.markers.defaults, fill: false })
    }

    /// Live keys and rows in first-appearance order (§3.4).
    pub fn iter(&self) -> impl Iterator<Item = (&str, Row<'_>)> {
        self.table.slots.iter().flatten().map(|s| (&*s.key, row_of(s)))
    }

    pub fn first(&self) -> Option<(&str, Row<'_>)> {
        self.iter().next()
    }

    pub fn last(&self) -> Option<(&str, Row<'_>)> {
        self.table.slots.iter().rev().flatten().next().map(|s| (&*s.key, row_of(s)))
    }

    pub fn stopped(&self) -> Option<&Damage> {
        self.replay.stopped.as_ref()
    }

    pub fn damaged(&self) -> bool {
        self.replay.damaged
    }

    pub fn mismatches(&self) -> &[Mismatch] {
        &self.replay.mismatches
    }

    pub fn header(&self) -> Option<&str> {
        self.replay.header.as_deref()
    }

    pub fn parts(&self) -> &[PartInfo] {
        &self.parts
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib store`
Expected: 10 passed.

- [ ] **Step 5: Run every library test and clippy**

Run: `cargo test --lib && cargo clippy --lib --all-targets -- -D warnings`
Expected: all pass, no warnings.

- [ ] **Step 6: Commit**

```bash
git add src/store.rs src/lib.rs
git commit -m "Store: replay a store's parts into an ordered map of live rows, with scopes and missing-key reads." -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---
### Task 10: The command line — arguments and the reading operations

**Files:**
- Modify: `Cargo.toml` (add the `[[bin]]` section), `src/lib.rs` (add `#[cfg(feature = "cli")] pub mod cli;`)
- Create: `src/main.rs`, `src/cli/mod.rs`, `src/cli/args.rs`, `src/cli/output.rs`, `src/cli/read.rs`, `tests/cli.rs`

**Interfaces:**
- Consumes: `Store`, `Options`, `Scope`, `Row`, `parts::list`, `escape::encode_into`, `name::format_of`, `Error`, `Warning`, `WarnFn`.
- Produces:
  - `cli::main(argv: Vec<OsString>) -> i32` (argv without the program name; returns the §20.6 status).
  - `cli::args::{OPERATIONS, OutputFormat, Args, Parsed, parse}`: `Args { format: Option<OutputFormat>, quiet: bool, verbose: bool, delimiter: Option<u8>, op: String, store: String, args: Vec<String> }`, `Parsed::{Help, Version, Run(Args)}`, `parse(&[String]) -> Result<Parsed, String>` (Err is a usage message, exit 2).
  - `cli::Ctx { args, store: PathBuf, format: Format, table: bool, warn: WarnFn, out: BufWriter<StdoutLock<'static>> }` with `open(&self, Scope, verify: bool) -> Result<Store, Error>`, `options(&self, Scope, bool) -> Options`, `info(&self, &str)`.
  - `cli::output::{Emitter, record, fields, stdout_error}`: `Emitter::new(&mut Ctx)`, `Emitter::row<'a>(&mut self, key: &str, cells: impl Iterator<Item = &'a str>) -> Result<(), Error>`, `Emitter::finish(self) -> Result<(), Error>`; `fields(out: &mut impl Write, &[&str]) -> io::Result<()>`.
  - Each operation is `fn(&mut Ctx) -> Result<i32, Error>` in its module; an `Err` prints `tsvz: <error>` and exits 1.

Until Task 14, `--format table` prints records; until Tasks 12 and 13, the write operations print `not built yet` and exit 1.

- [ ] **Step 1: Add the binary target**

Append to `Cargo.toml`:

```toml
[[bin]]
name = "tsvz"
path = "src/main.rs"
required-features = ["cli"]
doc = false
```

Add to `src/lib.rs`:

```rust
#[cfg(feature = "cli")]
pub mod cli;
```

File: `src/main.rs`

```rust
fn main() {
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    std::process::exit(tsvz::cli::main(args));
}
```

- [ ] **Step 2: Write the failing parser tests**

File: `src/cli/args.rs` (tests first; the implementation goes above them in Step 4)

```rust
//! §20.1 argument parsing.

#[cfg(test)]
mod tests {
    use super::*;

    fn p(args: &[&str]) -> Result<Parsed, String> {
        parse(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    fn run(args: &[&str]) -> Args {
        match p(args) {
            Ok(Parsed::Run(a)) => a,
            other => panic!("{args:?}: {other:?}"),
        }
    }

    #[test]
    fn canonical_and_file_first_forms() {
        let a = run(&["get", "s.tsvz", "k1", "k2"]);
        assert_eq!((a.op.as_str(), a.store.as_str(), a.args.clone()), ("get", "s.tsvz", vec!["k1".to_string(), "k2".to_string()]));
        let b = run(&["s.tsvz", "append", "k", "v"]);
        assert_eq!((b.op.as_str(), b.store.as_str()), ("append", "s.tsvz"));
        let c = run(&["s.tsvz"]);
        assert_eq!(c.op, "read");
        assert!(p(&["s.tsvz", "alice"]).unwrap_err().contains("unknown operation"));
        assert!(p(&["./read", "keys"]).is_ok());
    }

    #[test]
    fn options_anywhere_before_double_dash() {
        let a = run(&["set", "-q", "s.tsvz", "k", "--format", "records", "v", "-v"]);
        assert!(a.quiet && a.verbose);
        assert_eq!(a.format, Some(OutputFormat::Records));
        assert_eq!(a.args, vec!["k", "v"]);
        let b = run(&["set", "s.tsvz", "--", "carol", "-q", "--x"]);
        assert_eq!(b.args, vec!["carol", "-q", "--x"]);
        assert!(!b.quiet);
    }

    #[test]
    fn dashes_and_negative_numbers_are_values() {
        assert_eq!(run(&["set", "s.tsvz", "-"]).args, vec!["-"]);
        assert_eq!(run(&["set", "s.tsvz", "k", "-5", "-.5", "-1x"]).args, vec!["k", "-5", "-.5", "-1x"]);
    }

    #[test]
    fn grouped_flags_attached_values_and_prefixes() {
        let a = run(&["-qv", "read", "s.tsv", "-dcomma"]);
        assert!(a.quiet && a.verbose);
        assert_eq!(a.delimiter, Some(b','));
        assert_eq!(run(&["read", "s.tsv", "-d=|"]).delimiter, Some(b'|'));
        assert_eq!(run(&["read", "s.tsv", "--delimiter=null"]).delimiter, Some(0));
        assert_eq!(run(&["read", "s.tsv", "--delim", "tab"]).delimiter, Some(b'\t'));
        assert_eq!(run(&["--form=table", "read", "s.tsvz"]).format, Some(OutputFormat::Table));
        assert!(p(&["--ver", "read", "s.tsvz"]).unwrap_err().contains("ambiguous"));
    }

    #[test]
    fn usage_errors() {
        for (args, want) in [
            (&["read"][..], "needs a STORE"),
            (&[][..], "missing OPERATION"),
            (&["--bogus", "read", "s.tsvz"][..], "unknown option"),
            (&["-x", "read", "s.tsvz"][..], "unknown option"),
            (&["-ü", "read", "s.tsvz"][..], "unknown option"),
            (&["--format", "json", "read", "s.tsvz"][..], "--format"),
            (&["--format"][..], "needs a value"),
            (&["--quiet=yes", "read", "s.tsvz"][..], "takes no value"),
            (&["-d", "ab", "read", "s.tsv"][..], "-d takes"),
            (&["read", "s.tsvz", "extra"][..], "takes no arguments"),
            (&["popitem", "s.tsvz", "middle"][..], "first or last"),
            (&["has", "s.tsvz", "a", "b"][..], "one KEY"),
            (&["setdefault", "s.tsvz", "k"][..], "a KEY and a VALUE"),
            (&["get", "s.tsvz"][..], "needs a KEY"),
            (&["get", "s.tsvz", "a", ""][..], "must not be empty"),
            (&["set", "s.tsvz", "", "v"][..], "must not be empty"),
        ] {
            let err = p(args).unwrap_err();
            assert!(err.contains(want), "{args:?}: {err}");
        }
    }

    #[test]
    fn help_and_version_win() {
        assert_eq!(p(&["--help", "bogus"]).unwrap(), Parsed::Help);
        assert_eq!(p(&["read", "s.tsvz", "-V"]).unwrap(), Parsed::Version);
    }
}
```

- [ ] **Step 3: Run the parser tests to verify they fail**

Create `src/cli/mod.rs` containing only `pub mod args;` for now, then run: `cargo test --lib cli::args`
Expected: compile errors: `parse`, `Parsed`, `Args`, `OutputFormat` not found.

- [ ] **Step 4: Implement the parser**

Add above the tests in `src/cli/args.rs`:

```rust
use std::collections::VecDeque;

/// The operations of §20.2, plus `serve` and `stop` (§20.2.6).
pub const OPERATIONS: &[&str] = &[
    "read", "get", "set", "append", "delete", "clear", "scrub", "verify", "parts", "has", "len", "keys", "pop", "popitem",
    "setdefault", "serve", "stop",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputFormat {
    Table,
    Records,
}

/// A parsed command line.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Args {
    pub format: Option<OutputFormat>,
    pub quiet: bool,
    pub verbose: bool,
    pub delimiter: Option<u8>,
    pub op: String,
    pub store: String,
    pub args: Vec<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Parsed {
    Help,
    Version,
    Run(Args),
}

/// (spelling, destination, takes a value).
const OPTIONS: &[(&str, &str, bool)] = &[
    ("-h", "help", false),
    ("--help", "help", false),
    ("-V", "version", false),
    ("--version", "version", false),
    ("--format", "format", true),
    ("-q", "quiet", false),
    ("--quiet", "quiet", false),
    ("-v", "verbose", false),
    ("--verbose", "verbose", false),
    ("-d", "delimiter", true),
    ("--delimiter", "delimiter", true),
];

/// An option's destination and whether it takes a value; a long option may be given by a
/// unique prefix, as TSVZ.py's parser accepts (`--delim`).
fn option(name: &str) -> Result<(&'static str, bool), String> {
    if let Some(&(_, dest, takes)) = OPTIONS.iter().find(|o| o.0 == name) {
        return Ok((dest, takes));
    }
    if name.starts_with("--") {
        let matches: Vec<_> = OPTIONS.iter().filter(|o| o.0.starts_with("--") && o.0.starts_with(name)).collect();
        if let Some(first) = matches.first() {
            if matches.iter().all(|o| o.1 == first.1) {
                return Ok((first.1, first.2));
            }
            let names: Vec<&str> = matches.iter().map(|o| o.0).collect();
            return Err(format!("ambiguous option {name}: {}", names.join(", ")));
        }
    }
    Err(format!("unknown option {name}"))
}

/// `-` followed by a digit or `.` is a value, not an option (§20.1.2).
fn is_negative_number(arg: &str) -> bool {
    let mut chars = arg.chars();
    chars.next() == Some('-') && chars.next().is_some_and(|c| c.is_ascii_digit() || c == '.')
}

fn parse_delimiter(text: &str) -> Result<u8, String> {
    match text {
        "tab" => Ok(b'\t'),
        "comma" => Ok(b','),
        "pipe" => Ok(b'|'),
        "null" => Ok(0),
        t if t.len() == 1 && t.is_ascii() && t != "\n" && t != "\r" => Ok(t.as_bytes()[0]),
        t => Err(format!("-d takes one ASCII character or tab, comma, pipe or null, not {t:?}")),
    }
}

/// Parse a command line per §20.1. Err is the message for a usage error (exit 2).
pub fn parse(argv: &[String]) -> Result<Parsed, String> {
    let mut a = Args::default();
    let (mut help, mut version) = (false, false);
    let (mut format, mut delimiter) = (None, None);
    let mut positionals = Vec::new();
    let mut queue: VecDeque<String> = argv.iter().cloned().collect();
    while let Some(arg) = queue.pop_front() {
        if arg == "--" {
            positionals.extend(queue.drain(..));
            break;
        }
        if !arg.starts_with('-') || arg == "-" || is_negative_number(&arg) {
            positionals.push(arg);
            continue;
        }
        let (name, value) = if let Some(long) = arg.strip_prefix("--") {
            match long.split_once('=') {
                Some((n, v)) => (format!("--{n}"), Some(v.to_string())),
                None => (arg.clone(), None),
            }
        } else {
            let split = arg.char_indices().nth(2).map_or(arg.len(), |(i, _)| i);
            let (flag, attached) = arg.split_at(split);
            if attached.is_empty() {
                (flag.to_string(), None)
            } else if option(flag).is_ok_and(|o| o.1) {
                (flag.to_string(), Some(attached.strip_prefix('=').unwrap_or(attached).to_string())) // -dcomma, -d=,
            } else {
                queue.push_front(format!("-{attached}")); // -qv: one flag at a time
                (flag.to_string(), None)
            }
        };
        let (dest, takes) = option(&name)?;
        let value = match (takes, value) {
            (true, Some(v)) => v,
            (true, None) => queue.pop_front().ok_or_else(|| format!("option {name} needs a value"))?,
            (false, Some(_)) => return Err(format!("option {name} takes no value")),
            (false, None) => String::new(),
        };
        match dest {
            "help" => help = true,
            "version" => version = true,
            "format" => format = Some(value),
            "quiet" => a.quiet = true,
            "verbose" => a.verbose = true,
            _ => delimiter = Some(value),
        }
    }
    if help {
        return Ok(Parsed::Help);
    }
    if version {
        return Ok(Parsed::Version);
    }
    a.format = match format.as_deref() {
        None => None,
        Some("table") => Some(OutputFormat::Table),
        Some("records") => Some(OutputFormat::Records),
        Some(other) => return Err(format!("--format must be table or records, not {other:?}")),
    };
    a.delimiter = delimiter.as_deref().map(parse_delimiter).transpose()?;
    let mut pos = positionals.into_iter();
    let first = pos.next().ok_or("missing OPERATION and STORE")?;
    if OPERATIONS.contains(&first.as_str()) {
        a.store = pos.next().ok_or_else(|| format!("{first} needs a STORE"))?;
        a.op = first;
        a.args = pos.collect();
    } else {
        a.store = first;
        match pos.next() {
            None => a.op = "read".to_string(),
            Some(op) if OPERATIONS.contains(&op.as_str()) => {
                a.op = op;
                a.args = pos.collect();
            }
            Some(op) => {
                return Err(format!("unknown operation: neither {:?} nor {op:?} is one of {}", a.store, OPERATIONS.join(", ")));
            }
        }
    }
    validate(&a)?;
    Ok(Parsed::Run(a))
}

fn validate(a: &Args) -> Result<(), String> {
    let op = a.op.as_str();
    let n = a.args.len();
    if matches!(op, "read" | "clear" | "scrub" | "verify" | "parts" | "len" | "keys" | "serve" | "stop") && n > 0 {
        return Err(format!("{op} takes no arguments after STORE"));
    }
    if op == "popitem" && !(n == 0 || (n == 1 && matches!(a.args[0].as_str(), "first" | "last"))) {
        return Err("popitem takes first or last".into());
    }
    if matches!(op, "has" | "pop") && n > 1 {
        return Err(format!("{op} takes one KEY"));
    }
    if op == "setdefault" && n == 1 {
        return Err("setdefault needs a KEY and a VALUE".into());
    }
    if matches!(op, "get" | "set" | "append" | "delete" | "has" | "pop" | "setdefault") {
        if n == 0 {
            return Err(format!("{op} needs a KEY"));
        }
        let keys = if matches!(op, "get" | "delete") { &a.args[..] } else { &a.args[..1] };
        if keys.iter().any(String::is_empty) {
            return Err(format!("{op}: a KEY must not be empty"));
        }
    }
    Ok(())
}
```

- [ ] **Step 5: Run the parser tests to verify they pass**

Run: `cargo test --lib cli::args`
Expected: 6 passed.

- [ ] **Step 6: Write the failing end-to-end tests**

File: `tests/cli.rs`

```rust
//! End-to-end tests of the `tsvz` binary.

use std::fs;
use std::path::Path;
use std::process::{Command, Output, Stdio};

fn tsvz() -> Command {
    Command::new(env!("CARGO_BIN_EXE_tsvz"))
}

fn run_in(dir: &Path, args: &[&str], stdin: &[u8]) -> (i32, String, String) {
    let mut child = tsvz()
        .current_dir(dir)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    child.stdin.take().unwrap().write_all(stdin).unwrap();
    let Output { status, stdout, stderr } = child.wait_with_output().unwrap();
    (status.code().unwrap_or(-1), String::from_utf8_lossy(&stdout).into(), String::from_utf8_lossy(&stderr).into())
}

fn records(dir: &Path, args: &[&str]) -> (i32, String, String) {
    let mut all = vec!["--format", "records"];
    all.extend_from_slice(args);
    run_in(dir, &all, b"")
}

fn store(content: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("s.tsvz"), content).unwrap();
    dir
}

#[test]
fn read_get_has_len_keys() {
    let d = store("#_defaults_#\td1\td2\n<#>h\tv<lt>\na\tx\n");
    assert_eq!(records(d.path(), &["read", "s.tsvz"]), (0, "<#>h\tv<lt>\td2\na\tx\td2\n".into(), String::new()));
    assert_eq!(records(d.path(), &["get", "s.tsvz", "a  ", "zz"]), (3, "a\tx\td2\nzz\td1\td2\n".into(), String::new()));
    assert_eq!(records(d.path(), &["has", "s.tsvz", "a"]).0, 0);
    assert_eq!(records(d.path(), &["has", "s.tsvz", "zz"]).0, 3);
    assert_eq!(records(d.path(), &["len", "s.tsvz"]).1, "2\n");
    assert_eq!(records(d.path(), &["keys", "s.tsvz"]).1, "<#>h\na\n");
    assert_eq!(records(d.path(), &["s.tsvz"]).1, "<#>h\tv<lt>\td2\na\tx\td2\n");
}

#[test]
fn verify_and_parts() {
    let d = store("#_checksum_crc32_#\na\tx\n#_checksum_crc32_#\tDEADBEEF\n");
    let (code, out, _) = records(d.path(), &["verify", "s.tsvz"]);
    assert_eq!(code, 4);
    assert_eq!(out, format!("s.tsvz\t3\tcrc32\tdeadbeef\t{:08x}\n", crc32fast::hash(b"a\tx\n")));
    fs::write(d.path().join("s.tsvz.a.gz"), b"").unwrap();
    assert_eq!(records(d.path(), &["parts", "s.tsvz"]).1, "0\t\ts.tsvz\t\n1\ta\ts.tsvz.a.gz\tactive,gzip\n");
}

#[test]
fn version_help_and_usage_errors() {
    let d = tempfile::tempdir().unwrap();
    assert_eq!(run_in(d.path(), &["-V"], b"").1, format!("tsvz-rs {}\n", env!("CARGO_PKG_VERSION")));
    let (code, out, _) = run_in(d.path(), &["--help"], b"");
    assert!(code == 0 && out.contains("OPERATION STORE"));
    let (code, _, err) = run_in(d.path(), &["frobnicate", "s.tsvz"], b"");
    assert!(code == 2 && err.contains("unknown operation"), "{err}");
}

#[test]
fn missing_and_unreadable_stores_exit_1_naming_the_path() {
    let d = tempfile::tempdir().unwrap();
    let (code, out, err) = records(d.path(), &["read", "none.tsvz"]);
    assert_eq!((code, out.as_str()), (1, ""));
    assert!(err.contains("none.tsvz"), "{err}");
    fs::create_dir(d.path().join("dir.tsvz")).unwrap();
    let (code, _, err) = records(d.path(), &["get", "dir.tsvz", "k"]);
    assert!(code == 1 && err.contains("dir.tsvz") && !err.contains("panicked"), "{err}");
    fs::write(d.path().join("p.tsvz"), "a\tx\n").unwrap();
    fs::create_dir(d.path().join("p.tsvz.1")).unwrap();
    let (code, _, err) = records(d.path(), &["read", "p.tsvz"]);
    assert!(code == 1 && err.contains("p.tsvz.1") && !err.contains("panicked"), "{err}");
    let (code, _, err) = records(d.path(), &["read", "notes.txt"]);
    assert!(code == 1 && err.contains("not a TSVZ store path"), "{err}");
}

#[test]
fn awkward_paths() {
    let d = tempfile::tempdir().unwrap();
    fs::write(d.path().join("my store ü.tsvz"), "k\tv\n").unwrap();
    assert_eq!(records(d.path(), &["get", "my store ü.tsvz", "k"]).1, "k\tv\n");
    fs::write(d.path().join("-dash.tsvz"), "k\tv\n").unwrap();
    assert_eq!(records(d.path(), &["--", "-dash.tsvz", "get", "k"]).1, "k\tv\n");
    use std::os::unix::ffi::OsStringExt;
    let out = tsvz().current_dir(d.path()).arg("read").arg(std::ffi::OsString::from_vec(b"bad\xff.tsvz".to_vec())).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(!String::from_utf8_lossy(&out.stderr).contains("panicked"));
}

#[test]
fn a_closed_stdout_ends_quietly() {
    let d = tempfile::tempdir().unwrap();
    let big: String = (0..200_000).map(|i| format!("k{i}\tvalue\n")).collect();
    fs::write(d.path().join("big.tsvz"), big).unwrap();
    let mut child = tsvz()
        .current_dir(d.path())
        .args(["--format", "records", "read", "big.tsvz"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut first = [0u8; 16];
    use std::io::Read;
    child.stdout.as_mut().unwrap().read_exact(&mut first).unwrap();
    drop(child.stdout.take());
    let out = child.wait_with_output().unwrap();
    assert!(!String::from_utf8_lossy(&out.stderr).contains("panicked"), "{}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn a_stopped_read_prints_what_it_read_and_exits_1() {
    let d = store("#_on_corruption_#\tstop\na\t1\n\0\0c\t3\nd\t4\n");
    let (code, out, _) = records(d.path(), &["read", "s.tsvz"]);
    assert_eq!((code, out.as_str()), (1, "a\t1\n"));
}

#[test]
fn serve_and_stop_are_not_provided() {
    let d = store("a\tx\n");
    assert_eq!(records(d.path(), &["serve", "s.tsvz"]).0, 1);
    assert_eq!(records(d.path(), &["stop", "s.tsvz"]).0, 1);
}
```

Add to `Cargo.toml` under `[dev-dependencies]`: nothing new (`crc32fast` is already a normal dependency, usable from integration tests).

- [ ] **Step 7: Run them to verify they fail**

Run: `cargo test --test cli`
Expected: compile error: `tsvz::cli::main` not found (and the binary does not build).

- [ ] **Step 8: Implement the CLI entry point, output and reading operations**

File: `src/cli/mod.rs`

```rust
//! The command-line tool of §20.

pub mod args;
pub(crate) mod output;
mod read;

use std::ffi::OsString;
use std::io::{BufWriter, StdoutLock, Write};
use std::path::PathBuf;
use std::sync::Arc;

use crate::name::{self, Format};
use crate::{Error, Options, Scope, Store, WarnFn, Warning};
use args::{Args, OutputFormat, Parsed};

pub const USAGE: &str = "\
usage: tsvz [OPTION ...] OPERATION STORE [ARG ...]
       tsvz [OPTION ...] STORE [OPERATION] [ARG ...]

Read and write tsvz-spec-v1 key-value stores (.tsvz .csvz .nsvz .psvz, and .tsv .csv .nsv .psv).

operations:
  read STORE                   print every live row
  get STORE KEY [KEY ...]      print the rows of KEYs (exit 3 if one is missing)
  set STORE KEY [VALUE ...]    write a row; KEY alone deletes it; '-' reads rows from stdin
  append ...                   same as set
  delete STORE KEY [KEY ...]   delete KEYs; '-' reads keys from stdin
  clear STORE                  empty the store, keeping its header and markers
  scrub STORE                  compact a one-part store in place
  verify STORE                 check #_checksum_<algo>_# segments (exit 4 on a mismatch)
  parts STORE                  list the parts of the store
  has STORE KEY                exit 0 if KEY is live, 3 if not
  len STORE                    print the number of live keys
  keys STORE                   print the live keys
  pop STORE KEY                print the row of KEY and delete it (exit 3 if missing)
  popitem STORE [first|last]   pop the first or last live key (default: last)
  setdefault STORE KEY VALUE [VALUE ...]
                               print the row of KEY, setting it to the VALUEs if missing
  serve STORE, stop STORE      not provided by this tsvz (exit 1)

options:
  -h, --help                   show this help
  -V, --version                show the version
  --format table|records       output format (default: table on a terminal, else records)
  -q, --quiet                  print errors only
  -v, --verbose                print informational messages
  -d, --delimiter D            delimiter of a .tsv-family file: tab, comma, pipe, null or one character
  --                           every argument after this is positional

exit status: 0 done, 1 failed or refused, 2 usage error, 3 missing key, 4 checksum mismatch
";

/// Run the tool with `argv` (without the program name) and return its exit status (§20.6).
pub fn main(argv: Vec<OsString>) -> i32 {
    // SAFETY: restores the default action for SIGPIPE before any other thread exists, so writing
    // to a closed pipe ends the process quietly, as it does other Unix tools.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    let mut text = Vec::with_capacity(argv.len());
    for arg in argv {
        match arg.into_string() {
            Ok(s) => text.push(s),
            Err(bad) => {
                eprintln!("tsvz: an argument is not valid UTF-8: {}", bad.to_string_lossy());
                return 2;
            }
        }
    }
    match args::parse(&text) {
        Err(message) => {
            eprintln!("tsvz: {message}\nTry 'tsvz --help'.");
            2
        }
        Ok(Parsed::Help) => {
            print!("{USAGE}");
            0
        }
        Ok(Parsed::Version) => {
            println!("tsvz-rs {}", env!("CARGO_PKG_VERSION"));
            0
        }
        Ok(Parsed::Run(args)) => match Ctx::new(args) {
            Ok(mut ctx) => ctx.run(),
            Err(e) => {
                eprintln!("tsvz: {e}");
                1
            }
        },
    }
}

/// What every operation needs.
pub(crate) struct Ctx {
    pub args: Args,
    pub store: PathBuf,
    pub format: Format,
    #[allow(dead_code)] // read by the table output (Task 14), which removes this line
    pub table: bool,
    pub warn: WarnFn,
    pub out: BufWriter<StdoutLock<'static>>,
}

impl Ctx {
    fn new(args: Args) -> Result<Ctx, Error> {
        let store = PathBuf::from(&args.store);
        let mut format = name::format_of(&store).ok_or_else(|| Error::BadPath(store.clone()))?;
        let quiet = args.quiet;
        let warn: WarnFn = Arc::new(move |w: &Warning| {
            if !quiet {
                eprintln!("tsvz: {}: {}", w.part.display(), w.message);
            }
        });
        if let Some(d) = args.delimiter {
            if !format.strict {
                format.delimiter = d;
            } else if d != format.delimiter {
                warn(&Warning {
                    part: store.clone(),
                    offset: None,
                    message: "-d ignored: a strict store's extension decides its delimiter (§5.1)".into(),
                });
            }
        }
        // SAFETY: isatty only inspects file descriptor 1.
        let terminal = unsafe { libc::isatty(1) } == 1;
        let table = match args.format {
            Some(OutputFormat::Table) => true,
            Some(OutputFormat::Records) => false,
            None => terminal,
        };
        Ok(Ctx { args, store, format, table, warn, out: BufWriter::with_capacity(1 << 16, std::io::stdout().lock()) })
    }

    pub(crate) fn options(&self, scope: Scope, verify: bool) -> Options {
        Options { scope, verify, delimiter: self.args.delimiter, warnings: Some(self.warn.clone()) }
    }

    pub(crate) fn open(&self, scope: Scope, verify: bool) -> Result<Store, Error> {
        Store::open(&self.store, self.options(scope, verify))
    }

    /// An informational message, printed with -v (§20.7.1).
    pub(crate) fn info(&self, message: &str) {
        if self.args.verbose {
            eprintln!("tsvz: {}: {}", self.store.display(), message);
        }
    }

    fn run(&mut self) -> i32 {
        let result = match self.args.op.as_str() {
            "read" => read::read(self),
            "get" => read::get(self),
            "has" => read::has(self),
            "len" => read::len(self),
            "keys" => read::keys(self),
            "verify" => read::verify(self),
            "parts" => read::parts(self),
            "serve" | "stop" => {
                eprintln!("tsvz: {}: this tsvz does not provide a handler (§21); work on the files instead", self.args.op);
                Ok(1)
            }
            other => {
                eprintln!("tsvz: {other}: not built yet"); // replaced in Tasks 12 and 13
                Ok(1)
            }
        };
        let code = result.unwrap_or_else(|e| {
            eprintln!("tsvz: {e}");
            1
        });
        if let Err(e) = self.out.flush() {
            if e.kind() != std::io::ErrorKind::BrokenPipe {
                eprintln!("tsvz: stdout: {e}");
                return 1;
            }
        }
        code
    }
}
```

File: `src/cli/output.rs`

```rust
//! Records output (§20.4.1). Task 14 adds the table layout.

use std::io::{self, Write};
use std::path::Path;

use crate::cli::Ctx;
use crate::{escape, Error};

pub(crate) fn stdout_error(e: io::Error) -> Error {
    Error::io(Path::new("stdout"), e)
}

/// One row as a record: the key (`<#>` for a leading `#`), then its cells, joined by the
/// store's delimiter. Strict variants are §13-encoded; a loose file's cells are written as
/// they are (§20.4.4).
pub(crate) fn record<'a>(
    out: &mut impl Write,
    buf: &mut String,
    key: &str,
    cells: impl Iterator<Item = &'a str>,
    delimiter: u8,
    strict: bool,
) -> io::Result<()> {
    buf.clear();
    if strict {
        escape::encode_into(buf, key, delimiter, true);
    } else {
        buf.push_str(key);
    }
    for cell in cells {
        buf.push(delimiter as char);
        if strict {
            escape::encode_into(buf, cell, delimiter, false);
        } else {
            buf.push_str(cell);
        }
    }
    buf.push('\n');
    out.write_all(buf.as_bytes())
}

/// A `verify` or `parts` line: fields joined by TAB, each §13-encoded for TSVZ (§20.4.1).
pub(crate) fn fields(out: &mut impl Write, fields: &[&str]) -> io::Result<()> {
    let mut line = String::new();
    for (i, f) in fields.iter().enumerate() {
        if i > 0 {
            line.push('\t');
        }
        escape::encode_into(&mut line, f, b'\t', false);
    }
    line.push('\n');
    out.write_all(line.as_bytes())
}

/// Prints rows in the chosen format.
pub(crate) struct Emitter<'c> {
    ctx: &'c mut Ctx,
    buf: String,
}

impl<'c> Emitter<'c> {
    pub(crate) fn new(ctx: &'c mut Ctx) -> Emitter<'c> {
        Emitter { ctx, buf: String::new() }
    }

    pub(crate) fn row<'a>(&mut self, key: &str, cells: impl Iterator<Item = &'a str>) -> Result<(), Error> {
        let (d, strict) = (self.ctx.format.delimiter, self.ctx.format.strict);
        record(&mut self.ctx.out, &mut self.buf, key, cells, d, strict).map_err(stdout_error)
    }

    pub(crate) fn finish(self) -> Result<(), Error> {
        Ok(())
    }
}
```

File: `src/cli/read.rs`

```rust
//! The reading operations: read, get, has, len, keys, verify, parts (§20.2).

use std::io::Write;

use crate::cli::output::{fields, stdout_error, Emitter};
use crate::cli::Ctx;
use crate::{parts, Error, Scope, Store};

/// A read that stopped at damaged data fails (§4.10, §20.6).
fn status(store: &Store, code: i32) -> i32 {
    if store.stopped().is_some() { 1 } else { code }
}

pub(crate) fn read(ctx: &mut Ctx) -> Result<i32, Error> {
    let store = ctx.open(Scope::All, false)?;
    let mut out = Emitter::new(ctx);
    for (key, row) in store.iter() {
        out.row(key, row.cells())?;
    }
    out.finish()?;
    ctx.info(&format!("{} live keys in {} parts", store.len(), store.parts().len()));
    Ok(status(&store, 0))
}

pub(crate) fn get(ctx: &mut Ctx) -> Result<i32, Error> {
    let keys = ctx.args.args.clone();
    let store = ctx.open(Scope::Keys(keys.clone()), false)?;
    let mut missing = false;
    let mut out = Emitter::new(ctx);
    for requested in &keys {
        let key = store.lookup_key(requested);
        match store.get(key) {
            Some(row) => out.row(key, row.cells())?,
            None => {
                missing = true;
                if let Some(row) = store.missing() {
                    out.row(key, row.cells())?;
                }
            }
        }
    }
    out.finish()?;
    Ok(status(&store, if missing { 3 } else { 0 }))
}

pub(crate) fn has(ctx: &mut Ctx) -> Result<i32, Error> {
    let store = ctx.open(Scope::KeysOnly, false)?;
    let live = store.get(store.lookup_key(&ctx.args.args[0])).is_some();
    Ok(status(&store, if live { 0 } else { 3 }))
}

pub(crate) fn len(ctx: &mut Ctx) -> Result<i32, Error> {
    let store = ctx.open(Scope::KeysOnly, false)?;
    writeln!(ctx.out, "{}", store.len()).map_err(stdout_error)?;
    Ok(status(&store, 0))
}

pub(crate) fn keys(ctx: &mut Ctx) -> Result<i32, Error> {
    let store = ctx.open(Scope::KeysOnly, false)?;
    let mut out = Emitter::new(ctx);
    for (key, _) in store.iter() {
        out.row(key, std::iter::empty())?;
    }
    out.finish()?;
    Ok(status(&store, 0))
}

pub(crate) fn verify(ctx: &mut Ctx) -> Result<i32, Error> {
    let store = ctx.open(Scope::KeysOnly, true)?;
    for m in store.mismatches() {
        let part = m.part.display().to_string();
        fields(&mut ctx.out, &[&part, &m.line.to_string(), m.algo, &m.expected, &m.computed]).map_err(stdout_error)?;
    }
    Ok(status(&store, if store.mismatches().is_empty() { 0 } else { 4 }))
}

pub(crate) fn parts(ctx: &mut Ctx) -> Result<i32, Error> {
    let list = parts::list(&ctx.store)?;
    if list.is_empty() {
        return Err(Error::Missing(ctx.store.clone()));
    }
    let last = list.len() - 1;
    for (i, part) in list.iter().enumerate() {
        let mut flags = Vec::new();
        if i == last {
            flags.push("active");
        }
        if let Some(codec) = part.codec {
            flags.push(codec.name());
        }
        let path = part.path.display().to_string();
        fields(&mut ctx.out, &[&i.to_string(), part.ordinal.as_deref().unwrap_or(""), &path, &flags.join(",")]).map_err(stdout_error)?;
    }
    Ok(0)
}
```

- [ ] **Step 9: Run every test to verify they pass**

Run: `cargo test`
Expected: all library tests and the 8 tests in `tests/cli.rs` pass.

- [ ] **Step 10: Run the conformance suite's reader class**

Run:

```bash
cargo build --release
python3 ../TSVZ/tsvzconform.py --impl target/release/tsvz --class reader \
  --feature bzip2,xz,zstd,crc32,crc32c,sha256,blake3,file-first
```

Expected: `conformant: reader ✓`, no FAIL. Under Python older than 3.14 the zstd cases are skipped ("this Python has no zstd"); that is fine. If a case fails, its report names the clause, the input and a rerun command (`--only <case-id>`); fix the code it points at and rerun. Record any change of behaviour you had to make in the commit message.

- [ ] **Step 11: Commit**

```bash
git add Cargo.toml src/lib.rs src/main.rs src/cli tests/cli.rs
git commit -m "CLI: §20.1 arguments and the reading operations; the conformance reader class passes." -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---
### Task 11: The write lock and appends

**Files:**
- Create: `src/lock.rs`, `src/write.rs`
- Modify: `src/lib.rs` (add `pub mod lock;`, `pub mod write;`, `pub use write::{classify_key, KeyKind, Record, Writer};`)

**Interfaces:**
- Consumes: `parts::active`, `codec::encode_members`, `escape::{encode_into, is_marker_key}`, `name::{format_of, Format}`, `replay::DEFAULT_MEMBER_SIZE`, `Error`, `WarnFn`, `Warning`.
- Produces:
  - `lock::lock<'f>(file: &'f File, path: &Path) -> io::Result<lock::Guard<'f>>` — waits for the in-process gate, then the whole-file lock; released on drop. The file must be open for writing.
  - `write::Record` (`Data { key, values }`, `Tombstone { key }`, `Marker { key, values }`): `Data` and `Tombstone` keys are always data keys (a leading `#` is written `<#>`, even for a reserved-looking key); `Marker` keys are written as they are. `Record::from_args(key: &str, values: &[String]) -> Record` is what `set`/`delete` arguments mean (§20.2): a reserved key is a `Marker` (no values: a reset), a lone key a `Tombstone`.
  - `write::KeyKind` (`Marker`, `Data`) and `write::classify_key(&str) -> KeyKind`: what a key given as an argument means (§20.2, §21.4.3).
  - `write::encode_record(&Record, Format, &mut String) -> Result<(), Error>` (loose files refuse a field holding the delimiter, `\n` or `\r`).
  - `write::Writer`: `open(store) -> Result<Writer, Error>`, builder methods `member_size(usize)`, `fsync(bool)`, `warnings(Option<WarnFn>)`, `delimiter(Option<u8>)` (loose files only), `format()`, `append(&mut self, &[Record]) -> Result<(), Error>`, `append_payload(&mut self, &[u8]) -> Result<(), Error>` (an empty payload only creates a missing store).

- [ ] **Step 1: Write the failing tests**

File: `src/lock.rs` (tests first)

```rust
//! The write lock shared with TSVZ.py: an in-process gate per file, then an exclusive
//! whole-file lock — an open file description lock on Linux (a classic POSIX lock when the
//! kernel refuses it), a classic POSIX lock on macOS. Readers take no lock.

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::OpenOptions;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    #[test]
    fn two_handles_in_one_process_exclude_each_other() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.tsvz");
        std::fs::write(&path, "").unwrap();
        let order = Arc::new(Mutex::new(Vec::new()));
        let first = OpenOptions::new().append(true).open(&path).unwrap();
        let guard = lock(&first, &path).unwrap();
        let (p2, o2) = (path.clone(), order.clone());
        let t = std::thread::spawn(move || {
            let second = OpenOptions::new().append(true).open(&p2).unwrap();
            let _g = lock(&second, &p2).unwrap();
            o2.lock().unwrap().push("second");
        });
        std::thread::sleep(Duration::from_millis(150));
        order.lock().unwrap().push("first");
        drop(guard);
        t.join().unwrap();
        assert_eq!(*order.lock().unwrap(), vec!["first", "second"]);
    }
}
```

File: `src/write.rs` (tests first)

```rust
//! Appending records (§13.3, §16.4, §16.7, §17.7, §18.2).

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Options, Store};
    use std::fs;

    const TSVZ: Format = Format { delimiter: b'\t', strict: true };

    fn enc(r: Record, f: Format) -> Result<String, Error> {
        let mut s = String::new();
        encode_record(&r, f, &mut s).map(|()| s)
    }

    #[test]
    fn records_encode_per_variant() {
        let d = |k: &str, v: &[&str]| Record::Data { key: k.into(), values: v.iter().map(|s| s.to_string()).collect() };
        assert_eq!(enc(d("#k", &["a\tb", "<", "#v"]), TSVZ).unwrap(), "<#>k\ta<sep>b\t<lt>\t#v\n");
        assert_eq!(enc(d("#_defaults_#", &["x"]), TSVZ).unwrap(), "<#>_defaults_#\tx\n");
        assert_eq!(enc(Record::Tombstone { key: "#_x_#".into() }, TSVZ).unwrap(), "<#>_x_#\n");
        assert_eq!(enc(Record::Marker { key: "#_defaults_#".into(), values: vec!["a,b".into()] }, Format { delimiter: b',', strict: true }).unwrap(), "#_defaults_#,a<sep>b\n");
        assert_eq!(enc(d("k", &["a\0b"]), Format { delimiter: 0, strict: true }).unwrap(), "k\0a<sep>b\n");
        assert_eq!(enc(d("k", &["", ""]), TSVZ).unwrap(), "k\t\t\n");
        let loose = Format { delimiter: b',', strict: false };
        assert_eq!(enc(d("k", &["a<b", "#c"]), loose).unwrap(), "k,a<b,#c\n");
        assert!(matches!(enc(d("k", &["a,b"]), loose), Err(Error::Refused(_))));
        assert!(matches!(enc(d("k", &["a\nb"]), loose), Err(Error::Refused(_))));
    }

    #[test]
    fn records_from_arguments() {
        assert_eq!(Record::from_args("#_defaults_#", &[]), Record::Marker { key: "#_defaults_#".into(), values: vec![] });
        assert_eq!(Record::from_args("k", &[]), Record::Tombstone { key: "k".into() });
        assert_eq!(Record::from_args("k", &["".into()]), Record::Data { key: "k".into(), values: vec!["".into()] });
        assert_eq!(classify_key("#_x_#"), KeyKind::Marker);
        assert_eq!(classify_key("#x"), KeyKind::Data);
    }

    fn read(path: &Path) -> Vec<(String, Vec<String>)> {
        let s = Store::open(path, Options::default()).unwrap();
        s.iter().map(|(k, r)| (k.to_string(), r.to_vec())).collect()
    }

    #[test]
    fn appends_create_append_and_cut_a_torn_tail() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.tsvz");
        let mut w = Writer::open(&p).unwrap();
        w.append(&[Record::from_args("a", &["1".into()])]).unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"a\t1\n");
        fs::write(&p, b"a\t1\ntorn").unwrap();
        let warned = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = warned.clone();
        let mut w = Writer::open(&p).unwrap().warnings(Some(std::sync::Arc::new(move |w: &Warning| sink.lock().unwrap().push(w.message.clone()))));
        w.append(&[Record::from_args("b", &["2".into()])]).unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"a\t1\nb\t2\n");
        assert_eq!(warned.lock().unwrap().len(), 1);
        Writer::open(dir.path().join("e.tsvz")).unwrap().append(&[]).unwrap();
        assert_eq!(fs::read(dir.path().join("e.tsvz")).unwrap(), b"");
    }

    #[test]
    fn appends_go_to_the_last_part() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("s.tsvz"), "a\t1\n").unwrap();
        fs::write(dir.path().join("s.tsvz.f"), "b\t2\n").unwrap();
        fs::write(dir.path().join("s.tsvz.10"), "c\t3\n").unwrap();
        Writer::open(dir.path().join("s.tsvz")).unwrap().append(&[Record::Tombstone { key: "a".into() }]).unwrap();
        assert_eq!(fs::read(dir.path().join("s.tsvz.10")).unwrap(), b"c\t3\na\n");
        assert_eq!(fs::read(dir.path().join("s.tsvz")).unwrap(), b"a\t1\n");
    }

    #[test]
    fn compressed_appends_add_members() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.tsvz.gz");
        let mut w = Writer::open(&p).unwrap().member_size(16);
        w.append(&[Record::from_args("a", &["1".into()]), Record::from_args("bbbbbbbbbbbbbbbbbbbb", &["2".into()])]).unwrap();
        let first = fs::read(&p).unwrap();
        w.append(&[Record::from_args("c", &["3".into()])]).unwrap();
        let all = fs::read(&p).unwrap();
        assert!(all.starts_with(&first));
        assert_eq!(crate::codec::items(crate::name::Codec::Gzip, &all).len(), 3);
        assert_eq!(read(&p).into_iter().map(|r| r.0).collect::<Vec<_>>(), vec!["a", "bbbbbbbbbbbbbbbbbbbb", "c"]);
    }

    #[test]
    fn loose_writer_with_a_delimiter() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.tsv");
        let mut w = Writer::open(&p).unwrap().delimiter(Some(b';'));
        w.append(&[Record::from_args("k", &["a\tb".into()])]).unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"k;a\tb\n");
        assert!(w.append(&[Record::from_args("k", &["x;y".into()])]).is_err());
        assert_eq!(fs::read(&p).unwrap(), b"k;a\tb\n");
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib lock write` (after adding both modules to `src/lib.rs`)
Expected: compile errors: `lock`, `Record`, `Writer`, `encode_record` not found.

- [ ] **Step 3: Implement the lock**

Add above the tests in `src/lock.rs`:

```rust
use std::collections::HashMap;
use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, OnceLock};

type Gate = Arc<(Mutex<bool>, Condvar)>;

/// The in-process gate for a file: POSIX record locks do not exclude two handles of one process.
fn gate(path: &Path) -> Gate {
    static GATES: OnceLock<Mutex<HashMap<PathBuf, Gate>>> = OnceLock::new();
    let key = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let mut gates = GATES.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
    gates.entry(key).or_default().clone()
}

fn release(gate: &Gate) {
    let (held, cv) = &**gate;
    *held.lock().unwrap_or_else(|e| e.into_inner()) = false;
    cv.notify_one();
}

/// The write lock on one file, held until dropped.
pub struct Guard<'f> {
    file: &'f File,
    gate: Gate,
    ofd: bool,
}

/// Take the write lock on `file` (open for writing), whose path is `path`; waits as long as
/// another handle holds it.
pub fn lock<'f>(file: &'f File, path: &Path) -> io::Result<Guard<'f>> {
    let gate = gate(path);
    {
        let (held, cv) = &*gate;
        let mut held = held.lock().unwrap_or_else(|e| e.into_inner());
        while *held {
            held = cv.wait(held).unwrap_or_else(|e| e.into_inner());
        }
        *held = true;
    }
    match file_lock(file, true, None) {
        Ok(ofd) => Ok(Guard { file, gate, ofd }),
        Err(e) => {
            release(&gate);
            Err(e)
        }
    }
}

impl Drop for Guard<'_> {
    fn drop(&mut self) {
        let _ = file_lock(self.file, false, Some(self.ofd));
        release(&self.gate);
    }
}

#[cfg(target_os = "linux")]
const F_OFD_SETLK: libc::c_int = 37;
#[cfg(target_os = "linux")]
const F_OFD_SETLKW: libc::c_int = 38;

/// Lock (`on`) or unlock the whole file. `ofd` says which kind an unlock releases; the result
/// says whether an open file description lock was used.
fn file_lock(file: &File, on: bool, ofd: Option<bool>) -> io::Result<bool> {
    // SAFETY: an all-zero `flock` is valid; the fields set below describe the whole file.
    let mut fl: libc::flock = unsafe { std::mem::zeroed() };
    fl.l_type = (if on { libc::F_WRLCK } else { libc::F_UNLCK }) as _;
    fl.l_whence = libc::SEEK_SET as _;
    let fd = file.as_raw_fd();
    #[cfg(target_os = "linux")]
    if ofd != Some(false) {
        match fcntl(fd, if on { F_OFD_SETLKW } else { F_OFD_SETLK }, &fl) {
            Ok(()) => return Ok(true),
            Err(e) if on && e.raw_os_error() == Some(libc::EINVAL) => {} // a kernel or file system without them
            Err(e) => return Err(e),
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = ofd;
    fcntl(fd, if on { libc::F_SETLKW } else { libc::F_SETLK }, &fl).map(|()| false)
}

fn fcntl(fd: libc::c_int, cmd: libc::c_int, fl: &libc::flock) -> io::Result<()> {
    loop {
        // SAFETY: `fd` stays open for the call; `fl` is a valid `flock`.
        if unsafe { libc::fcntl(fd, cmd, fl as *const libc::flock) } == 0 {
            return Ok(());
        }
        let e = io::Error::last_os_error();
        if e.kind() != io::ErrorKind::Interrupted {
            return Err(e);
        }
    }
}
```

- [ ] **Step 4: Implement records and the writer**

Add above the tests in `src/write.rs`:

```rust
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

use crate::escape::{self, is_marker_key};
use crate::name::{self, Format};
use crate::replay::DEFAULT_MEMBER_SIZE;
use crate::{codec, lock, parts, Error, WarnFn, Warning};

/// A record to append.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Record {
    /// A data key and its value cells (an empty `values` writes a tombstone).
    Data { key: String, values: Vec<String> },
    /// A lone data key: deletes it (§9).
    Tombstone { key: String },
    /// A marker line: the key as written, then its values; no values resets it (§12.1).
    Marker { key: String, values: Vec<String> },
}

impl Record {
    /// What `set STORE KEY [VALUE ...]` and `delete STORE KEY` write (§20.2): a key matching the
    /// reserved pattern is a marker line; a lone data key is a tombstone.
    pub fn from_args(key: &str, values: &[String]) -> Record {
        if is_marker_key(key) {
            Record::Marker { key: key.into(), values: values.to_vec() }
        } else if values.is_empty() {
            Record::Tombstone { key: key.into() }
        } else {
            Record::Data { key: key.into(), values: values.to_vec() }
        }
    }
}

/// Whether a key is a marker key (§12.2.1) or a data key: what a key given as an argument or a
/// request field means (§20.2, §21.4.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyKind {
    Marker,
    Data,
}

pub fn classify_key(key: &str) -> KeyKind {
    if is_marker_key(key) { KeyKind::Marker } else { KeyKind::Data }
}

/// Append `record` and its `\n` to `out`, encoded for `format` (§13.3). A loose file has no
/// escaping, so a field holding its delimiter, `\n` or `\r` is refused.
pub fn encode_record(record: &Record, format: Format, out: &mut String) -> Result<(), Error> {
    let d = format.delimiter;
    let (key, values, marker) = match record {
        Record::Data { key, values } => (key, &values[..], false),
        Record::Tombstone { key } => (key, &[][..], false),
        Record::Marker { key, values } => (key, &values[..], true),
    };
    if !format.strict {
        for field in std::iter::once(key).chain(values) {
            if field.bytes().any(|b| b == d || b == b'\n' || b == b'\r') {
                return Err(Error::Refused(format!(
                    "{field:?} holds the delimiter or a line break, which a loose file cannot hold; nothing was written"
                )));
            }
        }
        out.push_str(key);
        for v in values {
            out.push(d as char);
            out.push_str(v);
        }
        out.push('\n');
        return Ok(());
    }
    if marker {
        out.push_str(key);
    } else {
        escape::encode_into(out, key, d, true);
    }
    for v in values {
        out.push(d as char);
        escape::encode_into(out, v, d, false);
    }
    out.push('\n');
    Ok(())
}

/// Appends batches of records to a store's active part (§17.7).
pub struct Writer {
    store: PathBuf,
    format: Format,
    member_size: usize,
    fsync: bool,
    warn: Option<WarnFn>,
}

impl Writer {
    pub fn open(store: impl AsRef<Path>) -> Result<Writer, Error> {
        let store = store.as_ref().to_path_buf();
        let format = name::format_of(&store).ok_or_else(|| Error::BadPath(store.clone()))?;
        Ok(Writer { store, format, member_size: DEFAULT_MEMBER_SIZE as usize, fsync: false, warn: None })
    }

    /// The most uncompressed bytes per compressed member (§16.7); default 1 MiB.
    pub fn member_size(mut self, bytes: usize) -> Writer {
        self.member_size = bytes.max(1);
        self
    }

    pub fn fsync(mut self, on: bool) -> Writer {
        self.fsync = on;
        self
    }

    pub fn warnings(mut self, warn: Option<WarnFn>) -> Writer {
        self.warn = warn;
        self
    }

    /// The delimiter of a loose file (`-d`); ignored for strict variants (§5.1).
    pub fn delimiter(mut self, delimiter: Option<u8>) -> Writer {
        if let (false, Some(d)) = (self.format.strict, delimiter) {
            self.format.delimiter = d;
        }
        self
    }

    pub fn format(&self) -> Format {
        self.format
    }

    /// Append `records` as one batch (§18.2), creating a missing store (§20.2.2).
    pub fn append(&mut self, records: &[Record]) -> Result<(), Error> {
        let mut payload = String::new();
        for r in records {
            encode_record(r, self.format, &mut payload)?;
        }
        self.append_payload(payload.as_bytes())
    }

    /// Append encoded records (each ending in `\n`) as one batch.
    pub fn append_payload(&mut self, payload: &[u8]) -> Result<(), Error> {
        let part = parts::active(&self.store)?;
        let path = part.path.as_path();
        let io = |e: io::Error| Error::io(path, e);
        match part.codec {
            Some(codec) => {
                let bytes = codec::encode_members(codec, payload, self.member_size)
                    .ok_or_else(|| Error::UnsupportedCodec { path: path.to_path_buf(), codec: codec.name() })?;
                let file = OpenOptions::new().append(true).create(true).open(path).map_err(io)?;
                let _guard = lock::lock(&file, path).map_err(io)?;
                (&file).write_all(&bytes).map_err(io)?;
                if self.fsync {
                    file.sync_data().map_err(io)?;
                }
            }
            None => {
                let file = OpenOptions::new().read(true).append(true).create(true).open(path).map_err(io)?;
                let _guard = lock::lock(&file, path).map_err(io)?;
                self.cut_torn_tail(&file, path).map_err(io)?;
                (&file).write_all(payload).map_err(io)?;
                if self.fsync {
                    file.sync_data().map_err(io)?;
                }
            }
        }
        Ok(())
    }

    /// An uncommitted tail would join the first appended record: cut it first, as TSVZ.py does.
    fn cut_torn_tail(&self, file: &File, path: &Path) -> io::Result<()> {
        let size = file.metadata()?.len();
        if size == 0 {
            return Ok(());
        }
        let mut last = [0u8; 1];
        file.read_exact_at(&mut last, size - 1)?;
        if last[0] == b'\n' {
            return Ok(());
        }
        let keep = last_newline_end(file, size)?;
        file.set_len(keep)?;
        if let Some(w) = &self.warn {
            w(&Warning {
                part: path.to_path_buf(),
                offset: Some(keep),
                message: format!("cut {} uncommitted bytes after the last newline before appending", size - keep),
            });
        }
        Ok(())
    }
}

fn last_newline_end(file: &File, size: u64) -> io::Result<u64> {
    let mut pos = size;
    let mut buf = vec![0u8; 1 << 16];
    while pos > 0 {
        let n = pos.min(buf.len() as u64) as usize;
        pos -= n as u64;
        file.read_exact_at(&mut buf[..n], pos)?;
        if let Some(i) = memchr::memrchr(b'\n', &buf[..n]) {
            return Ok(pos + i as u64 + 1);
        }
    }
    Ok(0)
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test --lib lock write`
Expected: 1 + 6 passed.

- [ ] **Step 6: Commit**

```bash
git add src/lock.rs src/write.rs src/lib.rs
git commit -m "Write lock shared with TSVZ.py, and appends to plain and compressed active parts." -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 12: The command line — writing operations

**Files:**
- Create: `src/cli/write.rs`
- Modify: `src/cli/mod.rs` (dispatch), `src/store.rs` (a crate-visible `Row::new`), `tests/cli.rs`

**Interfaces:**
- Consumes: `Writer`, `Record`, `frame::Framer`, `escape::{decode, is_marker_key}`, `replay::{Cells, Markers}`, `Ctx`, `Emitter`.
- Produces: `cli::write::{set, delete, pop, popitem, setdefault}`, each `fn(&mut Ctx) -> Result<i32, Error>`; `store::Row::new(cells: &'a Cells, defaults: &'a [String], fill: bool) -> Row<'a>` (`pub(crate)`).

- [ ] **Step 1: Write the failing end-to-end tests**

Append to `tests/cli.rs`:

```rust
fn file(dir: &Path, name: &str) -> String {
    String::from_utf8(fs::read(dir.join(name)).unwrap()).unwrap()
}

#[test]
fn set_append_delete_write_records() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    assert_eq!(records(dir, &["set", "s.tsvz", "#k", "a\tb", "<", ""]).0, 0);
    assert_eq!(records(dir, &["append", "s.tsvz", "#_defaults_#", "d1"]).0, 0);
    assert_eq!(records(dir, &["set", "s.tsvz", "gone"]).0, 0);
    assert_eq!(records(dir, &["delete", "s.tsvz", "x", "#_defaults_#"]).0, 0);
    assert_eq!(file(dir, "s.tsvz"), "<#>k\ta<sep>b\t<lt>\t\n#_defaults_#\td1\ngone\nx\n#_defaults_#\n");
    assert_eq!(records(dir, &["set", "-v", "s.tsvz", "--", "carol", "-5"]).0, 0);
    assert_eq!(records(dir, &["get", "s.tsvz", "carol"]).1, "carol\t-5\n");
}

#[test]
fn bulk_input() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    let input = b"\xef\xbb\xbfa\tx<LF>y\r\n# comment\n\n#_defaults_#\td\nb\na\nc\t\nunterminated\tz";
    let (code, _, err) = run_in(dir, &["set", "s.tsvz", "-"], input);
    assert_eq!(code, 0);
    assert!(err.contains("unterminated"), "{err}");
    assert_eq!(file(dir, "s.tsvz"), "a\tx<LF>y\n#_defaults_#\td\nb\na\nc\t\n");
    let (code, _, _) = run_in(dir, &["delete", "s.tsvz", "-"], b"c\textra\n#_defaults_#\n# skip\n<#>h\n");
    assert_eq!(code, 0);
    assert!(file(dir, "s.tsvz").ends_with("c\t\nc\n#_defaults_#\n<#>h\n"));
    fs::write(dir.join("c.csvz"), "").unwrap();
    run_in(dir, &["set", "c.csvz", "-"], b"k,a<sep>b\tc\n");
    assert_eq!(records(dir, &["get", "c.csvz", "k"]).1, "k,a<sep>b\tc\n");
}

#[test]
fn pop_popitem_setdefault() {
    let d = store("#_defaults_#\td1\td2\na\t1\nb\t2\nc\t3\n");
    let dir = d.path();
    assert_eq!(records(dir, &["pop", "s.tsvz", "b"]), (0, "b\t2\td2\n".into(), String::new()));
    assert_eq!(records(dir, &["pop", "s.tsvz", "b"]).0, 3);
    assert_eq!(records(dir, &["popitem", "s.tsvz", "first"]).1, "a\t1\td2\n");
    assert_eq!(records(dir, &["popitem", "s.tsvz"]).1, "c\t3\td2\n");
    assert_eq!(records(dir, &["popitem", "s.tsvz"]).0, 3);
    assert_eq!(records(dir, &["setdefault", "s.tsvz", "n", "v "]).1, "n\tv\td2\n");
    assert_eq!(records(dir, &["setdefault", "s.tsvz", "n", "other"]).1, "n\tv\td2\n");
    assert_eq!(records(dir, &["setdefault", "s.tsvz", "#_defaults_#", "x"]).0, 2);
    assert!(file(dir, "s.tsvz").starts_with("#_defaults_#\td1\td2\na\t1\nb\t2\nc\t3\nb\na\nc\nn\tv \n"));
}

#[test]
fn writes_create_missing_stores_and_reads_do_not() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    for (op, args) in [("set", vec!["k", "v"]), ("append", vec!["k", "v"]), ("delete", vec!["k"]), ("setdefault", vec!["k", "v"])] {
        let name = format!("{op}.tsvz");
        let mut all = vec![op, name.as_str()];
        all.extend(args);
        assert_eq!(records(dir, &all).0, 0, "{op}");
        assert!(dir.join(&name).exists(), "{op}");
    }
    for op in ["pop", "popitem"] {
        let name = format!("{op}.tsvz");
        let mut all = vec![op, name.as_str()];
        if op == "pop" {
            all.push("k");
        }
        assert_eq!(records(dir, &all).0, 1, "{op}");
        assert!(!dir.join(&name).exists(), "{op}");
    }
}

#[test]
fn a_stopped_store_is_not_changed_by_pop() {
    let d = store("#_on_corruption_#\tstop\na\t1\n\0\0c\t3\n");
    let before = fs::read(d.path().join("s.tsvz")).unwrap();
    assert_eq!(records(d.path(), &["pop", "s.tsvz", "a"]).0, 1);
    assert_eq!(fs::read(d.path().join("s.tsvz")).unwrap(), before);
}
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test --test cli`
Expected: the new tests fail: the write operations print `not built yet` and exit 1.

- [ ] **Step 3: Make `Row::new` available to the crate**

Add to the `impl<'a> Row<'a>` block in `src/store.rs`:

```rust
    /// A row resolved from `cells` under `defaults` and a fill-empty setting (§14.2, §14.3).
    pub(crate) fn new(cells: &'a Cells, defaults: &'a [String], fill: bool) -> Row<'a> {
        Row { cells, defaults, fill }
    }
```

- [ ] **Step 4: Implement the writing operations**

File: `src/cli/write.rs`

```rust
//! The writing operations: set, append, delete (with bulk input), pop, popitem, setdefault
//! (§20.2, §20.3).

use std::io::Read;
use std::path::Path;

use crate::cli::output::Emitter;
use crate::cli::Ctx;
use crate::escape::{self, is_marker_key};
use crate::frame::Framer;
use crate::replay::{Cells, Markers};
use crate::store::Row;
use crate::write::{Record, Writer};
use crate::{Error, Scope, Warning};

fn writer(ctx: &Ctx, member_size: Option<u64>) -> Result<Writer, Error> {
    let mut w = Writer::open(&ctx.store)?.warnings(Some(ctx.warn.clone())).delimiter(ctx.args.delimiter);
    if let Some(n) = member_size {
        w = w.member_size(n as usize);
    }
    Ok(w)
}

/// `set` and `append` (§20.2): one record, or the records on standard input for `-`.
pub(crate) fn set(ctx: &mut Ctx) -> Result<i32, Error> {
    let records = if ctx.args.args == ["-"] {
        bulk(ctx, false)?
    } else {
        vec![Record::from_args(&ctx.args.args[0], &ctx.args.args[1..])]
    };
    writer(ctx, None)?.append(&records)?;
    ctx.info(&format!("appended {} records", records.len()));
    Ok(0)
}

/// `delete` (§20.2): one tombstone per key (a marker key is reset), or the keys on standard input.
pub(crate) fn delete(ctx: &mut Ctx) -> Result<i32, Error> {
    let records = if ctx.args.args == ["-"] {
        bulk(ctx, true)?
    } else {
        ctx.args.args.iter().map(|k| Record::from_args(k, &[])).collect()
    };
    writer(ctx, None)?.append(&records)?;
    ctx.info(&format!("appended {} tombstones", records.len()));
    Ok(0)
}

/// Standard input read as records in the store's variant (§20.3.1).
fn bulk(ctx: &Ctx, keys_only: bool) -> Result<Vec<Record>, Error> {
    let mut input = Vec::new();
    std::io::stdin().lock().read_to_end(&mut input).map_err(|e| Error::io(Path::new("standard input"), e))?;
    let format = ctx.format;
    let d = format.delimiter;
    let decode = |f: &str| if format.strict { escape::decode(f, d).into_owned() } else { f.to_string() };
    let mut records = Vec::new();
    let mut framer = Framer::new();
    framer.push(&input, &mut |line| {
        let text = String::from_utf8_lossy(line.text());
        if text.is_empty() {
            return true;
        }
        let mut fields = text.split(d as char);
        let first = fields.next().unwrap_or("");
        let marker = format.strict && is_marker_key(first);
        if first.starts_with('#') && !marker {
            return true; // a comment
        }
        if marker {
            let values = if keys_only { Vec::new() } else { fields.map(&decode).collect() };
            records.push(Record::Marker { key: first.to_string(), values });
            return true;
        }
        let key = decode(first);
        if key.is_empty() {
            return true; // a reader ignores an empty key (§8.4)
        }
        let values: Vec<String> = if keys_only { Vec::new() } else { fields.map(&decode).collect() };
        records.push(if values.is_empty() { Record::Tombstone { key } } else { Record::Data { key, values } });
        true
    });
    if !framer.tail().is_empty() {
        (ctx.warn)(&Warning {
            part: "standard input".into(),
            offset: None,
            message: "ignored the unterminated last line".into(),
        });
    }
    Ok(records)
}

fn print(ctx: &mut Ctx, key: &str, cells: &[String]) -> Result<(), Error> {
    let mut out = Emitter::new(ctx);
    out.row(key, cells.iter().map(String::as_str))?;
    out.finish()
}

/// `pop` (§20.2): print KEY's row and append its tombstone; 3 when it is missing.
pub(crate) fn pop(ctx: &mut Ctx) -> Result<i32, Error> {
    let requested = ctx.args.args[0].clone();
    let store = ctx.open(Scope::Keys(vec![requested.clone()]), false)?;
    if store.stopped().is_some() {
        return Ok(1); // no change is based on a read that stopped (§4.10)
    }
    let key = store.lookup_key(&requested).to_string();
    let Some(row) = store.get(&key) else { return Ok(3) };
    let cells = row.to_vec();
    writer(ctx, Some(store.markers().member_size))?.append(&[Record::Tombstone { key: key.clone() }])?;
    print(ctx, &key, &cells)?;
    Ok(0)
}

/// `popitem [first|last]` (§20.2).
pub(crate) fn popitem(ctx: &mut Ctx) -> Result<i32, Error> {
    let store = ctx.open(Scope::All, false)?;
    if store.stopped().is_some() {
        return Ok(1);
    }
    let pick = if ctx.args.args.first().map(String::as_str) == Some("first") { store.first() } else { store.last() };
    let Some((key, row)) = pick else { return Ok(3) };
    let (key, cells) = (key.to_string(), row.to_vec());
    writer(ctx, Some(store.markers().member_size))?.append(&[Record::Tombstone { key: key.clone() }])?;
    print(ctx, &key, &cells)?;
    Ok(0)
}

/// `setdefault KEY VALUE ...` (§20.2): KEY's row when it is live; otherwise append the record
/// and print the row a reader then returns.
pub(crate) fn setdefault(ctx: &mut Ctx) -> Result<i32, Error> {
    let requested = ctx.args.args[0].clone();
    let values = ctx.args.args[1..].to_vec();
    if is_marker_key(&requested) {
        eprintln!("tsvz: setdefault: {requested} is a marker key (§21.7.3)");
        return Ok(2);
    }
    let store = match ctx.open(Scope::Keys(vec![requested.clone()]), false) {
        Ok(store) => Some(store),
        Err(Error::Missing(_)) => None,
        Err(e) => return Err(e),
    };
    if store.as_ref().is_some_and(|s| s.stopped().is_some()) {
        return Ok(1);
    }
    let markers = store.as_ref().map(|s| s.markers().clone()).unwrap_or_else(Markers::default);
    let strip = ctx.format.strict && markers.strip;
    let trim = |s: &str| if strip { s.trim_end_matches([' ', '\t']).to_string() } else { s.to_string() };
    let key = trim(&requested);
    if let Some(row) = store.as_ref().and_then(|s| s.get(&key)) {
        let cells = row.to_vec();
        print(ctx, &key, &cells)?;
        return Ok(0);
    }
    writer(ctx, Some(markers.member_size))?.append(&[Record::Data { key: requested.clone(), values: values.clone() }])?;
    let written = Cells::from_values(values.iter().map(|v| trim(v)));
    let cells = Row::new(&written, &markers.defaults, markers.fill_empty).to_vec();
    print(ctx, &key, &cells)?;
    Ok(0)
}
```

Replace in `src/cli/mod.rs`:

```rust
pub(crate) mod output;
mod read;
```

with:

```rust
pub(crate) mod output;
mod read;
mod write;
```

Replace in `src/cli/mod.rs`:

```rust
            "parts" => read::parts(self),
```

with:

```rust
            "parts" => read::parts(self),
            "set" | "append" => write::set(self),
            "delete" => write::delete(self),
            "pop" => write::pop(self),
            "popitem" => write::popitem(self),
            "setdefault" => write::setdefault(self),
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test`
Expected: everything passes.

- [ ] **Step 6: Run the conformance writer and cli classes**

```bash
cargo build --release
python3 ../TSVZ/tsvzconform.py --impl target/release/tsvz --class writer,cli \
  --feature bzip2,xz,zstd,crc32,crc32c,sha256,blake3,file-first
```

Expected: every FAIL or warn is a `clear` or `scrub` case (Task 13), or `20.4.3-terminal-output-is-a-table` (Task 14). Fix anything else before committing.

- [ ] **Step 7: Commit**

```bash
git add src/cli src/store.rs tests/cli.rs
git commit -m "CLI: set, append, delete with bulk input, pop, popitem and setdefault." -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 13: Maintenance — clear and scrub

**Files:**
- Create: `src/maint.rs`, `src/cli/maint.rs`
- Modify: `src/lib.rs` (add `pub mod maint;`), `src/cli/mod.rs` (dispatch), `tests/cli.rs`

**Interfaces:**
- Consumes: `Store`, `Options`, `Scope`, `parts::{list, Stamp}`, `codec::encode_members`, `lock::lock`, `escape::encode_into`, `replay::{Markers, Policy, DEFAULT_MEMBER_SIZE}`, `Writer`, `Record`.
- Produces:
  - `maint::Outcome` (`Written`, `Unchanged`, `Partial { backup: Option<PathBuf> }`).
  - `maint::clear(store: &Path, options: &Options) -> Result<Outcome, Error>`, `maint::scrub(store: &Path, options: &Options) -> Result<Outcome, Error>`; refusals are `Err(Error::Refused(..))` with nothing written.
  - `maint::snapshot(&Store) -> String` (the §19.2.3 content TSVZ.py writes).
  - `cli::maint::run(ctx: &mut Ctx, clear: bool) -> Result<i32, Error>`.

- [ ] **Step 1: Write the failing tests**

File: `src/maint.rs` (tests first)

```rust
//! In-place maintenance of a one-part store: clear and scrub (§19.8, §19.2.3, §19.4), written
//! the way TSVZ.py writes them.

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::MetadataExt;

    fn opts() -> Options {
        Options::default()
    }

    fn rows(p: &Path) -> Vec<Vec<String>> {
        let s = Store::open(p, opts()).unwrap();
        s.iter().map(|(k, r)| std::iter::once(k.to_string()).chain(r.to_vec()).collect()).collect()
    }

    #[test]
    fn scrub_layout_resolves_rows_and_keeps_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.tsvz");
        fs::write(&p, "#key\tval\n#_version_#\t1\n#_defaults_#\td1\td2\na\tx\nb\ty\n#note\na\tz\nb\nc\t\t\n").unwrap();
        let before = fs::metadata(&p).unwrap();
        assert_eq!(scrub(&p, &opts()).unwrap(), Outcome::Written);
        assert_eq!(fs::read_to_string(&p).unwrap(), "#key\tval\n#_version_#\t1\n#_defaults_#\td1\td2\na\tz\td2\nc\t\t\n");
        let after = fs::metadata(&p).unwrap();
        assert_eq!((after.ino(), after.mode()), (before.ino(), before.mode()));
        assert_eq!(scrub(&p, &opts()).unwrap(), Outcome::Unchanged);
    }

    #[test]
    fn scrub_markers_in_tsvz_py_order() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.tsvz");
        fs::write(&p, "#_on_corruption_#\tskip\n#_member_size_#\t9\n#_write_ack_#\tdisk\n#_rotate_#\trename\n#_return_defaults_when_missing_#\tno\n#_fill_empty_with_default_#\ton\n#_strip_trailing_whites_#\toff\n#_version_#\t7\nk\tv\n").unwrap();
        scrub(&p, &opts()).unwrap();
        assert_eq!(
            fs::read_to_string(&p).unwrap(),
            "#_version_#\t1\n#_strip_trailing_whites_#\tfalse\n#_fill_empty_with_default_#\ttrue\n#_return_defaults_when_missing_#\tfalse\n#_rotate_#\trename\n#_write_ack_#\tdisk\n#_member_size_#\t9\n#_on_corruption_#\tskip\nk\tv\n"
        );
    }

    #[test]
    fn scrub_compensates_when_rows_would_not_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.tsvz");
        fs::write(&p, "#_strip_trailing_whites_#\tfalse\na \tx \n#_strip_trailing_whites_#\ttrue\n#_defaults_#\td\n#_fill_empty_with_default_#\ttrue\n").unwrap();
        let before = rows(&p);
        scrub(&p, &opts()).unwrap();
        assert_eq!(
            fs::read_to_string(&p).unwrap(),
            "#_version_#\t1\n#_strip_trailing_whites_#\tfalse\n#_fill_empty_with_default_#\tfalse\n#_defaults_#\td\na \tx \n#_strip_trailing_whites_#\ttrue\n#_fill_empty_with_default_#\ttrue\n"
        );
        assert_eq!(rows(&p), before);
    }

    #[test]
    fn scrub_refuses_multipart_and_missing_stores() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("s.tsvz"), "a\t1\n").unwrap();
        fs::write(dir.path().join("s.tsvz.1"), "b\t2\n").unwrap();
        assert!(matches!(scrub(&dir.path().join("s.tsvz"), &opts()), Err(Error::Refused(_))));
        assert_eq!(fs::read_to_string(dir.path().join("s.tsvz")).unwrap(), "a\t1\n");
        assert!(matches!(scrub(&dir.path().join("none.tsvz"), &opts()), Err(Error::Missing(_))));
        assert!(!dir.path().join("none.tsvz").exists());
    }

    #[test]
    fn scrub_of_damaged_data_keeps_a_copy() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.tsvz");
        let damaged = "#_on_corruption_#\tstop\na\t1\n\0\0c\t3\nd\t4\n";
        fs::write(&p, damaged).unwrap();
        let Outcome::Partial { backup: Some(copy) } = scrub(&p, &opts()).unwrap() else { panic!("not partial") };
        assert_eq!(fs::read_to_string(&copy).unwrap(), damaged);
        assert!(copy.file_name().unwrap().to_str().unwrap().starts_with("s.tsvz.damaged-"));
        assert_eq!(fs::read_to_string(&p).unwrap(), "#_version_#\t1\n#_on_corruption_#\tstop\na\t1\n");
        assert_eq!(crate::parts::list(&p).unwrap().len(), 1, "the copy is not a part");
    }

    #[test]
    fn clear_keeps_header_and_markers_without_a_version_line() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.tsvz");
        fs::write(&p, "#hdr\n#_version_#\t1\n#_defaults_#\td\n#_strip_trailing_whites_#\tfalse\na\tx\n").unwrap();
        assert_eq!(clear(&p, &opts()).unwrap(), Outcome::Written);
        assert_eq!(fs::read_to_string(&p).unwrap(), "#hdr\n#_strip_trailing_whites_#\tfalse\n#_defaults_#\td\n");
    }

    #[test]
    fn clear_of_multipart_appends_tombstones_and_of_missing_creates() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("s.tsvz"), "#_defaults_#\td\na\t1\n").unwrap();
        fs::write(dir.path().join("s.tsvz.1"), "b\t2\nc\t3\n").unwrap();
        assert_eq!(clear(&dir.path().join("s.tsvz"), &opts()).unwrap(), Outcome::Written);
        assert_eq!(fs::read_to_string(dir.path().join("s.tsvz.1")).unwrap(), "b\t2\nc\t3\na\nb\nc\n");
        let none = dir.path().join("new.tsvz");
        assert_eq!(clear(&none, &opts()).unwrap(), Outcome::Written);
        assert_eq!(fs::read(&none).unwrap(), b"");
    }

    #[test]
    fn clear_refuses_a_stopped_read() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.tsvz");
        fs::write(&p, "#_on_corruption_#\tstop\na\t1\n\0\0\n").unwrap();
        assert!(matches!(clear(&p, &opts()), Err(Error::Refused(_))));
    }

    #[test]
    fn a_changed_part_is_not_rewritten() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.tsvz");
        fs::write(&p, "a\t1\n").unwrap();
        let stale = Stamp::of(&fs::metadata(&p).unwrap());
        fs::write(&p, "a\t1\nb\t2\n").unwrap();
        assert_eq!(rewrite(&p, b"x\n", stale, None, DEFAULT_MEMBER_SIZE).unwrap(), None);
        assert_eq!(fs::read_to_string(&p).unwrap(), "a\t1\nb\t2\n");
    }

    #[test]
    fn scrub_of_a_compressed_part_and_a_loose_file() {
        let dir = tempfile::tempdir().unwrap();
        let gz = dir.path().join("s.tsvz.gz");
        let mut data = crate::codec::encode_member(crate::name::Codec::Gzip, b"a\t1\na\t2\n").unwrap();
        data.extend(crate::codec::encode_member(crate::name::Codec::Gzip, b"b\t3\n").unwrap());
        fs::write(&gz, data).unwrap();
        scrub(&gz, &opts()).unwrap();
        assert_eq!(rows(&gz), vec![vec!["a".to_string(), "2".into()], vec!["b".to_string(), "3".into()]]);
        let tsv = dir.path().join("l.tsv");
        fs::write(&tsv, "#h\na\t1\nb\na\t2\n").unwrap();
        scrub(&tsv, &opts()).unwrap();
        assert_eq!(fs::read_to_string(&tsv).unwrap(), "#h\na\t2\n");
        clear(&tsv, &opts()).unwrap();
        assert_eq!(fs::read_to_string(&tsv).unwrap(), "#h\n");
    }
}
```

Append to `tests/cli.rs`:

```rust
#[test]
fn clear_and_scrub_from_the_command_line() {
    let d = store("#hdr\na\tx\nb\na\ty\n");
    assert_eq!(records(d.path(), &["scrub", "s.tsvz"]), (0, String::new(), String::new()));
    assert_eq!(file(d.path(), "s.tsvz"), "#hdr\n#_version_#\t1\na\ty\n");
    assert_eq!(records(d.path(), &["clear", "s.tsvz"]).0, 0);
    assert_eq!(file(d.path(), "s.tsvz"), "#hdr\n");
    fs::write(d.path().join("s.tsvz.1"), "z\t1\n").unwrap();
    let (code, _, err) = records(d.path(), &["scrub", "s.tsvz"]);
    assert!(code == 1 && err.contains("multi-part"), "{err}");
    assert_eq!(records(d.path(), &["scrub", "none.tsvz"]).0, 1);
    let damaged = store("#_on_corruption_#\tstop\na\t1\n\0\0c\t3\n");
    let (code, _, err) = records(damaged.path(), &["scrub", "s.tsvz"]);
    assert!(code == 1 && err.contains("damaged-"), "{err}");
}
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test --lib maint` (after adding `pub mod maint;` to `src/lib.rs`)
Expected: compile errors: `scrub`, `clear`, `Outcome`, `rewrite` not found.

- [ ] **Step 3: Implement**

Add above the tests in `src/maint.rs`:

```rust
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::escape;
use crate::name::Codec;
use crate::parts::{self, Stamp};
use crate::replay::{Markers, Policy, DEFAULT_MEMBER_SIZE};
use crate::write::{Record, Writer};
use crate::{codec, lock, Error, Options, Scope, Store, Warning};

/// What a clear or scrub did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The part was rewritten (or, for a multi-part `clear`, tombstones were appended).
    Written,
    /// The new content equals the old: nothing was written.
    Unchanged,
    /// The read stopped at damaged data (§4.10): the part was rewritten from what was read and
    /// its original bytes kept in `backup`; the operation still failed (exit 1).
    Partial { backup: Option<PathBuf> },
}

/// Reads a scrub or clear makes before it gives up on a store that keeps changing (§18.8).
const ATTEMPTS: usize = 3;

fn marker_line(out: &mut String, d: u8, key: &str, value: &str) {
    out.push_str(key);
    out.push(d as char);
    escape::encode_into(out, value, d, false);
    out.push('\n');
}

/// Marker lines that re-create `m` (§19.2.3a), in TSVZ.py's order. `compensate` writes stripping
/// and fill-empty off for the rows that follow (§19.4); `version` adds the `#_version_#` line.
fn preamble(out: &mut String, m: &Markers, d: u8, compensate: bool, version: bool) {
    if version {
        marker_line(out, d, "#_version_#", "1");
    }
    if compensate {
        marker_line(out, d, "#_strip_trailing_whites_#", "false");
        marker_line(out, d, "#_fill_empty_with_default_#", "false");
    } else {
        if !m.strip {
            marker_line(out, d, "#_strip_trailing_whites_#", "false");
        }
        if m.fill_empty {
            marker_line(out, d, "#_fill_empty_with_default_#", "true");
        }
    }
    if !m.return_missing {
        marker_line(out, d, "#_return_defaults_when_missing_#", "false");
    }
    if m.rotate != "keep" {
        marker_line(out, d, "#_rotate_#", m.rotate);
    }
    if m.write_ack != "memory" {
        marker_line(out, d, "#_write_ack_#", m.write_ack);
    }
    if m.member_size != DEFAULT_MEMBER_SIZE {
        marker_line(out, d, "#_member_size_#", &m.member_size.to_string());
    }
    if m.on_corruption != Policy::Recover {
        marker_line(out, d, "#_on_corruption_#", m.on_corruption.name());
    }
    if m.defaults.iter().any(|v| !v.is_empty()) {
        out.push_str("#_defaults_#");
        for v in m.defaults.iter() {
            out.push(d as char);
            escape::encode_into(out, v, d, false);
        }
        out.push('\n');
    }
}

/// Whether a written row reads back the same under the final markers (§19.4).
fn round_trips(key: &str, cells: &[String], m: &Markers) -> bool {
    if m.strip && std::iter::once(key).chain(cells.iter().map(String::as_str)).any(|c| c.ends_with([' ', '\t'])) {
        return false;
    }
    !(m.fill_empty && cells.iter().enumerate().any(|(j, c)| c.is_empty() && m.defaults.get(j).is_some_and(|d| !d.is_empty())))
}

/// The snapshot content of §19.2.3 for a store read whole, as TSVZ.py writes it: the header
/// comment, the markers, then one fully resolved row per live key, padded with empty cells to
/// the width of the final defaults; the compensating layout of §19.4 when a row would not read
/// back the same. A loose file keeps its header comment and its rows.
pub fn snapshot(s: &Store) -> String {
    let f = s.format();
    let d = f.delimiter;
    let m = s.markers();
    let rows: Vec<(&str, Vec<String>)> = s
        .iter()
        .map(|(k, r)| {
            let mut cells = r.to_vec();
            if cells.len() < m.defaults.len() {
                cells.resize(m.defaults.len(), String::new());
            }
            (k, cells)
        })
        .collect();
    let mut out = String::new();
    if let Some(h) = s.header() {
        out.push_str(h);
        out.push('\n');
    }
    if !f.strict {
        for (k, cells) in &rows {
            out.push_str(k);
            for c in cells {
                out.push(d as char);
                out.push_str(c);
            }
            out.push('\n');
        }
        return out;
    }
    let compensate = !rows.iter().all(|(k, cells)| round_trips(k, cells, m));
    preamble(&mut out, m, d, compensate, true);
    for (k, cells) in &rows {
        escape::encode_into(&mut out, k, d, true);
        for c in cells {
            out.push(d as char);
            escape::encode_into(&mut out, c, d, false);
        }
        out.push('\n');
    }
    if compensate {
        if m.strip {
            marker_line(&mut out, d, "#_strip_trailing_whites_#", "true");
        }
        if m.fill_empty {
            marker_line(&mut out, d, "#_fill_empty_with_default_#", "true");
        }
    }
    out
}

fn stamp(path: &Path) -> Result<Stamp, Error> {
    std::fs::metadata(path).map(|m| Stamp::of(&m)).map_err(|e| Error::io(path, e))
}

/// Replace a part's bytes in place, keeping the file (§18.7, §19.8), under the write lock and
/// only while the part still has the stamp it had before it was read. Ok(None): another writer
/// changed it; Ok(Some(false)): it already held these bytes.
pub(crate) fn rewrite(path: &Path, content: &[u8], expect: Stamp, codec: Option<Codec>, member_size: u64) -> Result<Option<bool>, Error> {
    let io = |e: io::Error| Error::io(path, e);
    let raw = match codec {
        Some(c) if !content.is_empty() => codec::encode_members(c, content, member_size as usize)
            .ok_or_else(|| Error::UnsupportedCodec { path: path.to_path_buf(), codec: c.name() })?,
        _ => content.to_vec(),
    };
    let file = OpenOptions::new().read(true).write(true).open(path).map_err(io)?;
    let _guard = lock::lock(&file, path).map_err(io)?;
    if Stamp::of(&file.metadata().map_err(io)?) != expect {
        return Ok(None);
    }
    let mut old = Vec::new();
    (&file).read_to_end(&mut old).map_err(io)?;
    if old == raw {
        return Ok(Some(false));
    }
    (&file).seek(SeekFrom::Start(0)).map_err(io)?;
    (&file).write_all(&raw).map_err(io)?;
    file.set_len(raw.len() as u64).map_err(io)?;
    file.sync_all().map_err(io)?;
    Ok(Some(true))
}

fn local_timestamp() -> String {
    // SAFETY: `time` and `localtime_r` only write to the values passed in.
    let tm = unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&now, &mut tm);
        tm
    };
    format!("{:04}{:02}{:02}T{:02}{:02}{:02}", tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday, tm.tm_hour, tm.tm_min, tm.tm_sec)
}

/// Copy a damaged part's bytes to `<part>.damaged-<local time>` (`.1`, `.2`, … when taken) and
/// sync it, before a rewrite drops the damage (§4.10, §16.6), as TSVZ.py does.
fn backup(path: &Path, options: &Options) -> Result<PathBuf, Error> {
    let io = |e: io::Error| Error::io(path, e);
    let file = OpenOptions::new().read(true).write(true).open(path).map_err(io)?;
    let _guard = lock::lock(&file, path).map_err(io)?;
    let first = PathBuf::from(format!("{}.damaged-{}", path.display(), local_timestamp()));
    let mut target = first.clone();
    let mut n = 0;
    let mut copy = loop {
        match OpenOptions::new().write(true).create_new(true).open(&target) {
            Ok(f) => break f,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                n += 1;
                target = PathBuf::from(format!("{}.{n}", first.display()));
            }
            Err(e) => return Err(Error::io(&target, e)),
        }
    };
    let copied = io::copy(&mut &file, &mut copy).and_then(|_| copy.sync_all());
    if let Err(e) = copied {
        let _ = std::fs::remove_file(&target);
        return Err(Error::io(&target, e));
    }
    if let Some(dir) = target.parent().filter(|p| !p.as_os_str().is_empty()) {
        if let Ok(d) = File::open(dir) {
            let _ = d.sync_all();
        }
    }
    if let Some(w) = &options.warnings {
        w(&Warning {
            part: path.to_path_buf(),
            offset: None,
            message: format!("kept the bytes of this damaged part, whose damage is not rewritten, in {}", target.display()),
        });
    }
    Ok(target)
}

fn whole(options: &Options) -> Options {
    Options { scope: Scope::All, verify: false, ..options.clone() }
}

/// Compact a one-part store in place with §19.4 fidelity (§19.8, §20.2 `scrub`).
pub fn scrub(store: &Path, options: &Options) -> Result<Outcome, Error> {
    let mut backed_up: Option<(Stamp, PathBuf)> = None;
    for _ in 0..ATTEMPTS {
        let list = parts::list(store)?;
        if list.is_empty() {
            return Err(Error::Missing(store.to_path_buf()));
        }
        if list.len() > 1 {
            return Err(Error::Refused(format!(
                "{}: scrub skipped: this tsvz does not compact multi-part stores; nothing was written",
                store.display()
            )));
        }
        let before = stamp(&list[0].path)?;
        let s = Store::open(store, whole(options))?;
        if s.parts().len() != 1 {
            continue; // the parts changed under the read
        }
        let part = &s.parts()[0];
        let mut copy = None;
        if s.damaged() || s.stopped().is_some() {
            copy = match &backed_up {
                Some((at, path)) if *at == before => Some(path.clone()),
                _ => {
                    let path = backup(&part.path, options)?;
                    backed_up = Some((before, path.clone()));
                    Some(path)
                }
            };
        }
        let content = snapshot(&s);
        match rewrite(&part.path, content.as_bytes(), before, part.codec, s.markers().member_size)? {
            None => continue, // another writer committed after the read
            Some(written) => {
                return Ok(if s.stopped().is_some() {
                    Outcome::Partial { backup: copy }
                } else if written {
                    Outcome::Written
                } else {
                    Outcome::Unchanged
                });
            }
        }
    }
    Err(Error::Refused(format!(
        "{}: scrub skipped: the store kept changing while it was being compacted; nothing was written",
        store.display()
    )))
}

/// Make the store empty (§20.2 `clear`): rewrite a one-part store keeping its header comment and
/// markers (§19.8); append a tombstone for every live key of a multi-part store (§18.8); create a
/// missing store (§20.2.2).
pub fn clear(store: &Path, options: &Options) -> Result<Outcome, Error> {
    for _ in 0..ATTEMPTS {
        let list = parts::list(store)?;
        if list.is_empty() {
            Writer::open(store)?.warnings(options.warnings.clone()).delimiter(options.delimiter).append(&[])?;
            return Ok(Outcome::Written);
        }
        let before = if list.len() == 1 { Some(stamp(&list[0].path)?) } else { None };
        let s = Store::open(store, whole(options))?;
        if s.stopped().is_some() {
            return Err(Error::Refused(format!(
                "{}: clear skipped: the read stopped at damaged data (§4.10); nothing was written",
                store.display()
            )));
        }
        if s.parts().len() > 1 {
            let tombstones: Vec<Record> = s.iter().map(|(k, _)| Record::Tombstone { key: k.to_string() }).collect();
            if tombstones.is_empty() {
                return Ok(Outcome::Unchanged);
            }
            Writer::open(store)?
                .member_size(s.markers().member_size as usize)
                .warnings(options.warnings.clone())
                .delimiter(options.delimiter)
                .append(&tombstones)?;
            return Ok(Outcome::Written);
        }
        let Some(before) = before else { continue };
        let mut content = String::new();
        if let Some(h) = s.header() {
            content.push_str(h);
            content.push('\n');
        }
        if s.format().strict {
            preamble(&mut content, s.markers(), s.format().delimiter, false, false);
        }
        let part = &s.parts()[0];
        match rewrite(&part.path, content.as_bytes(), before, part.codec, s.markers().member_size)? {
            None => continue,
            Some(true) => return Ok(Outcome::Written),
            Some(false) => return Ok(Outcome::Unchanged),
        }
    }
    Err(Error::Refused(format!(
        "{}: clear skipped: the store kept changing while it was being cleared; nothing was written",
        store.display()
    )))
}
```

File: `src/cli/maint.rs`

```rust
//! `clear` and `scrub` from the command line (§20.2).

use crate::cli::Ctx;
use crate::maint::{self, Outcome};
use crate::{Error, Scope};

pub(crate) fn run(ctx: &mut Ctx, clear: bool) -> Result<i32, Error> {
    let options = ctx.options(Scope::All, false);
    let outcome = if clear { maint::clear(&ctx.store, &options)? } else { maint::scrub(&ctx.store, &options)? };
    match outcome {
        Outcome::Written => ctx.info("rewritten"),
        Outcome::Unchanged => ctx.info("unchanged: nothing to write"),
        Outcome::Partial { backup } => {
            let kept = backup.map(|b| format!("; its original bytes are in {}", b.display())).unwrap_or_default();
            eprintln!("tsvz: {}: the read stopped at damaged data; rewrote what was read{kept}", ctx.store.display());
            return Ok(1);
        }
    }
    Ok(0)
}
```

Replace in `src/cli/mod.rs`:

```rust
pub(crate) mod output;
mod read;
mod write;
```

with:

```rust
mod maint;
pub(crate) mod output;
mod read;
mod write;
```

Replace in `src/cli/mod.rs`:

```rust
            "setdefault" => write::setdefault(self),
```

with:

```rust
            "setdefault" => write::setdefault(self),
            "clear" => maint::run(self, true),
            "scrub" => maint::run(self, false),
```

Replace in `src/cli/mod.rs` (no operation is left unbuilt):

```rust
            other => {
                eprintln!("tsvz: {other}: not built yet"); // replaced in Tasks 12 and 13
                Ok(1)
            }
```

with:

```rust
            other => {
                eprintln!("tsvz: unknown operation {other}");
                Ok(2)
            }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test`
Expected: everything passes.

- [ ] **Step 5: Run the conformance writer and cli classes**

```bash
cargo build --release
python3 ../TSVZ/tsvzconform.py --impl target/release/tsvz --class reader,writer,cli \
  --feature bzip2,xz,zstd,crc32,crc32c,sha256,blake3,file-first
```

Expected: `conformant: reader ✓   writer ✓   cli ✓`; at most a warn for `20.4.3-terminal-output-is-a-table` (Task 14).

- [ ] **Step 6: Commit**

```bash
git add src/maint.rs src/cli src/lib.rs tests/cli.rs
git commit -m "Maintenance: clear and single-part scrub in place, as TSVZ.py writes them; the writer and cli classes pass." -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---
### Task 14: Table output

**Files:**
- Create: `tools/gen_width.py`, `src/cli/width_table.rs` (generated), `src/cli/table.rs`
- Modify: `src/cli/mod.rs`, `src/cli/output.rs`, `src/cli/read.rs`, `tests/cli.rs`

**Interfaces:**
- Consumes: `Ctx.table`, `Emitter`.
- Produces: `cli::table::{char_width(char) -> usize, width(&str) -> usize, escape_cell(&str) -> Cow<str>, rows_table(&[Vec<String>]) -> String, fields_table(header: &[&str], rows: &[Vec<String>], cols: usize) -> String, terminal_columns() -> usize}`; `cli::output::Fields` (`new(&mut Ctx, header: &'static [&'static str])`, `line(&mut self, &[&str]) -> Result<(), Error>`, `finish(self) -> Result<(), Error>`).

The layout copies TSVZ.py's `pretty_format_table` and `_cliTableCells` (`../TSVZ/TSVZ.py`); read them first.

- [ ] **Step 1: Write the width-table generator and generate the table**

File: `tools/gen_width.py`

```python
#!/usr/bin/env python3
"""Generate src/cli/width_table.rs: terminal columns per character by TSVZ.py's rule
(pretty_format_table's char_width), from this Python's unicodedata.

Run from the repository root:  python3 tools/gen_width.py > src/cli/width_table.rs
"""
import sys
import unicodedata


def width(ch):
    if ch < '̀':
        return 1
    if unicodedata.combining(ch) or unicodedata.category(ch) in ('Mn', 'Me', 'Cf') or 'ᅠ' <= ch <= 'ᇿ':
        return 0
    return 2 if unicodedata.east_asian_width(ch) in ('W', 'F') else 1


def ranges(want):
    found, start = [], None
    for cp in range(0x300, 0x110000):
        hit = not 0xD800 <= cp <= 0xDFFF and width(chr(cp)) == want
        if hit and start is None:
            start = cp
        elif not hit and start is not None:
            found.append((start, cp - 1))
            start = None
    if start is not None:
        found.append((start, 0x10FFFF))
    return found


print('// Generated by tools/gen_width.py from Python %s (Unicode %s). Do not edit.'
      % (sys.version.split()[0], unicodedata.unidata_version))
print('// Characters below U+0300 are one column wide and are not listed.')
for name, want in (('ZERO', 0), ('WIDE', 2)):
    print('pub(crate) static %s: &[(u32, u32)] = &[' % name)
    for a, b in ranges(want):
        print('    (0x%X, 0x%X),' % (a, b))
    print('];')
```

Run: `python3 tools/gen_width.py > src/cli/width_table.rs`
Expected: a file starting `// Generated by tools/gen_width.py`, with `ZERO` and `WIDE` range tables.

- [ ] **Step 2: Write the failing table tests**

File: `src/cli/table.rs` (tests first)

```rust
//! The table format (§20.4.2), laid out as TSVZ.py lays it out.

#[cfg(test)]
mod tests {
    use super::*;

    fn v(cells: &[&str]) -> Vec<String> {
        cells.iter().map(|c| c.to_string()).collect()
    }

    #[test]
    fn widths_follow_tsvz_py() {
        for (c, w) in [('A', 1), ('é', 1), ('日', 2), ('\u{301}', 0), ('\u{200b}', 0), ('\u{1160}', 0), ('😀', 2), ('\u{ff21}', 2)] {
            assert_eq!(char_width(c), w, "{c:?}");
        }
        assert_eq!(width("日本a"), 5);
    }

    #[test]
    fn control_characters_are_escaped() {
        assert_eq!(escape_cell("plain"), "plain");
        assert_eq!(escape_cell("a\tb\nc\rd\u{1b}[31m\u{7f}\u{85}\u{202e}\u{2066}"), "a\\tb\\nc\\rd\\x1b[31m\\x7f\\x85\\u202e\\u2066");
        assert_eq!(escape_cell("back\\slash"), "back\\slash");
    }

    #[test]
    fn rows_pad_to_the_widest_and_are_never_shortened() {
        assert_eq!(rows_table(&[v(&["a", "x"]), v(&["bb", "yyy"]), v(&["c"])]), "a  | x  \nbb | yyy\nc  |    \n");
        assert_eq!(rows_table(&[v(&["ab", "c"]), v(&["", ""])]), "ab | c\n---+--\n");
        assert_eq!(rows_table(&[v(&["日本", "x"]), v(&["a", "y"])]), "日本 | x\na    | y\n");
        let long = "z".repeat(500);
        assert_eq!(rows_table(&[v(&["k", &long])]), format!("k | {long}\n"));
        assert_eq!(rows_table(&[]), "");
    }

    #[test]
    fn fields_fit_the_terminal() {
        let rows = [v(&["0", "/very/long/path/name"])];
        let header = ["index", "path"];
        assert_eq!(fields_table(&header, &rows, 80), "index | path                \n------+---------------------\n0     | /very/long/path/name\n");
        assert_eq!(fields_table(&header, &rows, 26), "index|path                \n-----+--------------------\n0    |/very/long/path/name\n");
        assert_eq!(fields_table(&header, &rows, 15), "index|path     \n-----+---------\n0    |/very/l..\n");
    }
}
```

- [ ] **Step 3: Run them to verify they fail**

Add `mod table;` and `mod width_table;` to `src/cli/mod.rs`, then run: `cargo test --lib cli::table`
Expected: compile errors: `char_width`, `width`, `escape_cell`, `rows_table`, `fields_table` not found.

- [ ] **Step 4: Implement the layout**

Add above the tests in `src/cli/table.rs`:

```rust
use std::borrow::Cow;
use std::cmp::Ordering;

use super::width_table::{WIDE, ZERO};

fn in_ranges(table: &[(u32, u32)], cp: u32) -> bool {
    table
        .binary_search_by(|&(a, b)| if b < cp { Ordering::Less } else if a > cp { Ordering::Greater } else { Ordering::Equal })
        .is_ok()
}

/// Terminal columns a character takes, by TSVZ.py's rule: 1 below U+0300; 0 for combining
/// marks, categories Mn, Me and Cf, and U+1160–U+11FF; 2 for East Asian Wide and Fullwidth.
pub(crate) fn char_width(c: char) -> usize {
    let cp = c as u32;
    if cp < 0x300 {
        1
    } else if in_ranges(ZERO, cp) {
        0
    } else if in_ranges(WIDE, cp) {
        2
    } else {
        1
    }
}

pub(crate) fn width(s: &str) -> usize {
    if s.is_ascii() { s.len() } else { s.chars().map(char_width).sum() }
}

fn is_control(c: char) -> bool {
    matches!(c, '\u{0}'..='\u{1f}' | '\u{7f}'..='\u{9f}' | '\u{61c}' | '\u{200e}' | '\u{200f}' | '\u{2028}' | '\u{2029}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

/// A cell as a table shows it: control characters escaped, so no stored value can move the
/// cursor, reorder the line or send the terminal an escape sequence. Backslashes are not
/// escaped: the table is for reading, records are exact.
pub(crate) fn escape_cell(s: &str) -> Cow<'_, str> {
    if !s.chars().any(is_control) {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c if is_control(c) && (c as u32) < 0x100 => out.push_str(&format!("\\x{:02x}", c as u32)),
            c if is_control(c) => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    Cow::Owned(out)
}

/// What fits in `w` columns: the cell, or what fits in `w - 2` columns and `..`.
fn truncate(s: &str, w: usize) -> Cow<'_, str> {
    if width(s) <= w {
        return Cow::Borrowed(s);
    }
    if w < 2 {
        return Cow::Owned(".".repeat(w));
    }
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let cw = char_width(c);
        if used + cw > w - 2 {
            break;
        }
        out.push(c);
        used += cw;
    }
    out.push_str("..");
    Cow::Owned(out)
}

fn pad(out: &mut String, s: &str, w: usize) {
    out.push_str(s);
    for _ in width(s)..w {
        out.push(' ');
    }
}

fn render(header: Option<&[Cow<'_, str>]>, rows: &[Vec<Cow<'_, str>>], widths: &[usize], sep: &str, hsep: &str) -> String {
    let rule = widths.iter().map(|&w| "-".repeat(w)).collect::<Vec<_>>().join(hsep);
    let mut out = String::new();
    if let Some(h) = header {
        for (j, (cell, &w)) in h.iter().zip(widths).enumerate() {
            if j > 0 {
                out.push_str(sep);
            }
            pad(&mut out, cell, w);
        }
        out.push('\n');
        out.push_str(&rule);
        out.push('\n');
    }
    for row in rows {
        if row.iter().all(|c| c.is_empty()) {
            out.push_str(&rule);
        } else {
            for (j, (cell, &w)) in row.iter().zip(widths).enumerate() {
                if j > 0 {
                    out.push_str(sep);
                }
                pad(&mut out, &truncate(cell, w), w);
            }
        }
        out.push('\n');
    }
    out
}

fn escaped<'a>(rows: &'a [Vec<String>], cols: usize) -> Vec<Vec<Cow<'a, str>>> {
    rows.iter().map(|r| (0..cols).map(|j| r.get(j).map_or(Cow::Borrowed(""), |c| escape_cell(c))).collect()).collect()
}

/// Row output (read, get, pop, popitem, setdefault, keys): no header; every row padded to the
/// widest row; each column padded to its widest cell; ` | ` between cells; nothing shortened;
/// an all-empty row drawn as the rule. TSVZ.py's `pretty_format_table(rows, header=False, full=True)`.
pub(crate) fn rows_table(rows: &[Vec<String>]) -> String {
    if rows.is_empty() {
        return String::new();
    }
    let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
    let cells = escaped(rows, cols);
    let widths: Vec<usize> = (0..cols).map(|j| cells.iter().map(|r| width(&r[j])).max().unwrap_or(0)).collect();
    render(None, &cells, &widths, " | ", "-+-")
}

/// `verify` and `parts`: a header and a rule, fitted to `cols` terminal columns: ` | ` between
/// cells when that fits, else `|`, else the columns wider than their header are shortened,
/// most first, and cut cells end in `..`. TSVZ.py's `pretty_format_table([header] + rows)`.
pub(crate) fn fields_table(header: &[&str], rows: &[Vec<String>], cols: usize) -> String {
    let n = header.len();
    let head: Vec<Cow<'_, str>> = header.iter().map(|h| escape_cell(h)).collect();
    let cells = escaped(rows, n);
    let mut widths: Vec<usize> =
        (0..n).map(|j| cells.iter().map(|r| width(&r[j])).chain([width(&head[j])]).max().unwrap_or(0)).collect();
    let total = |widths: &[usize], sep: usize| widths.iter().sum::<usize>() + sep * n.saturating_sub(1);
    if total(&widths, 3) <= cols {
        return render(Some(&head), &cells, &widths, " | ", "-+-");
    }
    if total(&widths, 1) > cols {
        let head_widths: Vec<usize> = head.iter().map(|h| width(h)).collect();
        let mut overflow = total(&widths, 1) - cols;
        let mut order: Vec<usize> = (0..n).collect();
        order.sort_by_key(|&j| std::cmp::Reverse(widths[j].saturating_sub(head_widths[j])));
        for j in order {
            let diff = widths[j].saturating_sub(head_widths[j]);
            if overflow == 0 || diff == 0 {
                continue;
            }
            let cut = diff.min(overflow);
            widths[j] -= cut;
            overflow -= cut;
        }
    }
    render(Some(&head), &cells, &widths, "|", "+")
}

/// The width of the terminal on stdout; `$COLUMNS` when it cannot be read; else 240.
pub(crate) fn terminal_columns() -> usize {
    // SAFETY: TIOCGWINSZ only writes the `winsize` passed in.
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    if unsafe { libc::ioctl(1, libc::TIOCGWINSZ, &mut ws) } == 0 {
        return ws.ws_col as usize;
    }
    std::env::var("COLUMNS").ok().and_then(|c| c.parse().ok()).unwrap_or(240)
}
```

- [ ] **Step 5: Run the table tests to verify they pass**

Run: `cargo test --lib cli::table`
Expected: 4 passed.

- [ ] **Step 6: Use the table in the output**

Replace in `src/cli/mod.rs`:

```rust
    #[allow(dead_code)] // read by the table output (Task 14), which removes this line
    pub table: bool,
```

with:

```rust
    pub table: bool,
```

Replace in `src/cli/output.rs`:

```rust
/// Prints rows in the chosen format.
pub(crate) struct Emitter<'c> {
    ctx: &'c mut Ctx,
    buf: String,
}

impl<'c> Emitter<'c> {
    pub(crate) fn new(ctx: &'c mut Ctx) -> Emitter<'c> {
        Emitter { ctx, buf: String::new() }
    }

    pub(crate) fn row<'a>(&mut self, key: &str, cells: impl Iterator<Item = &'a str>) -> Result<(), Error> {
        let (d, strict) = (self.ctx.format.delimiter, self.ctx.format.strict);
        record(&mut self.ctx.out, &mut self.buf, key, cells, d, strict).map_err(stdout_error)
    }

    pub(crate) fn finish(self) -> Result<(), Error> {
        Ok(())
    }
}
```

with:

```rust
/// Prints rows in the chosen format: records as they come, or a table at the end.
pub(crate) struct Emitter<'c> {
    ctx: &'c mut Ctx,
    buf: String,
    table: Option<Vec<Vec<String>>>,
}

impl<'c> Emitter<'c> {
    pub(crate) fn new(ctx: &'c mut Ctx) -> Emitter<'c> {
        let table = ctx.table.then(Vec::new);
        Emitter { ctx, buf: String::new(), table }
    }

    pub(crate) fn row<'a>(&mut self, key: &str, cells: impl Iterator<Item = &'a str>) -> Result<(), Error> {
        if let Some(rows) = &mut self.table {
            let mut row = vec![key.to_string()];
            row.extend(cells.map(str::to_string));
            rows.push(row);
            return Ok(());
        }
        let (d, strict) = (self.ctx.format.delimiter, self.ctx.format.strict);
        record(&mut self.ctx.out, &mut self.buf, key, cells, d, strict).map_err(stdout_error)
    }

    /// A table is followed by an empty line, as TSVZ.py prints it.
    pub(crate) fn finish(self) -> Result<(), Error> {
        if let Some(rows) = self.table {
            let text = table::rows_table(&rows) + "\n";
            self.ctx.out.write_all(text.as_bytes()).map_err(stdout_error)?;
        }
        Ok(())
    }
}

/// Prints `verify` and `parts` lines: TAB-joined, or a table under `header` (nothing at all
/// when there are no lines).
pub(crate) struct Fields<'c> {
    ctx: &'c mut Ctx,
    header: &'static [&'static str],
    rows: Vec<Vec<String>>,
}

impl<'c> Fields<'c> {
    pub(crate) fn new(ctx: &'c mut Ctx, header: &'static [&'static str]) -> Fields<'c> {
        Fields { ctx, header, rows: Vec::new() }
    }

    pub(crate) fn line(&mut self, values: &[&str]) -> Result<(), Error> {
        if self.ctx.table {
            self.rows.push(values.iter().map(|v| v.to_string()).collect());
            return Ok(());
        }
        fields(&mut self.ctx.out, values).map_err(stdout_error)
    }

    pub(crate) fn finish(self) -> Result<(), Error> {
        if self.ctx.table && !self.rows.is_empty() {
            let text = table::fields_table(self.header, &self.rows, table::terminal_columns()) + "\n";
            self.ctx.out.write_all(text.as_bytes()).map_err(stdout_error)?;
        }
        Ok(())
    }
}
```

Replace in `src/cli/output.rs`:

```rust
use crate::cli::Ctx;
```

with:

```rust
use crate::cli::{table, Ctx};
```

Replace in `src/cli/read.rs`:

```rust
use crate::cli::output::{fields, stdout_error, Emitter};
```

with:

```rust
use crate::cli::output::{stdout_error, Emitter, Fields};
```

Replace in `src/cli/read.rs`:

```rust
    let store = ctx.open(Scope::KeysOnly, true)?;
    for m in store.mismatches() {
        let part = m.part.display().to_string();
        fields(&mut ctx.out, &[&part, &m.line.to_string(), m.algo, &m.expected, &m.computed]).map_err(stdout_error)?;
    }
    Ok(status(&store, if store.mismatches().is_empty() { 0 } else { 4 }))
```

with:

```rust
    let store = ctx.open(Scope::KeysOnly, true)?;
    let mut out = Fields::new(ctx, &["path", "line", "algorithm", "expected", "computed"]);
    for m in store.mismatches() {
        let part = m.part.display().to_string();
        out.line(&[&part, &m.line.to_string(), m.algo, &m.expected, &m.computed])?;
    }
    out.finish()?;
    Ok(status(&store, if store.mismatches().is_empty() { 0 } else { 4 }))
```

Replace in `src/cli/read.rs`:

```rust
    let last = list.len() - 1;
    for (i, part) in list.iter().enumerate() {
        let mut flags = Vec::new();
        if i == last {
            flags.push("active");
        }
        if let Some(codec) = part.codec {
            flags.push(codec.name());
        }
        let path = part.path.display().to_string();
        fields(&mut ctx.out, &[&i.to_string(), part.ordinal.as_deref().unwrap_or(""), &path, &flags.join(",")]).map_err(stdout_error)?;
    }
    Ok(0)
```

with:

```rust
    let last = list.len() - 1;
    let mut out = Fields::new(ctx, &["index", "ordinal", "path", "flags"]);
    for (i, part) in list.iter().enumerate() {
        let mut flags = Vec::new();
        if i == last {
            flags.push("active");
        }
        if let Some(codec) = part.codec {
            flags.push(codec.name());
        }
        let path = part.path.display().to_string();
        out.line(&[&i.to_string(), part.ordinal.as_deref().unwrap_or(""), &path, &flags.join(",")])?;
    }
    out.finish()?;
    Ok(0)
```

Append to `tests/cli.rs`:

```rust
#[test]
fn table_format_on_request() {
    let d = store("alpha\tA\t1\nbeta\tB<sep>x\t2\n");
    let (code, out, _) = run_in(d.path(), &["--format", "table", "read", "s.tsvz"], b"");
    assert_eq!((code, out.as_str()), (0, "alpha | A    | 1\nbeta  | B\\tx | 2\n\n"));
    let (_, empty, _) = run_in(d.path(), &["--format", "table", "verify", "s.tsvz"], b"");
    assert_eq!(empty, "");
}
```

- [ ] **Step 7: Run every test and the conformance classes**

```bash
cargo test && cargo clippy --all-targets -- -D warnings
cargo build --release
python3 ../TSVZ/tsvzconform.py --impl target/release/tsvz --class reader,writer,cli \
  --feature bzip2,xz,zstd,crc32,crc32c,sha256,blake3,file-first
```

Expected: tests pass, no clippy warnings, `conformant: reader ✓ writer ✓ cli ✓` with no FAIL and no warn.

- [ ] **Step 8: Commit**

```bash
git add tools/gen_width.py src/cli tests/cli.rs
git commit -m "Table output laid out as TSVZ.py lays it out, with control characters escaped." -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 15: Loose files end to end

**Files:**
- Modify: `tests/cli.rs`

**Interfaces:**
- Consumes: the CLI as built in Tasks 10–14 (loose reading is Task 8's `loose_line`, loose writing Task 11's `encode_record`, loose maintenance Task 13's `snapshot`).
- Produces: nothing new; this task pins design section 5.6 end to end.

- [ ] **Step 1: Write the tests**

Append to `tests/cli.rs`:

```rust
#[test]
fn loose_files_by_the_simple_rule() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    fs::write(dir.join("s.csv"), "#name,age\nalice,30\nbob,40\nbob\ncarol,<lt>\n").unwrap();
    assert_eq!(records(dir, &["read", "s.csv"]).1, "alice,30\ncarol,<lt>\n");
    let (code, _, err) = records(dir, &["set", "s.csv", "dave", "a,b"]);
    assert!(code == 1 && err.contains("loose file"), "{err}");
    assert_eq!(records(dir, &["set", "s.csv", "dave", "41"]).0, 0);
    assert_eq!(records(dir, &["verify", "s.csv"]), (0, String::new(), String::new()));
    assert_eq!(records(dir, &["parts", "s.csv"]).1, "0\t\ts.csv\tactive\n");
    assert_eq!(records(dir, &["scrub", "s.csv"]).0, 0);
    assert_eq!(file(dir, "s.csv"), "#name,age\nalice,30\ncarol,<lt>\ndave,41\n");
    assert_eq!(records(dir, &["clear", "s.csv"]).0, 0);
    assert_eq!(file(dir, "s.csv"), "#name,age\n");
}

#[test]
fn the_delimiter_option() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    fs::write(dir.join("t.tsv"), "k;v\n").unwrap();
    assert_eq!(records(dir, &["-d", ";", "get", "t.tsv", "k"]).1, "k;v\n");
    assert_eq!(records(dir, &["-d", ";", "set", "t.tsv", "j", "w\tx"]).0, 0);
    assert_eq!(file(dir, "t.tsv"), "k;v\nj;w\tx\n");
    fs::write(dir.join("s.tsvz"), "k\tv\n").unwrap();
    let (code, out, err) = records(dir, &["-d", ";", "get", "s.tsvz", "k"]);
    assert_eq!((code, out.as_str()), (0, "k\tv\n"));
    assert!(err.contains("-d ignored"), "{err}");
    let (_, _, quiet) = records(dir, &["-q", "-d", ";", "get", "s.tsvz", "k"]);
    assert_eq!(quiet, "");
}
```

- [ ] **Step 2: Run them**

Run: `cargo test --test cli loose the_delimiter`
Expected: 2 passed. A failure here is a bug in Task 8, 11 or 13's loose handling: fix it there, keeping design section 5.6's rule.

- [ ] **Step 3: Commit**

```bash
git add tests/cli.rs
git commit -m "Tests: loose files and -d end to end (design section 5.6)." -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 16: Following the files — `Store::refresh`

**Files:**
- Modify: `src/store.rs`, `src/lib.rs` (add `Refreshed` to the `pub use store::{…}` line)

**Interfaces:**
- Consumes: `Follow` (Task 9), `Framer::resume`, `parts::{list, Stamp}`.
- Produces: `store::Refreshed` (`Unchanged`, `Followed`, `Reloaded`); `Store::refresh(&mut self) -> Result<Refreshed, Error>`.

Design section 3.7 gives the rules; TSVZ.py's `_ServeStore.sync` and `_follow` (`../TSVZ/TSVZ.py`) are the reference.

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module in `src/store.rs`:

```rust
    fn append(p: &Path, data: &[u8]) {
        use std::io::Write;
        fs::OpenOptions::new().append(true).open(p).unwrap().write_all(data).unwrap();
    }

    fn keys(s: &Store) -> Vec<String> {
        s.iter().map(|(k, _)| k.to_string()).collect()
    }

    #[test]
    fn refresh_follows_appends_and_notices_nothing_new() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(dir.path(), "s.tsvz", b"a\t1\n");
        let mut s = Store::open(&p, Options::default()).unwrap();
        assert_eq!(s.refresh().unwrap(), Refreshed::Unchanged);
        append(&p, b"b\t2\nc\t");
        assert_eq!(s.refresh().unwrap(), Refreshed::Followed);
        assert_eq!(keys(&s), vec!["a", "b"]);
        append(&p, b"3\na\n");
        assert_eq!(s.refresh().unwrap(), Refreshed::Followed);
        assert_eq!(keys(&s), vec!["b", "c"]);
        assert_eq!(s.get("c").unwrap().to_vec(), vec!["3".to_string()]);
    }

    #[test]
    fn an_in_place_rewrite_then_appends_past_the_old_size_reloads() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(dir.path(), "s.tsvz", b"a\t1\nb\t2\n");
        let mut s = Store::open(&p, Options::default()).unwrap();
        crate::maint::clear(&p, &Options::default()).unwrap();
        append(&p, b"x\t9\ny\t8\nz\t7\n");
        assert!(fs::metadata(&p).unwrap().len() > 8);
        assert_eq!(s.refresh().unwrap(), Refreshed::Reloaded);
        assert_eq!(keys(&s), vec!["x", "y", "z"]);
    }

    #[test]
    fn new_and_removed_parts_reload() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(dir.path(), "s.tsvz", b"a\t1\n");
        let mut s = Store::open(&p, Options::default()).unwrap();
        write(dir.path(), "s.tsvz.1", b"b\t2\n");
        assert_eq!(s.refresh().unwrap(), Refreshed::Reloaded);
        assert_eq!(keys(&s), vec!["a", "b"]);
        fs::remove_file(dir.path().join("s.tsvz.1")).unwrap();
        assert_eq!(s.refresh().unwrap(), Refreshed::Reloaded);
        assert_eq!(keys(&s), vec!["a"]);
    }

    #[test]
    fn compressed_and_stopped_stores_reload() {
        let dir = tempfile::tempdir().unwrap();
        let gz = dir.path().join("s.tsvz.gz");
        fs::write(&gz, crate::codec::encode_member(crate::name::Codec::Gzip, b"a\t1\n").unwrap()).unwrap();
        let mut s = Store::open(&gz, Options::default()).unwrap();
        append(&gz, &crate::codec::encode_member(crate::name::Codec::Gzip, b"b\t2\n").unwrap());
        assert_eq!(s.refresh().unwrap(), Refreshed::Reloaded);
        assert_eq!(keys(&s), vec!["a", "b"]);
        let p = write(dir.path(), "t.tsvz", b"#_on_corruption_#\tstop\na\t1\n\0\0\n");
        let mut t = Store::open(&p, Options::default()).unwrap();
        append(&p, b"b\t2\n");
        assert_eq!(t.refresh().unwrap(), Refreshed::Reloaded);
        assert!(t.stopped().is_some());
    }

    #[test]
    fn a_header_written_after_opening_an_empty_store_is_seen() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(dir.path(), "s.tsvz", b"");
        let mut s = Store::open(&p, Options::default()).unwrap();
        append(&p, b"#hdr\nk\tv\n");
        assert_eq!(s.refresh().unwrap(), Refreshed::Followed);
        assert_eq!(s.header(), Some("#hdr"));
    }
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test --lib store`
Expected: compile errors: `refresh`, `Refreshed` not found.

- [ ] **Step 3: Implement**

Replace in `src/store.rs`:

```rust
#[allow(dead_code)] // the directory fields are read by `refresh` (Task 16), which removes this line
pub(crate) struct Follow {
```

with:

```rust
pub(crate) struct Follow {
```

Replace in `src/store.rs`:

```rust
use std::io::Read;
```

with:

```rust
use std::io::{Read, Seek, SeekFrom};
```

Add to `src/store.rs`, after the `Follow` struct:

```rust
/// What `Store::refresh` did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refreshed {
    /// Nothing changed.
    Unchanged,
    /// Records appended to the last part were replayed.
    Followed,
    /// Something else changed: the store was read again.
    Reloaded,
}
```

Add to the `impl Store` block in `src/store.rs`:

```rust
    /// Catch up with the files (design section 3.7, TSVZ.py's `sync`): replay what was appended
    /// to the last part since the last read; read the whole store again when anything else
    /// changed — the part list, an earlier part, a compressed last part, a stopped read, or an
    /// in-place rewrite (the bytes before the old read offset no longer match).
    pub fn refresh(&mut self) -> Result<Refreshed, Error> {
        let dir = dir_mtime(&self.path);
        if dir != self.follow.dir_mtime || self.follow.dir_racy {
            let now = parts::list(&self.path)?;
            if now.iter().map(|p| &p.path).ne(self.parts.iter().map(|p| &p.path)) {
                return self.reload();
            }
            self.follow.dir_mtime = dir;
            self.follow.dir_racy = racy(dir);
        }
        let mut followed = false;
        for i in 0..self.parts.len() {
            let stamp = match std::fs::metadata(&self.parts[i].path) {
                Ok(meta) => Stamp::of(&meta),
                Err(_) => return self.reload(),
            };
            if self.follow.stamps.get(i).copied().flatten() == Some(stamp) {
                continue;
            }
            if i + 1 != self.parts.len() || !self.follow_append(stamp)? {
                return self.reload();
            }
            followed = true;
        }
        Ok(if followed { Refreshed::Followed } else { Refreshed::Unchanged })
    }

    fn reload(&mut self) -> Result<Refreshed, Error> {
        *self = Store::open(&self.path, self.options.clone())?;
        Ok(Refreshed::Reloaded)
    }

    /// Replay what was appended to the last part; false when it changed some other way.
    fn follow_append(&mut self, stamp: Stamp) -> Result<bool, Error> {
        let last = self.parts.len() - 1;
        let path = self.parts[last].path.clone();
        let Some(old) = self.follow.stamps[last] else { return Ok(false) };
        let offset = self.follow.offset;
        let want = offset.min(CHECK_BYTES) as usize;
        if self.replay.stopped.is_some()
            || self.parts[last].codec.is_some()
            || stamp.ino != old.ino
            || stamp.size < offset
            || stamp.size <= old.size
            || self.follow.check.len() != want
        {
            return Ok(false); // an append only ever grows the same file
        }
        let io = |e: std::io::Error| Error::io(&path, e);
        let file = File::open(&path).map_err(io)?;
        let mut check = vec![0u8; want];
        if file.read_exact_at(&mut check, offset - want as u64).is_err() || check != self.follow.check {
            return Ok(false); // rewritten in place (§19.8)
        }
        let mut data = Vec::new();
        (&file).seek(SeekFrom::Start(offset)).map_err(io)?;
        (&file).read_to_end(&mut data).map_err(io)?;
        let mut framer = Framer::resume(offset, self.follow.line);
        let (replay, table) = (&mut self.replay, &mut self.table);
        replay.begin_part(&path, last == 0 && offset == 0);
        framer.push(&data, &mut |l| replay.line(l, table));
        let committed = (framer.committed() - offset) as usize;
        if self.replay.stopped.is_none() && !framer.tail().is_empty() && framer.tail() != self.follow.tail.as_slice() {
            self.replay.tail(framer.tail().len());
        }
        let mut check = std::mem::take(&mut self.follow.check);
        check.extend_from_slice(&data[..committed.min(data.len())]);
        let cut = check.len().saturating_sub(CHECK_BYTES as usize);
        check.drain(..cut);
        self.follow.check = check;
        self.follow.offset = framer.committed();
        self.follow.line = framer.number();
        self.follow.tail = framer.tail().to_vec();
        self.follow.stamps[last] = Some(Stamp { size: offset + data.len() as u64, ..stamp });
        Ok(true)
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib store && cargo clippy --all-targets -- -D warnings`
Expected: all store tests pass (15), no warnings.

- [ ] **Step 5: Commit**

```bash
git add src/store.rs src/lib.rs
git commit -m "Store::refresh: follow appends, reload on any other change, including an in-place rewrite." -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---
### Task 17: Working together with TSVZ.py

**Files:**
- Create: `tests/interop.rs`

**Interfaces:**
- Consumes: the `tsvz` binary; `python3` and `../TSVZ/TSVZ.py` (or the path in `TSVZ_PY`). The tests are skipped, with a note on stderr, when either is missing.
- Produces: nothing new; this task pins design section 6.1.

- [ ] **Step 1: Write the tests**

File: `tests/interop.rs`

```rust
//! Working on the same stores as TSVZ.py: writers exclude each other, and reads agree.
//! Needs python3 and TSVZ.py (`TSVZ_PY`, default `../TSVZ/TSVZ.py`); skipped without them.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

fn tsvz_py() -> Option<PathBuf> {
    let path = std::env::var_os("TSVZ_PY")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../TSVZ/TSVZ.py"));
    let python = Command::new("python3").arg("-V").output().is_ok_and(|o| o.status.success());
    (python && path.is_file()).then_some(path)
}

macro_rules! need_tsvz_py {
    () => {
        match tsvz_py() {
            Some(path) => path,
            None => {
                eprintln!("skipped: python3 or TSVZ.py not found (set TSVZ_PY)");
                return;
            }
        }
    };
}

fn run(mut command: Command, stdin: &[u8]) -> Output {
    let mut child = command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(stdin).unwrap();
    child.wait_with_output().unwrap()
}

fn rust(args: &[&str], stdin: &[u8]) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_tsvz"));
    c.args(["--format", "records", "-q"]).args(args);
    run(c, stdin)
}

fn python(py: &Path, args: &[&str], stdin: &[u8]) -> Output {
    let mut c = Command::new("python3");
    c.arg(py).args(["--format", "records", "-q"]).args(args);
    run(c, stdin)
}

/// Lines with trailing empty cells removed (§3.6: a reader may pad).
fn rows(out: &[u8], d: u8) -> Vec<Vec<u8>> {
    out.split(|&b| b == b'\n')
        .map(|l| {
            let mut l = l.to_vec();
            while l.last() == Some(&d) {
                l.pop();
            }
            l
        })
        .collect()
}

#[test]
fn rust_and_python_writers_never_interleave() {
    let py = need_tsvz_py!();
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("s.tsvz");
    let value = "v".repeat(3000);
    let threads: Vec<_> = (0..8)
        .map(|w| {
            let (py, store, value) = (py.clone(), store.clone(), value.clone());
            std::thread::spawn(move || {
                let who = if w % 2 == 0 { "rs" } else { "py" };
                for round in 0..5 {
                    let batch: String = (0..20).map(|i| format!("{who}{w}-{round}-{i}\t{value}\n")).collect();
                    let s = store.to_str().unwrap();
                    let out = if who == "rs" {
                        rust(&["set", s, "-"], batch.as_bytes())
                    } else {
                        python(&py, &["set", s, "-"], batch.as_bytes())
                    };
                    assert!(out.status.success(), "{who}: {}", String::from_utf8_lossy(&out.stderr));
                }
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    let text = std::fs::read_to_string(&store).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 8 * 5 * 20);
    for line in lines {
        let (key, v) = line.split_once('\t').unwrap_or_else(|| panic!("torn line: {line:.80}"));
        assert!((key.starts_with("rs") || key.starts_with("py")) && v == value, "torn line: {line:.80}");
    }
}

#[test]
fn rust_waits_while_python_holds_the_lock() {
    let _py = need_tsvz_py!();
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("s.tsvz");
    std::fs::write(&store, "a\t1\n").unwrap();
    let mut holder = Command::new("python3")
        .arg("-c")
        .arg("import fcntl, sys, time\nf = open(sys.argv[1], 'ab')\nfcntl.lockf(f, fcntl.LOCK_EX)\nprint('locked', flush=True)\ntime.sleep(1.5)\n")
        .arg(&store)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(holder.stdout.as_mut().unwrap()).read_line(&mut line).unwrap();
    assert_eq!(line.trim(), "locked");
    let started = Instant::now();
    let out = rust(&["set", store.to_str().unwrap(), "b", "2"], b"");
    assert!(out.status.success());
    assert!(started.elapsed() >= Duration::from_millis(1000), "set did not wait for the lock: {:?}", started.elapsed());
    holder.wait().unwrap();
    assert_eq!(std::fs::read_to_string(&store).unwrap(), "a\t1\nb\t2\n");
}

#[test]
fn stores_written_by_one_read_the_same_in_the_other() {
    let py = need_tsvz_py!();
    let dir = tempfile::tempdir().unwrap();
    let input = "#_defaults_#\td1\td2\n<#>h\tv<lt>w\ta<sep>b\nk\t\tx\nk2\nz\t1\n#_fill_empty_with_default_#\ttrue\ne\t\t\n";
    for (writer, name) in [("rs", "a.tsvz"), ("py", "b.tsvz"), ("rs", "c.tsvz.gz"), ("py", "d.tsvz.gz"), ("py", "e.csvz")] {
        let path = dir.path().join(name);
        let s = path.to_str().unwrap();
        let (input, d) = if name.ends_with(".csvz") { (input.replace('\t', ","), b',') } else { (input.to_string(), b'\t') };
        let w = if writer == "rs" { rust(&["set", s, "-"], input.as_bytes()) } else { python(&py, &["set", s, "-"], input.as_bytes()) };
        assert!(w.status.success(), "{name}: {}", String::from_utf8_lossy(&w.stderr));
        for op in [&["read"][..], &["get", "k", "missing", "e"], &["len"], &["keys"]] {
            let mut args = vec![op[0], s];
            args.extend(&op[1..]);
            let (a, b) = (rust(&args, b""), python(&py, &args, b""));
            assert_eq!((a.status.code(), rows(&a.stdout, d)), (b.status.code(), rows(&b.stdout, d)), "{name}: {op:?}");
        }
    }
}
```

- [ ] **Step 2: Run them**

Run: `cargo test --test interop`
Expected: 3 passed (no "skipped" note on stderr when `../TSVZ` is present). A torn line means the locks do not exclude each other: check that `lock.rs` takes the lock on the same open file that is written, and that Linux uses `F_OFD_SETLKW` (38).

- [ ] **Step 3: Commit**

```bash
git add tests/interop.rs
git commit -m "Tests: Rust and TSVZ.py writers exclude each other, and stores read the same in both." -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 18: Conformance acceptance and the silent-choices list

**Files:**
- Create: `docs/silent-choices.md`

**Interfaces:**
- Consumes: the whole crate.
- Produces: `docs/silent-choices.md`, which later tasks and the README link to.

- [ ] **Step 1: Run the full acceptance command**

```bash
cargo build --release
python3 ../TSVZ/tsvzconform.py --impl target/release/tsvz --class reader,writer,cli \
  --feature bzip2,xz,zstd,crc32,crc32c,sha256,blake3,file-first --json /tmp/tsvz-rs-conformance.json
```

Run it once more under Python 3.14 or later (for example `uv run --no-project --python 3.14 python ../TSVZ/tsvzconform.py …`) so the zstd cases that need the runner to build zstd fixtures run too.
Expected: both report `conformant: reader ✓   writer ✓   cli ✓` with no FAIL and no warn. A FAIL report names the clause and gives a rerun command; fix the code, add a unit test that pins the fix, and rerun.

- [ ] **Step 2: Write the silent-choices list**

File: `docs/silent-choices.md`

```markdown
# Where tsvz-spec-v1 is silent

tsvz-rs follows `tsvz-spec-v1.md` wherever it decides. Where it leaves a choice open, tsvz-rs
does what TSVZ.py does, so the two tools agree in practice. Each choice is listed here with the
TSVZ.py code it copies. The differential harness (`difftest/`) finds new ones.

| Choice | tsvz-rs (as TSVZ.py) | TSVZ.py reference |
|---|---|---|
| `parts` ordinal | Printed as written in the file name (`002`, `B`), not normalized | `_partRows` |
| xz parts | A member may be an `.xz` stream or a legacy `.lzma` stream (auto-detected) | `lzma.LZMADecompressor()` |
| zstd skippable frames | Decoded as frames with no output | `ZstdDecompressor` |
| Appending after a torn tail | The uncommitted bytes after the last newline are cut first, with a warning, so the new record cannot join them | `_lockedAppend(…, 'truncate')` |
| `scrub` markers | `#_version_#` written as `1`; then strip, fill-empty, return-defaults, rotate, write-ack, member-size and on-corruption when not default; `#_defaults_#` last | `_specPreamble` |
| `scrub` rows | Fully resolved, padded with empty cells to the final defaults' width; the §19.4 compensating layout only when a row would not read back the same | `_specScrubCells`, `_specRoundTrips` |
| `scrub` and checksum markers | Not kept: a snapshot has no segments to verify | `_specScrub` |
| `clear` of one part | Header comment and markers, without `#_version_#` | `_specClearTabularFile` |
| A damaged part before a rewrite | Copied to `<part>.damaged-<YYYYmmddTHHMMSS>` (`.1`, `.2`, … when taken) and synced | `_specBackupPart` |
| The table format | `pretty_format_table`: rows never shortened; `verify`/`parts` fitted to the terminal; control characters escaped | `_cliEmitRows`, `_cliEmitFields`, `_cliTableCells` |
| Long options | A unique prefix is accepted (`--delim`), as argparse did | `_cliParseArgs` |
| `setdefault` with a marker key | Usage error (2), as §21.7.3 asks of a handler | `_ServeStore` |
| Bulk input | Lines with an empty key are skipped (a reader would ignore them) | `_cliStdinBatch` |
| Loose files | Design section 5.6's simple rule; TSVZ.py uses its 3.39 rules instead (§20.2.4 allows both) | — |
| `-V` | `tsvz-rs 0.1.0` | — |

## Where TSVZ.py departs from the spec

Found by the differential harness; tsvz-rs follows the spec. Each is in the harness's `KNOWN`
list until TSVZ.py is fixed.

| Difference | Spec | tsvz-rs | TSVZ.py 4.5 |
|---|---|---|---|
| Exit status of `len` and `keys` when the read stopped at damaged data | §4.10, §20.6: the read failed, exit 1 | 1 | 0 |
```

- [ ] **Step 3: Commit**

```bash
git add docs/silent-choices.md
git commit -m "Docs: where the spec is silent, what tsvz-rs does (as TSVZ.py), and where TSVZ.py departs." -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 19: The differential harness

**Files:**
- Create: `difftest/difftest.py`

**Interfaces:**
- Consumes: `target/release/tsvz`, `../TSVZ/TSVZ.py`.
- Produces: `python3 difftest/difftest.py --rust BIN [--tsvz PATH] [--seeds A:B] [--keep DIR]`, exit 0 when every seed agrees.

- [ ] **Step 1: Write the harness**

File: `difftest/difftest.py`

```python
#!/usr/bin/env python3
"""Differential test: random stores must read the same in tsvz-rs and in TSVZ.py.

Each seed builds a store from random lines (markers valid and invalid, tokens and bare `<`,
CRLF, zero fill, invalid UTF-8, tombstones, empty keys, checksum segments right and wrong,
a byte-order mark, a torn tail), split over one to three parts, some of them gzip parts with
members cut short, corrupted or padded. Both tools then answer the same reads; rows are
compared with trailing empty cells removed (§3.6 lets a reader pad), everything else exactly.

usage: python3 difftest/difftest.py --rust target/release/tsvz [--tsvz ../TSVZ/TSVZ.py]
                                    [--seeds 0:200] [--keep DIR]
Exit status: 0 when every seed agrees, 1 when one does not (its files are kept).
"""
import argparse
import gzip
import os
import random
import shutil
import subprocess
import sys
import tempfile
import zlib

DELIMITERS = {'tsvz': '\t', 'csvz': ',', 'psvz': '|', 'nsvz': '\0'}
TOKENS = ['<sep>', '<LF>', '<CR>', '<NUL>', '<lt>', '<#>', '<TAB>', '<', '<lf>']
KEYS = ['a', 'b', 'c', 'k1', 'k2', '<#>x', ' sp', 'tr ', 'ü', '']
MARKERS = [
    ('#_defaults_#', lambda r: [cell(r) for _ in range(r.randint(0, 3))]),
    ('#_strip_trailing_whites_#', lambda r: [r.choice(['true', 'false', 'maybe', ''])]),
    ('#_fill_empty_with_default_#', lambda r: [r.choice(['true', 'false', 'yes', 'no'])]),
    ('#_return_defaults_when_missing_#', lambda r: [r.choice(['true', 'false'])]),
    ('#_version_#', lambda r: [r.choice(['1', '2', '0', 'x'])]),
    ('#_on_corruption_#', lambda r: [r.choice(['recover', 'skip', 'stop', 'recover'])]),
    ('#__ext__#', lambda r: ['v']),
    ('#_DEFAULTS_#', lambda r: ['up']),
]


def cell(rng):
    parts = [rng.choice(['x', 'yy', 'ü', ' ', '', '#', '|', ',']) for _ in range(rng.randint(0, 3))]
    if rng.random() < 0.3:
        parts.insert(rng.randint(0, len(parts)), rng.choice(TOKENS))
    text = ''.join(parts)
    return text + '  ' if rng.random() < 0.1 else text


def line(rng, d):
    r = rng.random()
    if r < 0.08:
        return rng.choice(['#comment', '#name' + d + 'age', '#'])
    if r < 0.22:
        key, values = rng.choice(MARKERS)
        return d.join([key] + values(rng))
    key = rng.choice(KEYS)
    if rng.random() < 0.15:
        return key
    return d.join([key] + [cell(rng) for _ in range(rng.randint(1, 3))])


def body(rng, d, n, checksums):
    lines = [line(rng, d).encode('utf-8') for _ in range(n)]
    out = []
    for raw in lines:
        r = rng.random()
        if r < 0.03:
            raw = b'\0\0' + raw  # zero fill
        elif r < 0.05:
            raw += b'\xff\xfe'  # invalid UTF-8
        out.append(raw + (b'\r\n' if rng.random() < 0.1 else b'\n'))
    if checksums and out:
        # crc32 segments: the first marker arms; later ones carry the right digest, or a wrong one.
        marks = sorted(rng.sample(range(len(out) + 1), min(3, len(out) + 1)))
        with_marks, segment, armed = [], b'', False
        for i, raw in enumerate(out + [None]):
            if i in marks:
                value = 'ignored'
                if armed:
                    value = '%08x' % (zlib.crc32(segment) & 0xffffffff) if rng.random() < 0.7 else '00000000'
                with_marks.append(('#_checksum_crc32_#' + d + value + '\n').encode())
                segment, armed = b'', True
            if raw is not None:
                with_marks.append(raw)
                segment += raw
        out = with_marks
    data = b''.join(out)
    if rng.random() < 0.05:
        data = b'\xef\xbb\xbf' + data
    if rng.random() < 0.2:
        data += b'torn' + d.encode()
    return data


def gzip_part(rng, data):
    cuts = sorted(rng.sample(range(len(data) + 1), min(2, len(data) + 1)))
    out, prev = b'', 0
    for cut in cuts + [len(data)]:
        member = gzip.compress(data[prev:cut], mtime=0)
        prev = cut
        r = rng.random()
        if r < 0.1:
            member = member[:rng.randint(10, len(member))]  # cut short
        elif r < 0.2:
            b = bytearray(member)
            b[rng.randrange(len(b))] ^= 0xff  # corrupt
            member = bytes(b)
        out += member
        if rng.random() < 0.1:
            out += b'\0' * rng.randint(1, 20)  # padding
    return out


def make(rng, directory):
    ext = rng.choice(['tsvz', 'tsvz', 'csvz', 'psvz', 'nsvz'])
    d = DELIMITERS[ext]
    store = os.path.join(directory, 's.' + ext)
    count = rng.choice([1, 1, 1, 2, 3])
    names = ([] if rng.random() < 0.3 else ['s.' + ext])
    names += ['s.%s.%s' % (ext, o) for o in rng.sample(['1', 'a', 'B', '10', '002', 'ff'], count)]
    for name in names[:count]:
        data = body(rng, d, rng.randint(0, 25), checksums=count == 1)
        if rng.random() < 0.25:
            name, data = name + '.gz', gzip_part(rng, data)
        with open(os.path.join(directory, name), 'wb') as f:
            f.write(data)
    return store, d


def ask(command, store, op):
    try:
        done = subprocess.run(command + ['--format', 'records', '-q', op[0], store] + op[1:],
                              stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=60)
    except subprocess.TimeoutExpired:
        return ('timeout', b'')
    return (done.returncode, done.stdout)


# Differences where TSVZ.py, not tsvz-rs, departs from the spec: (reason, test). Remove an entry
# once TSVZ.py is fixed. Each is listed in docs/silent-choices.md.
KNOWN = [
    ('TSVZ.py exits 0 from len and keys after a read stopped at damaged data (§4.10, §20.6 say 1)',
     lambda op, rs, py: op[0] in ('len', 'keys') and rs == (1, py[1]) and py[0] == 0),
]


def known(op, rs, py):
    return next((reason for reason, test in KNOWN if test(op, rs, py)), None)


def rows(out, d):
    """Records with trailing empty cells removed (§3.6)."""
    return [line.rstrip(d.encode()) for line in out.split(b'\n')]


def main():
    parser = argparse.ArgumentParser(description='Differential test of tsvz-rs against TSVZ.py.')
    here = os.path.dirname(os.path.abspath(__file__))
    parser.add_argument('--rust', required=True, help='the tsvz-rs binary')
    parser.add_argument('--tsvz', default=os.path.join(here, '..', '..', 'TSVZ', 'TSVZ.py'), help='TSVZ.py')
    parser.add_argument('--seeds', default='0:200', help='START:END, end excluded')
    parser.add_argument('--keep', help='where to keep the stores of seeds that disagree (default: a temporary directory)')
    args = parser.parse_args()
    start, end = (int(x) for x in args.seeds.split(':'))
    rust = [os.path.abspath(args.rust)]
    python = [sys.executable, os.path.abspath(args.tsvz)]
    keep = args.keep or tempfile.mkdtemp(prefix='difftest-')
    failed = 0
    seen = {}
    ops = [['read'], ['len'], ['keys'], ['verify'], ['parts'], ['get', 'a', 'zz', 'tr  ', '#x', ' sp']]
    for seed in range(start, end):
        rng = random.Random(seed)
        directory = tempfile.mkdtemp(prefix='seed%d-' % seed)
        try:
            store, d = make(rng, directory)
            problems = []
            for op in ops:
                a, b = ask(rust, store, op), ask(python, store, op)
                same = a[0] == b[0] and (rows(a[1], d) == rows(b[1], d) if op[0] in ('read', 'get') else a[1] == b[1])
                if not same and known(op, a, b):
                    seen[known(op, a, b)] = seen.get(known(op, a, b), 0) + 1
                elif not same:
                    problems.append('  %s: tsvz-rs %s %r\n  %s  TSVZ.py %s %r' % (' '.join(op), a[0], a[1][:300], ' ' * len(' '.join(op)), b[0], b[1][:300]))
            if problems:
                failed += 1
                target = os.path.join(keep, 'seed%d' % seed)
                shutil.copytree(directory, target)
                print('seed %d disagrees (files in %s):\n%s' % (seed, target, '\n'.join(problems)))
        finally:
            shutil.rmtree(directory, ignore_errors=True)
    for reason, count in seen.items():
        print('known TSVZ.py difference, %d times: %s' % (count, reason))
    print('%d of %d seeds disagree' % (failed, end - start))
    return 1 if failed else 0


if __name__ == '__main__':
    sys.exit(main())
```

- [ ] **Step 2: Run it**

```bash
cargo build --release
python3 difftest/difftest.py --rust target/release/tsvz --seeds 0:500
```

Expected: `0 of 500 seeds disagree`, and one line `known TSVZ.py difference, N times: TSVZ.py exits 0 from len and keys …`. For a disagreeing seed, the report shows both answers and keeps its files: decide from the spec which tool is wrong. A tsvz-rs bug gets fixed with a unit test; a TSVZ.py departure gets a `KNOWN` entry and a row in `docs/silent-choices.md`, and is reported to the TSVZ maintainer — do not change TSVZ.py in this sub-project.

- [ ] **Step 3: Commit**

```bash
git add difftest/difftest.py
git commit -m "Differential harness: random stores, damaged gzip members included, read by tsvz-rs and TSVZ.py." -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 20: Robustness — random input on stable, fuzzing on nightly

**Files:**
- Create: `tests/robust.rs`, `fuzz/Cargo.toml`, `fuzz/fuzz_targets/replay.rs`, `fuzz/fuzz_targets/codec.rs`

**Interfaces:**
- Consumes: `Store`, `Options`, `maint::scrub`, `codec::{items, encode_member}`, `cli::args::parse`, `frame::Framer`, `replay::{Replay, Sink, Cells, Binding}`, `name::{Codec, Format}`.
- Produces: tests only.

- [ ] **Step 1: Write the stable tests**

File: `tests/robust.rs`

```rust
//! Arbitrary bytes and arguments never panic (`fuzz/` goes further on nightly).

use std::fs;

use tsvz::name::Codec;
use tsvz::{maint, Options, Store};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    /// Random bytes, mostly the ones the format gives meaning to.
    fn bytes(&mut self, max: usize) -> Vec<u8> {
        const SPECIAL: &[u8] = b"\t\n\r\0<>#_,|\xef\xbb\xbf\x1f\x8b\x08 sepLFCRNULlt";
        let n = self.below(max + 1);
        (0..n).map(|_| if self.below(3) == 0 { self.next() as u8 } else { SPECIAL[self.below(SPECIAL.len())] }).collect()
    }
}

#[test]
fn random_plain_parts_never_panic() {
    let dir = tempfile::tempdir().unwrap();
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    for i in 0..300 {
        for ext in ["tsvz", "csvz", "nsvz", "psvz", "tsv"] {
            let path = dir.path().join(format!("s{i}.{ext}"));
            let mut data = rng.bytes(400);
            if rng.below(4) == 0 {
                data = [b"#_on_corruption_#\tstop\n#_checksum_crc32_#\n".to_vec(), data].concat();
            }
            fs::write(&path, &data).unwrap();
            if let Ok(s) = Store::open(&path, Options { verify: true, ..Options::default() }) {
                let _ = s.iter().count();
                let _ = s.missing().map(|r| r.to_vec());
            }
            let _ = maint::scrub(&path, &Options::default());
            let _ = Store::open(&path, Options::default());
        }
    }
}

#[test]
fn random_compressed_parts_never_panic() {
    let mut rng = Rng(42);
    for codec in [Codec::Gzip, Codec::Bzip2, Codec::Xz, Codec::Zstd].into_iter().filter(|c| c.supported()) {
        let good = tsvz::codec::encode_member(codec, b"a\tx\nb\ty\n").unwrap();
        for _ in 0..300 {
            let mut data = rng.bytes(300);
            match rng.below(3) {
                0 => data = [good.clone(), data].concat(),
                1 => data = [good[..rng.below(good.len())].to_vec(), data].concat(),
                _ => {}
            }
            if !data.is_empty() && rng.below(2) == 0 {
                let at = rng.below(data.len());
                data[at] ^= 1 << rng.below(8);
            }
            let _ = tsvz::codec::items(codec, &data);
        }
    }
}

#[test]
fn random_arguments_never_panic() {
    let mut rng = Rng(7);
    let pool = ["-", "--", "-q", "-qv", "-d", "-dx", "-d=", "--format", "--form=table", "--x", "-5", "-.", "-ü", "get", "set", "read", "s.tsvz", "", "k", "=", "--=", "---"];
    for _ in 0..5000 {
        let args: Vec<String> = (0..rng.below(6)).map(|_| pool[rng.below(pool.len())].to_string()).collect();
        let _ = tsvz::cli::args::parse(&args);
    }
}
```

- [ ] **Step 2: Run them**

Run: `cargo test --release --test robust`
Expected: 3 passed. A panic gives the seed's input in the message: turn it into a unit test in the module that panicked, then fix.

- [ ] **Step 3: Write the fuzz targets**

File: `fuzz/Cargo.toml`

```toml
[package]
name = "tsvz-fuzz"
version = "0.0.0"
publish = false
edition = "2024"

[package.metadata]
cargo-fuzz = true

[dependencies]
libfuzzer-sys = "0.4"
tsvz = { path = "..", default-features = false, features = ["bzip2", "xz", "zstd"] }

[[bin]]
name = "replay"
path = "fuzz_targets/replay.rs"
test = false
doc = false
bench = false

[[bin]]
name = "codec"
path = "fuzz_targets/codec.rs"
test = false
doc = false
bench = false

[workspace]
members = ["."]
```

File: `fuzz/fuzz_targets/replay.rs`

```rust
#![no_main]
//! Bytes framed in chunks and replayed; the first byte picks the variant, strictness,
//! verification and chunk size.

use std::path::Path;
use std::sync::Arc;

use libfuzzer_sys::fuzz_target;
use tsvz::frame::Framer;
use tsvz::name::Format;
use tsvz::replay::{Binding, Cells, Replay, Sink};

struct Null;

impl Sink for Null {
    fn put(&mut self, _: &str, _: Cells, _: &Arc<Binding>) {}
    fn delete(&mut self, _: &str) {}
}

fuzz_target!(|data: &[u8]| {
    let Some((&sel, rest)) = data.split_first() else { return };
    let delimiter = [b'\t', b',', 0, b'|'][(sel & 3) as usize];
    let mut replay = Replay::new(Format { delimiter, strict: sel & 4 == 0 }, sel & 8 != 0, None);
    replay.begin_part(Path::new("fuzz.tsvz"), true);
    let mut framer = Framer::new();
    let mut sink = Null;
    for chunk in rest.chunks(1 + (sel >> 4) as usize) {
        if !framer.push(chunk, &mut |l| replay.line(l, &mut sink)) {
            break;
        }
    }
});
```

File: `fuzz/fuzz_targets/codec.rs`

```rust
#![no_main]
//! A compressed part's bytes split into members; the first byte picks the codec.

use libfuzzer_sys::fuzz_target;
use tsvz::name::Codec;

fuzz_target!(|data: &[u8]| {
    let Some((&sel, rest)) = data.split_first() else { return };
    let codec = [Codec::Gzip, Codec::Bzip2, Codec::Xz, Codec::Zstd][(sel & 3) as usize];
    let _ = tsvz::codec::items(codec, rest);
});
```

- [ ] **Step 4: Fuzz each target for five minutes (needs nightly and cargo-fuzz)**

```bash
cargo install cargo-fuzz        # once
cd fuzz
cargo +nightly fuzz run replay -- -max_total_time=300 -max_len=4096 -rss_limit_mb=2048
cargo +nightly fuzz run codec -- -max_total_time=300 -max_len=4096 -rss_limit_mb=2048
```

Expected: no crash. A decompression bomb that legitimately expands past the RSS limit is not a bug (members are held whole, as TSVZ.py holds them); any panic, hang or other OOM is. Add each crash input as a unit test before fixing it.

- [ ] **Step 5: Commit**

```bash
git add tests/robust.rs fuzz/Cargo.toml fuzz/fuzz_targets
git commit -m "Robustness: random bytes and arguments on stable; cargo-fuzz targets for replay and codecs." -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 21: Benchmarks against TSVZ.py

**Files:**
- Create: `bench/RESULTS.md`

**Interfaces:**
- Consumes: `target/release/tsvz`, `../TSVZ/tsvzbench.py`, `../TSVZ/TSVZ.py`.
- Produces: `bench/RESULTS.md`.

- [ ] **Step 1: Run the standard suite twice**

The work directory goes on `/mnt/gen5` (f2fs): never `/tmp` (tmpfs) or `/mnt/klein` (mergerfs), which give invalid numbers.

```bash
cargo build --release
mkdir -p /mnt/gen5/tsvzbench-rs
for run in 1 2; do
  python3 ../TSVZ/tsvzbench.py --impl "py=python3 ../TSVZ/TSVZ.py" --impl "rs=target/release/tsvz" \
    --suite standard --only cold,bulk,perop,maint --work-dir /mnt/gen5/tsvzbench-rs \
    --json /mnt/gen5/tsvzbench-rs/run$run.json | tee /mnt/gen5/tsvzbench-rs/run$run.txt
done
```

Expected: every `rs` cell faster than `py` (ratio below 1) in both runs; handler workloads show `n/a — no handler` for `rs`. Timings here can flip between a fast and a slow mode (about 25% at microsecond scales), so a ratio counts only when both runs agree.

- [ ] **Step 2: Fix any workload that is not faster**

For each such workload, keep its fixture (`--keep`), profile the slow command:

```bash
perf record -g -- target/release/tsvz --format records <the workload's operation and store>
perf report --stdio | head -60
```

then fix what the profile shows, add a test if the fix changes behaviour, and rerun Step 1.

- [ ] **Step 3: Record the results**

File: `bench/RESULTS.md` holds, in this order: the date; the commands of Step 1; the machine (`lscpu | grep 'Model name'`, `uname -r`, `findmnt -no FSTYPE /mnt/gen5`); `python3 --version`, `python3 ../TSVZ/TSVZ.py -V` and `target/release/tsvz -V`; then both runs' tables pasted from `run1.txt` and `run2.txt`; then one sentence per family stating the median ratio range.

- [ ] **Step 4: Commit**

```bash
git add bench/RESULTS.md
git commit -m "Bench: tsvz-rs against TSVZ.py on the standard tsvzbench suite." -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 22: CI, release workflow and README

**Files:**
- Create: `.github/workflows/ci.yml`, `.github/workflows/release.yml`, `README.md`

**Interfaces:**
- Consumes: everything above.
- Produces: CI on every push; release binaries on a `v*` tag; the README.

- [ ] **Step 1: Write the CI workflow**

File: `.github/workflows/ci.yml`

```yaml
name: ci
on:
  push:
  pull_request:
jobs:
  test:
    strategy:
      fail-fast: false
      matrix:
        os: [ubuntu-latest, macos-latest]
    runs-on: ${{ matrix.os }}
    steps:
      - uses: actions/checkout@v4
      - uses: actions/checkout@v4
        with:
          repository: yufei-pan/TSVZ
          path: TSVZ
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: clippy, rustfmt
      - uses: actions/setup-python@v5
        with:
          python-version: "3.14"
      - run: cargo fmt --check
      - run: cargo clippy --all-targets -- -D warnings
      - run: cargo test
        env:
          TSVZ_PY: ${{ github.workspace }}/TSVZ/TSVZ.py
      - run: cargo build --release
      - run: python3 TSVZ/tsvzconform.py --impl target/release/tsvz --class reader,writer,cli --feature bzip2,xz,zstd,crc32,crc32c,sha256,blake3,file-first
      - run: python3 difftest/difftest.py --rust target/release/tsvz --tsvz TSVZ/TSVZ.py --seeds 0:200
```

- [ ] **Step 2: Write the release workflow**

File: `.github/workflows/release.yml`

```yaml
name: release
on:
  push:
    tags: ["v*"]
permissions:
  contents: write
jobs:
  create-release:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: taiki-e/create-gh-release-action@v1
        with:
          token: ${{ secrets.GITHUB_TOKEN }}
  binaries:
    needs: create-release
    strategy:
      matrix:
        include:
          - { target: x86_64-unknown-linux-musl, os: ubuntu-latest }
          - { target: aarch64-unknown-linux-musl, os: ubuntu-latest }
          - { target: aarch64-apple-darwin, os: macos-latest }
          - { target: x86_64-apple-darwin, os: macos-latest }
    runs-on: ${{ matrix.os }}
    steps:
      - uses: actions/checkout@v4
      - uses: taiki-e/upload-rust-binary-action@v1
        with:
          bin: tsvz
          target: ${{ matrix.target }}
          archive: tsvz-$tag-$target
          token: ${{ secrets.GITHUB_TOKEN }}
```

- [ ] **Step 3: Write the README**

File: `README.md`

````markdown
# tsvz-rs

A fast implementation of [tsvz-spec-v1](https://tsvz.org) in Rust: the `tsvz` command-line
tool, and the `tsvz` library crate it is built on. TSVZ stores are append-only key-value logs
in plain TSV, CSV, NSV or PSV text; see the specification and the Python reference
implementation, [TSVZ](https://github.com/yufei-pan/TSVZ).

## Install

```bash
cargo install tsvz
```

or download a binary for Linux (x86_64, aarch64; static) or macOS (arm64, x86_64) from the
GitHub releases. TSVZ.py also installs a `tsvz` command; both speak the same specification, so
whichever comes first on your `PATH` works the same on spec stores.

## Use

```bash
tsvz set people.tsvz alice Alice 30
tsvz get people.tsvz alice
tsvz read people.tsvz | tsvz set copy.tsvz -
tsvz verify people.tsvz
tsvz --help
```

Every operation of §20 is supported except `serve` and `stop`; the handler (§21) comes in a
later release. Stores may be compressed (gzip, bzip2, xz, zstd) and split into parts; all four
digests (crc32, crc32c, sha256, blake3) are verified.

## Library

```rust
use tsvz::{Options, Record, Store, Writer};

let mut w = Writer::open("people.tsvz")?;
w.append(&[Record::from_args("alice", &["Alice".into(), "30".into()])])?;
let store = Store::open("people.tsvz", Options::default())?;
for (key, row) in store.iter() {
    println!("{key}: {:?}", row.to_vec());
}
# Ok::<(), tsvz::Error>(())
```

Build without the command line and C libraries with
`default-features = false, features = ["bzip2"]` (gzip and all digests are always in).

## Conformance and compatibility

tsvz-rs passes the reader, writer and cli classes of the TSVZ conformance suite with every
codec and digest claimed, and is checked against TSVZ.py on random stores. Rust and Python
writers can share a store: they take the same file locks. Where the specification leaves a
choice open, tsvz-rs does what TSVZ.py does; [docs/silent-choices.md](docs/silent-choices.md)
lists those choices. Benchmarks against TSVZ.py are in [bench/RESULTS.md](bench/RESULTS.md).

## License

GPL-3.0-or-later.
````

- [ ] **Step 4: Check the README example compiles**

Add to `src/lib.rs`, below the module doc comment:

```rust
#![doc = include_str!("../README.md")]
```

Run: `cargo test --doc`
Expected: the library example compiles and runs (it creates `people.tsvz` in the crate directory while testing; `.gitignore` it: append `/people.tsvz` to `.gitignore`).

- [ ] **Step 5: Run everything once more, then commit**

```bash
cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test
git add .github README.md src/lib.rs .gitignore
git commit -m "CI on Linux and macOS with conformance and the differential harness; release binaries; README." -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 23: Release 0.1.0

**Files:**
- Modify: none, unless review finds something.

**Interfaces:**
- Consumes: the finished crate.
- Produces: the GitHub repository, crate `tsvz` 0.1.0 on crates.io, a `v0.1.0` release with four binaries.

Every step here is outward-facing: ask the user before each of Steps 3, 4 and 5, and do not continue without a yes.

- [ ] **Step 1: Review the whole crate**

Use superpowers:requesting-code-review on the whole repository against the spec. Fix what it finds (each fix with a test), and rerun `cargo test`, the conformance command of Task 18 and the harness of Task 19.

- [ ] **Step 2: Check the done list (design section 10)**

- [ ] Conformance: reader, writer, cli conformant, no FAIL, no unlisted warn, on Linux and macOS (CI green).
- [ ] Differential harness: `0 of 200 seeds disagree` in CI; known TSVZ.py differences only as listed.
- [ ] `bench/RESULTS.md`: faster than TSVZ.py on every `cold`, `bulk`, `perop` CLI and `maint` workload.
- [ ] Unit, refresh, interop, robustness tests pass; fuzzing ran clean (Task 20 Step 4).
- [ ] `docs/silent-choices.md` complete.

- [ ] **Step 3: Create the GitHub repository and push (ask first)**

```bash
gh repo create yufei-pan/tsvz-rs --public --source . --remote origin --description "Fast Rust implementation of tsvz-spec-v1: the tsvz command-line tool and library"
git push -u origin main
```

Expected: CI runs and passes on both platforms.

- [ ] **Step 4: Publish the crate (ask first)**

```bash
cargo publish --dry-run
cargo publish
```

Expected: `tsvz 0.1.0` on crates.io; `cargo install tsvz` installs the binary.

- [ ] **Step 5: Tag the release (ask first)**

```bash
git tag -a v0.1.0 -m "tsvz-rs 0.1.0"
git push origin v0.1.0
```

Expected: the release workflow attaches `tsvz-v0.1.0-<target>` archives for the four targets.

- [ ] **Step 6: Note the follow-ups for the user**

Report, without doing them in this sub-project: a Rust row in tsvz.org's implementations table (TSVZ repo); a tsvz-rs row in `cas/CLAUDE.md`; the TSVZ.py departures in `docs/silent-choices.md` (fix in TSVZ.py, plus conformance cases for `len`/`keys`/`has`/`verify` after a stopped read); a handler conformance case for an in-place rewrite followed by appends past the old size; sub-project 2 (handler) and 3 (multi-part scrub).

---

## Self-review notes

- **Spec coverage.** Sections 1–2: Tasks 1–11, 13. Section 3: Tasks 5–9, 16. Section 4: Tasks 11, 13. Section 5: Tasks 10, 12–15. Section 6: Tasks 17–19. Section 7: Tasks 1–20 (7.1 unit tests in every module, 7.2 Task 18, 7.3 Task 19, 7.4 Task 20, 7.5 Tasks 16–17). Section 8: Task 21. Section 9: Tasks 22–23. Section 10: Task 23 Step 2.
- **Checked while planning.** Tasks 1–16 were extracted from this plan into a scratch crate and built: all their tests pass, clippy is clean, the conformance reader, writer and cli classes pass with no FAIL and no warn (Python 3.13 and 3.14 runners), and the Task 19 harness agrees with TSVZ.py on 500 seeds apart from the one known TSVZ.py departure.
