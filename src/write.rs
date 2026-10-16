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

/// Append `record` and its `\n` to `out`, encoded for `format` (§13.3). A loose file has no
/// escaping, so a field holding its delimiter, `\n` or `\r` is refused.
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
    if !format.strict {
        if !name::usable_delimiter(d) {
            return Err(Error::Refused(format!(
                "the delimiter {d:#04x} is not one ASCII character other than a line break; nothing was written"
            )));
        }
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

/// `records` encoded for `format` as runs, each with the member size in force where it starts
/// (§16.7): `member_size`, then from the record after each valid `#_member_size_#` marker on,
/// the size it sets, as a reader replays them (a loose file has no markers).
pub(crate) fn runs(
    records: &[Record],
    format: Format,
    member_size: u64,
) -> Result<Vec<(String, u64)>, Error> {
    let mut runs = vec![(String::new(), member_size)];
    for r in records {
        let at = runs.len() - 1;
        encode_record(r, format, &mut runs[at].0)?;
        if let (true, Some((key, m))) = (format.strict, marker_effect(r)) {
            if key == "#_member_size_#" && m.member_size != runs[at].1 {
                runs.push((String::new(), m.member_size));
            }
        }
    }
    runs.retain(|(run, _)| !run.is_empty());
    Ok(runs)
}

/// Where [`Writer::append_runs_at`] appended, the part's stamp just before and after, and
/// the file it appended through (still open: it syncs that file even once renamed).
#[doc(hidden)]
pub struct Appended {
    pub part: PathBuf,
    pub before: parts::Stamp,
    pub after: parts::Stamp,
    pub file: lock::Handle,
}

/// Whether `path` still names the file `file` was opened as.
fn names(path: &Path, file: &lock::Handle) -> bool {
    use std::os::unix::fs::MetadataExt;
    let opened = file.opened();
    std::fs::metadata(path).is_ok_and(|m| m.dev() == opened.dev() && m.ino() == opened.ino())
}

fn stamp(file: &File) -> io::Result<parts::Stamp> {
    file.metadata().map(|m| parts::Stamp::of(&m))
}

/// Appends batches of records to a store's active part (§17.7).
///
/// Each append holds the write lock shared with TSVZ.py: where only a classic POSIX lock is
/// available (macOS, and Linux without open file description locks), closing another
/// descriptor of the part in this process releases it. A program that appends with a `Writer`
/// should not open the store's parts by other means meanwhile.
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

    /// Append `records` as one batch (§18.2), creating a missing store (§20.2.2).
    pub fn append(&mut self, records: &[Record]) -> Result<(), Error> {
        let runs = runs(records, self.format, self.member_size as u64)?;
        let runs: Vec<(&[u8], u64)> = runs.iter().map(|(r, n)| (r.as_bytes(), *n)).collect();
        self.append_runs_at(&runs).map(|_| ())
    }

    /// Append encoded records (each ending in `\n`) as one batch.
    pub fn append_payload(&mut self, payload: &[u8]) -> Result<(), Error> {
        self.append_runs_at(&[(payload, self.member_size as u64)])
            .map(|_| ())
    }

    /// Append encoded records as one batch: runs of them, each in members of its own size
    /// (§16.7) on a compressed part. Says where the batch went and how the part looked, under
    /// the lock, just before and after it: what the handler needs to tell its own appends from
    /// other writers'.
    #[doc(hidden)]
    pub fn append_runs_at(&mut self, runs: &[(&[u8], u64)]) -> Result<Appended, Error> {
        // The part is locked as it was opened; when by then it is no longer the store's active
        // part (a part was started meanwhile, §17.7), or its name is another file's (it was
        // rotated away), the active part is looked up again. Only a store with no part yet is
        // created: a part's name is never made anew once it has gone.
        // The error of a part found missing by every try so far, the same part each time.
        let mut missing: Option<Error> = None;
        for at in 0..3 {
            // Sampled before the listing, so that a name made meanwhile shows as a change.
            let dir = self.dirs.stamp(&self.store);
            let listed = parts::list(&self.store)?;
            let create = listed.is_empty();
            let part = match listed.into_iter().last() {
                Some(part) => part,
                None => parts::unnumbered(&self.store)?,
            };
            let path = part.path.as_path();
            let io = |e: io::Error| Error::io(path, e);
            let bytes: Cow<[u8]> = match part.codec {
                Some(codec) => {
                    let mut bytes = Vec::new();
                    for (run, member_size) in runs {
                        let size = usize::try_from(*member_size).unwrap_or(usize::MAX).max(1);
                        bytes.extend(codec::encode_members(codec, run, size).ok_or_else(|| {
                            Error::UnsupportedCodec {
                                path: path.to_path_buf(),
                                codec: codec.name(),
                            }
                        })?);
                    }
                    bytes.into()
                }
                None => match runs {
                    [(run, _)] => (*run).into(),
                    _ => runs
                        .iter()
                        .map(|(r, _)| *r)
                        .collect::<Vec<_>>()
                        .concat()
                        .into(),
                },
            };
            let mut options = OpenOptions::new();
            options
                .append(true)
                .create(create)
                .read(part.codec.is_none());
            let file = match lock::Handle::open(path, &options) {
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
            let guard = lock::lock(&file, path).map_err(io)?;
            // Still the active part: the directory kept its timestamp, and shows no name made,
            // renamed or removed since the listing (parts::DirTrust); else listed again.
            let unchanged = dir.is_some() && parts::DirStamp::of(&self.store) == dir;
            let active = unchanged || parts::active(&self.store)?.path == part.path;
            if !active || !names(path, &file) {
                missing = None;
                continue;
            }
            let before = stamp(&file).map_err(io)?;
            let start = match part.codec {
                Some(_) => before.size,
                None => {
                    self.cut_torn_tail(&file, path).map_err(io)?;
                    file.metadata().map_err(io)?.len()
                }
            };
            self.write_batch(&file, path, &bytes, start)?;
            let after = stamp(&file).map_err(io)?;
            drop(guard);
            return Ok(Appended {
                part: part.path.clone(),
                before,
                after,
                file,
            });
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
                message: format!(
                    "cut {} uncommitted bytes after the last newline before appending",
                    size - keep
                ),
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
        assert_eq!(enc(d("k", &["a<b", "#c"]), loose).unwrap(), "k,a<b,#c\n");
        assert!(matches!(
            enc(d("k", &["a,b"]), loose),
            Err(Error::Refused(_))
        ));
        assert!(matches!(
            enc(d("k", &["a\nb"]), loose),
            Err(Error::Refused(_))
        ));
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
    fn records_after_a_member_size_marker_take_its_size_in_the_same_append() {
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
        // a, b and the marker in a 1 MiB member; then c and d, 23 bytes each, one per member.
        assert_eq!(members, [3, 1, 1]);
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
        let held = lock::Handle::open(&p, OpenOptions::new().append(true)).unwrap();
        let guard = lock::lock(&held, &p).unwrap();
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

    fn listings() -> usize {
        parts::LISTINGS.with(|n| n.get())
    }

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
    fn a_loose_store_s_writer_does_not_look_at_its_directory() {
        // A loose file is one file (§20.2.4): its directory is never listed, so its timestamp
        // would tell nothing.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.tsv");
        fs::write(&p, "a\t1\n").unwrap();
        let looks = || parts::STATS.with(|n| n.get()) + parts::ASKED.with(|n| n.get());
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
        let held = lock::Handle::open(&one, OpenOptions::new().append(true)).unwrap();
        let guard = lock::lock(&held, &one).unwrap();
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
        let held = lock::Handle::open(&one, OpenOptions::new().append(true)).unwrap();
        let guard = lock::lock(&held, &one).unwrap();
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

    #[test]
    fn loose_writer_with_a_delimiter() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.tsv");
        let mut w = Writer::open(&p).unwrap().delimiter(Some(b';'));
        w.append(&[Record::from_args("k", &["a\tb".into()])])
            .unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"k;a\tb\n");
        assert!(
            w.append(&[Record::from_args("k", &["x;y".into()])])
                .is_err()
        );
        assert_eq!(fs::read(&p).unwrap(), b"k;a\tb\n");
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
