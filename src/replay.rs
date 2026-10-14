//! The reading pipeline of §7 as a state machine: classification, markers (§12), integrity
//! (§15), zero fill (§4.9), tombstones (§9), stripping (§10), escaping (§13) and the binding
//! of rows to the defaults in force where they appear (§14.4).

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
pub static EMPTY_CELLS: Cells = Cells {
    text: String::new(),
    ends: Vec::new(),
};

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
    /// The read met a checksum marker of a known algorithm (§15), verified or not.
    pub checksums: bool,
    /// The highest spec version a `#_version_#` line declared (1 when none did, §6).
    pub newest: u64,
    /// The first committed line of the first part, when it is a comment (§19.2.3c).
    pub header: Option<String>,
    /// The store's width in value columns, which rows are padded to (§3.6 lets a reader pad):
    /// set by the first data row, as TSVZ.py sets it, to the count of the defaults then active
    /// when there are any, else to that row's written cells. None before any data row.
    pub width: Option<usize>,
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
/// The highest tsvz-spec version this crate implements (§6.3).
pub(crate) const SPEC_VERSION: u64 = 1;

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
            checksums: false,
            newest: 1,
            header: None,
            width: None,
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
            w(&Warning {
                part: self.part.clone(),
                offset,
                message,
            });
        }
    }

    /// The binding a row takes at the current position.
    pub fn binding(&mut self) -> Arc<Binding> {
        let markers = &self.markers;
        self.binding
            .get_or_insert_with(|| {
                Arc::new(Binding {
                    defaults: markers.defaults.clone(),
                    fill_empty: markers.fill_empty,
                })
            })
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
                        self.stopped = Some(Damage {
                            part: self.part.clone(),
                            offset: line.offset,
                            kind: DamageKind::ZeroFill,
                        });
                        self.warn(Some(line.offset), format!("line {n} holds zero bytes (zero-fill damage); reading stopped there (#_on_corruption_# stop)"));
                        return false;
                    }
                    Policy::Skip => {
                        self.feed(line.raw, None); // digests cover the line as written (§4.9)
                        self.warn(
                            Some(line.offset),
                            format!("line {n} holds zero bytes (zero-fill damage); skipped it"),
                        );
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
            self.warn(
                Some(line.offset),
                format!(
                    "line {} is not valid UTF-8; replaced the bad bytes with U+FFFD",
                    line.number
                ),
            );
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
                if let Some(name) = lower
                    .strip_prefix("#_checksum_")
                    .and_then(|n| n.strip_suffix("_#"))
                {
                    let algo = Algo::from_name(name);
                    self.checksums |= algo.is_some();
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
        let value = rest
            .map(|r| escape::decode(split_first(r, d).0, d).into_owned())
            .unwrap_or_default();
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
            self.markers.defaults =
                Arc::new(fields.map(|f| escape::decode(f, d).into_owned()).collect());
            self.binding = None;
            return;
        }
        let value = fields
            .next()
            .map(|f| escape::decode(f, d))
            .unwrap_or(Cow::Borrowed(""));
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
                    self.newest = self.newest.max(n);
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
            self.warn(
                Some(line.offset),
                format!(
                    "line {}: {} has an invalid value {:?}; ignored",
                    line.number, key, value
                ),
            );
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
        self.set_width(rest);
        let binding = self.binding();
        let mut cells = Cells::default();
        if sink.wants_cells(&key) {
            for field in rest.split(d as char) {
                cells.push(&escape::decode(
                    if strip { trim_ws(field) } else { field },
                    d,
                ));
            }
        }
        sink.put(&key, cells, &binding);
    }

    /// Set the width at the first data row, whose value fields are `rest` (see `width`).
    fn set_width(&mut self, rest: &str) {
        if self.width.is_none() {
            let written = memchr::memchr_iter(self.format.delimiter, rest.as_bytes()).count() + 1;
            let defaults = self.markers.defaults.len();
            self.width = Some(if defaults > 0 { defaults } else { written });
        }
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
        self.set_width(rest);
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
        self.warn(
            Some(offset as u64),
            format!("the member at byte {offset} is cut short; its whole lines are read"),
        );
    }

    /// A corrupt member (§16.6): what the policy in force says to do with its output (§4.10).
    pub fn corrupt(&mut self, offset: usize, decoded: usize) -> Policy {
        self.damaged = true;
        let policy = self.markers.on_corruption;
        let message = match policy {
            Policy::Recover => format!(
                "used {decoded} bytes the corrupt member at byte {offset} decoded before it failed; they are not verified"
            ),
            Policy::Skip => format!("skipped the corrupt member at byte {offset}"),
            Policy::Stop => {
                self.stopped = Some(Damage {
                    part: self.part.clone(),
                    offset: offset as u64,
                    kind: DamageKind::CorruptMember,
                });
                format!(
                    "the member at byte {offset} is corrupt; reading stopped there (#_on_corruption_# stop)"
                )
            }
        };
        self.warn(Some(offset as u64), message);
        policy
    }

    /// Bytes after a part's last newline were ignored (§4.3).
    pub fn tail(&mut self, bytes: usize) {
        if bytes > 0 {
            self.warn(
                None,
                format!("ignored {bytes} uncommitted bytes after the last newline"),
            );
        }
    }
}

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

    const TSVZ: Format = Format {
        delimiter: b'\t',
        strict: true,
    };

    fn run(format: Format, verify: bool, data: &[u8]) -> (Replay, Rows, Vec<String>) {
        let warnings = Arc::new(Mutex::new(Vec::new()));
        let sink = warnings.clone();
        let warn: WarnFn =
            Arc::new(move |w: &Warning| sink.lock().unwrap().push(w.message.clone()));
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
        assert_eq!(
            table(&rows),
            vec![row("a", &["x \n"]), row("#h", &["v\tw"])]
        );
        let (_, off, _) = run(
            TSVZ,
            false,
            b"#_strip_trailing_whites_#\tfalse\na \tx \n \t1\n",
        );
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
        let (r, rows, _) = run(
            TSVZ,
            false,
            b"#_defaults_#\td<sep>e \tf\na\t1\nb\t2\n#_defaults_#\tg\nc\t3\n",
        );
        assert_eq!(*r.markers.defaults, vec!["g".to_string()]);
        assert_eq!(
            *rows.0[0].2.defaults,
            vec!["d\te ".to_string(), "f".to_string()]
        );
        assert!(Arc::ptr_eq(&rows.0[0].2, &rows.0[1].2));
        assert!(!Arc::ptr_eq(&rows.0[0].2, &rows.0[2].2));
    }

    #[test]
    fn comments_unknown_markers_and_the_header() {
        let (r, rows, _) = run(
            TSVZ,
            false,
            b"#name\tage\n#__ext__#\tx\n#_frobnicate_#\ty\n #k\tv\n#_defaults_# \tz\n",
        );
        assert_eq!(r.header.as_deref(), Some("#name\tage"));
        assert_eq!(table(&rows), vec![row(" #k", &["v"])]);
        assert!(
            r.markers.defaults.is_empty(),
            "a marker key with a trailing space is a comment"
        );
        let (m, _, _) = run(TSVZ, false, b"#_version_#\t1\n#c\n");
        assert_eq!(m.header, None);
    }

    #[test]
    fn zero_fill_under_each_policy() {
        let (_, rec, w) = run(TSVZ, false, b"a\t1\n\0\0b\t2\nc\t3\n");
        assert_eq!(
            table(&rec),
            vec![row("a", &["1"]), row("b", &["2"]), row("c", &["3"])]
        );
        assert_eq!(w.len(), 1);
        let (_, skip, _) = run(
            TSVZ,
            false,
            b"#_on_corruption_#\tskip\na\t1\n\0\0b\t2\nc\t3\n",
        );
        assert_eq!(table(&skip), vec![row("a", &["1"]), row("c", &["3"])]);
        let (r, stop, _) = run(
            TSVZ,
            false,
            b"#_on_corruption_#\tstop\na\t1\n\0\0b\t2\nc\t3\n",
        );
        assert_eq!(table(&stop), vec![row("a", &["1"])]);
        assert_eq!(
            r.stopped.as_ref().map(|d| (d.offset, d.kind)),
            Some((27, DamageKind::ZeroFill))
        );
        let (_, nsv, _) = run(
            Format {
                delimiter: 0,
                strict: true,
            },
            false,
            b"a\0x\n\0y\n",
        );
        assert_eq!(table(&nsv), vec![row("a", &["x"])]);
    }

    #[test]
    fn invalid_utf8_becomes_replacement_characters() {
        let (_, rows, w) = run(TSVZ, false, b"a\t\xff\xfeok\xe2\x82\n");
        assert_eq!(
            table(&rows),
            vec![row("a", &["\u{fffd}\u{fffd}ok\u{fffd}"])]
        );
        assert_eq!(w.len(), 1);
    }

    #[test]
    fn verify_segments() {
        let seg = b"a\tx\n#_checksum_sha256_#\n";
        let crc = format!("{:08x}", crc32fast::hash(seg));
        let good = format!(
            "#_checksum_crc32_#\tignored\n{}#_CHECKSUM_CRC32_#\t{}\n",
            std::str::from_utf8(seg).unwrap(),
            crc.to_uppercase()
        );
        let (r, rows, _) = run(TSVZ, true, good.as_bytes());
        assert!(r.mismatches.is_empty(), "{:?}", r.mismatches);
        assert_eq!(table(&rows), vec![row("a", &["x"])]);
        let bad = b"#_checksum_crc32_#\na\tx\n#_checksum_crc32_#\tDEADBEEF\n#_checksum_crc32_#\t\n#_checksum_crc32_#\t11111111\n";
        let (r, _, _) = run(TSVZ, true, bad);
        assert_eq!(r.mismatches.len(), 2);
        assert_eq!(
            (
                r.mismatches[0].line,
                r.mismatches[0].algo,
                r.mismatches[0].expected.as_str()
            ),
            (3, "crc32", "deadbeef")
        );
        assert_eq!(
            r.mismatches[0].computed,
            format!("{:08x}", crc32fast::hash(b"a\tx\n"))
        );
        assert_eq!(r.mismatches[1].line, 5);
        let (off, _, _) = run(TSVZ, false, bad);
        assert!(off.mismatches.is_empty());
    }

    #[test]
    fn loose_lines_are_split_raw() {
        let (r, rows, _) = run(
            Format {
                delimiter: b',',
                strict: false,
            },
            false,
            b"#h,dr\nk,x<lt>,y \n#_defaults_#,d\nb\nk2, \n",
        );
        assert_eq!(r.header.as_deref(), Some("#h,dr"));
        assert!(r.markers.defaults.is_empty());
        assert_eq!(
            table(&rows),
            vec![row("k", &["x<lt>", "y "]), row("k2", &[" "])]
        );
    }

    #[test]
    fn keys_only_sinks_get_no_cells() {
        struct Keys(Vec<(String, usize)>);
        impl Sink for Keys {
            fn wants_cells(&self, _: &str) -> bool {
                false
            }
            fn put(&mut self, key: &str, cells: Cells, _: &Arc<Binding>) {
                self.0.push((key.into(), cells.len()));
            }
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
        assert_eq!(
            r.stopped.as_ref().map(|d| (d.offset, d.kind)),
            Some((99, DamageKind::CorruptMember))
        );
    }
}
