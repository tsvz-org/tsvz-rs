//! In-place maintenance of a one-part store: clear and scrub (§19.8, §19.2.3, §19.4), written
//! the way TSVZ.py writes them.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
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
    // The copy is created with the part's own permission bits (less the umask), so it is never
    // readable by anyone the part was closed to.
    let mode = file.metadata().map_err(io)?.permissions().mode() & 0o7777;
    let first = PathBuf::from(format!("{}.damaged-{}", path.display(), local_timestamp()));
    let mut target = first.clone();
    let mut n = 0;
    let mut copy = loop {
        match OpenOptions::new().write(true).create_new(true).mode(mode).open(&target) {
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

    #[test]
    fn the_backup_of_a_private_part_is_as_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.tsvz");
        fs::write(&p, "#_on_corruption_#\tstop\na\t1\n\0\0c\t3\n").unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o600)).unwrap();
        let Outcome::Partial { backup: Some(copy) } = scrub(&p, &opts()).unwrap() else { panic!("not partial") };
        let mode = fs::metadata(&copy).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode & 0o077, 0, "backup mode {mode:o} opens a private part to others");
    }
}
