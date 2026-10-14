//! In-place maintenance of a one-part store: clear and scrub (§19.8, §19.2.3, §19.4), written
//! the way TSVZ.py writes them.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use crate::escape;
use crate::name::Codec;
use crate::parts::{self, Stamp};
use crate::replay::{DEFAULT_MEMBER_SIZE, Markers, Policy, SPEC_VERSION};
use crate::write::Writer;
use crate::{Error, Options, Scope, Store, Warning, codec, lock};

/// What a clear or scrub did.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
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
    if m.strip
        && std::iter::once(key)
            .chain(cells.iter().map(String::as_str))
            .any(|c| c.ends_with([' ', '\t']))
    {
        return false;
    }
    !(m.fill_empty
        && cells
            .iter()
            .enumerate()
            .any(|(j, c)| c.is_empty() && m.defaults.get(j).is_some_and(|d| !d.is_empty())))
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

/// The content of the part a multi-part scrub compacts into, while the parts before it are
/// still read first (§19.5 `keep`, or until they are rotated): it must leave no trace of their
/// state. Stripping goes off and every key they leave live is deleted, every stateful marker is
/// set explicitly, then come the rows, fully resolved (§19.4), and the final stripping and
/// fill-empty settings. Read after those parts, or alone, it reads as the whole prefix does.
fn restating_snapshot(s: &Store, earlier: &Store) -> String {
    let d = s.format().delimiter;
    let m = s.markers();
    let flag = |on: bool| if on { "true" } else { "false" };
    let mut out = String::new();
    if let Some(h) = s.header() {
        out.push_str(h);
        out.push('\n');
    }
    marker_line(&mut out, d, "#_version_#", "1");
    marker_line(&mut out, d, "#_strip_trailing_whites_#", "false");
    for (key, _) in earlier.iter() {
        escape::encode_into(&mut out, key, d, true);
        out.push('\n');
    }
    marker_line(&mut out, d, "#_fill_empty_with_default_#", "false");
    marker_line(
        &mut out,
        d,
        "#_return_defaults_when_missing_#",
        flag(m.return_missing),
    );
    marker_line(&mut out, d, "#_rotate_#", m.rotate);
    marker_line(&mut out, d, "#_write_ack_#", m.write_ack);
    marker_line(&mut out, d, "#_member_size_#", &m.member_size.to_string());
    marker_line(&mut out, d, "#_on_corruption_#", m.on_corruption.name());
    out.push_str("#_defaults_#");
    for v in m.defaults.iter() {
        out.push(d as char);
        escape::encode_into(&mut out, v, d, false);
    }
    out.push('\n');
    for (key, row) in s.iter() {
        let mut cells = row.to_vec();
        if cells.len() < m.defaults.len() {
            cells.resize(m.defaults.len(), String::new());
        }
        escape::encode_into(&mut out, key, d, true);
        for c in &cells {
            out.push(d as char);
            escape::encode_into(&mut out, c, d, false);
        }
        out.push('\n');
    }
    marker_line(&mut out, d, "#_strip_trailing_whites_#", flag(m.strip));
    marker_line(
        &mut out,
        d,
        "#_fill_empty_with_default_#",
        flag(m.fill_empty),
    );
    out
}

fn stamp(path: &Path) -> Result<Stamp, Error> {
    std::fs::metadata(path)
        .map(|m| Stamp::of(&m))
        .map_err(|e| Error::io(path, e))
}

/// Replace a part's bytes in place, keeping the file (§18.7, §19.8), under the write lock and
/// only while the part still has the stamp it had before it was read. Ok(None): another writer
/// changed it; Ok(Some(false)): it already held these bytes.
pub(crate) fn rewrite(
    path: &Path,
    content: &[u8],
    expect: Stamp,
    codec: Option<Codec>,
    member_size: u64,
    recovery: Option<&Path>,
) -> Result<Option<bool>, Error> {
    let io = |e: io::Error| Error::io(path, e);
    let raw = match codec {
        Some(c) if !content.is_empty() => codec::encode_members(c, content, member_size as usize)
            .ok_or_else(|| Error::UnsupportedCodec {
            path: path.to_path_buf(),
            codec: c.name(),
        })?,
        _ => content.to_vec(),
    };
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(io)?;
    let _guard = lock::lock(&file, path).map_err(io)?;
    if Stamp::of(&file.metadata().map_err(io)?) != expect {
        return Ok(None);
    }
    let mut old = Vec::new();
    (&file).read_to_end(&mut old).map_err(io)?;
    if old == raw {
        return Ok(Some(false));
    }
    // Reserve the space first (§19.8), so a full disk or a file-size limit refuses the rewrite
    // before any byte changes.
    reserve(&file, raw.len() as u64).map_err(io)?;
    replace(path, &raw, &old, recovery, |bytes| put(&file, bytes))?;
    Ok(Some(true))
}

/// Make `bytes` the whole content of `file`, from offset 0, and sync it.
fn put(file: &File, bytes: &[u8]) -> io::Result<()> {
    let mut f = file;
    f.seek(SeekFrom::Start(0))?;
    f.write_all(bytes)?;
    file.set_len(bytes.len() as u64)?;
    file.sync_all()
}

/// Why a part holds neither its old bytes nor its new ones: an in-place rewrite failed, and so
/// did putting the old bytes back.
#[derive(Debug)]
struct Damaged(String);

impl std::fmt::Display for Damaged {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Damaged {}

/// Whether `e` says a part was left damaged by a failed rewrite.
fn damaged(e: &Error) -> bool {
    matches!(e, Error::Io { source, .. } if source.get_ref().is_some_and(|r| r.is::<Damaged>()))
}

/// Put `new` in place of `old` with `put`. When that fails, put the old bytes back (they fit
/// before) rather than leave a half-rewritten part; when that fails too, the part holds
/// neither, and the error says so ("the part is damaged") and where the `recovery` copy of
/// the new content is.
fn replace(
    path: &Path,
    new: &[u8],
    old: &[u8],
    recovery: Option<&Path>,
    mut put: impl FnMut(&[u8]) -> io::Result<()>,
) -> Result<(), Error> {
    let Err(e) = put(new) else {
        return Ok(());
    };
    let Err(again) = put(old) else {
        return Err(Error::io(path, e));
    };
    let kept = recovery
        .map(|r| format!("; its new content is in {}", r.display()))
        .unwrap_or_default();
    let why =
        format!("{e}; putting its old bytes back failed too ({again}): the part is damaged{kept}");
    Err(Error::io(path, io::Error::new(e.kind(), Damaged(why))))
}

/// Reserve `len` bytes from the start of `file` before it is rewritten. A file system that
/// cannot reserve space is written without it.
#[cfg(target_os = "linux")]
fn reserve(file: &File, len: u64) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    if len == 0 {
        return Ok(());
    }
    // SAFETY: posix_fallocate only uses the descriptor and the range given.
    match unsafe { libc::posix_fallocate(file.as_raw_fd(), 0, len as libc::off_t) } {
        0 | libc::EOPNOTSUPP | libc::ENOSYS | libc::EINVAL => Ok(()),
        e => Err(io::Error::from_raw_os_error(e)),
    }
}

/// Reserve `len` bytes before a rewrite: not available here; the restore on failure remains.
#[cfg(not(target_os = "linux"))]
fn reserve(_file: &File, _len: u64) -> io::Result<()> {
    Ok(())
}

fn local_timestamp() -> String {
    // SAFETY: `time` and `localtime_r` only write to the values passed in.
    let tm = unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&now, &mut tm);
        tm
    };
    format!(
        "{:04}{:02}{:02}T{:02}{:02}{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    )
}

/// Give a new copy the owner and group of the part `part` describes, as TSVZ.py's
/// `_matchOwner` does. Where that is not allowed, a copy left in another group loses its group
/// bits, so no one reads it who could not read the part.
fn match_owner(copy: &File, part: &std::fs::Metadata) -> io::Result<()> {
    use std::os::unix::fs::{MetadataExt, fchown};
    let gid = Some(part.gid());
    if fchown(copy, Some(part.uid()), gid).is_ok() || fchown(copy, None, gid).is_ok() {
        return Ok(());
    }
    let created = copy.metadata()?;
    if created.gid() != part.gid() {
        // What the umask left, without the group bits.
        copy.set_permissions(std::fs::Permissions::from_mode(created.mode() & 0o7707))?;
    }
    Ok(())
}

/// Copy a damaged part's bytes to `<part>.damaged-<local time>` (`.1`, `.2`, … when taken) and
/// sync it, before a rewrite drops the damage (§4.10, §16.6), as TSVZ.py does.
fn backup(path: &Path, options: &Options) -> Result<PathBuf, Error> {
    let io = |e: io::Error| Error::io(path, e);
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(io)?;
    let _guard = lock::lock(&file, path).map_err(io)?;
    // The copy is readable by whoever could read the part, and writable by its owner only: the
    // part's read bits plus owner write, never its setuid, setgid, sticky or execute bits, nor
    // write access for others (the umask still applies on top).
    let part = file.metadata().map_err(io)?;
    let mode = part.permissions().mode() & 0o644;
    let first = PathBuf::from(format!("{}.damaged-{}", path.display(), local_timestamp()));
    let mut target = first.clone();
    let mut n = 0;
    let mut copy = loop {
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(&target)
        {
            Ok(f) => break f,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                n += 1;
                target = PathBuf::from(format!("{}.{n}", first.display()));
            }
            Err(e) => return Err(Error::io(&target, e)),
        }
    };
    let copied = match_owner(&copy, &part)
        .and_then(|()| io::copy(&mut &file, &mut copy))
        .and_then(|_| copy.sync_all());
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
            message: format!(
                "kept the bytes of this damaged part, whose damage is not rewritten, in {}",
                target.display()
            ),
        });
    }
    Ok(target)
}

/// A path naming one numbered part is not a store: rewriting that part alone would drop the
/// tombstones and markers the rest of the store needs (TSVZ.py refuses it too).
fn refuse_a_part(op: &str, path: &Path) -> Result<(), Error> {
    if crate::name::names_a_part(path) {
        return Err(Error::Refused(format!(
            "{}: {op} skipped: the path names one part of a multi-part store; {op} the store path instead; nothing was written",
            path.display()
        )));
    }
    Ok(())
}

/// A whole read for a rewrite: never past a part it cannot read, since what it writes would
/// lose that part's rows.
fn whole(options: &Options) -> Options {
    Options {
        scope: Scope::All,
        verify: false,
        skip_unreadable: false,
        ..options.clone()
    }
}

/// Compact a store with §19.4 fidelity (§20.2 `scrub`), in place (§19.8): the part of a
/// one-part store; in a multi-part store, the parts before the active one into the last of them,
/// which no writer appends to (§17.7), after which `#_rotate_#` applies to the others (§19.5).
/// The active part of a multi-part store is never touched, so writers go on appending. The new
/// content is first written to a recovery copy beside the part, removed once the part holds it.
/// A multi-part scrub is refused, with nothing written, when those parts cannot be read whole
/// or hold checksum segments (§15), which compacting them would break for the active part.
pub fn scrub(store: &Path, options: &Options) -> Result<Outcome, Error> {
    refuse_a_part("scrub", store)?;
    let refused = |why: &str| {
        Err(Error::Refused(format!(
            "{}: scrub skipped: {why}; nothing was written",
            store.display()
        )))
    };
    let mut backed_up: Option<(Stamp, PathBuf)> = None;
    for _ in 0..ATTEMPTS {
        let list = parts::list(store)?;
        if list.is_empty() {
            return Err(Error::Missing(store.to_path_buf()));
        }
        let multi = list.len() > 1;
        let prefix = if multi {
            &list[..list.len() - 1]
        } else {
            &list[..]
        };
        let (others, target) = prefix.split_at(prefix.len() - 1);
        let target = &target[0];
        let before: Vec<Stamp> = prefix
            .iter()
            .map(|p| stamp(&p.path))
            .collect::<Result<_, _>>()?;
        let s = if multi {
            Store::open_prefix(store, whole(options), prefix.len())?
        } else {
            Store::open(store, whole(options))?
        };
        if s.parts()
            .iter()
            .map(|p| &p.path)
            .ne(prefix.iter().map(|p| &p.path))
        {
            continue; // the parts changed under the read
        }
        if s.newest_version() > SPEC_VERSION {
            return refused(&format!(
                "{} declare{} spec version {} (#_version_#), newer than this tsvz implements, which reads {} as version {SPEC_VERSION} only (§6.3)",
                if multi {
                    "the parts before the active one"
                } else {
                    "it"
                },
                if multi { "" } else { "s" },
                s.newest_version(),
                if multi { "them" } else { "it" },
            ));
        }
        if multi && s.incomplete() {
            return refused("the parts before the active one could not be read whole (§4.10)");
        }
        if multi && s.has_checksums() {
            return refused(
                "the parts before the active one hold checksum segments (§15), which compacting them would break for the active part",
            );
        }
        let mut copy = None;
        if s.damaged() || s.stopped().is_some() {
            copy = match &backed_up {
                Some((at, path)) if *at == before[before.len() - 1] => Some(path.clone()),
                _ => {
                    let path = backup(&target.path, options)?;
                    backed_up = Some((before[before.len() - 1], path.clone()));
                    Some(path)
                }
            };
        }
        // The parts left before the one compacted into, read whole: what it must undo.
        let earlier = if others.is_empty() {
            None
        } else {
            let earlier = Store::open_prefix(store, whole(options), others.len())?;
            if earlier
                .parts()
                .iter()
                .map(|p| &p.path)
                .ne(others.iter().map(|p| &p.path))
            {
                continue; // the parts changed under the read
            }
            Some(earlier)
        };
        let content = match &earlier {
            None => snapshot(&s),
            Some(earlier) => restating_snapshot(&s, earlier),
        };
        if multi {
            narrow_mode(&target.path, prefix)?;
        }
        let recovery = write_recovery(store, &target.path, content.as_bytes())?;
        if multi
            && !(reads_as(&recovery, &s)
                && earlier.is_none_or(|e| reads_as_after(e, &recovery, &s)))
        {
            let _ = std::fs::remove_file(&recovery);
            return refused(
                "the compacted content did not read back as the parts it replaces (please report this)",
            );
        }
        let unchanged = others
            .iter()
            .zip(&before)
            .all(|(p, b)| stamp(&p.path).is_ok_and(|now| now == *b));
        if !unchanged {
            let _ = std::fs::remove_file(&recovery);
            continue; // another process changed a part being compacted
        }
        let written = rewrite(
            &target.path,
            content.as_bytes(),
            before[before.len() - 1],
            target.codec,
            s.markers().member_size,
            Some(&recovery),
        );
        // The recovery copy goes, unless the part was left damaged: then it is all there is.
        if !written.as_ref().is_err_and(damaged) {
            let _ = std::fs::remove_file(&recovery);
        }
        let written = written?;
        let Some(written) = written else {
            continue; // another writer committed after the read
        };
        if multi {
            // A part whose damage was recovered is never deleted: rename instead (§19.5).
            let action = match s.markers().rotate {
                "delete" if s.damaged() => "rename",
                action => action,
            };
            rotate(others, action)?;
        }
        return Ok(if s.stopped().is_some() {
            Outcome::Partial { backup: copy }
        } else if written {
            Outcome::Written
        } else {
            Outcome::Unchanged
        });
    }
    Err(Error::Refused(format!(
        "{}: scrub skipped: the store kept changing while it was being compacted; nothing was written",
        store.display()
    )))
}

/// The variant extension of a store path as it is spelled (`tsvz`, `CSV`, …).
fn variant_ext(store: &Path) -> String {
    let name = store
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let base = crate::name::split_codec(&name).0;
    base.rsplit_once('.')
        .map_or_else(|| "tsvz".to_string(), |(_, ext)| ext.to_string())
}

/// Write the new content of `part` to a recovery copy beside it, private and synced, before the
/// part is rewritten in place (§19.8 is not crash-atomic): `<part>.scrub-<time>-<pid>.<ext>`, a
/// plain store of its own (no part of this store) that a crash would leave behind.
fn write_recovery(store: &Path, part: &Path, content: &[u8]) -> Result<PathBuf, Error> {
    let path = PathBuf::from(format!(
        "{}.scrub-{}-{}.{}",
        part.display(),
        local_timestamp(),
        std::process::id(),
        variant_ext(store)
    ));
    let io = |e: io::Error| Error::io(&path, e);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .map_err(io)?;
    if let Err(e) = file.write_all(content).and_then(|()| file.sync_all()) {
        let _ = std::fs::remove_file(&path);
        return Err(io(e));
    }
    Ok(path)
}

/// Whether the recovery copy reads as the store it compacts: the same rows (padding aside, §3.6)
/// and the same final markers (§19.4). A guard before a part is rewritten.
fn reads_as(recovery: &Path, s: &Store) -> bool {
    let options = Options {
        scope: Scope::All,
        ..Options::default()
    };
    Store::open(recovery, options).is_ok_and(|r| same_reading(&r, s))
}

/// Whether the recovery copy, read after the parts `earlier` read, reads as the store it
/// compacts: what a reader of the store sees while those parts stay (§19.5).
fn reads_as_after(earlier: Store, recovery: &Path, s: &Store) -> bool {
    earlier
        .read_after(recovery)
        .is_ok_and(|r| same_reading(&r, s))
}

fn same_reading(r: &Store, s: &Store) -> bool {
    !r.incomplete() && r.markers() == s.markers() && trimmed_rows(r) == trimmed_rows(s)
}

/// Make `target` no more readable than any of `prefix`, whose rows it is about to hold: the
/// intersection of their modes, without group access unless they all share its group.
fn narrow_mode(target: &Path, prefix: &[parts::Part]) -> Result<(), Error> {
    use std::os::unix::fs::MetadataExt;
    let metas: Vec<std::fs::Metadata> = prefix
        .iter()
        .map(|p| std::fs::metadata(&p.path).map_err(|e| Error::io(&p.path, e)))
        .collect::<Result<_, _>>()?;
    let own = std::fs::metadata(target).map_err(|e| Error::io(target, e))?;
    let mut mode = metas
        .iter()
        .fold(own.mode() & 0o7777, |m, meta| m & meta.mode());
    if metas.iter().any(|m| m.gid() != own.gid()) {
        mode &= !0o070;
    }
    if mode != own.mode() & 0o7777 {
        std::fs::set_permissions(target, std::fs::Permissions::from_mode(mode))
            .map_err(|e| Error::io(target, e))?;
    }
    Ok(())
}

fn sync_dir(path: &Path) {
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    if let Ok(d) = File::open(dir) {
        let _ = d.sync_all();
    }
}

/// Live rows with the empty cells a reader pads them with removed (§3.6): what compacted content
/// must reproduce, however the reader pads it.
fn trimmed_rows(s: &Store) -> Vec<(String, Vec<String>)> {
    s.iter()
        .map(|(k, r)| {
            let mut cells = r.to_vec();
            while cells.last().is_some_and(String::is_empty) {
                cells.pop();
            }
            (k.to_string(), cells)
        })
        .collect()
}

/// Apply `#_rotate_#` to parts whose rows another part now holds (§19.5): `rename` gives each
/// the `.rotated` component, never replacing a file; `delete` removes them; `keep` leaves them.
fn rotate(parts: &[parts::Part], action: &str) -> Result<(), Error> {
    for part in parts {
        let io = |e: io::Error| Error::io(&part.path, e);
        match action {
            "rename" => {
                std::fs::hard_link(&part.path, parts::rotated_path(&part.path)).map_err(io)?;
                std::fs::remove_file(&part.path).map_err(io)?;
            }
            "delete" => std::fs::remove_file(&part.path).map_err(io)?,
            _ => return Ok(()),
        }
    }
    if let Some(first) = parts.first() {
        sync_dir(&first.path);
    }
    Ok(())
}

/// Make the store empty (§20.2 `clear`): rewrite a one-part store keeping its header comment and
/// markers (§19.8); append a tombstone for every live key of a multi-part store (§18.8); create a
/// missing store (§20.2.2).
pub fn clear(store: &Path, options: &Options) -> Result<Outcome, Error> {
    refuse_a_part("clear", store)?;
    for _ in 0..ATTEMPTS {
        let list = parts::list(store)?;
        if list.is_empty() {
            Writer::open(store)?
                .warnings(options.warnings.clone())
                .delimiter(options.delimiter)
                .append(&[])?;
            return Ok(Outcome::Written);
        }
        let before = if list.len() == 1 {
            Some(stamp(&list[0].path)?)
        } else {
            None
        };
        let s = Store::open(store, whole(options))?;
        if s.stopped().is_some() {
            return Err(Error::Refused(format!(
                "{}: clear skipped: the read stopped at damaged data (§4.10); nothing was written",
                store.display()
            )));
        }
        if s.parts().len() > 1 {
            let tombstones = crate::write::tombstones(
                s.iter().map(|(k, _)| k),
                s.format().strict && s.markers().strip,
                s.format().delimiter,
            );
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
            preamble(
                &mut content,
                s.markers(),
                s.format().delimiter,
                false,
                false,
            );
        }
        let part = &s.parts()[0];
        match rewrite(
            &part.path,
            content.as_bytes(),
            before,
            part.codec,
            s.markers().member_size,
            None,
        )? {
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
        s.iter()
            .map(|(k, r)| std::iter::once(k.to_string()).chain(r.to_vec()).collect())
            .collect()
    }

    #[test]
    fn scrub_layout_resolves_rows_and_keeps_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.tsvz");
        fs::write(
            &p,
            "#key\tval\n#_version_#\t1\n#_defaults_#\td1\td2\na\tx\nb\ty\n#note\na\tz\nb\nc\t\t\n",
        )
        .unwrap();
        let before = fs::metadata(&p).unwrap();
        assert_eq!(scrub(&p, &opts()).unwrap(), Outcome::Written);
        assert_eq!(
            fs::read_to_string(&p).unwrap(),
            "#key\tval\n#_version_#\t1\n#_defaults_#\td1\td2\na\tz\td2\nc\t\t\n"
        );
        let after = fs::metadata(&p).unwrap();
        assert_eq!((after.ino(), after.mode()), (before.ino(), before.mode()));
        assert_eq!(scrub(&p, &opts()).unwrap(), Outcome::Unchanged);
    }

    #[test]
    fn scrub_markers_in_tsvz_py_order() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.tsvz");
        fs::write(&p, "#_on_corruption_#\tskip\n#_member_size_#\t9\n#_write_ack_#\tdisk\n#_rotate_#\trename\n#_return_defaults_when_missing_#\tno\n#_fill_empty_with_default_#\ton\n#_strip_trailing_whites_#\toff\n#_version_#\t1\nk\tv\n").unwrap();
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
    fn scrub_of_consecutive_parts_and_of_a_missing_store() {
        // No ordinal lies between .1 and .2: the part before the active one is compacted in place.
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("s.tsvz.1"), "a\t1\na\t9\n").unwrap();
        fs::write(dir.path().join("s.tsvz.2"), "b\t2\n").unwrap();
        let p = dir.path().join("s.tsvz");
        assert_eq!(scrub(&p, &opts()).unwrap(), Outcome::Written);
        assert_eq!(
            fs::read_to_string(dir.path().join("s.tsvz.1")).unwrap(),
            "#_version_#\t1\na\t9\n"
        );
        assert_eq!(files(dir.path()), ["s.tsvz.1", "s.tsvz.2"]);
        assert_eq!(rows(&p), [["a", "9"], ["b", "2"]]);
        assert_eq!(
            scrub(&p, &opts()).unwrap(),
            Outcome::Unchanged,
            "again: nothing to do"
        );
        assert!(matches!(
            scrub(&dir.path().join("none.tsvz"), &opts()),
            Err(Error::Missing(_))
        ));
        assert!(!dir.path().join("none.tsvz").exists());
    }

    #[test]
    fn scrub_of_damaged_data_keeps_a_copy() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.tsvz");
        let damaged = "#_on_corruption_#\tstop\na\t1\n\0\0c\t3\nd\t4\n";
        fs::write(&p, damaged).unwrap();
        let Outcome::Partial { backup: Some(copy) } = scrub(&p, &opts()).unwrap() else {
            panic!("not partial")
        };
        assert_eq!(fs::read_to_string(&copy).unwrap(), damaged);
        assert!(
            copy.file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("s.tsvz.damaged-")
        );
        assert_eq!(
            fs::read_to_string(&p).unwrap(),
            "#_version_#\t1\n#_on_corruption_#\tstop\na\t1\n"
        );
        assert_eq!(
            crate::parts::list(&p).unwrap().len(),
            1,
            "the copy is not a part"
        );
    }

    #[test]
    fn clear_keeps_header_and_markers_without_a_version_line() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.tsvz");
        fs::write(
            &p,
            "#hdr\n#_version_#\t1\n#_defaults_#\td\n#_strip_trailing_whites_#\tfalse\na\tx\n",
        )
        .unwrap();
        assert_eq!(clear(&p, &opts()).unwrap(), Outcome::Written);
        assert_eq!(
            fs::read_to_string(&p).unwrap(),
            "#hdr\n#_strip_trailing_whites_#\tfalse\n#_defaults_#\td\n"
        );
    }

    #[test]
    fn clear_of_multipart_appends_tombstones_and_of_missing_creates() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("s.tsvz"), "#_defaults_#\td\na\t1\n").unwrap();
        fs::write(dir.path().join("s.tsvz.1"), "b\t2\nc\t3\n").unwrap();
        assert_eq!(
            clear(&dir.path().join("s.tsvz"), &opts()).unwrap(),
            Outcome::Written
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("s.tsvz.1")).unwrap(),
            "b\t2\nc\t3\na\nb\nc\n"
        );
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
        assert_eq!(
            rewrite(&p, b"x\n", stale, None, DEFAULT_MEMBER_SIZE, None).unwrap(),
            None
        );
        assert_eq!(fs::read_to_string(&p).unwrap(), "a\t1\nb\t2\n");
    }

    #[test]
    fn scrub_of_a_compressed_part_and_a_loose_file() {
        let dir = tempfile::tempdir().unwrap();
        let gz = dir.path().join("s.tsvz.gz");
        let mut data =
            crate::codec::encode_member(crate::name::Codec::Gzip, b"a\t1\na\t2\n").unwrap();
        data.extend(crate::codec::encode_member(crate::name::Codec::Gzip, b"b\t3\n").unwrap());
        fs::write(&gz, data).unwrap();
        scrub(&gz, &opts()).unwrap();
        assert_eq!(
            rows(&gz),
            vec![
                vec!["a".to_string(), "2".into()],
                vec!["b".to_string(), "3".into()]
            ]
        );
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
        let Outcome::Partial { backup: Some(copy) } = scrub(&p, &opts()).unwrap() else {
            panic!("not partial")
        };
        let mode = fs::metadata(&copy).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode & 0o077,
            0,
            "backup mode {mode:o} opens a private part to others"
        );
    }

    #[test]
    fn the_backup_keeps_the_part_owner() {
        use std::os::unix::fs::MetadataExt;
        // SAFETY: geteuid only reads the process's user id.
        if unsafe { libc::geteuid() } != 0 {
            eprintln!("skipped: only root can give a file to another owner");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.tsvz");
        fs::write(&p, "#_on_corruption_#\tstop\na\t1\n\0\0c\t3\n").unwrap();
        std::os::unix::fs::chown(&p, Some(65534), Some(65534)).unwrap();
        let Outcome::Partial { backup: Some(copy) } = scrub(&p, &opts()).unwrap() else {
            panic!("not partial")
        };
        let meta = fs::metadata(&copy).unwrap();
        assert_eq!((meta.uid(), meta.gid()), (65534, 65534));
    }

    #[test]
    fn the_backup_never_carries_special_execute_or_shared_write_bits() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.tsvz");
        fs::write(&p, "#_on_corruption_#\tstop\na\t1\n\0\0c\t3\n").unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o6777)).unwrap();
        let Outcome::Partial { backup: Some(copy) } = scrub(&p, &opts()).unwrap() else {
            panic!("not partial")
        };
        let mode = fs::metadata(&copy).unwrap().permissions().mode() & 0o7777;
        assert_eq!(
            mode & 0o7133,
            0,
            "backup mode {mode:o} has setuid, setgid, sticky, execute or shared write bits"
        );
    }

    #[test]
    fn clear_never_reads_past_an_unreadable_part() {
        // Tombstones for the keys a partial read saw would miss the unread part's keys.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.tsvz");
        fs::write(&p, "a\t1\n").unwrap();
        fs::create_dir(dir.path().join("s.tsvz.1")).unwrap();
        let last = dir.path().join("s.tsvz.2");
        fs::write(&last, "b\t2\n").unwrap();
        let skipping = Options {
            skip_unreadable: true,
            ..opts()
        };
        assert!(matches!(clear(&p, &skipping), Err(Error::Io { .. })));
        assert_eq!(fs::read_to_string(&last).unwrap(), "b\t2\n");
    }

    fn files(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn scrub_of_a_multipart_store_compacts_the_parts_before_the_active_one_into_the_last() {
        // The conformance store of 20.2-scrub-of-a-multipart-store: part 0 is compacted in place.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.tsvz");
        fs::write(&p, "#hdr\n#_defaults_#\td1\td2\na\t1\na\t3\n").unwrap();
        fs::write(dir.path().join("s.tsvz.1"), "b\t2\na\n").unwrap();
        let before = rows(&p);
        assert_eq!(scrub(&p, &opts()).unwrap(), Outcome::Written);
        assert_eq!(
            files(dir.path()),
            ["s.tsvz", "s.tsvz.1"],
            "no new part, nothing left over"
        );
        assert_eq!(
            fs::read_to_string(&p).unwrap(),
            "#hdr\n#_version_#\t1\n#_defaults_#\td1\td2\na\t3\td2\n"
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("s.tsvz.1")).unwrap(),
            "b\t2\na\n"
        );
        assert_eq!(rows(&p), before);
        // Appends still go to the active part, after the compacted one.
        crate::Writer::open(&p)
            .unwrap()
            .append(&[crate::Record::Data {
                key: "a".into(),
                values: vec!["9".into()],
            }])
            .unwrap();
        assert_eq!(rows(&p), [["b", "2", "d2"], ["a", "9", "d2"]]);
        assert_eq!(scrub(&p, &opts()).unwrap(), Outcome::Unchanged);
    }

    #[test]
    fn the_rotate_marker_renames_deletes_or_keeps_the_other_parts_compacted() {
        let cases = [
            ("rename", &["s.tsvz.3.gz", "s.tsvz.8", "s.tsvz.rotated"][..]),
            ("delete", &["s.tsvz.3.gz", "s.tsvz.8"][..]),
            ("keep", &["s.tsvz", "s.tsvz.3.gz", "s.tsvz.8"][..]),
        ];
        for (action, left) in cases {
            let dir = tempfile::tempdir().unwrap();
            let p = dir.path().join("s.tsvz");
            fs::write(&p, format!("#_rotate_#\t{action}\na\t1\n")).unwrap();
            let gz = crate::codec::encode_member(crate::name::Codec::Gzip, b"b\t2\n").unwrap();
            fs::write(dir.path().join("s.tsvz.3.gz"), gz).unwrap();
            fs::write(dir.path().join("s.tsvz.8"), "c\t3\na\n").unwrap();
            let before = rows(&p);
            assert_eq!(scrub(&p, &opts()).unwrap(), Outcome::Written, "{action}");
            assert_eq!(files(dir.path()), left, "{action}");
            assert_eq!(rows(&p), before, "{action}");
            // The part compacted into keeps its codec and holds every row it subsumes.
            let gz = fs::read(dir.path().join("s.tsvz.3.gz")).unwrap();
            assert_eq!(&gz[..2], b"\x1f\x8b", "{action}");
            let alone = Store::open(dir.path().join("s.tsvz.3.gz"), opts()).unwrap();
            let keys: Vec<&str> = alone.iter().map(|(k, _)| k).collect();
            assert_eq!(keys, ["a", "b"], "{action}");
        }
    }

    #[test]
    fn scrub_refuses_a_prefix_it_cannot_snapshot_faithfully() {
        for (prefix, why) in [
            ("#_on_corruption_#\tstop\na\t1\n\0\0c\t3\n", "read whole"),
            ("#_checksum_crc32_#\na\t1\n", "checksum"),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let p = dir.path().join("s.tsvz");
            fs::write(&p, prefix).unwrap();
            fs::write(dir.path().join("s.tsvz.1"), "b\t2\n").unwrap();
            match scrub(&p, &opts()) {
                Err(Error::Refused(message)) => assert!(message.contains(why), "{message}"),
                other => panic!("{why}: {other:?}"),
            }
            assert_eq!(files(dir.path()), ["s.tsvz", "s.tsvz.1"], "{why}");
            assert_eq!(fs::read_to_string(&p).unwrap(), prefix);
        }
        // A part that cannot be read at all is an error, and nothing is written either.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.tsvz");
        fs::write(&p, "a\t1\n").unwrap();
        fs::create_dir(dir.path().join("s.tsvz.1")).unwrap();
        fs::write(dir.path().join("s.tsvz.4"), "b\t2\n").unwrap();
        assert!(matches!(scrub(&p, &opts()), Err(Error::Io { .. })));
        assert_eq!(files(dir.path()), ["s.tsvz", "s.tsvz.1", "s.tsvz.4"]);
        assert_eq!(fs::read_to_string(&p).unwrap(), "a\t1\n");
    }

    #[test]
    fn the_part_compacted_into_is_never_more_readable_than_a_part_it_holds_rows_of() {
        use std::os::unix::fs::PermissionsExt;
        // Part 0 is private; the part compacted into is not: it ends as private as part 0.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.tsvz");
        fs::write(&p, "secret\t1\n").unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o600)).unwrap();
        let open = dir.path().join("s.tsvz.2");
        fs::write(&open, "b\t2\n").unwrap();
        fs::set_permissions(&open, fs::Permissions::from_mode(0o644)).unwrap();
        fs::write(dir.path().join("s.tsvz.8"), "c\t3\n").unwrap();
        scrub(&p, &opts()).unwrap();
        let mode = fs::metadata(&open).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "mode {:o}", mode & 0o777);
        assert!(fs::read_to_string(&open).unwrap().contains("secret"));
    }

    #[test]
    fn a_damaged_prefix_is_backed_up_and_never_deleted() {
        // Recovered damage (a corrupt member under `recover`): the rewritten part is copied first
        // and `delete` is downgraded to `rename` (§19.5), so no damaged byte is destroyed.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.tsvz");
        fs::write(&p, "#_rotate_#\tdelete\na\t1\n").unwrap();
        let mut bad = crate::codec::encode_member(crate::name::Codec::Gzip, b"b\t2\n").unwrap();
        let n = bad.len();
        bad[n - 8] ^= 1; // a CRC mismatch: corrupt, but what it decoded is used
        fs::write(dir.path().join("s.tsvz.2.gz"), &bad).unwrap();
        fs::write(dir.path().join("s.tsvz.5"), "c\t3\n").unwrap();
        let before = rows(&p);
        assert_eq!(scrub(&p, &opts()).unwrap(), Outcome::Written);
        let names = files(dir.path());
        assert!(names.contains(&"s.tsvz.rotated".to_string()), "{names:?}");
        let copy = names
            .iter()
            .find(|n| n.contains(".damaged-"))
            .expect("a backup");
        assert_eq!(fs::read(dir.path().join(copy)).unwrap(), bad);
        assert_eq!(rows(&p), before);
    }

    #[test]
    fn no_recovery_copy_is_left_after_a_scrub() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.tsvz");
        fs::write(&p, "a\t1\na\t2\n").unwrap();
        scrub(&p, &opts()).unwrap();
        fs::write(dir.path().join("s.tsvz.4"), "b\t2\nb\t3\n").unwrap();
        fs::write(dir.path().join("s.tsvz.9"), "c\t3\n").unwrap();
        scrub(&p, &opts()).unwrap();
        assert_eq!(files(dir.path()), ["s.tsvz", "s.tsvz.4", "s.tsvz.9"]);
    }

    #[test]
    fn a_rewrite_whose_old_bytes_cannot_be_put_back_says_the_part_is_damaged() {
        let p = Path::new("s.tsvz");
        let copy = Path::new("s.tsvz.scrub-copy.tsvz");
        let full = || io::Error::new(io::ErrorKind::StorageFull, "no space left");
        let mut calls = Vec::new();
        replace(p, b"new", b"old", None, |b| {
            calls.push(b.to_vec());
            Ok(())
        })
        .unwrap();
        assert_eq!(calls, [b"new".to_vec()], "a rewrite that works writes once");
        calls.clear();
        let e = replace(p, b"new", b"old", Some(copy), |b| {
            calls.push(b.to_vec());
            if b == b"new" { Err(full()) } else { Ok(()) }
        })
        .unwrap_err();
        assert_eq!(
            calls,
            [b"new".to_vec(), b"old".to_vec()],
            "the old bytes go back"
        );
        assert!(!e.to_string().contains("damaged") && !damaged(&e), "{e}");
        let e = replace(p, b"new", b"old", Some(copy), |_| Err(full())).unwrap_err();
        let text = e.to_string();
        assert!(
            text.contains("s.tsvz") && text.contains("the part is damaged"),
            "{text}"
        );
        assert!(text.contains("s.tsvz.scrub-copy.tsvz"), "{text}");
        assert!(
            damaged(&e),
            "scrub keeps the recovery copy of a damaged part"
        );
    }

    #[test]
    fn scrub_refuses_to_rewrite_parts_that_declare_a_newer_spec_version() {
        // This tsvz reads version-2 lines as version 1 (§6.3); a rewrite would make that
        // degraded reading permanent for every reader.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.tsvz");
        fs::write(&p, "#_version_#\t2\na\t1\n").unwrap();
        let e = scrub(&p, &opts()).unwrap_err();
        assert!(matches!(e, Error::Refused(_)), "{e}");
        assert!(e.to_string().contains("version 2"), "{e}");
        assert_eq!(fs::read(&p).unwrap(), b"#_version_#\t2\na\t1\n");
        // Declared in the parts to compact, even when set back to 1 later.
        fs::write(dir.path().join("s.tsvz.1"), "#_version_#\t1\nb\t2\n").unwrap();
        fs::write(dir.path().join("s.tsvz.2"), "c\t3\n").unwrap();
        assert!(matches!(scrub(&p, &opts()), Err(Error::Refused(_))));
        assert_eq!(
            fs::read(dir.path().join("s.tsvz.1")).unwrap(),
            b"#_version_#\t1\nb\t2\n"
        );
        // Declared only in the active part, which a scrub never rewrites: compacted.
        fs::write(&p, "a\t1\n").unwrap();
        fs::write(dir.path().join("s.tsvz.1"), "b\t2\n").unwrap();
        fs::write(dir.path().join("s.tsvz.2"), "#_version_#\t2\nc\t3\n").unwrap();
        assert_eq!(scrub(&p, &opts()).unwrap(), Outcome::Written);
    }

    #[test]
    fn the_guard_reads_the_new_content_after_the_parts_left_before_it() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.tsvz");
        fs::write(&p, "a\t1\nb\t2\n").unwrap();
        fs::write(dir.path().join("s.tsvz.1"), "a\n").unwrap();
        let s = Store::open_prefix(&p, opts(), 2).unwrap();
        let earlier = || Store::open_prefix(&p, opts(), 1).unwrap();
        let copy = dir.path().join("copy.tsvz");
        // A plain snapshot reads right alone but brings `a` back after the part before it.
        fs::write(&copy, snapshot(&s)).unwrap();
        assert!(reads_as(&copy, &s));
        assert!(
            !reads_as_after(earlier(), &copy, &s),
            "seed 107: `a` resurrects"
        );
        fs::write(&copy, restating_snapshot(&s, &earlier())).unwrap();
        assert!(reads_as(&copy, &s));
        assert!(reads_as_after(earlier(), &copy, &s));
    }

    #[test]
    fn compacting_into_the_last_part_keeps_what_the_parts_before_it_read_as() {
        // The parts before the one compacted into are still read first (#_rotate_# keep): what
        // that part deleted, re-ordered or reset must stay so (difftest seed 107).
        for (first, last) in [
            ("a\t1\nb\t2\n", "a\n"),
            ("x\t1\ny\t2\n", "x\nx\t3\n"),
            (
                "#_fill_empty_with_default_#\ttrue\n#_defaults_#\td\na\t\n",
                "#_fill_empty_with_default_#\n#_defaults_#\nb\t\n",
            ),
            (
                "k \tv\n#_strip_trailing_whites_#\tfalse\nk \tw\n",
                "#_strip_trailing_whites_#\n",
            ),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let p = dir.path().join("s.tsvz");
            fs::write(&p, first).unwrap();
            fs::write(dir.path().join("s.tsvz.1"), last).unwrap();
            fs::write(dir.path().join("s.tsvz.2"), "z\t9\nc\t\n").unwrap();
            let before = rows(&p);
            let markers = Store::open(&p, opts()).unwrap().markers().clone();
            assert_eq!(scrub(&p, &opts()).unwrap(), Outcome::Written, "{first:?}");
            assert_eq!(rows(&p), before, "{first:?} then {last:?}");
            assert_eq!(
                Store::open(&p, opts()).unwrap().markers(),
                &markers,
                "{first:?}"
            );
            // Read alone, as a reader starting at it would, it holds the same rows.
            let alone = Store::open(dir.path().join("s.tsvz.1"), opts()).unwrap();
            let all = Store::open_prefix(&p, opts(), 2).unwrap();
            assert_eq!(trimmed_rows(&alone), trimmed_rows(&all), "{first:?}");
            assert_eq!(scrub(&p, &opts()).unwrap(), Outcome::Unchanged, "{first:?}");
        }
    }
}
