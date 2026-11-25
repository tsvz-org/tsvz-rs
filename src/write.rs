//! Appending records (§13.3, §16.4, §16.7, §17.7, §18.2).

use std::borrow::Cow;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

use crate::escape::{self, is_marker_key};
use crate::name::{self, Format};
use crate::replay::DEFAULT_MEMBER_SIZE;
use crate::{Error, WarnFn, Warning, codec, lock, parts};

/// A record to append.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Record {
    /// A data key and its value cells (an empty `values` writes a tombstone).
    Data { key: String, values: Vec<String> },
    /// A lone data key: deletes it (§9).
    Tombstone { key: String },
    /// A marker line: the key as written, then its values; no values resets it (§12.1). The key
    /// must match the reserved pattern `#_name_#` (§12.2): a write of any other is refused.
    Marker { key: String, values: Vec<String> },
}

impl Record {
    /// What `set STORE KEY [VALUE ...]` and `delete STORE KEY` write (§20.2): a key matching the
    /// reserved pattern is a marker line; a lone data key is a tombstone.
    pub fn from_args(key: &str, values: &[String]) -> Record {
        if is_marker_key(key) {
            Record::Marker {
                key: key.into(),
                values: values.to_vec(),
            }
        } else if values.is_empty() {
            Record::Tombstone { key: key.into() }
        } else {
            Record::Data {
                key: key.into(),
                values: values.to_vec(),
            }
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
    if is_marker_key(key) {
        KeyKind::Marker
    } else {
        KeyKind::Data
    }
}

/// Tombstones that delete exactly these stored keys, for a store with this `delimiter`. While
/// the store's final stripping is on (`strip`), a reader strips a tombstone's key before
/// deleting it (§9.1), so a key whose encoding ends in a raw space or tab would delete another
/// key: such tombstones are written with stripping off around them, then on again (§9.6). A
/// trailing tab in a .tsvz key is written `<sep>`, which needs no guard.
pub fn tombstones<'k>(
    keys: impl IntoIterator<Item = &'k str>,
    strip: bool,
    delimiter: u8,
) -> Vec<Record> {
    let mut records: Vec<Record> = keys
        .into_iter()
        .map(|k| Record::Tombstone { key: k.to_string() })
        .collect();
    let exposed = |r: &Record| matches!(r, Record::Tombstone { key } if strippable(key, delimiter));
    if strip && records.iter().any(exposed) {
        let set = |value: &str| Record::Marker {
            key: "#_strip_trailing_whites_#".into(),
            values: vec![value.into()],
        };
        records.insert(0, set("false"));
        records.push(set("true"));
    }
    records
}

/// Whether a reader that strips (§7.7) would change `key` in a store with this `delimiter`:
/// its encoded form (§13) ends in a space or tab.
pub fn strippable(key: &str, delimiter: u8) -> bool {
    key.ends_with([' ', '\t']) && escape::encode(key, delimiter, true).ends_with([' ', '\t'])
}

/// What a reader that strips resolves `key` to (§7.7): the key without the trailing spaces and
/// tabs of its encoded form.
pub fn resolve(key: &str, delimiter: u8) -> Cow<'_, str> {
    if !strippable(key, delimiter) {
        return Cow::Borrowed(key);
    }
    let encoded = escape::encode(key, delimiter, true);
    let trimmed = encoded.trim_end_matches([' ', '\t']);
    Cow::Owned(escape::decode(trimmed, delimiter).into_owned())
}

/// The key a requested `key` names (§20.2.7) in a strict store whose final stripping is
/// `strip` and whose live keys `live` tells: while stripping is on, a key a reader would strip
/// names the live key equal to it if there is one, else what a reader resolves it to; while it
/// is off, a key names itself.
pub fn name_key<'k>(
    key: &'k str,
    delimiter: u8,
    strip: bool,
    live: impl Fn(&str) -> bool,
) -> Cow<'k, str> {
    if !strip || live(key) {
        Cow::Borrowed(key)
    } else {
        resolve(key, delimiter)
    }
}

/// Whether `key` is `#_snapshot_#`, which a writer appending records never writes in a strict
/// store: as the first line of a part it would make readers skip every part before it
/// (§19.10.2, §19.10.3). A loose file carries no marker semantics (§19.10.2): there it is a
/// comment, as TSVZ.py writes it.
fn is_snapshot_marker(key: &[u8]) -> bool {
    key.eq_ignore_ascii_case(b"#_snapshot_#")
}

fn snapshot_refused(key: &str) -> Error {
    Error::Refused(format!(
        "{key} is not written by a writer appending records: it marks a part that holds the store's whole state, written by a snapshot (§19.10.2); nothing was written"
    ))
}

/// Append `record` and its `\n` to `out`, encoded for `format` (§13.3). A loose file's fields are
/// encoded as TSVZ.py writes them ([`crate::loose::sanitize`]); a record whose line would not read
/// back as it (`loose::reads_back`) is refused.
pub fn encode_record(record: &Record, format: Format, out: &mut String) -> Result<(), Error> {
    let d = format.delimiter;
    let (key, values, marker) = match record {
        Record::Data { key, values } => (key, &values[..], false),
        Record::Tombstone { key } => (key, &[][..], false),
        Record::Marker { key, values } => (key, &values[..], true),
    };
    if marker && !is_marker_key(key) {
        // Else it would be a data row or a tombstone, or, with a line break, lines of its own.
        return Err(Error::Refused(format!(
            "{key:?} is not a marker key (#_name_#, §12.2); nothing was written"
        )));
    }
    if marker && format.strict && is_snapshot_marker(key.as_bytes()) {
        return Err(snapshot_refused(key));
    }
    if !format.strict {
        if !name::usable_delimiter(d) {
            return Err(Error::Refused(format!(
                "the delimiter {d:#04x} is not one ASCII character other than a line break; nothing was written"
            )));
        }
        // What is written must read back as the record, less the whitespace a reader trims at a
        // cell's end (`loose::reads_back`): the reader's own rule, so a key alone too, which in a
        // file of one column, or with no keyed line yet, is a row. A delete never asks for a
        // tombstone that would not ([`crate::maint::delete_loose`] names keys by that rule). A record
        // whose values are all blank is written, and a reader reads it as a deletion at more than one
        // column (the approved C2 rule). Each cell is written as the reader names it, without that
        // whitespace.
        let fields: Vec<&str> = std::iter::once(key.as_str())
            .chain(values.iter().map(String::as_str))
            .collect();
        let start = out.len();
        for (i, f) in fields.iter().enumerate() {
            if i > 0 {
                out.push(d as char);
            }
            out.push_str(crate::loose::sanitize(f, d).trim_end_matches(crate::loose::py_space));
        }
        if let Err(cells) = crate::loose::reads_back(&out[start..], d, &fields) {
            let read = match cells {
                None => "nothing".to_string(),
                Some(cells) => format!("{cells:?}"),
            };
            out.truncate(start);
            return Err(Error::Refused(format!(
                "{fields:?} would be read from a loose file as {read}: {}; nothing was written",
                crate::loose::cause(&fields, d)
            )));
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

/// The key of marker record `r`, lowercased, and the markers it leaves (§12), when its value is
/// valid; None for other records and invalid values, which change nothing (§12.5).
pub(crate) fn marker_effect(r: &Record) -> Option<(String, crate::Markers)> {
    let Record::Marker { key, values } = r else {
        return None;
    };
    let key = key.to_ascii_lowercase();
    let mut m = crate::Markers::default();
    let value = values.first().map_or("", String::as_str);
    crate::replay::apply_marker(&mut m, &key, value).then_some((key, m))
}

/// `records` encoded for `format`, one after another.
pub(crate) fn encode(records: &[Record], format: Format) -> Result<String, Error> {
    let mut out = String::new();
    for r in records {
        encode_record(r, format, &mut out)?;
    }
    Ok(out)
}

/// Where [`Writer::append_at`] appended, the part's stamp just before and after, and
/// the file it appended through (still open: it syncs that file even once renamed).
#[doc(hidden)]
pub struct Appended {
    pub part: PathBuf,
    pub before: parts::Stamp,
    pub after: parts::Stamp,
    pub file: File,
}

/// Whether `path` still names the file `file` is open on.
pub(crate) fn names(path: &Path, file: &File) -> bool {
    use std::os::unix::fs::MetadataExt;
    let (Ok(open), Ok(named)) = (file.metadata(), std::fs::metadata(path)) else {
        return false;
    };
    named.dev() == open.dev() && named.ino() == open.ino()
}

fn stamp(file: &File) -> io::Result<parts::Stamp> {
    file.metadata().map(|m| parts::Stamp::of(&m))
}

/// A part as a read of it found it, for a writer that appends only while the part has only grown
/// since: the file (its inode), how long it was, and its last bytes, as `Store::refresh` tells an
/// append from a rewrite in place ([`crate::store::CHECK_BYTES`]). A clear, which leaves the file
/// shorter, and a rewrite, which changes those bytes or the file, show; an append by another
/// writer does not.
#[derive(Debug)]
pub(crate) struct Seen {
    ino: u64,
    size: u64,
    tail: Vec<u8>,
}

impl Seen {
    /// How `path` is now; None when it was truncated between its length and its last bytes being
    /// read (the caller reads it again).
    #[cfg(feature = "cli")]
    pub(crate) fn of(path: &Path) -> io::Result<Option<Seen>> {
        use std::os::unix::fs::MetadataExt;
        let file = File::open(path)?;
        let meta = file.metadata()?;
        let want = meta.len().min(crate::store::CHECK_BYTES);
        Ok(
            bytes_at(&file, meta.len() - want, want as usize)?.map(|tail| Seen {
                ino: meta.ino(),
                size: meta.len(),
                tail,
            }),
        )
    }

    /// Whether `file` is the file seen, no shorter, with the bytes it ended in.
    fn grew(&self, file: &File) -> io::Result<bool> {
        use std::os::unix::fs::MetadataExt;
        let meta = file.metadata()?;
        if meta.ino() != self.ino || meta.len() < self.size {
            return Ok(false);
        }
        let at = self.size - self.tail.len() as u64;
        Ok(bytes_at(file, at, self.tail.len())?.is_some_and(|tail| tail == self.tail))
    }
}

/// The `len` bytes of `file` at `at`; None when it ends before them: a file that another process
/// truncated since its length was taken is a file that changed, not an I/O error.
fn bytes_at(file: &File, at: u64, len: usize) -> io::Result<Option<Vec<u8>>> {
    let mut bytes = vec![0u8; len];
    match file.read_exact_at(&mut bytes, at) {
        Ok(()) => Ok(Some(bytes)),
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Ok(None),
        Err(e) => Err(e),
    }
}

/// Appends batches of records to a store's active part (§17.7).
///
/// Each append holds the write lock shared with TSVZ.py (an open file description lock on
/// Linux, `flock` on macOS). Where the kernel or file system refuses it, the lock is a classic
/// POSIX lock, which closing any other descriptor of the part in this process releases
/// (docs/limitations.md).
pub struct Writer {
    store: PathBuf,
    format: Format,
    member_size: usize,
    fsync: bool,
    warn: Option<WarnFn>,
    dirs: parts::DirTrust,
}

impl Writer {
    pub fn open(store: impl AsRef<Path>) -> Result<Writer, Error> {
        let store = store.as_ref().to_path_buf();
        let format = name::format_of(&store).ok_or_else(|| Error::BadPath(store.clone()))?;
        Ok(Writer {
            store,
            format,
            member_size: DEFAULT_MEMBER_SIZE as usize,
            fsync: false,
            warn: None,
            dirs: parts::DirTrust::default(),
        })
    }

    /// What an earlier writer of the store learnt of its directory ([`Writer::dirs_known`]),
    /// so that its file system is not asked again at every batch.
    pub(crate) fn dirs(mut self, known: parts::DirTrust) -> Writer {
        self.dirs = known;
        self
    }

    pub(crate) fn dirs_known(&self) -> parts::DirTrust {
        self.dirs
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

    /// Append `records` as one batch (§18.2), creating a missing store (§20.2.2). A compressed
    /// batch is written in members of the writer's member size, also when it sets
    /// `#_member_size_#` itself (§16.7 is a SHOULD): the writer's size does not follow the
    /// marker (the handler passes it on to its next batch with [`Writer::append_at`]).
    pub fn append(&mut self, records: &[Record]) -> Result<(), Error> {
        let payload = encode(records, self.format)?;
        self.append_at(payload.as_bytes(), self.member_size as u64)
            .map(|_| ())
    }

    /// Append encoded records (each ending in `\n`) as one batch. A `#_snapshot_#` line in a
    /// strict store is refused, as [`Writer::append`] refuses it.
    pub fn append_payload(&mut self, payload: &[u8]) -> Result<(), Error> {
        if self.format.strict {
            let d = self.format.delimiter;
            for line in payload.split(|&b| b == b'\n') {
                let line = line.strip_prefix(b"\xef\xbb\xbf").unwrap_or(line);
                let key = line.split(|&b| b == d).next().unwrap_or(line);
                let key = key.strip_suffix(b"\r").unwrap_or(key);
                if is_snapshot_marker(key) {
                    return Err(snapshot_refused(&String::from_utf8_lossy(key)));
                }
            }
        }
        self.append_at(payload, self.member_size as u64).map(|_| ())
    }

    /// Append encoded records as one batch, in members of `member_size` bytes on a compressed
    /// part (§16.7). Says where the batch went and how the part looked, under the lock, just
    /// before and after it: what the handler needs to tell its own appends from other writers'.
    #[doc(hidden)]
    pub fn append_at(&mut self, payload: &[u8], member_size: u64) -> Result<Appended, Error> {
        let appended = self.append_unless_changed(payload, member_size, None)?;
        Ok(appended.expect("appended: nothing was expected of the part"))
    }

    /// Append `records` only while the part is the file `seen` took, which has only grown since:
    /// false, with nothing written, when it was cleared, rewritten or replaced (a loose delete
    /// decides from the head of the file it read, which other writers' appends leave as it was).
    #[cfg(feature = "cli")]
    pub(crate) fn append_if_grown(
        &mut self,
        records: &[Record],
        seen: &Seen,
    ) -> Result<bool, Error> {
        let payload = encode(records, self.format)?;
        let appended =
            self.append_unless_changed(payload.as_bytes(), self.member_size as u64, Some(seen))?;
        Ok(appended.is_some())
    }

    /// [`Writer::append_at`], which writes nothing and answers None when the part under the lock is
    /// not the file `expect` saw, grown.
    fn append_unless_changed(
        &mut self,
        payload: &[u8],
        member_size: u64,
        expect: Option<&Seen>,
    ) -> Result<Option<Appended>, Error> {
        // The part is locked as it was opened; when by then it is no longer the store's active
        // part (a part was started meanwhile, §17.7), or its name is another file's (it was
        // rotated away), the active part is looked up again. Only a store with no part yet is
        // created: a part's name is never made anew once it has gone.
        // The error of a part found missing by every try so far, the same part each time.
        let mut missing: Option<Error> = None;
        for at in 0..3 {
            // Sampled before the listing, so that a name made meanwhile shows as a change.
            let dir = self.dirs.stamp(&self.store);
            let (listed, linked) = parts::list_linked(&self.store)?;
            let create = listed.is_empty();
            let part = match listed.into_iter().last() {
                Some(part) => part,
                None => parts::unnumbered(&self.store)?,
            };
            let path = part.path.as_path();
            let io = |e: io::Error| Error::io(path, e);
            let bytes: Cow<[u8]> = match part.codec {
                Some(codec) => {
                    let size = usize::try_from(member_size).unwrap_or(usize::MAX).max(1);
                    codec::encode_members(codec, payload, size)
                        .ok_or_else(|| Error::UnsupportedCodec {
                            path: path.to_path_buf(),
                            codec: codec.name(),
                        })?
                        .into()
                }
                None => payload.into(),
            };
            let mut options = OpenOptions::new();
            options
                .append(true)
                .create(create)
                .read(part.codec.is_none() || expect.is_some());
            let file = match options.open(path) {
                Ok(file) => file,
                Err(e) if e.kind() == io::ErrorKind::NotFound && !create => {
                    let e = io(e);
                    let same = missing
                        .as_ref()
                        .is_some_and(|m| m.to_string() == e.to_string());
                    missing = (at == 0 || same).then_some(e); // reported should it stay so
                    continue;
                }
                Err(e) => return Err(io(e)),
            };
            let guard = lock::lock(&file).map_err(io)?;
            // Still the active part: the directory kept its timestamp, and shows no name made,
            // renamed or removed since the listing (parts::DirTrust), in which part 0 was no
            // symbolic link (its target is elsewhere); else listed again.
            let unchanged = !linked && dir.is_some() && parts::DirStamp::of(&self.store) == dir;
            let active = unchanged || parts::active(&self.store)?.path == part.path;
            if !active || !names(path, &file) {
                missing = None;
                continue;
            }
            // The warning of a cut tail is sent once the lock is let go, whatever else happened:
            // a callback that opens this part would otherwise release a classic lock
            // (docs/limitations.md).
            let mut cut = None;
            let locked = (|| {
                let before = stamp(&file).map_err(io)?;
                if let Some(seen) = expect {
                    if !seen.grew(&file).map_err(io)? {
                        return Ok(None);
                    }
                }
                let mut bytes = Cow::Borrowed(&bytes[..]);
                let start = match part.codec {
                    Some(_) => before.size,
                    None if self.format.strict => {
                        cut = self.cut_torn_tail(&file, path).map_err(io)?;
                        file.metadata().map_err(io)?.len()
                    }
                    None => {
                        // TSVZ.py ends a loose file's unterminated last line, which it reads as a
                        // row, before it appends.
                        if !ends_a_line(&file, before.size).map_err(io)? {
                            bytes = Cow::Owned([&b"\n"[..], &bytes].concat());
                        }
                        before.size
                    }
                };
                self.write_batch(&file, path, &bytes, start)?;
                Ok::<_, Error>(Some((before, stamp(&file).map_err(io)?)))
            })();
            drop(guard);
            if let (Some(w), Some(warning)) = (&self.warn, &cut) {
                w(warning);
            }
            let Some((before, after)) = locked? else {
                return Ok(None);
            };
            return Ok(Some(Appended {
                part: part.path.clone(),
                before,
                after,
                file,
            }));
        }
        if let Some(e) = missing {
            return Err(e);
        }
        Err(Error::Refused(format!(
            "{}: the active part kept changing while its lock was waited for; nothing was written",
            self.store.display()
        )))
    }

    /// Write `bytes` at the end of `file` (which ends at `start`), synced when asked, all or
    /// nothing: a batch that fails part-way (a full disk, a size limit, a failed sync) is cut
    /// back to `start` before the error is returned, so that a write answered as failed is not
    /// partly made. When even that fails, the error says the part may hold part of the batch.
    fn write_batch(&self, file: &File, path: &Path, bytes: &[u8], start: u64) -> Result<(), Error> {
        let written = (&*file)
            .write_all(bytes)
            .and_then(|()| if self.fsync { file.sync_data() } else { Ok(()) });
        let Err(e) = written else {
            return Ok(());
        };
        let undone = file
            .set_len(start)
            .and_then(|()| if self.fsync { file.sync_data() } else { Ok(()) });
        Err(Error::io(
            path,
            match undone {
                Ok(()) => e,
                Err(cut) => io::Error::new(
                    e.kind(),
                    format!(
                        "{e}; the part may hold part of the batch, which could not be cut back ({cut})"
                    ),
                ),
            },
        ))
    }

    /// An uncommitted tail would join the first appended record, so it is cut first, as TSVZ.py
    /// does in a store (§4.3). Says what was cut, as the warning to send.
    fn cut_torn_tail(&self, file: &File, path: &Path) -> io::Result<Option<Warning>> {
        let size = file.metadata()?.len();
        if size == 0 {
            return Ok(None);
        }
        let mut last = [0u8; 1];
        file.read_exact_at(&mut last, size - 1)?;
        if last[0] == b'\n' {
            return Ok(None);
        }
        let keep = last_newline_end(file, size)?;
        file.set_len(keep)?;
        Ok(Some(Warning {
            part: path.to_path_buf(),
            offset: Some(keep),
            message: format!(
                "cut {} uncommitted bytes after the last newline before appending",
                size - keep
            ),
        }))
    }
}

/// Whether `file`, `size` bytes long, is empty or ends with a newline.
fn ends_a_line(file: &File, size: u64) -> io::Result<bool> {
    if size == 0 {
        return Ok(true);
    }
    let mut last = [0u8; 1];
    file.read_exact_at(&mut last, size - 1)?;
    Ok(last[0] == b'\n')
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Options, Store};
    use std::fs;

    const TSVZ: Format = Format {
        delimiter: b'\t',
        strict: true,
    };

    fn enc(r: Record, f: Format) -> Result<String, Error> {
        let mut s = String::new();
        encode_record(&r, f, &mut s).map(|()| s)
    }

    #[test]
    fn a_loose_writer_refuses_a_delimiter_it_could_not_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.tsv");
        for d in [0xA7, b'\n'] {
            let row = Record::Data {
                key: "a".into(),
                values: vec!["1".into()],
            };
            let e = Writer::open(&p)
                .unwrap()
                .delimiter(Some(d))
                .append(&[row])
                .unwrap_err();
            assert!(e.to_string().contains("delimiter"), "{d:#x}: {e}");
        }
        assert!(
            !p.exists() || std::fs::read(&p).unwrap().is_empty(),
            "nothing written"
        );
    }

    #[test]
    fn a_writer_never_writes_the_snapshot_marker() {
        // As the first line of a new part, it would make readers skip the parts before it.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.tsvz");
        std::fs::write(&p, "a\t1\n").unwrap();
        // With a value or without one (§19.10.2).
        for (key, values) in [
            ("#_snapshot_#", vec![]),
            ("#_SNAPSHOT_#", vec![]),
            ("#_snapshot_#", vec!["x".to_string()]),
        ] {
            let marker = Record::Marker {
                key: key.into(),
                values,
            };
            let e = Writer::open(&p).unwrap().append(&[marker]).unwrap_err();
            assert!(e.to_string().contains("§19.10.2"), "{key:?}: {e}");
        }
        assert_eq!(
            std::fs::read_to_string(&p).unwrap(),
            "a\t1\n",
            "nothing written"
        );
    }

    #[test]
    fn a_payload_never_writes_the_snapshot_marker_into_a_strict_store() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.tsvz");
        std::fs::write(&p, "a\t1\n").unwrap();
        for payload in [
            &b"b\t2\n#_SNAPSHOT_#\r\n"[..],
            b"#_snapshot_#\t\n",
            b"#_snapshot_#\tx\n",
            b"\xef\xbb\xbf#_snapshot_#\n",
        ] {
            let e = Writer::open(&p)
                .unwrap()
                .append_payload(payload)
                .unwrap_err();
            assert!(e.to_string().contains("§19.10.2"), "{payload:?}: {e}");
        }
        assert_eq!(
            std::fs::read_to_string(&p).unwrap(),
            "a\t1\n",
            "nothing written"
        );
        Writer::open(&p)
            .unwrap()
            .append_payload(b"<#>_snapshot_#\tdata\n")
            .unwrap();
        // The delimiter of the store's variant ends the key: a .csvz store's is a comma.
        let csv = dir.path().join("c.csvz");
        std::fs::write(&csv, "a,1\n").unwrap();
        let e = Writer::open(&csv)
            .unwrap()
            .append_payload(b"#_snapshot_#,\n")
            .unwrap_err();
        assert!(e.to_string().contains("§19.10.2"), "{e}");
        Writer::open(&csv)
            .unwrap()
            .append_payload(b"b,2\n")
            .unwrap();
        assert_eq!(std::fs::read_to_string(&csv).unwrap(), "a,1\nb,2\n");
        // A loose file carries no marker semantics (§19.10.2): there the line is a comment, which a
        // payload may hold; a record is refused, since no reader would read it.
        let loose = dir.path().join("l.tsv");
        let marker = Record::Marker {
            key: "#_snapshot_#".into(),
            values: Vec::new(),
        };
        assert!(Writer::open(&loose).unwrap().append(&[marker]).is_err());
        Writer::open(&loose)
            .unwrap()
            .append_payload(b"#_snapshot_#\n")
            .unwrap();
        assert_eq!(std::fs::read_to_string(&loose).unwrap(), "#_snapshot_#\n");
    }

    #[test]
    fn a_marker_record_must_have_a_marker_key() {
        // Else it writes a data row or a tombstone, or, with a line break, lines of its own.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.tsvz");
        std::fs::write(&p, "a\t1\n").unwrap();
        for key in ["a", "defaults", "#_x_#\nevil", "#_x_", "#__#"] {
            let marker = Record::Marker {
                key: key.into(),
                values: vec!["v".into()],
            };
            let e = Writer::open(&p).unwrap().append(&[marker]).unwrap_err();
            assert!(e.to_string().contains("not a marker key"), "{key:?}: {e}");
        }
        assert_eq!(
            std::fs::read_to_string(&p).unwrap(),
            "a\t1\n",
            "nothing written"
        );
        let marker = Record::Marker {
            key: "#_X-y_#".into(),
            values: vec!["v".into()],
        };
        Writer::open(&p).unwrap().append(&[marker]).unwrap();
    }

    #[test]
    fn records_encode_per_variant() {
        let d = |k: &str, v: &[&str]| Record::Data {
            key: k.into(),
            values: v.iter().map(|s| s.to_string()).collect(),
        };
        assert_eq!(
            enc(d("#k", &["a\tb", "<", "#v"]), TSVZ).unwrap(),
            "<#>k\ta<sep>b\t<lt>\t#v\n"
        );
        assert_eq!(
            enc(d("#_defaults_#", &["x"]), TSVZ).unwrap(),
            "<#>_defaults_#\tx\n"
        );
        assert_eq!(
            enc(
                Record::Tombstone {
                    key: "#_x_#".into()
                },
                TSVZ
            )
            .unwrap(),
            "<#>_x_#\n"
        );
        assert_eq!(
            enc(
                Record::Marker {
                    key: "#_defaults_#".into(),
                    values: vec!["a,b".into()]
                },
                Format {
                    delimiter: b',',
                    strict: true
                }
            )
            .unwrap(),
            "#_defaults_#,a<sep>b\n"
        );
        assert_eq!(
            enc(
                d("k", &["a\0b"]),
                Format {
                    delimiter: 0,
                    strict: true
                }
            )
            .unwrap(),
            "k\0a<sep>b\n"
        );
        assert_eq!(enc(d("k", &["", ""]), TSVZ).unwrap(), "k\t\t\n");
        let loose = Format {
            delimiter: b',',
            strict: false,
        };
        // A loose file's cells as TSVZ.py writes them (`_sanitize`), without the whitespace a
        // reader trims at a cell's end: what reads back as another token is refused.
        assert_eq!(enc(d("k", &["a<b", "#c"]), loose).unwrap(), "k,a<b,#c\n");
        assert_eq!(enc(d("k", &["a,b"]), loose).unwrap(), "k,a<sep>b\n");
        assert_eq!(
            enc(d("k\nl", &["a\nb<sep>", "x<LF>\r"]), loose).unwrap(),
            "k<LF>l,a<LF>b</sep/>,x</LF/>\n"
        );
        assert!(matches!(
            enc(d("k", &["a</sep/>b"]), loose),
            Err(Error::Refused(_))
        ));
    }

    /// A loose file takes no field whose encoding holds the delimiter (`-d e`: `<sep>` has an
    /// `e`), nor a key it could not read back.
    #[test]
    fn a_loose_record_that_could_not_be_read_back_is_refused() {
        let loose = |d: u8| Format {
            delimiter: d,
            strict: false,
        };
        let data = |k: &str, v: &[&str]| Record::Data {
            key: k.into(),
            values: v.iter().map(|v| v.to_string()).collect(),
        };
        let refused = |r: Record, f: Format| matches!(enc(r, f), Err(Error::Refused(_)));
        // The delimiter is a character of the tokens: `h` + the delimiter, or the newline `<LF>`
        // and the token `<sep>`, which a `/` or an `L` delimiter meets in their encodings.
        for (d, field) in [
            (b'<', "h<"),
            (b'>', "h>"),
            (b's', "hs"),
            (b'e', "he"),
            (b'p', "hp"),
            (b'L', "h\nh"),
            (b'F', "h\nh"),
            (b'/', "h<sep>"),
        ] {
            assert!(refused(data("k", &[field]), loose(d)), "{d} {field:?}");
            assert!(refused(data(field, &["1"]), loose(d)), "{d} {field:?}");
        }
        // Any other delimiter is encoded away (`<sep>`), and a field without it is no concern.
        assert_eq!(
            enc(data("k", &["h,llo"]), loose(b',')).unwrap(),
            "k,h<sep>llo\n"
        );
        assert_eq!(enc(data("k", &["hllo"]), loose(b'e')).unwrap(), "kehllo\n");
        for key in ["#tag", " ", " \r", "", "\0a"] {
            assert!(refused(data(key, &["1"]), loose(b'\t')), "{key:?}");
            assert!(refused(data(key, &[]), loose(b'\t')), "{key:?}");
        }
        // `#_defaults_#` is a marker a loose file reads; any other marker is a comment there.
        let marker = |key: &str| Record::Marker {
            key: key.into(),
            values: vec!["a".into()],
        };
        assert_eq!(
            enc(marker("#_defaults_#"), loose(b'\t')).unwrap(),
            "#_defaults_#\ta\n"
        );
        assert!(refused(marker("#_rotate_#"), loose(b'\t')));
        // A key alone is refused as a key with a value is: in a file of one column, or with no
        // keyed line yet, it is a row. (`maint::delete_loose` asks for no tombstone of such a key.)
        let tomb = |key: &str| Record::Tombstone { key: key.into() };
        for key in ["#tag", " ", "\0a"] {
            assert!(refused(tomb(key), loose(b'\t')), "{key:?}");
        }
        assert!(refused(tomb("hello"), loose(b'e')));
        assert_eq!(enc(tomb("k"), loose(b'\t')).unwrap(), "k\n");
    }

    #[test]
    fn records_from_arguments() {
        assert_eq!(
            Record::from_args("#_defaults_#", &[]),
            Record::Marker {
                key: "#_defaults_#".into(),
                values: vec![]
            }
        );
        assert_eq!(
            Record::from_args("k", &[]),
            Record::Tombstone { key: "k".into() }
        );
        assert_eq!(
            Record::from_args("k", &["".into()]),
            Record::Data {
                key: "k".into(),
                values: vec!["".into()]
            }
        );
        assert_eq!(classify_key("#_x_#"), KeyKind::Marker);
        assert_eq!(classify_key("#x"), KeyKind::Data);
    }

    #[test]
    fn tombstones_guard_keys_that_stripping_would_change() {
        assert_eq!(
            tombstones(["a", "b"], true, b'\t'),
            vec![
                Record::Tombstone { key: "a".into() },
                Record::Tombstone { key: "b".into() }
            ]
        );
        let guarded = tombstones(["a", "k "], true, b'\t');
        assert_eq!(guarded.len(), 4);
        assert_eq!(
            guarded[0],
            Record::Marker {
                key: "#_strip_trailing_whites_#".into(),
                values: vec!["false".into()]
            }
        );
        assert_eq!(
            guarded[3],
            Record::Marker {
                key: "#_strip_trailing_whites_#".into(),
                values: vec!["true".into()]
            }
        );
        assert_eq!(
            tombstones(["k "], false, b'\t'),
            vec![Record::Tombstone { key: "k ".into() }]
        );
    }

    #[test]
    fn only_a_key_whose_encoding_ends_in_white_space_is_guarded() {
        // §9.6: in .tsvz a trailing tab is written `<sep>`, which stripping leaves alone.
        assert_eq!(
            tombstones(["k\t"], true, b'\t'),
            vec![Record::Tombstone { key: "k\t".into() }]
        );
        assert_eq!(tombstones(["k "], true, b'\t').len(), 3);
        for d in [b',', b'|', 0] {
            assert_eq!(tombstones(["k\t"], true, d).len(), 3, "{d}");
        }
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
        let mut w =
            Writer::open(&p)
                .unwrap()
                .warnings(Some(std::sync::Arc::new(move |w: &Warning| {
                    sink.lock().unwrap().push(w.message.clone())
                })));
        w.append(&[Record::from_args("b", &["2".into()])]).unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"a\t1\nb\t2\n");
        assert_eq!(warned.lock().unwrap().len(), 1);
        Writer::open(dir.path().join("e.tsvz"))
            .unwrap()
            .append(&[])
            .unwrap();
        assert_eq!(fs::read(dir.path().join("e.tsvz")).unwrap(), b"");
    }

    #[test]
    fn the_warning_of_a_cut_tail_is_sent_after_the_lock_is_let_go() {
        // The callback asks, from another process, whether the part is locked: it is not, so the
        // lock is let go before the warning is sent. The lock taken here is the open file
        // description lock (`flock` on macOS); the classic fallback has tests of its own in
        // lock.rs.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.tsvz");
        fs::write(&p, b"a\t1\ntorn").unwrap();
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let (sink, part) = (seen.clone(), p.clone());
        let mut w =
            Writer::open(&p)
                .unwrap()
                .warnings(Some(std::sync::Arc::new(move |_: &Warning| {
                    sink.lock().unwrap().push(lock::locked_elsewhere(&part))
                })));
        w.append(&[Record::from_args("b", &["2".into()])]).unwrap();
        assert_eq!(*seen.lock().unwrap(), [false], "locked while it was sent");
        assert_eq!(fs::read(&p).unwrap(), b"a\t1\nb\t2\n");
    }

    #[test]
    fn appends_go_to_the_last_part() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("s.tsvz"), "a\t1\n").unwrap();
        fs::write(dir.path().join("s.tsvz.f"), "b\t2\n").unwrap();
        fs::write(dir.path().join("s.tsvz.10"), "c\t3\n").unwrap();
        Writer::open(dir.path().join("s.tsvz"))
            .unwrap()
            .append(&[Record::Tombstone { key: "a".into() }])
            .unwrap();
        assert_eq!(
            fs::read(dir.path().join("s.tsvz.10")).unwrap(),
            b"c\t3\na\n"
        );
        assert_eq!(fs::read(dir.path().join("s.tsvz")).unwrap(), b"a\t1\n");
    }

    #[test]
    fn compressed_appends_add_members() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.tsvz.gz");
        let mut w = Writer::open(&p).unwrap().member_size(16);
        w.append(&[
            Record::from_args("a", &["1".into()]),
            Record::from_args("bbbbbbbbbbbbbbbbbbbb", &["2".into()]),
        ])
        .unwrap();
        let first = fs::read(&p).unwrap();
        w.append(&[Record::from_args("c", &["3".into()])]).unwrap();
        let all = fs::read(&p).unwrap();
        assert!(all.starts_with(&first));
        assert_eq!(crate::codec::items(crate::name::Codec::Gzip, &all).len(), 3);
        assert_eq!(
            read(&p).into_iter().map(|r| r.0).collect::<Vec<_>>(),
            vec!["a", "bbbbbbbbbbbbbbbbbbbb", "c"]
        );
    }

    #[test]
    fn an_append_that_sets_a_member_size_is_written_at_the_size_it_started_with() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.tsvz.gz");
        let row = |k: &str| Record::from_args(k, &["v".repeat(20)]);
        let marker = Record::Marker {
            key: "#_MEMBER_SIZE_#".into(),
            values: vec!["30".into()],
        };
        let mut w = Writer::open(&p).unwrap();
        w.append(&[row("a"), row("b"), marker, row("c"), row("d")])
            .unwrap();
        let members: Vec<usize> =
            crate::codec::items(crate::name::Codec::Gzip, &fs::read(&p).unwrap())
                .iter()
                .map(|i| match i {
                    crate::codec::Item::Data(out) => out.iter().filter(|&&b| b == b'\n').count(),
                    other => panic!("{other:?}"),
                })
                .collect();
        // One batch, one member size (§16.7 is a SHOULD, as TSVZ.py takes it): all five lines in
        // one 1 MiB member, where 30 bytes would give c and d one each.
        assert_eq!(members, [5]);
    }

    /// Wait until a thread waits at the lock of `path`.
    fn until_waiting(path: &Path) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while lock::waiters_on(path) == 0 {
            assert!(std::time::Instant::now() < deadline, "no writer came");
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    #[test]
    fn a_write_that_waited_for_the_lock_goes_to_the_part_the_store_names_then() {
        // While a writer waits for the lock on part 0, a new part is started and part 0
        // rotated away (renamed `.0.rotated`): the write must not land in the rotated file.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.tsvz");
        fs::write(&p, "a\t1\n").unwrap();
        let held = OpenOptions::new().append(true).open(&p).unwrap();
        let guard = lock::lock(&held).unwrap();
        let store = p.clone();
        let writer = std::thread::spawn(move || {
            Writer::open(&store)
                .unwrap()
                .append(&[Record::from_args("b", &["2".into()])])
        });
        until_waiting(&p);
        fs::write(dir.path().join("s.tsvz.1"), "c\t3\n").unwrap();
        let rotated = dir.path().join("s.tsvz.0.rotated");
        fs::rename(&p, &rotated).unwrap();
        drop(guard);
        writer.join().unwrap().unwrap();
        assert_eq!(
            fs::read_to_string(&rotated).unwrap(),
            "a\t1\n",
            "into the rotated part"
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("s.tsvz.1")).unwrap(),
            "c\t3\nb\t2\n"
        );
    }

    /// Run this thread's clock a minute ahead, so that the directory of `store` looks last
    /// changed a minute ago; None (the test is skipped) where its timestamp is not trusted.
    fn kept_and_aged(store: &Path) -> Option<parts::RunAhead> {
        parts::fine_times(store).then(|| parts::RunAhead::by(60))
    }

    use parts::listings;

    #[test]
    fn a_write_lists_the_parts_once_where_the_kept_directory_shows_no_change() {
        // The part chosen from the listing is still the active one after the lock: the
        // directory's timestamp, kept and old, shows no name made, renamed or removed since.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.tsvz");
        fs::write(&p, "a\t1\n").unwrap();
        let one = dir.path().join("s.tsvz.1");
        fs::write(&one, "b\t2\n").unwrap();
        let Some(_ahead) = kept_and_aged(&p) else {
            return;
        };
        let before = listings();
        Writer::open(&p)
            .unwrap()
            .append(&[Record::from_args("c", &["3".into()])])
            .unwrap();
        assert_eq!(listings() - before, 1);
        assert_eq!(fs::read_to_string(&one).unwrap(), "b\t2\nc\t3\n");
    }

    #[test]
    fn a_write_lists_the_parts_again_after_the_lock_where_the_directory_is_not_kept() {
        // On mergerfs, NFS, SMB, … an old, unchanged timestamp proves nothing.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.tsvz");
        fs::write(&p, "a\t1\n").unwrap();
        let one = dir.path().join("s.tsvz.1");
        fs::write(&one, "b\t2\n").unwrap();
        let Some(_ahead) = kept_and_aged(&p) else {
            return;
        };
        let before = listings();
        Writer::open(&p)
            .unwrap()
            .dirs(parts::DirTrust::not_kept())
            .append(&[Record::from_args("c", &["3".into()])])
            .unwrap();
        assert_eq!(listings() - before, 2);
    }

    #[test]
    fn a_write_lists_the_parts_again_after_the_lock_where_part_0_is_a_link() {
        // Part 0 is listed by its target, which the directory's timestamp does not follow.
        let root = tempfile::tempdir().unwrap();
        let (d, o) = (root.path().join("d"), root.path().join("o"));
        fs::create_dir(&d).unwrap();
        fs::create_dir(&o).unwrap();
        let real = o.join("real.tsvz");
        fs::write(&real, "a\t1\n").unwrap();
        let p = d.join("s.tsvz");
        std::os::unix::fs::symlink(&real, &p).unwrap();
        let Some(_ahead) = kept_and_aged(&d.join("other.tsvz")) else {
            return;
        };
        let before = listings();
        Writer::open(&p)
            .unwrap()
            .append(&[Record::from_args("b", &["2".into()])])
            .unwrap();
        assert_eq!(listings() - before, 2);
        assert_eq!(fs::read_to_string(&real).unwrap(), "a\t1\nb\t2\n");
    }

    #[test]
    fn a_loose_store_s_writer_does_not_look_at_its_directory() {
        // A loose file is one file (§20.2.4): its directory is never listed, so its timestamp
        // would tell nothing.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.tsv");
        fs::write(&p, "a\t1\n").unwrap();
        let looks = || parts::stats() + parts::asked();
        let before = looks();
        Writer::open(&p)
            .unwrap()
            .append(&[Record::from_args("b", &["2".into()])])
            .unwrap();
        assert_eq!(looks() - before, 0);
        assert_eq!(fs::read_to_string(&p).unwrap(), "a\t1\nb\t2\n");
    }

    #[test]
    fn a_write_lists_the_parts_again_after_the_lock_where_the_directory_changed_just_now() {
        // A timestamp this recent may not show a name made within the same tick.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.tsvz");
        fs::write(&p, "a\t1\n").unwrap();
        let before = listings();
        Writer::open(&p)
            .unwrap()
            .append(&[Record::from_args("b", &["2".into()])])
            .unwrap();
        assert_eq!(listings() - before, 2);
    }

    #[test]
    fn a_part_started_while_the_writer_waits_takes_the_write_in_a_kept_directory_too() {
        // The directory was listed while its timestamp was old: the new part moves it, so the
        // parts are listed again after the lock and the write goes to the new part (§17.7).
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.tsvz");
        fs::write(&p, "a\t1\n").unwrap();
        let one = dir.path().join("s.tsvz.1");
        fs::write(&one, "b\t2\n").unwrap();
        let Some(_ahead) = kept_and_aged(&p) else {
            return;
        };
        let held = OpenOptions::new().append(true).open(&one).unwrap();
        let guard = lock::lock(&held).unwrap();
        let store = p.clone();
        let writer = std::thread::spawn(move || {
            let _ahead = parts::RunAhead::by(60);
            Writer::open(&store)
                .unwrap()
                .append(&[Record::from_args("c", &["3".into()])])
        });
        until_waiting(&one);
        fs::write(dir.path().join("s.tsvz.2"), "").unwrap();
        drop(guard);
        writer.join().unwrap().unwrap();
        assert_eq!(fs::read_to_string(&one).unwrap(), "b\t2\n");
        assert_eq!(
            fs::read_to_string(dir.path().join("s.tsvz.2")).unwrap(),
            "c\t3\n"
        );
    }

    #[test]
    fn a_write_that_waited_for_the_lock_goes_to_the_part_started_meanwhile() {
        // Another writer starts a new part while this one waits for the lock on the active
        // part: the write goes to the new last part, not to the one now before it (§17.7).
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.tsvz");
        fs::write(&p, "a\t1\n").unwrap();
        let one = dir.path().join("s.tsvz.1");
        fs::write(&one, "b\t2\n").unwrap();
        let held = OpenOptions::new().append(true).open(&one).unwrap();
        let guard = lock::lock(&held).unwrap();
        let store = p.clone();
        let writer = std::thread::spawn(move || {
            Writer::open(&store)
                .unwrap()
                .append(&[Record::from_args("c", &["3".into()])])
        });
        until_waiting(&one);
        fs::write(dir.path().join("s.tsvz.2"), "").unwrap();
        drop(guard);
        writer.join().unwrap().unwrap();
        assert_eq!(fs::read_to_string(&one).unwrap(), "b\t2\n");
        assert_eq!(
            fs::read_to_string(dir.path().join("s.tsvz.2")).unwrap(),
            "c\t3\n"
        );
    }

    #[test]
    fn a_last_part_that_stays_missing_is_reported_by_name() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.tsvz");
        fs::write(&p, "a\t1\n").unwrap();
        std::os::unix::fs::symlink(dir.path().join("nowhere"), dir.path().join("s.tsvz.1"))
            .unwrap();
        let err = Writer::open(&p)
            .unwrap()
            .append(&[Record::from_args("b", &["2".into()])])
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("s.tsvz.1") && !err.contains("kept changing"),
            "{err}"
        );
        assert!(
            !dir.path().join("nowhere").exists(),
            "the link's target is not made"
        );
    }

    #[cfg(feature = "cli")]
    #[test]
    fn an_append_made_on_what_was_read_is_made_to_a_file_that_only_grew() {
        // A loose delete appends its key alone only to the file it read, or that file with more
        // appended: cleared meanwhile, with no keyed line left, the key alone would be a row.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.tsv");
        fs::write(&p, "a\t1\nb\t2\n").unwrap();
        let seen = Seen::of(&p).unwrap().unwrap();
        let gone = [Record::from_args("a", &[])];
        let mut w = Writer::open(&p).unwrap();
        // another writer appended meanwhile: the head is as it was
        fs::OpenOptions::new()
            .append(true)
            .open(&p)
            .unwrap()
            .write_all(b"c\t3\n")
            .unwrap();
        assert!(w.append_if_grown(&gone, &seen).unwrap());
        assert_eq!(fs::read(&p).unwrap(), b"a\t1\nb\t2\nc\t3\na\n");
        // cleared, then rows of other columns written until it is as long again: not that file
        fs::write(&p, "").unwrap();
        assert!(!w.append_if_grown(&gone, &seen).unwrap());
        fs::write(&p, "x\ny\nzzzzzzzzzzzzzzzzzzzz\n").unwrap();
        assert!(!w.append_if_grown(&gone, &seen).unwrap());
        // rewritten in place at the same length
        fs::write(&p, "a\t1\nb\t3\n").unwrap();
        assert!(!w.append_if_grown(&gone, &seen).unwrap());
        // replaced by another file with the same bytes
        fs::remove_file(&p).unwrap();
        fs::write(&p, "a\t1\nb\t2\n").unwrap();
        assert!(!w.append_if_grown(&gone, &seen).unwrap());
        assert_eq!(fs::read(&p).unwrap(), b"a\t1\nb\t2\n", "nothing written");
        let now = Seen::of(&p).unwrap().unwrap();
        assert!(w.append_if_grown(&gone, &now).unwrap());
        assert_eq!(fs::read(&p).unwrap(), b"a\t1\nb\t2\na\n");
    }

    #[test]
    fn bytes_beyond_the_end_of_a_file_truncated_meanwhile_are_none_not_an_error() {
        // `Seen::of` and `grew` take a length, then read the bytes before it: another process
        // may truncate the file between the two.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.tsv");
        fs::write(&p, "a\t1\nb\t2\n").unwrap();
        let file = File::open(&p).unwrap();
        assert_eq!(bytes_at(&file, 4, 4).unwrap().unwrap(), b"b\t2\n");
        fs::write(&p, "a\t1\n").unwrap(); // the same inode, shorter
        assert_eq!(bytes_at(&file, 4, 4).unwrap(), None);
        assert_eq!(bytes_at(&file, 0, 0).unwrap().unwrap(), b"");
    }

    #[test]
    fn a_loose_files_unterminated_last_line_is_ended_before_an_append() {
        // TSVZ.py reads it as a row, and ends it before it appends; a store's is cut (§4.3).
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.tsv");
        fs::write(&p, "a\t1\nb\t2").unwrap();
        Writer::open(&p)
            .unwrap()
            .append(&[Record::from_args("c", &["3".into()])])
            .unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"a\t1\nb\t2\nc\t3\n");
    }

    #[test]
    fn loose_writer_with_a_delimiter() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.tsv");
        let mut w = Writer::open(&p).unwrap().delimiter(Some(b';'));
        w.append(&[Record::from_args("k", &["a\tb".into()])])
            .unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"k;a\tb\n");
        w.append(&[Record::from_args("k", &["x;y".into()])])
            .unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"k;a\tb\nk;x<sep>y\n");
    }

    #[test]
    fn a_key_names_itself_unless_a_reader_would_strip_it() {
        let live = |k: &str| k == "k ";
        assert!(strippable("k ", b'\t') && strippable("k\t", b','));
        assert!(!strippable("k\t", b'\t'), "written k<sep>");
        assert!(!strippable("k", b'\t'));
        assert_eq!(resolve("#k \t ", b','), "#k");
        assert_eq!(
            resolve("k\t ", b'\t'),
            "k\t",
            "only the encoded form's white space"
        );
        assert_eq!(
            name_key("k ", b'\t', true, live),
            "k ",
            "the live key equal to it"
        );
        assert_eq!(name_key("x  ", b'\t', true, live), "x");
        assert_eq!(
            name_key("x  ", b'\t', false, live),
            "x  ",
            "stripping is off"
        );
        assert_eq!(name_key("k\t", b'\t', true, live), "k\t");
    }
}

/// The loose writer's rule against the reader's, over many records (the 0.2.1 re-review's
/// property check, rr021b): a record is written exactly when its line, encoded as TSVZ.py
/// encodes it, reads back ([`crate::loose::parse`]) as its fields less the whitespace at their
/// ends; and what is written reads back so.
#[cfg(test)]
mod loose_round_trip {
    use super::*;
    use crate::loose::{parse, py_space, sanitize};

    #[rustfmt::skip]
    const ATOMS: &[&str] = &[
        "a", "e", "s", "p", "L", "F", "<", ">", "/", "#", "_", "\0", " ", "\t", "\n", "\r",
        "<sep>", "<LF>", "</sep/>", "</LF/>", "#_defaults_#", "\u{1c}", "\u{a0}", ",", "x",
    ];
    const DS: &[u8] = b"\t,e</#>L \0s_x";

    /// The cells `line` reads back as equal `fields` less some whitespace at their ends.
    fn same(line: &str, d: u8, fields: &[String]) -> bool {
        parse(line, d, true).is_some_and(|cells| {
            cells.len() == fields.len()
                && cells.iter().zip(fields).all(|(c, f)| {
                    f.strip_prefix(&**c)
                        .is_some_and(|r| r.chars().all(py_space))
                })
        })
    }

    fn check(fields: &[String], d: u8, bad: &mut Vec<String>) {
        let rec = if fields.len() == 1 {
            Record::Tombstone {
                key: fields[0].clone(),
            }
        } else {
            Record::Data {
                key: fields[0].clone(),
                values: fields[1..].to_vec(),
            }
        };
        let raw: Vec<String> = fields.iter().map(|f| sanitize(f, d).into_owned()).collect();
        let readable = same(&raw.join(&(d as char).to_string()), d, fields);
        let mut out = String::new();
        let format = Format {
            delimiter: d,
            strict: false,
        };
        let written = encode_record(&rec, format, &mut out).is_ok();
        let reads = written && same(out.strip_suffix('\n').unwrap_or(&out), d, fields);
        if written != readable || written != reads || (!written && !out.is_empty()) {
            bad.push(format!("{:?} {fields:?} written={written}", d as char));
        }
    }

    /// A fixed table that shares nothing with `parse`: the line each record must write in a loose
    /// file, or None for the refusal. Each row was read by TSVZ.py 5.18 (`readTSV`, whose
    /// `_processLine` splits, strips and decodes a line): a written line gives back its cells; a
    /// refused one is read as a comment (`#tag`), skipped (`" "`), as `<sep>` (`</sep/>`), or as
    /// three cells (`-d e`, `hello`).
    #[test]
    fn a_loose_write_is_what_tsvz_py_reads_back() {
        #[rustfmt::skip]
        let table: &[(u8, &[&str], Option<&str>)] = &[
            (b'\t', &["a", "1"], Some("a\t1\n")),
            (b'\t', &["a ", "5 "], Some("a\t5\n")),
            (b'\t', &["a\n", "1"], Some("a<LF>\t1\n")),
            (b'\t', &["a<sep>", "1"], Some("a</sep/>\t1\n")),
            (b'\t', &["a", "b\tc"], Some("a\tb<sep>c\n")),
            (b'\t', &["#_defaults_#", "P"], Some("#_defaults_#\tP\n")),
            (b'\t', &["k"], Some("k\n")),
            (b'\t', &["k", "x", "a"], Some("k\tx\ta\n")),
            (b'e', &["k", "hllo"], Some("kehllo\n")),
            (b'#', &["#tag"], Some("<sep>tag\n")),
            (b' ', &[" "], Some("<sep>\n")),
            (b'\t', &["#tag", "1"], None),
            (b'\t', &[" ", "1"], None),
            (b'\t', &["k", "</sep/>"], None),
            (b'e', &["k", "hello"], None),
        ];
        for &(d, fields, line) in table {
            let rec = match fields {
                [key] => Record::Tombstone {
                    key: key.to_string(),
                },
                [key, values @ ..] => Record::Data {
                    key: key.to_string(),
                    values: values.iter().map(|v| v.to_string()).collect(),
                },
                [] => unreachable!(),
            };
            let format = Format {
                delimiter: d,
                strict: false,
            };
            let mut out = String::new();
            let written = encode_record(&rec, format, &mut out);
            match line {
                Some(line) => assert!(written.is_ok() && out == line, "{fields:?}: {out:?}"),
                None => assert!(written.is_err() && out.is_empty(), "{fields:?}: {out:?}"),
            }
        }
    }

    #[test]
    fn a_loose_write_is_refused_exactly_when_it_would_not_read_back() {
        let mut bad = Vec::new();
        let mut keys = vec![String::new()];
        for _ in 0..3 {
            let next: Vec<String> = keys
                .iter()
                .flat_map(|k| ATOMS.iter().map(move |a| format!("{k}{a}")))
                .collect();
            keys.extend(next);
            keys.sort();
            keys.dedup();
        }
        for &d in DS {
            for k in &keys {
                check(std::slice::from_ref(k), d, &mut bad);
            }
        }
        let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut next = move || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        for _ in 0..300_000 {
            let d = DS[(next() % DS.len() as u64) as usize];
            let n = 1 + (next() % 4) as usize;
            let fields: Vec<String> = (0..n)
                .map(|_| {
                    (0..next() % 5)
                        .map(|_| ATOMS[(next() % ATOMS.len() as u64) as usize])
                        .collect()
                })
                .collect();
            check(&fields, d, &mut bad);
        }
        assert!(
            bad.is_empty(),
            "{} mismatches: {:?}",
            bad.len(),
            &bad[..bad.len().min(20)]
        );
    }
}
