//! A store: the ordered map of live rows after replaying its parts (§3, §14, §17).

use std::borrow::Cow;
use std::collections::HashSet;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::fs::{FileExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use hashbrown::HashMap;

use crate::codec::{self, Item};
use crate::escape;
use crate::frame::{Framer, Line};
use crate::name::{self, Codec, Format};
use crate::parts::{self, Stamp};
use crate::replay::{
    Binding, Cells, DEFAULT_MEMBER_SIZE, Damage, EMPTY_CELLS, Markers, Mismatch, Policy, Replay,
    Sink,
};
use crate::{Error, WarnFn, Warning};

/// What a store keeps of each row.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Scope {
    /// Every row with its cells.
    #[default]
    All,
    /// Keys and their order, no cells (`len`, `keys`, `has`).
    KeysOnly,
    /// Only these keys (as requested, and as a reader resolves them), with cells (`get`).
    Keys(Vec<String>),
}

/// How to open a store.
#[derive(Clone, Default)]
#[non_exhaustive]
pub struct Options {
    pub scope: Scope,
    /// Compute digests and record mismatches (§15); off for plain reads.
    pub verify: bool,
    /// The delimiter of a loose file (`-d`): one ASCII character other than a line break, else
    /// the open is refused; ignored for strict variants (§5.1).
    pub delimiter: Option<u8>,
    pub warnings: Option<WarnFn>,
    /// Skip a part that cannot be opened or read, or whose codec this build lacks (§16.5),
    /// with a warning, instead of failing: the store then holds what could be read, and
    /// [`Store::incomplete`] is true (§21.8.5). Off by default.
    pub skip_unreadable: bool,
    /// Give a loose file's lines to a rewrite's planner ([`crate::loose`]), and read no row.
    pub(crate) lines: Option<crate::loose::Sink>,
    /// Replay every part, also those before the last snapshot part, where a read otherwise begins
    /// (§19.10.3): a rewrite reads all it compacts. [`Options::verify`] reads every part too
    /// (§19.10.4).
    pub(crate) every_part: bool,
    /// Read a loose file only as far as its first keyed line, which sets its column count
    /// ([`Store::one_column`]). A compressed file's member is still decoded to its check before
    /// its lines are used (§16.6), holding no more of it than a member of the default size.
    pub(crate) head: bool,
}

/// One part as `parts` lists it (§20.2).
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct PartInfo {
    pub index: usize,
    pub ordinal: Option<String>,
    pub path: PathBuf,
    pub codec: Option<Codec>,
    pub active: bool,
}

/// A live key's resolved row (§14.2, §14.3), or a missing key's defaults (§14.5), padded to
/// the store's width as TSVZ.py pads it (§3.6): the count of the defaults active at the first
/// data row when there were any, else that row's written cells. Cells past the written ones
/// take the row's defaults, then the empty string; no cell is ever cut.
#[derive(Clone, Copy)]
pub struct Row<'a> {
    cells: &'a Cells,
    defaults: &'a [String],
    fill: bool,
    width: usize,
}

impl<'a> Row<'a> {
    /// A row resolved from `cells` under `defaults` and a fill-empty setting (§14.2, §14.3),
    /// padded to `width` value columns.
    pub(crate) fn new(
        cells: &'a Cells,
        defaults: &'a [String],
        fill: bool,
        width: usize,
    ) -> Row<'a> {
        Row {
            cells,
            defaults,
            fill,
            width,
        }
    }

    pub fn len(&self) -> usize {
        self.cells.len().max(self.defaults.len()).max(self.width)
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

fn row_of(slot: &Slot, width: usize) -> Row<'_> {
    Row::new(
        &slot.cells,
        &slot.binding.defaults,
        slot.binding.fill_empty,
        width,
    )
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
    /// A table for `scope`, of a strict or a loose store; under `Scope::Keys` it keeps each requested
    /// key and the key it names: for a strict store the key a reader resolves it to (§20.2.7), for a
    /// loose one the key a reader names it by ([`crate::loose::lookup`]).
    fn new(scope: &Scope, format: Format) -> Table {
        let wanted = match scope {
            Scope::Keys(keys) => Some(
                keys.iter()
                    .flat_map(|k| {
                        let resolved = if format.strict {
                            crate::write::resolve(k, format.delimiter).into_owned()
                        } else {
                            crate::loose::lookup(k, format.delimiter).into_owned()
                        };
                        [k.clone(), resolved]
                    })
                    .collect(),
            ),
            _ => None,
        };
        Table {
            cells: !matches!(scope, Scope::KeysOnly),
            wanted,
            ..Table::default()
        }
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
                self.slots.push(Some(Slot {
                    key: key.into(),
                    cells,
                    binding: binding.clone(),
                }));
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

/// What `refresh` (Task 16) needs to follow the files.
#[derive(Clone, Debug, Default)]
pub(crate) struct Follow {
    /// The directory as it was just before the parts were last listed, where its timestamp is
    /// kept and was old enough to show a change ([`parts::DirTrust`]), and part 0 was no symbolic
    /// link in that listing ([`parts::list_linked`]); else None, and the next check lists.
    pub dir: Option<parts::DirStamp>,
    pub stamps: Vec<Option<Stamp>>,
    /// Offset just after the last committed line of the last part (plain parts only).
    pub offset: u64,
    pub line: u64,
    pub tail: Vec<u8>,
    /// Up to CHECK_BYTES bytes just before `offset`.
    pub check: Vec<u8>,
}

/// What an attempt to read a store says, held while the attempt may still be thrown away: one that
/// begins at a snapshot part says nothing until the read confirms the part, and is dropped with
/// what it said when it does not (the store is then read again from an earlier part, and the
/// damage the attempt met is met and said once).
struct Hold {
    real: Option<WarnFn>,
    /// None: said as they come.
    held: Mutex<Option<Vec<Warning>>>,
}

impl Hold {
    fn new(real: Option<WarnFn>) -> Arc<Hold> {
        Arc::new(Hold {
            real,
            held: Mutex::new(None),
        })
    }

    /// Where the attempt says its warnings; None where nothing hears them.
    fn sink(self: &Arc<Hold>) -> Option<WarnFn> {
        self.real.as_ref()?;
        let hold = self.clone();
        Some(Arc::new(move |w: &Warning| {
            let mut held = hold.held.lock().unwrap_or_else(|e| e.into_inner());
            match held.as_mut() {
                Some(list) => list.push(w.clone()),
                None => {
                    drop(held);
                    hold.real.iter().for_each(|real| real(w));
                }
            }
        }))
    }

    /// Hold what is said from now on.
    fn begin(&self) {
        *self.held.lock().unwrap_or_else(|e| e.into_inner()) = Some(Vec::new());
    }

    /// The attempt is kept: say what was held, and the rest as it comes.
    fn release(&self) {
        let list = self.held.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(real) = &self.real {
            list.iter().flatten().for_each(|w| real(w));
        }
    }
}

/// How many times a store is read when an earlier part changes under the read.
const READS: usize = 3;

#[cfg(test)]
thread_local! {
    /// How many more reads this thread takes as torn: for tests.
    static TORN: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// What `Store::refresh` did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Refreshed {
    /// Nothing changed.
    Unchanged,
    /// Records appended to the last part were replayed.
    Followed,
    /// Something else changed: the store was read again.
    Reloaded,
}

fn unsupported(part: &parts::Part) -> Error {
    Error::UnsupportedCodec {
        path: part.path.clone(),
        codec: part.codec.map_or("", Codec::name),
    }
}

/// The page a part's first lines are looked at from. A gzip or xz member gives its first lines
/// from its first 4 KiB (a zstd member from its first 56 KiB, a bzip2 one from a whole block, 184
/// KiB of input in a probe of 0.8 MB of text), and a first line that is no comment settles a
/// part, so 4 KiB settles nearly every plain, gzip or xz part, and a larger page only decodes
/// more of each.
const SNAPSHOT_HINT: usize = 1 << 12;

/// What the first committed lines of a part say (§19.10.1), fed one at a time.
struct SnapshotLines {
    delimiter: u8,
    seen: usize,
    verdict: Option<bool>,
}

impl SnapshotLines {
    fn new(delimiter: u8) -> SnapshotLines {
        SnapshotLines {
            delimiter,
            seen: 0,
            verdict: None,
        }
    }

    /// Judge one more line; false once the verdict is in.
    fn line(&mut self, l: &Line<'_>) -> bool {
        if self.verdict.is_some() {
            return false;
        }
        let d = self.delimiter;
        self.seen += 1;
        if d != 0 && l.text().contains(&0) {
            // Zero-filled bytes (a torn write, a file system that wrote the size before the data)
            // in a first line are damage: its comment is no comment, whatever else it holds.
            self.damage();
            return false;
        }
        let text = String::from_utf8_lossy(l.text());
        let (key, rest) = match memchr::memchr(d, text.as_bytes()) {
            Some(i) => (&text[..i], Some(&text[i + 1..])),
            None => (&text[..], None),
        };
        if key.eq_ignore_ascii_case("#_snapshot_#") {
            self.verdict = Some(rest.is_none_or(|r| r.split(d as char).all(str::is_empty)));
        } else if self.seen == 2 || !key.starts_with('#') || escape::is_marker_key(key) {
            self.verdict = Some(false); // a first line that is not a comment, or a second line
        }
        self.verdict.is_none()
    }

    /// Damage met before the verdict: the part's first lines cannot be read whole, so it is no
    /// snapshot part (§19.10.3).
    fn damage(&mut self) {
        self.verdict.get_or_insert(false);
    }
}

/// The lines at the start of a compressed part, decoded a page at a time from `reader`, member
/// after member (a line can span members; gzip's zero padding between them is skipped), and
/// given to `on_line` as they come out, until it returns false, the part ends or a data error
/// stops it. What a page decodes before an error is dropped with it. The output is used before
/// its member's check, so what it says is only a first look.
fn first_lines(codec: Codec, reader: &mut impl Read, on_line: &mut dyn FnMut(&Line<'_>) -> bool) {
    let mut decoder = codec::Decoder::new(codec);
    // A member ended: what follows begins another. A gzip part may begin with zero padding, which
    // `codec::read_items` skips before its first member too.
    let mut between = codec == Codec::Gzip;
    let mut framer = Framer::new();
    let (mut page, mut out) = (vec![0u8; SNAPSHOT_HINT], Vec::new());
    loop {
        let n = match reader.read(&mut page) {
            Ok(n) if n > 0 => n,
            _ => return,
        };
        let mut input = &page[..n];
        while !input.is_empty() {
            if between {
                if codec == Codec::Gzip {
                    let zeros = input.iter().take_while(|&&b| b == 0).count();
                    input = &input[zeros..];
                    if input.is_empty() {
                        break;
                    }
                }
                decoder = codec::Decoder::new(codec);
                between = false;
            }
            out.clear();
            let Ok((used, ended)) = decoder.feed(input, &mut out) else {
                return;
            };
            if !framer.push(&out, on_line) {
                return;
            }
            if !ended {
                break;
            }
            between = true;
            input = &input[used.max(1)..]; // as `codec::read_items` steps past a member
        }
    }
}

/// Whether the first lines of `part` look like a snapshot part's (§19.10.1): its first committed
/// line, or its second after a comment, is `#_snapshot_#` without a value. Only the start of the
/// part is read, a page at a time, until those lines say yes or no (a compressed one's before its
/// member's check, and as far as the first block of a bzip2 one: up to 900 kB of text). A look
/// only chooses where a read begins: the read of that part confirms it (`Store::marked`).
fn looks_marked(part: &parts::Part, format: Format) -> bool {
    let Ok(file) = File::open(&part.path) else {
        return false;
    };
    let mut reader: &File = &file;
    let mut lines = SnapshotLines::new(format.delimiter);
    match part.codec {
        Some(c) if !c.supported() => return false,
        Some(c) => first_lines(c, &mut reader, &mut |l| lines.line(l)),
        None => {
            let mut framer = Framer::new();
            // A snapshot part's marker follows at most one comment, a long one read on.
            let mut buf = vec![0u8; SNAPSHOT_HINT];
            let mut first = true;
            loop {
                let n = match reader.read(&mut buf) {
                    Ok(n) if n > 0 => n,
                    _ => break,
                };
                if std::mem::take(&mut first) {
                    // A first line that is not a comment settles it before it is read whole.
                    let start = buf[..n].strip_prefix(b"\xef\xbb\xbf").unwrap_or(&buf[..n]);
                    if start.first().is_some_and(|&b| b != b'#') {
                        return false;
                    }
                }
                if !framer.push(&buf[..n], &mut |l| lines.line(l)) {
                    break;
                }
            }
        }
    }
    lines.verdict == Some(true)
}

/// Where a read of `list` begins (§19.10.3): the last part before `below` whose first lines look
/// like a snapshot part's (part 0 is read from the start anyway), else 0.
fn snapshot_start(list: &[parts::Part], below: usize, format: Format) -> usize {
    (1..below.min(list.len()))
        .rev()
        .find(|&i| looks_marked(&list[i], format))
        .unwrap_or(0)
}

/// Where the lines of a part being read go: the replay, and the judge of its first lines.
struct Feed<'a> {
    replay: &'a mut Replay,
    table: &'a mut Table,
    judge: Option<&'a mut SnapshotLines>,
}

impl Feed<'_> {
    fn line(&mut self, l: &Line<'_>) -> bool {
        if let Some(j) = self.judge.as_deref_mut() {
            j.line(l);
        }
        self.replay.line(l, self.table)
    }

    /// Damage met in the part: before the verdict, it is no snapshot part.
    fn damage(&mut self) {
        if let Some(j) = self.judge.as_deref_mut() {
            j.damage();
        }
    }
}

fn read_check(file: &File, offset: u64) -> Vec<u8> {
    let start = offset.saturating_sub(CHECK_BYTES);
    let mut buf = vec![0u8; (offset - start) as usize];
    match file.read_exact_at(&mut buf, start) {
        Ok(()) => buf,
        Err(_) => Vec::new(),
    }
}

/// A store read from its files. It keeps its last part open, to follow what is appended to it
/// ([`Store::refresh`]): one descriptor, and that file's space if it is deleted meanwhile, until
/// the store is dropped or reads another last part.
pub struct Store {
    path: PathBuf,
    format: Format,
    options: Options,
    table: Table,
    replay: Replay,
    parts: Vec<PartInfo>,
    unreadable: Vec<PathBuf>,
    /// Read only the first this many parts (a prefix to snapshot, §19.2).
    limit: Option<usize>,
    pub(crate) follow: Follow,
    /// An earlier part kept changing while the store was read (an in-place rewrite, §19.8):
    /// the read may be torn.
    torn: bool,
    /// The last part (plain), kept open to follow what is appended to it, with its inode.
    last: Option<(File, u64)>,
    /// Whether its directory's timestamp shows every change of its names.
    pub(crate) dirs: parts::DirTrust,
    /// The first part read: the last snapshot part, or 0 (§19.10.3).
    start: usize,
    /// The first part read began with `#_snapshot_#` (§19.10.1), by the lines the read decoded.
    marked: bool,
}

impl Store {
    /// Read the store at `path` (the unnumbered file's path, §20.1.3).
    pub fn open(path: impl AsRef<Path>, options: Options) -> Result<Store, Error> {
        Store::open_limited(path.as_ref(), options, None, parts::DirTrust::default())
    }

    /// Read only the first `count` parts of the store at `path`: the immutable prefix a
    /// snapshot subsumes (§19.2). Its last part is reported as the active one.
    pub(crate) fn open_prefix(
        path: impl AsRef<Path>,
        options: Options,
        count: usize,
    ) -> Result<Store, Error> {
        Store::open_limited(
            path.as_ref(),
            options,
            Some(count),
            parts::DirTrust::default(),
        )
    }

    /// This read continued with the plain file `path` as one more part: what a reader would see
    /// were it the next part. For a check that is then thrown away; the store is not refreshed.
    pub(crate) fn read_after(mut self, path: &Path) -> Result<Store, Error> {
        let codec = path.to_str().and_then(|p| name::split_codec(p).1);
        let part = parts::Part {
            path: path.to_path_buf(),
            ordinal: None,
            codec,
        };
        self.replay.begin_part(path, false);
        self.read_part(&part, true, None)?;
        Ok(self)
    }

    /// Readers take no lock, and an earlier part may be rewritten in place while it is read (a
    /// scrub, §19.8): a read that finds one changed afterwards reads the store again, up to
    /// [`READS`] times, then is taken as torn ([`Store::incomplete`]).
    /// What was learnt of the directory (`dirs`) is kept from one read to the next.
    fn open_limited(
        path: &Path,
        options: Options,
        limit: Option<usize>,
        dirs: parts::DirTrust,
    ) -> Result<Store, Error> {
        let mut store = Store::open_once(path, options.clone(), limit, dirs)?;
        for _ in 1..READS {
            if store.earlier_parts_kept() {
                return Ok(store);
            }
            store = Store::open_once(path, options.clone(), limit, store.dirs)?;
        }
        if !store.earlier_parts_kept() {
            store.torn = true;
            if let Some(w) = &store.options.warnings {
                w(&Warning {
                    part: store.path.clone(),
                    offset: None,
                    message: format!(
                        "the parts before the last kept changing while they were read ({READS} reads); the read may be torn"
                    ),
                });
            }
        }
        Ok(store)
    }

    /// Whether every part but the last is as it was when it was opened.
    pub(crate) fn earlier_parts_kept(&self) -> bool {
        #[cfg(test)]
        if TORN.with(|t| t.replace(t.get().saturating_sub(1))) > 0 {
            return false;
        }
        let earlier = self.parts.len().saturating_sub(1);
        self.parts[..earlier].iter().enumerate().all(|(i, p)| {
            let now = std::fs::metadata(&p.path).ok().map(|m| Stamp::of(&m));
            now.is_some() && now == self.follow.stamps.get(i).copied().flatten()
        })
    }

    /// Read the store once: from the last part whose first lines look like a snapshot part's
    /// (§19.10.3), unless every part is to be read; when the read of that part does not confirm
    /// it (damage before its marker, or a member that fails its check), from the last one before
    /// it that looks so, down to part 0.
    fn open_once(
        path: &Path,
        options: Options,
        limit: Option<usize>,
        mut dirs: parts::DirTrust,
    ) -> Result<Store, Error> {
        let path = path.to_path_buf();
        let mut format = name::format_of(&path).ok_or_else(|| Error::BadPath(path.clone()))?;
        if let (false, Some(d)) = (format.strict, options.delimiter) {
            if !name::usable_delimiter(d) {
                return Err(Error::Refused(format!(
                    "{}: the delimiter {d:#04x} is not one ASCII character other than a line break",
                    path.display()
                )));
            }
            format.delimiter = d;
        }
        let mut below = usize::MAX;
        loop {
            let hold = Hold::new(options.warnings.clone());
            let attempt = Options {
                warnings: hold.sink(),
                ..options.clone()
            };
            let mut replay = Replay::new(format, options.verify, attempt.warnings.clone());
            if !format.strict {
                replay.lines = options.lines.clone();
            }
            replay.head = options.head && !format.strict;
            // A loose rewrite works on the lines it keeps and reads no row: no key is wanted.
            let no_rows = Scope::Keys(Vec::new());
            let scope = if options.lines.is_some() && !format.strict {
                &no_rows
            } else {
                &options.scope
            };
            let mut store = Store {
                replay,
                table: Table::new(scope, format),
                path: path.clone(),
                format,
                options: attempt,
                parts: Vec::new(),
                unreadable: Vec::new(),
                limit,
                follow: Follow::default(),
                torn: false,
                last: None,
                dirs,
                start: 0,
                marked: false,
            };
            let loaded = store.load(below, &hold);
            if loaded.is_err() || store.start == 0 || store.marked {
                hold.release();
                loaded?;
                store.options.warnings = options.warnings.clone();
                store.replay.set_warn(options.warnings.clone());
                return Ok(store);
            }
            below = store.start;
            dirs = store.dirs;
        }
    }

    /// Read the parts, from the last one before `below` that looks like a snapshot part; `hold`
    /// holds what the read says while it begins at one, until the part is confirmed.
    fn load(&mut self, below: usize, hold: &Hold) -> Result<(), Error> {
        // Sampled before the listing, so that a name made meanwhile shows as a change.
        let dir = self.dirs.stamp(&self.path);
        let (mut list, linked) = parts::list_linked(&self.path)?;
        if let Some(n) = self.limit {
            list.truncate(n);
        }
        if list.is_empty() {
            return Err(Error::Missing(self.path.clone()));
        }
        let skips = self.format.strict && !self.options.verify && !self.options.every_part;
        self.start = if skips {
            snapshot_start(&list, below, self.format)
        } else {
            0
        };
        let start = self.start;
        if start > 0 {
            hold.begin();
        }
        if !self.options.skip_unreadable {
            if let Some(part) = list[start..]
                .iter()
                .find(|p| p.codec.is_some_and(|c| !c.supported()))
            {
                return Err(unsupported(part));
            }
        }
        self.follow = Follow {
            dir: dir.filter(|_| !linked),
            ..Follow::default()
        };
        // The parts before it are not read, but they are stamped: a change to one of them is a
        // change to the store (§19.10.3), which `refresh` reads again.
        for part in &list[..start] {
            let meta = std::fs::metadata(&part.path).ok();
            self.follow.stamps.push(meta.map(|m| Stamp::of(&m)));
        }
        let last = list.len() - 1;
        let mut judge = SnapshotLines::new(self.format.delimiter);
        for (i, part) in list.iter().enumerate().skip(start) {
            self.replay.begin_part(&part.path, i == start);
            let judging = (i == start).then_some(&mut judge);
            if let Err(e) = self.read_part(part, i == last, judging) {
                if !self.options.skip_unreadable {
                    return Err(e);
                }
                self.skip(i, part, &e);
            }
            if i == start && judge.verdict == Some(true) {
                hold.release(); // the start is confirmed: what it said is said, the rest as it comes
            }
            if self.replay.stopped.is_some() {
                break; // a stop policy ends the read of the whole store (§4.10)
            }
        }
        self.marked = judge.verdict == Some(true);
        // Parts a stop left unread are stamped too, so that `refresh` sees whether they change.
        for part in &list[self.follow.stamps.len()..] {
            let meta = std::fs::metadata(&part.path).ok();
            self.follow.stamps.push(meta.map(|m| Stamp::of(&m)));
        }
        self.parts = list
            .into_iter()
            .enumerate()
            .map(|(i, p)| PartInfo {
                index: i,
                ordinal: p.ordinal,
                path: p.path,
                codec: p.codec,
                active: i == last,
            })
            .collect();
        Ok(())
    }

    /// Replay one part; `last` marks the store's last part, whose end the follow state records.
    /// `judge` gets the part's first lines, and the damage met before its verdict.
    fn read_part(
        &mut self,
        part: &parts::Part,
        last: bool,
        judge: Option<&mut SnapshotLines>,
    ) -> Result<(), Error> {
        let file = File::open(&part.path).map_err(|e| Error::io(&part.path, e))?;
        let mut reader: &File = &file;
        let meta = file.metadata().map_err(|e| Error::io(&part.path, e))?;
        if meta.is_dir() {
            return Err(Error::io(
                &part.path,
                std::io::Error::from(std::io::ErrorKind::IsADirectory),
            ));
        }
        self.follow.stamps.push(Some(Stamp::of(&meta)));
        if part.codec.is_some_and(|c| !c.supported()) {
            return Err(unsupported(part));
        }
        // A read that stops at a file's first keyed line holds about as much of a compressed member
        // as a member of the default size ([`codec::read_items`] says what more): a larger one is
        // decoded again once it passed its check. A whole read holds each member's output once.
        let keep = if self.replay.head {
            DEFAULT_MEMBER_SIZE as usize
        } else {
            usize::MAX
        };
        let mut feed = Feed {
            replay: &mut self.replay,
            table: &mut self.table,
            judge,
        };
        let mut framer = Framer::new();
        match part.codec {
            Some(codec) => {
                codec::read_items(codec, &file, keep, &mut |item| match item {
                    Item::Data(out) => framer.push(&out, &mut |l| feed.line(l)),
                    Item::CutShort { offset, out } => {
                        feed.damage();
                        feed.replay.cut_short(offset);
                        framer.push(&out, &mut |l| feed.line(l))
                    }
                    Item::Corrupt { offset, out } => {
                        feed.damage();
                        match feed.replay.corrupt(offset, out.len()) {
                            Policy::Recover => framer.push(&out, &mut |l| feed.line(l)),
                            Policy::Skip => true,
                            Policy::Stop => false,
                        }
                    }
                    Item::Break => {
                        feed.damage();
                        framer.brk();
                        true
                    }
                })
                .map_err(|e| Error::io(&part.path, e))?;
            }
            None => {
                let mut buf = vec![0u8; 1 << 20];
                loop {
                    let n = match reader.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => n,
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(e) => return Err(Error::io(&part.path, e)),
                    };
                    if !framer.push(&buf[..n], &mut |l| feed.line(l)) {
                        break;
                    }
                }
            }
        }
        if self.replay.stopped.is_some() {
            return Ok(());
        }
        if self.format.strict {
            self.replay.tail(framer.tail().len());
        } else if (!framer.tail().is_empty() || framer.tail_after_bom())
            && !(self.replay.head && self.replay.one_column.is_some())
        {
            // TSVZ.py reads a loose file's unterminated last line as a line (a file of only a
            // byte-order mark holds one: the mark).
            let at = (framer.number() + 1, framer.committed());
            let bom = framer.tail_after_bom();
            self.replay
                .loose_tail(framer.tail(), bom, at, &mut self.table);
        }
        if last && part.codec.is_none() {
            self.follow.offset = framer.committed();
            self.follow.line = framer.number();
            self.follow.tail = framer.tail().to_vec();
            self.follow.check = read_check(&file, framer.committed());
            self.last = Some((file, meta.ino()));
        }
        Ok(())
    }

    /// Record part `i` as skipped: it could not be read (§21.8.5). The replay goes on with the
    /// next part from the state it had reached.
    fn skip(&mut self, i: usize, part: &parts::Part, e: &Error) {
        if self.follow.stamps.len() == i {
            let meta = std::fs::metadata(&part.path).ok();
            self.follow.stamps.push(meta.map(|m| Stamp::of(&m)));
        }
        let reason = match e {
            Error::Io { source, .. } => source.to_string(),
            other => other.to_string(),
        };
        if let Some(w) = &self.options.warnings {
            w(&Warning {
                part: part.path.clone(),
                offset: None,
                message: format!(
                    "skipped this part, which cannot be read ({reason}); the read is incomplete"
                ),
            });
        }
        self.unreadable.push(part.path.clone());
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

    /// The key a requested KEY names (§20.2.7): while the final stripping is on, a key a reader
    /// would strip names the live key equal to it, else what a reader resolves it to (§7.7).
    /// In a loose file, the key a reader names it by ([`crate::loose::name`]: a line break or a
    /// delimiter at its end stays, a blank goes), or, for a key no row can have, the key itself.
    pub fn lookup_key<'k>(&self, requested: &'k str) -> Cow<'k, str> {
        if !self.format.strict {
            return crate::loose::lookup(requested, self.format.delimiter);
        }
        let strip = self.format.strict && self.replay.markers.strip;
        crate::write::name_key(requested, self.format.delimiter, strip, |k| {
            self.table.index.contains_key(k)
        })
    }

    pub fn get(&self, key: &str) -> Option<Row<'_>> {
        let i = *self.table.index.get(key)?;
        let width = self.width();
        self.table.slots[i].as_ref().map(|s| row_of(s, width))
    }

    /// What a read of a missing key returns (§14.5): the current defaults, or None (not-found).
    /// A loose file returns none, as TSVZ.py's command line does.
    pub fn missing(&self) -> Option<Row<'_>> {
        if !self.format.strict || !self.replay.markers.return_missing {
            return None;
        }
        Some(Row::new(
            &EMPTY_CELLS,
            &self.replay.markers.defaults,
            false,
            self.width(),
        ))
    }

    /// The width rows are padded to, in value columns: see [`Row`]. 0 before any data row.
    pub fn width(&self) -> usize {
        self.replay.width.unwrap_or(0)
    }

    /// Live keys and rows in first-appearance order (§3.4).
    pub fn iter(&self) -> impl Iterator<Item = (&str, Row<'_>)> {
        let width = self.width();
        self.table
            .slots
            .iter()
            .flatten()
            .map(move |s| (&*s.key, row_of(s, width)))
    }

    pub fn first(&self) -> Option<(&str, Row<'_>)> {
        self.iter().next()
    }

    pub fn last(&self) -> Option<(&str, Row<'_>)> {
        let width = self.width();
        self.table
            .slots
            .iter()
            .rev()
            .flatten()
            .next()
            .map(|s| (&*s.key, row_of(s, width)))
    }

    pub fn stopped(&self) -> Option<&Damage> {
        self.replay.stopped.as_ref()
    }

    /// Whether the first part this read replayed begins with `#_snapshot_#` (§19.10.1), as its
    /// lines came out of the read: its first committed line, or its second after a comment, with
    /// no damage met before it.
    pub(crate) fn marked(&self) -> bool {
        self.marked
    }

    /// The highest spec version a `#_version_#` line of the read declared (1 when none did).
    pub(crate) fn newest_version(&self) -> u64 {
        self.replay.newest
    }

    /// Whether the store still has the parts it read, by name: known without listing them while
    /// the directory is as the last check found it and old enough for its timestamp to show a
    /// change; else they are listed, and the directory's timestamp recorded when they are,
    /// unless part 0 was a symbolic link in that listing.
    pub(crate) fn same_parts(&mut self) -> Result<bool, Error> {
        // Where the directory's timestamp is kept (parts::DirTrust), the same device, inode and
        // timestamp, not too recent when last listed, mean the same names; elsewhere (a union
        // file system shows one branch's; network ones lag) it is listed.
        let now = self.dirs.stamp(&self.path);
        if now.is_some() && now == self.follow.dir {
            return Ok(true);
        }
        let (mut listed, linked) = parts::list_linked(&self.path)?;
        if let Some(n) = self.limit {
            listed.truncate(n);
        }
        if listed
            .iter()
            .map(|p| &p.path)
            .ne(self.parts.iter().map(|p| &p.path))
        {
            return Ok(false);
        }
        self.follow.dir = now.filter(|_| !linked);
        Ok(true)
    }

    /// Whether the read met a checksum marker of a known algorithm (§15).
    pub(crate) fn has_checksums(&self) -> bool {
        self.replay.checksums
    }

    /// The parts skipped because they could not be read (with [`Options::skip_unreadable`]).
    pub fn unreadable(&self) -> &[PathBuf] {
        &self.unreadable
    }

    /// Whether the read missed something: a part was skipped, or a `stop` policy ended it
    /// (§4.10). Such a read has failed, and nothing should be changed on its strength.
    pub fn incomplete(&self) -> bool {
        self.replay.stopped.is_some() || !self.unreadable.is_empty() || self.torn
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

    /// Whether a loose file's column count is one; None before its first keyed line.
    #[cfg(feature = "cli")]
    pub(crate) fn one_column(&self) -> Option<bool> {
        self.replay.one_column
    }

    pub fn parts(&self) -> &[PartInfo] {
        &self.parts
    }

    /// Catch up with the files (TSVZ.py's `sync`): replay what was appended
    /// to the last part since the last read; read the whole store again when anything else
    /// changed — the part list, an earlier part, a compressed last part, a stopped read, or an
    /// in-place rewrite (the bytes before the old read offset no longer match).
    pub fn refresh(&mut self) -> Result<Refreshed, Error> {
        if !self.same_parts()? {
            return self.reload();
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
        Ok(if followed {
            Refreshed::Followed
        } else {
            Refreshed::Unchanged
        })
    }

    pub(crate) fn reload(&mut self) -> Result<Refreshed, Error> {
        *self = Store::open_limited(&self.path, self.options.clone(), self.limit, self.dirs)?;
        Ok(Refreshed::Reloaded)
    }

    /// Replay what was appended to the last part; false when it changed some other way.
    fn follow_append(&mut self, stamp: Stamp) -> Result<bool, Error> {
        let last = self.parts.len() - 1;
        let path = self.parts[last].path.clone();
        let Some(old) = self.follow.stamps.get(last).copied().flatten() else {
            return Ok(false);
        };
        let offset = self.follow.offset;
        let want = offset.min(CHECK_BYTES) as usize;
        if self.replay.stopped.is_some()
            || self.parts[last].codec.is_some()
            || self.unreadable.contains(&path)
            || stamp.ino != old.ino
            || stamp.size < offset
            || stamp.size <= old.size
            || self.follow.check.len() != want
            // The read ended on a byte-order mark alone, which the framer took off: an appended
            // line is the part's line 1 (its header, its key, its column count), which a follow
            // that resumes after the mark does not make it.
            || (offset > 0 && self.follow.line == 0)
        {
            return Ok(false); // an append only ever grows the same file
        }
        let io = |e: std::io::Error| Error::io(&path, e);
        if self.last.as_ref().is_none_or(|(_, ino)| *ino != stamp.ino) {
            let file = File::open(&path).map_err(io)?;
            let ino = file.metadata().map_err(io)?.ino();
            self.last = Some((file, ino));
        }
        let (file, _) = self.last.as_ref().expect("opened above");
        let mut check = vec![0u8; want];
        if file
            .read_exact_at(&mut check, offset - want as u64)
            .is_err()
            || check != self.follow.check
        {
            return Ok(false); // rewritten in place (§19.8)
        }
        let mut data = Vec::new();
        let mut reader: &File = file;
        reader.seek(SeekFrom::Start(offset)).map_err(io)?;
        reader.read_to_end(&mut data).map_err(io)?;
        if !self.format.strict && (!self.follow.tail.is_empty() || !data.ends_with(b"\n")) {
            // A loose file's unterminated last line is read as a line (TSVZ.py's): read again
            // where one was, or is.
            return Ok(false);
        }
        let mut framer = Framer::resume(offset, self.follow.line);
        let (replay, table) = (&mut self.replay, &mut self.table);
        replay.begin_part(&path, last == self.start && offset == 0);
        framer.push(&data, &mut |l| replay.line(l, table));
        let committed = (framer.committed() - offset) as usize;
        if self.replay.stopped.is_none()
            && !framer.tail().is_empty()
            && framer.tail() != self.follow.tail.as_slice()
        {
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
        self.follow.stamps[last] = Some(Stamp {
            size: offset + data.len() as u64,
            ..stamp
        });
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;

    fn write(dir: &Path, name: &str, data: &[u8]) -> PathBuf {
        let p = dir.join(name);
        fs::write(&p, data).unwrap();
        p
    }

    fn rows(store: &Store) -> Vec<Vec<String>> {
        store
            .iter()
            .map(|(k, r)| std::iter::once(k.to_string()).chain(r.to_vec()).collect())
            .collect()
    }

    fn v(cells: &[&str]) -> Vec<String> {
        cells.iter().map(|c| c.to_string()).collect()
    }

    #[test]
    fn a_directory_that_does_not_keep_its_timestamp_is_listed_at_every_check() {
        // A union file system (mergerfs) may create a file on a branch whose directory's
        // timestamp is not the one shown: an unchanged timestamp proves nothing there.
        let dir = tempfile::tempdir().unwrap();
        let p = write(dir.path(), "s.tsvz", b"a\t1\n");
        let _ahead = parts::RunAhead::by(60);
        let mut store = Store::open(&p, Options::default()).unwrap();
        store.dirs = parts::DirTrust::not_kept(); // as on mergerfs, NFS, SMB, …
        let before = parts::listings();
        store.refresh().unwrap();
        assert_eq!(parts::listings() - before, 1);
        write(dir.path(), "s.tsvz.1", b"b\t2\n");
        store.refresh().unwrap();
        assert_eq!(store.len(), 2, "the new part was read");
    }

    #[test]
    fn a_part_0_that_is_a_symbolic_link_is_listed_at_every_check() {
        // It is listed by its target, which may appear elsewhere, the directory unchanged.
        let root = tempfile::tempdir().unwrap();
        let (d, o) = (root.path().join("d"), root.path().join("o"));
        fs::create_dir(&d).unwrap();
        fs::create_dir(&o).unwrap();
        if !parts::fine_times(&d.join("other.tsvz")) {
            return;
        }
        let p = d.join("s.tsvz");
        std::os::unix::fs::symlink(o.join("real.tsvz"), &p).unwrap();
        write(&d, "s.tsvz.1", b"b\t2\n");
        let _ahead = parts::RunAhead::by(60);
        let mut store = Store::open(&p, Options::default()).unwrap();
        assert_eq!(store.len(), 1);
        // A check that lists the same names keeps no stamp either.
        assert_eq!(store.refresh().unwrap(), Refreshed::Unchanged);
        write(&o, "real.tsvz", b"a\t1\n");
        store.refresh().unwrap();
        assert_eq!(store.len(), 2, "part 0 was read");
    }

    #[test]
    fn a_part_0_that_became_a_symbolic_link_is_listed_at_every_check() {
        // After a stamp was trusted: the listing, not the file system, says part 0 is a link.
        let root = tempfile::tempdir().unwrap();
        let (d, o) = (root.path().join("d"), root.path().join("o"));
        fs::create_dir(&d).unwrap();
        fs::create_dir(&o).unwrap();
        let p = write(&d, "s.tsvz", b"a\t1\n");
        write(&d, "s.tsvz.1", b"b\t2\n");
        if !parts::fine_times(&p) {
            return;
        }
        let _ahead = parts::RunAhead::by(60);
        let mut store = Store::open(&p, Options::default()).unwrap();
        assert_eq!(store.refresh().unwrap(), Refreshed::Unchanged);
        fs::remove_file(&p).unwrap();
        std::os::unix::fs::symlink(o.join("real.tsvz"), &p).unwrap();
        let _later = parts::RunAhead::by(60);
        store.refresh().unwrap();
        assert_eq!(store.len(), 1);
        write(&o, "real.tsvz", b"a\t9\n");
        store.refresh().unwrap();
        assert_eq!(store.len(), 2, "part 0 was read");
    }

    #[test]
    fn a_part_0_that_is_no_longer_a_link_lets_the_directory_be_trusted_again() {
        let root = tempfile::tempdir().unwrap();
        let (d, o) = (root.path().join("d"), root.path().join("o"));
        fs::create_dir(&d).unwrap();
        fs::create_dir(&o).unwrap();
        let p = d.join("s.tsvz");
        std::os::unix::fs::symlink(write(&o, "real.tsvz", b"a\t1\n"), &p).unwrap();
        write(&d, "s.tsvz.1", b"b\t2\n");
        if !parts::fine_times(&d.join("other.tsvz")) {
            return;
        }
        let _ahead = parts::RunAhead::by(60);
        let mut store = Store::open(&p, Options::default()).unwrap();
        fs::remove_file(&p).unwrap();
        write(&d, "s.tsvz", b"a\t1\n");
        let _later = parts::RunAhead::by(60);
        store.refresh().unwrap();
        let before = parts::listings();
        assert_eq!(store.refresh().unwrap(), Refreshed::Unchanged);
        assert_eq!(parts::listings(), before, "trusted again");
    }

    #[test]
    fn a_read_done_again_keeps_what_the_store_learnt_of_its_directory() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(dir.path(), "s.tsvz", b"a\t1\n");
        write(dir.path(), "s.tsvz.1", b"b\t2\n");
        TORN.with(|t| t.set(2));
        let before = parts::asked();
        Store::open(&p, Options::default()).unwrap();
        assert_eq!(parts::asked() - before, 1, "asked by the first read only");
    }

    #[test]
    fn a_kept_directory_timestamp_spares_the_listing_until_a_name_changes() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(dir.path(), "s.tsvz", b"a\t1\n");
        assert!(!parts::keeps_dir_times(Path::new("/nowhere/at/all/s.tsvz")));
        if !parts::fine_times(&p) {
            return;
        }
        // A directory last changed long ago: its timestamp is trusted as it is.
        let _ahead = parts::RunAhead::by(60);
        let mut store = Store::open(&p, Options::default()).unwrap();
        let before = parts::listings();
        assert_eq!(store.refresh().unwrap(), Refreshed::Unchanged);
        assert_eq!(
            parts::listings(),
            before,
            "nothing changed: the directory is not listed"
        );
        // A new name moves the timestamp: the directory is listed, the new part read.
        write(dir.path(), "s.tsvz.1", b"b\t2\n");
        store.refresh().unwrap();
        assert_eq!(store.len(), 2);
    }

    #[test]
    fn a_new_part_is_seen_though_the_directory_modification_time_was_set_back() {
        // rsync -a, tar x, cp -p, touch -r set a directory's modification time back to what it
        // was; its change time, which every name change sets, no program can.
        let dir = tempfile::tempdir().unwrap();
        let p = write(dir.path(), "s.tsvz", b"a\t1\n");
        if !parts::fine_times(&p) {
            return;
        }
        let _ahead = parts::RunAhead::by(60);
        let mut store = Store::open(&p, Options::default()).unwrap();
        let listed = fs::metadata(dir.path()).unwrap().modified().unwrap();
        write(dir.path(), "s.tsvz.1", b"b\t2\n");
        fs::File::open(dir.path())
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(listed))
            .unwrap();
        store.refresh().unwrap();
        assert_eq!(store.len(), 2, "the new part was read");
    }

    #[test]
    fn a_reload_keeps_what_the_store_learnt_of_its_directory() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(dir.path(), "s.tsvz", b"a\t1\n");
        let mut store = Store::open(&p, Options::default()).unwrap();
        store.dirs = parts::DirTrust::not_kept();
        let before = parts::asked();
        store.reload().unwrap();
        assert_eq!(parts::asked(), before, "not asked again");
    }

    #[test]
    fn following_appends_keeps_a_classic_lock() {
        // A handler follows the files while its writer holds the lock. Where the lock is a
        // classic POSIX one, closing any descriptor of the file in the process releases it, so
        // a follow must open and close none.
        let dir = tempfile::tempdir().unwrap();
        let p = write(dir.path(), "s.tsvz", b"a\t1\n");
        let mut store = Store::open(&p, Options::default()).unwrap();
        let file = fs::OpenOptions::new().append(true).open(&p).unwrap();
        let guard = crate::lock::lock_classic(&file).unwrap();
        for i in 0..3 {
            (&file).write_all(format!("k{i}\tv\n").as_bytes()).unwrap();
            assert_eq!(store.refresh().unwrap(), Refreshed::Followed);
            assert!(
                crate::lock::locked_elsewhere(&p),
                "follow {i} released the classic lock"
            );
        }
        drop(guard);
        assert_eq!(store.len(), 4);
    }

    #[test]
    fn a_loose_delimiter_must_be_one_ascii_character_other_than_a_line_break() {
        // A byte past ASCII cuts characters in two (a panic); a line break splits rows.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.tsv");
        fs::write(&p, "a\u{a7}1\n\u{e7}\n").unwrap();
        for d in [0xA7, 0x80, b'\n', b'\r'] {
            let options = Options {
                delimiter: Some(d),
                ..Options::default()
            };
            let e = Store::open(&p, options).err().expect("refused");
            assert!(e.to_string().contains("delimiter"), "{d:#x}: {e}");
        }
    }

    #[test]
    fn a_part_rewritten_while_the_store_is_read_is_read_again() {
        // A multi-part scrub rewrites an earlier part in place, and readers take no lock: a
        // read that saw it change reads the store again rather than answer from torn bytes.
        use std::sync::atomic::{AtomicUsize, Ordering};
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.tsvz");
        fs::write(&p, "a\t1\n#_version_#\tx\nb\t2\n").unwrap();
        fs::write(dir.path().join("s.tsvz.1"), "z\t9\n").unwrap();
        let fired = Arc::new(AtomicUsize::new(0));
        let mut options = Options::default();
        let (part, count) = (p.clone(), fired.clone());
        options.warnings = Some(Arc::new(move |_: &Warning| {
            if count.fetch_add(1, Ordering::SeqCst) == 0 {
                fs::write(&part, "c\t3\n").unwrap(); // the compaction lands mid-read
            }
        }));
        let s = Store::open(&p, options).unwrap();
        let keys: Vec<&str> = s.iter().map(|(k, _)| k).collect();
        assert_eq!(keys, ["c", "z"]);
        assert!(!s.incomplete());
    }

    #[test]
    fn order_last_wins_and_rewrite_after_delete_moves_to_the_end() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(dir.path(), "s.tsvz", b"a\t1\nb\t2\na\nc\t3\na\t4\nb\t5\n");
        let s = Store::open(&p, Options::default()).unwrap();
        assert_eq!(
            rows(&s),
            vec![v(&["b", "5"]), v(&["c", "3"]), v(&["a", "4"])]
        );
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
        assert_eq!(s.get("bob").unwrap().to_vec(), v(&["Bob", ""]));
        assert_eq!(s.missing().unwrap().to_vec(), v(&["new", ""]));
        assert!(s.get("dave").is_none());
        let q = write(dir.path(), "q.tsvz", b"#_fill_empty_with_default_#\ttrue\n#_defaults_#\td1\td2\na\t\tq\n#_return_defaults_when_missing_#\tfalse\n");
        let s = Store::open(&q, Options::default()).unwrap();
        assert_eq!(s.get("a").unwrap().to_vec(), v(&["d1", "q"]));
        assert!(s.missing().is_none());
    }

    #[test]
    fn rows_pad_to_the_width_of_the_first_data_row() {
        let dir = tempfile::tempdir().unwrap();
        // The first data row sets the width even when a tombstone later removes it.
        let p = write(
            dir.path(),
            "s.tsvz",
            b"z\n#_defaults_#\na\t1\t2\t3\na\nb\tx\nc\tp\tq\tr\ts\n",
        );
        let s = Store::open(&p, Options::default()).unwrap();
        assert_eq!(s.get("b").unwrap().to_vec(), v(&["x", "", ""]));
        assert_eq!(s.get("b").unwrap().written(), 1);
        assert_eq!(s.get("c").unwrap().to_vec(), v(&["p", "q", "r", "s"]));
        assert_eq!(s.missing().unwrap().to_vec(), v(&["", "", ""]));
        // With defaults active at the first data row, their count is the width.
        let q = write(
            dir.path(),
            "q.tsvz",
            b"#_defaults_#\td1\nk\tv\tw\tz\n#_defaults_#\nm\tq\n",
        );
        let s = Store::open(&q, Options::default()).unwrap();
        assert_eq!(s.get("m").unwrap().to_vec(), v(&["q"]));
        assert_eq!(s.missing().unwrap().to_vec(), v(&[""]));
        // An empty defaults field still counts, and padding takes the defaults bound to the row.
        let e = write(
            dir.path(),
            "e.tsvz",
            b"#_defaults_#\t\t\nk\tv\n#_defaults_#\tn1\nm\tq\n",
        );
        let s = Store::open(&e, Options::default()).unwrap();
        assert_eq!(s.get("k").unwrap().to_vec(), v(&["v", ""]));
        assert_eq!(s.get("m").unwrap().to_vec(), v(&["q", ""]));
        assert_eq!(s.missing().unwrap().to_vec(), v(&["n1", ""]));
        // No data row: no width, so a missing key is its defaults alone.
        let n = write(dir.path(), "n.tsvz", b"#_defaults_#\td\n");
        let s = Store::open(&n, Options::default()).unwrap();
        assert_eq!(s.missing().unwrap().to_vec(), v(&["d"]));
    }

    #[test]
    fn multipart_markers_span_parts_and_lookup_key_strips() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "s.tsvz", b"#_defaults_#\td\nk\tfrom-0\n");
        write(dir.path(), "s.tsvz.10", b"k\tfrom-10\n");
        write(dir.path(), "s.tsvz.f", b"k\tfrom-f\nz\n");
        let s = Store::open(dir.path().join("s.tsvz"), Options::default()).unwrap();
        assert_eq!(rows(&s), vec![v(&["k", "from-10"])]);
        assert_eq!(
            s.parts()
                .iter()
                .map(|p| (p.index, p.active))
                .collect::<Vec<_>>(),
            vec![(0, false), (1, false), (2, true)]
        );
        assert_eq!(s.lookup_key("k  "), "k");
        assert_eq!(
            s.lookup_key("k \t"),
            "k \t",
            "a tab written <sep> is not stripped (§20.2.7)"
        );
    }

    /// Whether part 1 of the store `path` is a snapshot part (§19.10.1), as a scrub asks: read
    /// alone, it begins with the marker.
    fn part_1_is_snapshot_part(path: &Path) -> bool {
        let list = parts::list(path).unwrap();
        Store::open(&list[1].path, Options::default()).is_ok_and(|s| s.marked())
    }

    /// Whether the first lines of part 1 of the store `path` look like a snapshot part's.
    fn part_1_looks_marked(path: &Path) -> bool {
        let list = parts::list(path).unwrap();
        looks_marked(&list[1], name::format_of(path).unwrap())
    }

    #[test]
    fn a_read_begins_at_the_last_snapshot_part_and_verify_reads_every_part() {
        // §19.10.3, conformance case 19.10.3-a-reader-starts-at-the-last-snapshot-part. These
        // parts break §19.10.2a on purpose (part 2 restates neither a nor b), so that the skip
        // shows. Its header is the snapshot part's (§19.10.2b).
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "s.tsvz", b"#old\na\t1\n");
        write(dir.path(), "s.tsvz.1", b"#_snapshot_#\nb\t2\n");
        write(dir.path(), "s.tsvz.2", b"#h\n#_snapshot_#\nc\t3\n");
        write(dir.path(), "s.tsvz.3", b"d\t4\n");
        let p = dir.path().join("s.tsvz");
        let s = Store::open(&p, Options::default()).unwrap();
        assert_eq!(keys(&s), ["c", "d"]);
        assert_eq!((s.parts().len(), s.header()), (4, Some("#h")));
        // verify replays every part (§19.10.4), and so does a rewrite (a scrub's read).
        let all = |o: Options| keys(&Store::open(&p, o).unwrap());
        let verifying = Options {
            verify: true,
            ..Options::default()
        };
        assert_eq!(all(verifying), ["a", "b", "c", "d"]);
        let every = Options {
            every_part: true,
            ..Options::default()
        };
        assert_eq!(all(every), ["a", "b", "c", "d"]);
    }

    #[test]
    fn the_parts_before_a_snapshot_part_are_not_read() {
        // Part 1 cannot be read (a directory): the read never opens it.
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "s.tsvz", b"x\t0\n");
        fs::create_dir(dir.path().join("s.tsvz.1")).unwrap();
        write(dir.path(), "s.tsvz.2", b"#_snapshot_#\nb\t2\n");
        let s = Store::open(dir.path().join("s.tsvz"), Options::default()).unwrap();
        assert_eq!(keys(&s), ["b"]);
        assert!(!s.incomplete());
    }

    #[test]
    fn damage_before_the_snapshot_part_is_not_met() {
        // §4.10 governs damaged data a reader meets; one that begins at the snapshot part
        // (§19.10.3) meets none before it, as TSVZ.py's read does: under stop it reads the store.
        for (codec, part0) in [
            (None, b"#_on_corruption_#\tstop\na\t1\n\0\0b\t2\n".to_vec()),
            (Some(Codec::Gzip), {
                let mut bad = crate::codec::encode_member(Codec::Gzip, b"b\t2\n").unwrap();
                let n = bad.len();
                bad[n - 8] ^= 1;
                let head = b"#_on_corruption_#\tstop\na\t1\n";
                [crate::codec::encode_member(Codec::Gzip, head).unwrap(), bad].concat()
            }),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let name = |n: &str| match codec {
                Some(c) => format!("{n}.{}", c.name()),
                None => n.to_string(),
            };
            write(dir.path(), &name("s.tsvz"), &part0);
            write(dir.path(), &name("s.tsvz.1"), &{
                let snap = b"#_snapshot_#\n#_on_corruption_#\tstop\na\t1\n";
                match codec {
                    Some(c) => crate::codec::encode_member(c, snap).unwrap(),
                    None => snap.to_vec(),
                }
            });
            let p = dir.path().join(name("s.tsvz"));
            let s = Store::open(&p, Options::default()).unwrap();
            assert_eq!(keys(&s), ["a"], "{codec:?}");
            assert!(!s.incomplete() && !s.damaged(), "{codec:?}");
            let verifying = Options {
                verify: true,
                ..Options::default()
            };
            assert!(Store::open(&p, verifying).unwrap().stopped().is_some());
        }
    }

    #[test]
    fn a_part_that_only_looks_marked_is_read_after_the_parts_before_it() {
        // The marker is in a member that fails its check at its end, past the look's first page:
        // the look, from output before the check, takes it for a snapshot part; the read of it
        // says no, and the store is read from the last part before it that is one, or from part
        // 0.
        let gz = |t: &[u8]| crate::codec::encode_member(Codec::Gzip, t).unwrap();
        let mut bad = gz(noise("#_snapshot_#\nc\t3\n", 2000).as_bytes());
        let n = bad.len();
        bad[n - 8] ^= 1;
        for (earlier, from) in [(&b"b\t2\n"[..], 0), (b"#_snapshot_#\nb\t2\n", 1)] {
            let dir = tempfile::tempdir().unwrap();
            write(dir.path(), "s.tsvz.gz", &gz(b"a\t1\n"));
            write(dir.path(), "s.tsvz.1.gz", &gz(earlier));
            write(dir.path(), "s.tsvz.2.gz", &bad);
            write(dir.path(), "s.tsvz.3.gz", &gz(b"d\t4\n"));
            let p = dir.path().join("s.tsvz.gz");
            let list = parts::list(&p).unwrap();
            assert!(looks_marked(&list[2], name::format_of(&p).unwrap()));
            let (said, options) = hearing();
            let s = Store::open(&p, options).unwrap();
            let every = Options {
                every_part: true,
                ..Options::default()
            };
            let all = keys(&Store::open(&p, every).unwrap());
            assert_eq!(keys(&s), all[from..], "{earlier:?}");
            assert!(s.damaged(), "{earlier:?}: the damaged part was read");
            // The attempt that began at the part said nothing: the read says its damage once.
            let said = said.lock().unwrap();
            let used = said.iter().filter(|m| m.contains("not verified")).count();
            assert_eq!(used, 1, "{earlier:?}: {said:?}");
        }
    }

    /// Options whose warnings are collected, as text.
    fn hearing() -> (Arc<Mutex<Vec<String>>>, Options) {
        let said = Arc::new(Mutex::new(Vec::new()));
        let sink = said.clone();
        let options = Options {
            warnings: Some(Arc::new(move |w: &Warning| {
                sink.lock().unwrap().push(w.message.clone())
            })),
            ..Options::default()
        };
        (said, options)
    }

    #[test]
    fn a_read_that_falls_back_from_a_part_that_only_looks_marked_says_what_the_policy_says() {
        // Under stop in part 0 the damaged member stops the read: the attempt from the part before
        // it, with the policy it began with (recover), used the member's bytes and said so, which
        // never happened; the read that says the stop is the one said.
        let gz = |t: &[u8]| crate::codec::encode_member(Codec::Gzip, t).unwrap();
        let mut bad = gz(noise("#_snapshot_#\nc\t3\n", 2000).as_bytes());
        let n = bad.len();
        bad[n - 8] ^= 1;
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "s.tsvz.gz",
            &gz(b"#_on_corruption_#\tstop\na\t1\n"),
        );
        write(dir.path(), "s.tsvz.1.gz", &gz(b"b\t2\n"));
        write(dir.path(), "s.tsvz.2.gz", &bad);
        let p = dir.path().join("s.tsvz.gz");
        let (said, options) = hearing();
        let s = Store::open(&p, options).unwrap();
        assert_eq!(keys(&s), ["a", "b"]);
        assert!(s.stopped().is_some());
        let said = said.lock().unwrap();
        assert!(!said.iter().any(|m| m.contains("not verified")), "{said:?}");
        assert_eq!(said.len(), 1, "{said:?}");
        assert!(said[0].contains("reading stopped there"), "{said:?}");
    }

    #[test]
    fn a_read_from_a_confirmed_snapshot_part_says_its_warnings_before_it_reads_on() {
        // The snapshot part is confirmed by its first lines; the damage it meets after them is
        // said at once, not kept until every later part is read (memory, and the order of what is
        // said and done). The sink appends to the last part when it first hears: said before the
        // last part is read, the read sees the row.
        let gz = |t: &[u8]| crate::codec::encode_member(Codec::Gzip, t).unwrap();
        let mut part1 = gz(b"#_snapshot_#\nb\t2\n");
        let mut bad = gz(b"x\t9\n");
        let n = bad.len();
        bad[n - 8] ^= 1; // the member's checksum
        part1.extend(bad);
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "s.tsvz.gz", &gz(b"a\t1\n"));
        write(dir.path(), "s.tsvz.1.gz", &part1);
        let last = write(dir.path(), "s.tsvz.2.gz", &gz(b"c\t3\n"));
        let heard = Arc::new(Mutex::new(0));
        let (count, to) = (heard.clone(), last.clone());
        let options = Options {
            warnings: Some(Arc::new(move |_: &Warning| {
                let mut count = count.lock().unwrap();
                *count += 1;
                if *count == 1 {
                    append(&to, &gz(b"d\t4\n"));
                }
            })),
            ..Options::default()
        };
        let s = Store::open(dir.path().join("s.tsvz.gz"), options).unwrap();
        assert_eq!(*heard.lock().unwrap(), 1);
        assert!(keys(&s).contains(&"d".to_string()), "{:?}", keys(&s));
        assert!(
            !keys(&s).contains(&"a".to_string()),
            "begun at the snapshot part"
        );
    }

    #[test]
    fn a_read_from_a_snapshot_part_follows_appends_and_reads_again_when_a_part_before_changes() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(dir.path(), "s.tsvz", b"a\t1\n");
        write(dir.path(), "s.tsvz.1", b"#h\n#_snapshot_#\nb\t2\n");
        let last = write(dir.path(), "s.tsvz.2", b"c\t3\n");
        let mut s = Store::open(&p, Options::default()).unwrap();
        assert_eq!(keys(&s), ["b", "c"]);
        append(&last, b"d\t4\n");
        assert_eq!(s.refresh().unwrap(), Refreshed::Followed);
        assert_eq!(
            (keys(&s), s.header()),
            (vec!["b".to_string(), "c".into(), "d".into()], Some("#h"))
        );
        // A part before the snapshot part changed (against §17.7): a change to the store.
        append(&p, b"z\t9\n");
        assert_eq!(s.refresh().unwrap(), Refreshed::Reloaded);
        assert_eq!(keys(&s), ["b", "c", "d"]);
        // The snapshot part loses its marker: every part is read again.
        fs::write(dir.path().join("s.tsvz.1"), "#h\nb\t2\n").unwrap();
        assert_eq!(s.refresh().unwrap(), Refreshed::Reloaded);
        assert_eq!(keys(&s), ["a", "z", "b", "c", "d"]);
    }

    #[test]
    fn what_is_not_a_snapshot_part() {
        // The verdict a scrub reads a part by (§19.10.1): its first committed line, or its second
        // after a comment, is `#_snapshot_#` with no value. The look and the read agree on it.
        let dir = tempfile::tempdir().unwrap();
        for (name, part1, yes) in [
            ("later", &b"b\t2\n#_snapshot_#\n"[..], false),
            (
                "after-a-marker",
                b"#_version_#\t1\n#_snapshot_#\nb\t2\n",
                false,
            ),
            ("with-a-value", b"#_snapshot_#\tx\nb\t2\n", false),
            ("third-line", b"#c\n#d\n#_snapshot_#\nb\t2\n", false),
            ("uncommitted", b"#_snapshot_#", false),
            ("zero-fill", b"\0\0#_snapshot_#\nb\t2\n", false),
            // Zeros in a comment before the marker are damage, which no snapshot part has.
            ("zero-in-comment", b"#h\0\0\n#_snapshot_#\nb\t2\n", false),
            ("zero-in-marker", b"#_snapshot_#\0\nb\t2\n", false),
            ("first", b"#_snapshot_#\nb\t2\n", true),
            ("after-a-comment", b"#h\n#_snapshot_#\nb\t2\n", true),
            // The key in any case, an empty value as none, after a byte order mark, with CRLF.
            ("case", b"\xef\xbb\xbf#_SNAPSHOT_#\t\r\nb\t2\n", true),
        ] {
            write(dir.path(), &format!("{name}.tsvz"), b"a\t1\n");
            write(dir.path(), &format!("{name}.tsvz.1"), part1);
            let p = dir.path().join(format!("{name}.tsvz"));
            assert_eq!(part_1_is_snapshot_part(&p), yes, "{name}");
            assert_eq!(part_1_looks_marked(&p), yes, "{name}");
            let every = Options {
                every_part: true,
                ..Options::default()
            };
            let all = keys(&Store::open(&p, every).unwrap());
            let read = keys(&Store::open(&p, Options::default()).unwrap());
            assert_eq!(
                read,
                if yes { vec!["b".to_string()] } else { all },
                "{name}"
            );
        }
    }

    #[test]
    fn a_part_that_only_looks_marked_for_the_zeros_in_its_comment_does_not_hide_a_stop() {
        // Part 0 stops the read at the damage that follows (§4.10); part 1 is no snapshot part
        // for it, so the read begins at part 0 as the read of every part does, and stops there.
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "s.tsvz", b"#_on_corruption_#\tstop\na\t1\n");
        write(
            dir.path(),
            "s.tsvz.1",
            b"#h\0\0\n#_snapshot_#\na\t1\nb\t2\n",
        );
        let p = dir.path().join("s.tsvz");
        let s = Store::open(&p, Options::default()).unwrap();
        let every = Options {
            every_part: true,
            ..Options::default()
        };
        let all = Store::open(&p, every).unwrap();
        assert_eq!(keys(&s), ["a"]);
        assert_eq!(keys(&s), keys(&all));
        assert!(s.incomplete() && all.incomplete());
    }

    #[test]
    fn a_gzip_snapshot_part_that_begins_with_zero_padding_is_found() {
        // `read_items` skips zeros before a gzip member, the first one too: the look does.
        let gz = |t: &[u8]| crate::codec::encode_member(Codec::Gzip, t).unwrap();
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "s.tsvz.gz",
            &gz(b"#_on_corruption_#\tstop\na\t1\n"),
        );
        let mut part = vec![0u8; 5];
        part.extend(gz(b"#h\n#_snapshot_#\nb\t2\n"));
        write(dir.path(), "s.tsvz.1.gz", &part);
        let p = dir.path().join("s.tsvz.gz");
        let list = parts::list(&p).unwrap();
        assert!(looks_marked(&list[1], name::format_of(&p).unwrap()));
        let s = Store::open(&p, Options::default()).unwrap();
        assert_eq!(keys(&s), ["b"]);
        assert!(!s.incomplete());
    }

    #[test]
    fn a_compressed_snapshot_part_counts_only_when_its_start_reads_whole() {
        use crate::codec::encode_member;
        use crate::name::Codec::Gzip;
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "s.tsvz", b"a\t1\n");
        let part = encode_member(Gzip, b"#_snapshot_#\nb\t2\n").unwrap();
        write(dir.path(), "s.tsvz.1.gz", &part);
        assert!(part_1_is_snapshot_part(&dir.path().join("s.tsvz")));
        // A corrupt member: its output is read (recover), but it is damage before the marker.
        let mut bad = part.clone();
        let n = bad.len();
        bad[n - 8] ^= 1;
        write(dir.path(), "d.tsvz", b"a\t1\n");
        write(dir.path(), "d.tsvz.1.gz", &bad);
        assert!(!part_1_is_snapshot_part(&dir.path().join("d.tsvz")));
        // Its one large member fails its check at the end: the marker is in a corrupt member, so
        // it is no snapshot part, though its start alone showed the marker.
        let part = encode_member(Gzip, noise("#_snapshot_#\n", 20000).as_bytes()).unwrap();
        assert!(part.len() > SNAPSHOT_HINT);
        let mut bad = part.clone();
        let n = bad.len();
        bad[n - 8] ^= 1;
        write(dir.path(), "big.tsvz", b"a\t1\n");
        write(dir.path(), "big.tsvz.1.gz", &part);
        assert!(part_1_is_snapshot_part(&dir.path().join("big.tsvz")));
        write(dir.path(), "big.tsvz.1.gz", &bad);
        assert!(part_1_looks_marked(&dir.path().join("big.tsvz")));
        assert!(!part_1_is_snapshot_part(&dir.path().join("big.tsvz")));
    }

    #[test]
    fn a_member_with_a_codec_magic_in_its_first_bytes_is_one_member() {
        // §16.6 rule 2 ends a damaged member's bytes at the next magic, but a magic inside a
        // member that passes its check is its own bytes: in a stored block the data, in
        // compressed data any three bytes. A look that split a member there once said no (3 of
        // 3,000 random 65 KB gzip parts).
        let mut content = b"#_snapshot_#\nm\t\x1f\x8b\x08\n".to_vec();
        content.extend(noise("", 600).bytes());
        let mut member = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::none());
        member.write_all(&content).unwrap(); // stored blocks keep the row's bytes as they are
        let part = member.finish().unwrap();
        let at = memchr::memmem::find(&part[1..], Codec::Gzip.magic()).unwrap() + 1;
        assert!(at + 3 < SNAPSHOT_HINT && part.len() > 3 * SNAPSHOT_HINT);
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "s.tsvz", b"a\t1\n");
        write(dir.path(), "s.tsvz.1.gz", &part);
        let p = dir.path().join("s.tsvz");
        assert!(part_1_looks_marked(&p) && part_1_is_snapshot_part(&p));
        assert_eq!(Store::open(&p, Options::default()).unwrap().len(), 601);
    }

    /// Rows of pseudo-random hex, which compress little: `n` of them after `head`.
    fn noise(head: &str, n: usize) -> String {
        let mut out = String::from(head);
        let mut x: u64 = 1;
        for i in 0..n {
            x = x
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            out.push_str(&format!("k{i}\t{x:016x}\n"));
        }
        out
    }

    /// A reader that counts the bytes it hands out.
    struct Counting<'a> {
        rest: &'a [u8],
        read: usize,
    }

    impl Read for Counting<'_> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = buf.len().min(self.rest.len());
            buf[..n].copy_from_slice(&self.rest[..n]);
            self.rest = &self.rest[n..];
            self.read += n;
            Ok(n)
        }
    }

    #[test]
    fn a_look_reads_no_more_of_a_part_than_its_first_lines_take() {
        use crate::codec::encode_member;
        for codec in [Codec::Gzip, Codec::Bzip2, Codec::Xz, Codec::Zstd] {
            if !codec.supported() {
                continue;
            }
            let says = |content: &str| {
                let member = encode_member(codec, content.as_bytes()).unwrap();
                let mut reader = Counting {
                    rest: &member,
                    read: 0,
                };
                let mut lines = SnapshotLines::new(b'\t');
                first_lines(codec, &mut reader, &mut |l| lines.line(l));
                (lines.verdict, reader.read, member.len())
            };
            // A first line that is not a comment, or a second that is not the marker, and the
            // marker: what the decoder has given by then settles it (bzip2 gives nothing before
            // a whole block).
            for (head, verdict) in [
                ("", false),
                ("#h\n", false),
                ("#_snapshot_#\n", true),
                ("#h\n#_snapshot_#\n", true),
            ] {
                let (got, read, whole) = says(&noise(head, 100_000));
                assert_eq!(got, Some(verdict), "{codec:?} {head:?}");
                assert!(
                    read * 2 < whole,
                    "{codec:?} {head:?}: {read} of {whole} bytes"
                );
            }
        }
    }

    #[test]
    fn a_compressed_snapshot_part_is_found_however_far_its_first_lines_lie() {
        // bzip2 gives nothing before a whole block (up to 900 kB of text), and a long header
        // comment compresses past any fixed first read: the look reads on until its first lines
        // come out, and the read confirms them, so that a read begins there whatever the codec.
        use crate::name::Codec;
        let long_header = format!("#{}\n", noise("", 15000).replace(['\t', '\n'], " "));
        for (what, content) in [
            ("rows", noise("#h\n#_snapshot_#\n", 20000)),
            (
                "long header",
                noise(&format!("{long_header}#_snapshot_#\n"), 10),
            ),
        ] {
            for codec in [Codec::Gzip, Codec::Bzip2, Codec::Xz, Codec::Zstd] {
                if !codec.supported() {
                    continue;
                }
                let dir = tempfile::tempdir().unwrap();
                write(dir.path(), "s.tsvz", b"#h\na\t1\n");
                let part =
                    crate::codec::encode_members(codec, content.as_bytes(), 1 << 20).unwrap();
                assert!(part.len() > SNAPSHOT_HINT, "{what} {codec:?}");
                write(dir.path(), &format!("s.tsvz.1.{}", codec.name()), &part);
                let path = dir.path().join("s.tsvz");
                assert!(part_1_looks_marked(&path), "{what} {codec:?}");
                assert!(part_1_is_snapshot_part(&path), "{what} {codec:?}");
                let s = Store::open(&path, Options::default()).unwrap();
                assert!(s.get("a").is_none(), "{what} {codec:?}: begun at part 1");
            }
        }
    }

    #[test]
    fn a_loose_files_unterminated_last_line_is_read_and_a_follow_reads_it_again_once_ended() {
        // TSVZ.py reads it as a line; a follow that appends to it reads the file again, so the
        // line is read whole (`b<TAB>23`), and a byte-order mark stays part of line 1's key.
        let dir = tempfile::tempdir().unwrap();
        let p = write(dir.path(), "s.tsv", b"\xef\xbb\xbfa\t1");
        let mut s = Store::open(&p, Options::default()).unwrap();
        assert_eq!(keys(&s), ["\u{feff}a"]);
        append(&p, b"\nb\t2");
        assert_eq!(s.refresh().unwrap(), Refreshed::Reloaded);
        assert_eq!(s.get("b").unwrap().to_vec(), ["2"]);
        append(&p, b"3\n");
        assert_eq!(s.refresh().unwrap(), Refreshed::Reloaded);
        assert_eq!(s.get("b").unwrap().to_vec(), ["23"]);
        append(&p, b"c\t4\n");
        assert_eq!(s.refresh().unwrap(), Refreshed::Followed);
        assert_eq!(keys(&s), ["\u{feff}a", "b", "c"]);
        // A file of only a byte-order mark holds one line, the mark, a key alone.
        let m = write(dir.path(), "m.tsv", b"\xef\xbb\xbf");
        assert_eq!(
            keys(&Store::open(&m, Options::default()).unwrap()),
            ["\u{feff}"]
        );
    }

    #[test]
    fn an_append_to_a_file_of_only_a_byte_order_mark_is_read_again_not_followed() {
        // The appended line is line 1, joined with the mark: a loose file's key `\u{feff}k` (not a
        // row `\u{feff}` and a key `k` of one column), a strict store's header comment.
        let dir = tempfile::tempdir().unwrap();
        let m = write(dir.path(), "m.tsv", b"\xef\xbb\xbf");
        let mut s = Store::open(&m, Options::default()).unwrap();
        append(&m, b"k\t1\nj\n");
        assert_eq!(s.refresh().unwrap(), Refreshed::Reloaded);
        let fresh = Store::open(&m, Options::default()).unwrap();
        assert_eq!(keys(&s), keys(&fresh));
        assert_eq!(keys(&s), ["\u{feff}k"]);
        let z = write(dir.path(), "m.tsvz", b"\xef\xbb\xbf");
        let mut s = Store::open(&z, Options::default()).unwrap();
        append(&z, b"#name\tage\nk\t1\n");
        assert_eq!(s.refresh().unwrap(), Refreshed::Reloaded);
        assert_eq!(keys(&s), ["k"]);
        assert_eq!(s.header(), Some("#name\tage"));
        append(&z, b"j\t2\n");
        assert_eq!(s.refresh().unwrap(), Refreshed::Followed);
        assert_eq!(keys(&s), ["k", "j"]);
    }

    #[test]
    fn a_loose_files_header_is_line_1_without_the_nuls_at_its_start() {
        // As a reader reads the line (`_processLine`), and as 0.2.0 gave the header.
        let dir = tempfile::tempdir().unwrap();
        let p = write(dir.path(), "h.tsv", b"\0\0#name\tage\na\t1\n");
        let s = Store::open(&p, Options::default()).unwrap();
        assert_eq!(
            (s.header(), keys(&s)),
            (Some("#name\tage"), vec!["a".to_string()])
        );
    }

    #[test]
    fn a_loose_files_last_member_cut_short_leaves_its_unfinished_line_unread() {
        // A line a cut member ends in is no line (TSVZ.py reads the whole lines of such a member):
        // the unterminated last line of a plain loose file is read, this one is not.
        let dir = tempfile::tempdir().unwrap();
        let gz = |data: &[u8]| crate::codec::encode_member(crate::name::Codec::Gzip, data).unwrap();
        let mut part = gz(b"a\t1\n");
        let mut cut = gz(b"b\t2\ncc\t3");
        cut.truncate(cut.len() - 8); // the check bytes
        part.extend(&cut);
        let p = write(dir.path(), "s.tsv.gz", &part);
        let s = Store::open(&p, Options::default()).unwrap();
        assert_eq!(keys(&s), ["a", "b"]);
        assert!(s.damaged());
    }

    #[test]
    fn compressed_parts_and_the_corruption_policy() {
        let dir = tempfile::tempdir().unwrap();
        let mut bad =
            crate::codec::encode_member(crate::name::Codec::Gzip, b"b\t2\nc\t3\n").unwrap();
        let n = bad.len();
        bad[n - 8] ^= 1; // CRC mismatch: a corrupt member whose output is still decoded
        for (policy, want) in [
            ("recover", vec!["a", "b", "c", "d"]),
            ("skip", vec!["a", "d"]),
            ("stop", vec!["a"]),
        ] {
            let mut part = crate::codec::encode_member(
                crate::name::Codec::Gzip,
                format!("#_on_corruption_#\t{policy}\na\t1\n").as_bytes(),
            )
            .unwrap();
            part.extend(&bad);
            part.extend(crate::codec::encode_member(crate::name::Codec::Gzip, b"d\t4\n").unwrap());
            let p = write(dir.path(), &format!("{policy}.tsvz.gz"), &part);
            let s = Store::open(&p, Options::default()).unwrap();
            assert_eq!(
                s.iter().map(|(k, _)| k).collect::<Vec<_>>(),
                want,
                "{policy}"
            );
            assert_eq!(s.stopped().is_some(), policy == "stop");
            assert!(s.damaged());
        }
    }

    #[test]
    fn scopes() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(dir.path(), "s.tsvz", b"a\tx\ty\nb\tz\nc\tw\n");
        let keys = Store::open(
            &p,
            Options {
                scope: Scope::KeysOnly,
                ..Options::default()
            },
        )
        .unwrap();
        assert_eq!(
            keys.iter()
                .map(|(k, r)| (k, r.written()))
                .collect::<Vec<_>>(),
            vec![("a", 0), ("b", 0), ("c", 0)]
        );
        let some = Store::open(
            &p,
            Options {
                scope: Scope::Keys(vec!["b  ".into(), "zz".into()]),
                ..Options::default()
            },
        )
        .unwrap();
        assert_eq!(some.len(), 1);
        assert_eq!(
            some.get(&some.lookup_key("b  ")).unwrap().to_vec(),
            v(&["z", ""])
        );
    }

    #[test]
    fn verify_reports_mismatches_with_their_part() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "s.tsvz", b"#_checksum_crc32_#\na\tx\n");
        write(
            dir.path(),
            "s.tsvz.1",
            b"b\ty\n#_checksum_crc32_#\t00000000\n",
        );
        let s = Store::open(
            dir.path().join("s.tsvz"),
            Options {
                verify: true,
                ..Options::default()
            },
        )
        .unwrap();
        let m = &s.mismatches()[0];
        assert_eq!(
            (m.part.clone(), m.line, m.computed.as_str()),
            (dir.path().join("s.tsvz.1"), 2, "c1ec8234")
        );
    }

    #[test]
    fn header_comment() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(dir.path(), "s.tsvz", b"#key\tval\na\tx\n");
        assert_eq!(
            Store::open(&p, Options::default()).unwrap().header(),
            Some("#key\tval")
        );
    }

    /// A loose header loses the NULs a reader drops: at its start, and at the end of an
    /// unterminated line 1 (a torn write), not those of a terminated one.
    #[test]
    fn a_loose_header_loses_the_nuls_a_reader_drops() {
        let dir = tempfile::tempdir().unwrap();
        for (name, data, header) in [
            ("cut.tsv", &b"\0#h\0\0"[..], "#h"),
            ("ended.tsv", b"\0#h\0\n", "#h\0"),
        ] {
            let p = write(dir.path(), name, data);
            let s = Store::open(&p, Options::default()).unwrap();
            assert_eq!(s.header(), Some(header), "{name}");
        }
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
        assert!(matches!(
            Store::open(dir.path().join("none.tsvz"), Options::default()),
            Err(Error::Missing(_))
        ));
        assert!(matches!(
            Store::open(dir.path().join("x.txt"), Options::default()),
            Err(Error::BadPath(_))
        ));
    }

    #[test]
    fn loose_delimiter_override() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(dir.path(), "s.tsv", b"a;x\n");
        let s = Store::open(
            &p,
            Options {
                delimiter: Some(b';'),
                ..Options::default()
            },
        )
        .unwrap();
        assert_eq!(rows(&s), vec![v(&["a", "x"])]);
        let strict = write(dir.path(), "s.tsvz", b"a;x\tb\n");
        let s = Store::open(
            &strict,
            Options {
                delimiter: Some(b';'),
                ..Options::default()
            },
        )
        .unwrap();
        assert_eq!(rows(&s), vec![v(&["a;x", "b"])]);
    }

    fn append(p: &Path, data: &[u8]) {
        use std::io::Write;
        fs::OpenOptions::new()
            .append(true)
            .open(p)
            .unwrap()
            .write_all(data)
            .unwrap();
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
        fs::write(
            &gz,
            crate::codec::encode_member(crate::name::Codec::Gzip, b"a\t1\n").unwrap(),
        )
        .unwrap();
        let mut s = Store::open(&gz, Options::default()).unwrap();
        append(
            &gz,
            &crate::codec::encode_member(crate::name::Codec::Gzip, b"b\t2\n").unwrap(),
        );
        assert_eq!(s.refresh().unwrap(), Refreshed::Reloaded);
        assert_eq!(keys(&s), vec!["a", "b"]);
        let p = write(
            dir.path(),
            "t.tsvz",
            b"#_on_corruption_#\tstop\na\t1\n\0\0\n",
        );
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

    fn skipping(warned: &Arc<std::sync::Mutex<Vec<crate::Warning>>>) -> Options {
        let w = warned.clone();
        Options {
            skip_unreadable: true,
            warnings: Some(Arc::new(move |x: &crate::Warning| {
                w.lock().unwrap().push(x.clone())
            })),
            ..Options::default()
        }
    }

    #[test]
    fn an_unreadable_part_is_skipped_only_when_asked() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(dir.path(), "s.tsvz", b"#_defaults_#\td1\td2\na\t1\n");
        let hole = dir.path().join("s.tsvz.1");
        fs::create_dir(&hole).unwrap();
        write(dir.path(), "s.tsvz.2", b"b\t2\n");
        assert!(matches!(
            Store::open(&p, Options::default()),
            Err(Error::Io { .. })
        ));
        let warned = Arc::new(std::sync::Mutex::new(Vec::new()));
        let s = Store::open(&p, skipping(&warned)).unwrap();
        // The replay goes on past the skipped part with the markers it had reached.
        assert_eq!(rows(&s), vec![v(&["a", "1", "d2"]), v(&["b", "2", "d2"])]);
        assert_eq!(s.unreadable(), std::slice::from_ref(&hole));
        assert!(s.incomplete() && s.stopped().is_none());
        let warned = warned.lock().unwrap();
        assert_eq!(warned.len(), 1);
        assert_eq!(warned[0].part, hole);
        assert!(warned[0].message.contains("skipped"), "{:?}", warned[0]);
        let whole = write(dir.path(), "t.tsvz", b"a\t1\n");
        assert!(
            !Store::open(whole, skipping(&warned_none()))
                .unwrap()
                .incomplete()
        );
    }

    fn warned_none() -> Arc<std::sync::Mutex<Vec<crate::Warning>>> {
        Arc::new(std::sync::Mutex::new(Vec::new()))
    }

    #[test]
    fn a_store_with_a_skipped_part_follows_appends_and_reloads_when_it_changes() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(dir.path(), "s.tsvz", b"a\t1\n");
        let hole = dir.path().join("s.tsvz.1");
        fs::create_dir(&hole).unwrap();
        let last = write(dir.path(), "s.tsvz.2", b"b\t2\n");
        let mut s = Store::open(&p, skipping(&warned_none())).unwrap();
        assert_eq!(s.refresh().unwrap(), Refreshed::Unchanged);
        append(&last, b"c\t3\n");
        assert_eq!(s.refresh().unwrap(), Refreshed::Followed);
        assert_eq!(keys(&s), vec!["a", "b", "c"]);
        assert!(s.incomplete());
        fs::remove_dir(&hole).unwrap();
        fs::write(&hole, "x\t9\n").unwrap();
        assert_eq!(s.refresh().unwrap(), Refreshed::Reloaded);
        assert_eq!(keys(&s), vec!["a", "x", "b", "c"]);
        assert!(s.unreadable().is_empty() && !s.incomplete());
    }

    #[test]
    fn a_read_that_stopped_before_the_last_part_refreshes_without_panicking() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(
            dir.path(),
            "s.tsvz",
            b"#_on_corruption_#\tstop\na\t1\n\0x\t1\n",
        );
        let last = write(dir.path(), "s.tsvz.1", b"b\t2\n");
        let mut s = Store::open(&p, Options::default()).unwrap();
        assert_eq!(keys(&s), vec!["a"]);
        assert_eq!(s.refresh().unwrap(), Refreshed::Unchanged);
        append(&last, b"c\t3\n");
        assert_eq!(s.refresh().unwrap(), Refreshed::Reloaded);
        assert_eq!(keys(&s), vec!["a"]);
        assert!(s.stopped().is_some());
    }

    #[test]
    fn a_requested_key_names_a_key_of_the_store() {
        // §20.2.7: k<space> was written while stripping was off; stripping is on at the end.
        let dir = tempfile::tempdir().unwrap();
        let both = b"#_strip_trailing_whites_#\tfalse\nk \tv1\n#_strip_trailing_whites_#\ttrue\nk\tv2\nt<sep>\tw\n";
        let p = write(dir.path(), "s.tsvz", both);
        for scope in [
            Scope::All,
            Scope::KeysOnly,
            Scope::Keys(vec!["k ".into(), "t\t ".into()]),
        ] {
            let s = Store::open(
                &p,
                Options {
                    scope: scope.clone(),
                    ..Options::default()
                },
            )
            .unwrap();
            assert_eq!(s.lookup_key("k "), "k ", "{scope:?}");
            assert_eq!(s.lookup_key("k"), "k");
            assert_eq!(s.lookup_key("t\t "), "t\t", "{scope:?}");
            assert!(s.get(&s.lookup_key("t\t ")).is_some(), "{scope:?}");
        }
        let s = Store::open(&p, Options::default()).unwrap();
        assert_eq!(
            s.lookup_key("k  "),
            "k",
            "no key k<space><space>: what a reader resolves"
        );
        let off = write(
            dir.path(),
            "o.tsvz",
            b"#_strip_trailing_whites_#\tfalse\nk\tv\n",
        );
        let s = Store::open(&off, Options::default()).unwrap();
        assert_eq!(
            s.lookup_key("k "),
            "k ",
            "stripping is off: a key names itself"
        );
    }

    #[test]
    fn a_skipped_last_part_reloads_on_any_change() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(dir.path(), "s.tsvz", b"a\t1\n");
        let hole = dir.path().join("s.tsvz.1");
        fs::create_dir(&hole).unwrap();
        let mut s = Store::open(&p, skipping(&warned_none())).unwrap();
        assert_eq!(s.refresh().unwrap(), Refreshed::Unchanged);
        fs::remove_dir(&hole).unwrap();
        fs::write(&hole, "b\t2\n").unwrap();
        assert_eq!(s.refresh().unwrap(), Refreshed::Reloaded);
        assert_eq!(keys(&s), vec!["a", "b"]);
        assert!(!s.incomplete());
    }

    #[test]
    fn a_prefix_of_the_parts_reads_alone() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(dir.path(), "s.tsvz", b"#_defaults_#\td\na\t1\n");
        write(dir.path(), "s.tsvz.1", b"b\t2\n#_checksum_crc32_#\n");
        write(dir.path(), "s.tsvz.2", b"a\n");
        let mut two = Store::open_prefix(&p, Options::default(), 2).unwrap();
        assert_eq!(keys(&two), vec!["a", "b"]);
        assert_eq!(two.parts().len(), 2);
        assert!(
            two.parts()[1].active,
            "the last part read is the prefix's last"
        );
        assert!(two.has_checksums());
        two.reload().unwrap();
        assert_eq!(keys(&two), vec!["a", "b"], "a reload keeps the limit");
        assert_ne!(
            two.refresh().unwrap(),
            Refreshed::Reloaded,
            "the parts after it are not its"
        );
        let one = Store::open_prefix(&p, Options::default(), 1).unwrap();
        assert_eq!(keys(&one), vec!["a"]);
        assert!(!one.has_checksums());
        let all = Store::open(&p, Options::default()).unwrap();
        assert_eq!(keys(&all), vec!["b"]);
    }
}
