//! A store: the ordered map of live rows after replaying its parts (§3, §14, §17).

use std::borrow::Cow;
use std::collections::HashSet;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::fs::{FileExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use hashbrown::HashMap;

use crate::codec::{self, Item};
use crate::frame::Framer;
use crate::lock::Handle;
use crate::name::{self, Codec, Format};
use crate::parts::{self, Stamp};
use crate::replay::{Binding, Cells, Damage, EMPTY_CELLS, Markers, Mismatch, Policy, Replay, Sink};
use crate::{Error, WarnFn, Warning};

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
    /// A table for `scope`; under `Scope::Keys` it keeps each requested key and, for a strict
    /// store, the key a reader resolves it to (§20.2.7).
    fn new(scope: &Scope, format: Format) -> Table {
        let wanted = match scope {
            Scope::Keys(keys) => Some(
                keys.iter()
                    .flat_map(|k| {
                        let resolved = if format.strict {
                            crate::write::resolve(k, format.delimiter).into_owned()
                        } else {
                            k.clone()
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
    /// kept and was old enough to show a change ([`parts::DirTrust`]).
    pub dir: Option<parts::DirStamp>,
    pub stamps: Vec<Option<Stamp>>,
    /// Offset just after the last committed line of the last part (plain parts only).
    pub offset: u64,
    pub line: u64,
    pub tail: Vec<u8>,
    /// Up to CHECK_BYTES bytes just before `offset`.
    pub check: Vec<u8>,
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
    /// The last part (plain), kept open to follow what is appended to it: a follow opens no
    /// file, so none waits to be closed while this process holds a classic lock on it
    /// ([`Handle`]).
    last: Option<Handle>,
    /// Whether its directory's timestamp shows every change of its names.
    pub(crate) dirs: parts::DirTrust,
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
        self.read_part(&part, true)?;
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

    fn open_once(
        path: &Path,
        options: Options,
        limit: Option<usize>,
        dirs: parts::DirTrust,
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
        let mut store = Store {
            replay: Replay::new(format, options.verify, options.warnings.clone()),
            table: Table::new(&options.scope, format),
            path,
            format,
            options,
            parts: Vec::new(),
            unreadable: Vec::new(),
            limit,
            follow: Follow::default(),
            torn: false,
            last: None,
            dirs,
        };
        store.load()?;
        Ok(store)
    }

    fn load(&mut self) -> Result<(), Error> {
        // Sampled before the listing, so that a name made meanwhile shows as a change.
        let dir = self.dirs.stamp(&self.path);
        let (mut list, linked) = parts::list_linked(&self.path)?;
        if let Some(n) = self.limit {
            list.truncate(n);
        }
        if list.is_empty() {
            return Err(Error::Missing(self.path.clone()));
        }
        if !self.options.skip_unreadable {
            if let Some(part) = list
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
        let last = list.len() - 1;
        for (i, part) in list.iter().enumerate() {
            self.replay.begin_part(&part.path, i == 0);
            if let Err(e) = self.read_part(part, i == last) {
                if !self.options.skip_unreadable {
                    return Err(e);
                }
                self.skip(i, part, &e);
            }
            if self.replay.stopped.is_some() {
                break; // a stop policy ends the read of the whole store (§4.10)
            }
        }
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
    fn read_part(&mut self, part: &parts::Part, last: bool) -> Result<(), Error> {
        let file = Handle::open_read(&part.path).map_err(|e| Error::io(&part.path, e))?;
        let mut reader: &File = &file;
        let meta = file.opened();
        if meta.is_dir() {
            return Err(Error::io(
                &part.path,
                std::io::Error::from(std::io::ErrorKind::IsADirectory),
            ));
        }
        self.follow.stamps.push(Some(Stamp::of(meta)));
        if part.codec.is_some_and(|c| !c.supported()) {
            return Err(unsupported(part));
        }
        let (replay, table) = (&mut self.replay, &mut self.table);
        let mut framer = Framer::new();
        match part.codec {
            Some(codec) => {
                let mut data = Vec::new();
                reader
                    .read_to_end(&mut data)
                    .map_err(|e| Error::io(&part.path, e))?;
                codec::for_each_item(codec, &data, &mut |item| match item {
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
                });
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
                    if !framer.push(&buf[..n], &mut |l| replay.line(l, table)) {
                        break;
                    }
                }
            }
        }
        if self.replay.stopped.is_some() {
            return Ok(());
        }
        self.replay.tail(framer.tail().len());
        if last && part.codec.is_none() {
            self.follow.offset = framer.committed();
            self.follow.line = framer.number();
            self.follow.tail = framer.tail().to_vec();
            self.follow.check = read_check(&file, framer.committed());
            self.last = Some(file);
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
    /// A loose file's keys name themselves.
    pub fn lookup_key<'k>(&self, requested: &'k str) -> Cow<'k, str> {
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
    pub fn missing(&self) -> Option<Row<'_>> {
        if self.format.strict && !self.replay.markers.return_missing {
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

    /// The highest spec version a `#_version_#` line of the read declared (1 when none did).
    pub(crate) fn newest_version(&self) -> u64 {
        self.replay.newest
    }

    /// Whether the store still has the parts it read, by name: known without listing them while
    /// the directory is as the last check found it and old enough for its timestamp to show a
    /// change; else they are listed, and the directory's timestamp recorded when they are.
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

    pub fn parts(&self) -> &[PartInfo] {
        &self.parts
    }

    /// Catch up with the files (design section 3.7, TSVZ.py's `sync`): replay what was appended
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
        {
            return Ok(false); // an append only ever grows the same file
        }
        let io = |e: std::io::Error| Error::io(&path, e);
        if self
            .last
            .as_ref()
            .is_none_or(|h| h.opened().ino() != stamp.ino)
        {
            self.last = Some(Handle::open_read(&path).map_err(io)?);
        }
        let file = self.last.as_ref().expect("opened above");
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
        let mut framer = Framer::resume(offset, self.follow.line);
        let (replay, table) = (&mut self.replay, &mut self.table);
        replay.begin_part(&path, last == 0 && offset == 0);
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
    fn following_appends_opens_no_file_to_close_under_a_lock() {
        // A handler follows the files while its writer holds the lock: on macOS (classic
        // locks) every descriptor it closed meanwhile would wait for the lock's release.
        let dir = tempfile::tempdir().unwrap();
        let p = write(dir.path(), "s.tsvz", b"a\t1\n");
        let mut store = Store::open(&p, Options::default()).unwrap();
        let file = Handle::open(&p, fs::OpenOptions::new().append(true)).unwrap();
        let guard = crate::lock::lock_classic(&file, &p).unwrap();
        for i in 0..3 {
            (&*file).write_all(format!("k{i}\tv\n").as_bytes()).unwrap();
            assert_eq!(store.refresh().unwrap(), Refreshed::Followed);
        }
        assert_eq!(
            crate::lock::waiting_on(&p),
            Some(0),
            "descriptors waiting for the lock"
        );
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
