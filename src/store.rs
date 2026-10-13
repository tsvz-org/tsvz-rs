//! A store: the ordered map of live rows after replaying its parts (§3, §14, §17).

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
    /// A row resolved from `cells` under `defaults` and a fill-empty setting (§14.2, §14.3).
    pub(crate) fn new(cells: &'a Cells, defaults: &'a [String], fill: bool) -> Row<'a> {
        Row { cells, defaults, fill }
    }

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
