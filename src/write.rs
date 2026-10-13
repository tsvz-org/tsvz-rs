//! Appending records (§13.3, §16.4, §16.7, §17.7, §18.2).

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
