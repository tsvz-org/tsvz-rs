//! Maintenance: clear and scrub (§19.8, §19.2.3, §19.4). A one-part store is rewritten in place
//! the way TSVZ.py writes it; a multi-part store is cleared with tombstones, and scrubbed by
//! compacting the parts before the active one into the last of them, in place.

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
/// state. Stripping goes off and every key in `deleted` (the keys any of them leave live) is
/// deleted, every stateful marker is set explicitly, then come the rows, fully resolved
/// (§19.4), and the final stripping and fill-empty settings. Read after those parts, or after
/// any first ones of them, or alone, it reads as the whole prefix does.
fn restating_snapshot(s: &Store, deleted: &[String]) -> String {
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
    for key in deleted {
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

/// The spec version newer than this tsvz implements that a `#_version_#` line of the read
/// declared, as words; None when every one is implemented (§6.3).
fn newer_version(s: &Store) -> Option<String> {
    match s.newest_version() {
        n if n <= SPEC_VERSION => None,
        u64::MAX => Some("a spec version of 20 digits or more".into()),
        n => Some(format!("spec version {n}")),
    }
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
    let file = lock::Handle::open(path, OpenOptions::new().read(true).write(true)).map_err(io)?;
    let _guard = lock::lock(&file, path).map_err(io)?;
    if Stamp::of(&file.metadata().map_err(io)?) != expect {
        return Ok(None);
    }
    let mut old = Vec::new();
    (&*file).read_to_end(&mut old).map_err(io)?;
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
    let file = lock::Handle::open(path, OpenOptions::new().read(true).write(true)).map_err(io)?;
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
        .and_then(|()| io::copy(&mut &*file, &mut copy))
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

/// Compact a store with §19.4 fidelity (§20.2 `scrub`). A one-part store is compacted in place
/// (§19.8). In a multi-part store, the parts before the active one, which no writer appends to
/// (§17.7), are snapshotted into a new part between them and the active one when an ordinal is
/// free there (§19.2: written aside, checked, then linked in whole, so readers see the parts
/// before or after it, never a part being rewritten), else compacted into the last of them, in
/// place; then `#_rotate_#` applies to the parts the snapshot holds (§19.5). The active part is
/// never touched, so writers go on appending. A part rewritten in place is first written to a
/// recovery copy beside it, removed once the part holds it. A multi-part scrub is refused, with
/// nothing written, when those parts cannot be read whole or hold checksum segments (§15),
/// which compacting them would break for the active part.
pub fn scrub(store: &Path, options: &Options) -> Result<Outcome, Error> {
    refuse_a_part("scrub", store)?;
    let mut backed_up: Vec<(Stamp, PathBuf)> = Vec::new();
    let mut told = false;
    let mut why = "the store kept changing while it was being compacted".to_string();
    // The error of a file found missing by every attempt so far, the same file each time.
    let mut missing: Option<Error> = None;
    for at in 0..ATTEMPTS {
        match attempt(store, options, &mut backed_up, &mut told) {
            Ok(Step::Done(outcome)) => {
                // Copies of a damaged part an attempt made, other than the one the outcome names.
                let named = match &outcome {
                    Outcome::Partial { backup } => backup.clone(),
                    _ => None,
                };
                if let Some(w) = &options.warnings {
                    for (_, copy) in backed_up.iter().filter(|(_, c)| Some(c) != named.as_ref()) {
                        w(&Warning {
                            part: copy.clone(),
                            offset: None,
                            message: "a copy of the damaged part, as it was before it changed"
                                .into(),
                        });
                    }
                }
                return Ok(outcome);
            }
            Ok(Step::Again(gone)) => {
                let same = |a: &Error, b: &Error| io_path(a).is_some() && io_path(a) == io_path(b);
                missing = match (gone, missing.take()) {
                    (Some(e), None) if at == 0 => Some(e),
                    (Some(e), Some(before)) if same(&e, &before) => Some(e),
                    _ => None,
                };
            }
            Ok(Step::Refused(reason)) => {
                why = reason;
                missing = None;
                break;
            }
            Err(e) => return Err(e),
        }
    }
    // A file missing at every attempt is no change of the parts: its error says which.
    if let Some(e) = missing {
        why = e.to_string();
    }
    // A refusal after an attempt backed up a damaged part says so.
    let copies: Vec<String> = backed_up
        .iter()
        .map(|(_, c)| c.display().to_string())
        .collect();
    let written = match copies.len() {
        0 => "nothing was written".to_string(),
        1 => format!(
            "nothing was written but {}, a copy of the damaged part",
            copies[0]
        ),
        _ => format!(
            "nothing was written but {}, copies of the damaged part",
            copies.join(", ")
        ),
    };
    Err(Error::Refused(format!(
        "{}: scrub skipped: {why}; {written}",
        store.display()
    )))
}

/// What one attempt of a scrub came to.
enum Step {
    Done(Outcome),
    /// The parts changed under the reads: try again (with the error of a file found missing,
    /// reported should it stay so).
    Again(Option<Error>),
    /// Refused, for this reason.
    Refused(String),
}

/// The file an I/O error is about.
fn io_path(e: &Error) -> Option<&Path> {
    match e {
        Error::Io { path, .. } => Some(path),
        _ => None,
    }
}

/// Whether `e` says a file being read is not there (any more): never a rewrite that left a
/// part damaged, whatever its kind.
fn gone(e: &Error) -> bool {
    matches!(e, Error::Io { source, .. } if source.kind() == io::ErrorKind::NotFound) && !damaged(e)
}

/// The value of a read, or, when its file is gone (the parts changed), an early return of
/// `$again` with the error.
macro_rules! read {
    ($e:expr, $again:path) => {
        match $e {
            Ok(v) => v,
            Err(e) if gone(&e) => return Ok($again(Some(e))),
            Err(e) => return Err(e),
        }
    };
}

/// The metadata of each of `parts`.
fn metas(parts: &[parts::Part]) -> Result<Vec<std::fs::Metadata>, Error> {
    parts
        .iter()
        .map(|p| std::fs::metadata(&p.path).map_err(|e| Error::io(&p.path, e)))
        .collect()
}

/// One attempt of [`scrub`]; `backed_up` is the copy of a damaged part made by an attempt,
/// and `told` whether the in-place warning was given.
fn attempt(
    store: &Path,
    options: &Options,
    backed_up: &mut Vec<(Stamp, PathBuf)>,
    told: &mut bool,
) -> Result<Step, Error> {
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
    let before: Vec<Stamp> = read!(
        prefix
            .iter()
            .map(|p| stamp(&p.path))
            .collect::<Result<_, _>>(),
        Step::Again
    );
    let s = read!(
        if multi {
            Store::open_prefix(store, whole(options), prefix.len())
        } else {
            Store::open(store, whole(options))
        },
        Step::Again
    );
    if !read_parts(&s, prefix) {
        return Ok(Step::Again(None)); // the parts changed under the read
    }
    if let Some(version) = newer_version(&s) {
        return Ok(Step::Refused(format!(
            "{} declare{} {version} (#_version_#), newer than this tsvz implements, which reads {} as version {SPEC_VERSION} only (§6.3)",
            if multi {
                "the parts before the active one"
            } else {
                "it"
            },
            if multi { "" } else { "s" },
            if multi { "them" } else { "it" },
        )));
    }
    if multi && s.incomplete() {
        return Ok(Step::Refused(
            "the parts before the active one could not be read whole (§4.10)".into(),
        ));
    }
    if multi && s.has_checksums() {
        return Ok(Step::Refused(
            "the parts before the active one hold checksum segments (§15), which compacting them would break for the active part".into(),
        ));
    }
    // An ordinal free between the parts before the active one and it: the snapshot goes
    // there as a new part, atomically (§19.2); else they are compacted into the last of
    // them, in place (§19.8).
    let active = &list[list.len() - 1];
    let slot = active
        .ordinal
        .as_deref()
        .and_then(|a| parts::snapshot_ordinal(target.ordinal.as_deref(), a));
    let name = |p: &Path| {
        p.file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
    };
    let why = match (multi, slot) {
        (true, Some(ordinal)) => {
            match snapshot_part(store, options, prefix, &ordinal, &s, &before)? {
                Snapshotted::Done(outcome) => return Ok(Step::Done(outcome)),
                Snapshotted::Again(gone) => return Ok(Step::Again(gone)),
                Snapshotted::Refused(why) => return Ok(Step::Refused(why)),
                Snapshotted::InPlace(why) => why,
            }
        }
        _ => format!(
            "no ordinal is free between {} and the active part {}; sparse ordinals, such as UUIDv7 (§17.6), let a scrub write a new part instead",
            name(&target.path),
            name(&active.path)
        ),
    };
    if multi {
        use std::os::unix::fs::MetadataExt;
        let owner = read!(stamp_meta(&target.path), Step::Again).uid();
        let others_metas = read!(metas(others), Step::Again);
        if let Some((part, uid)) = unreadable_to(owner, others, &others_metas) {
            return Ok(Step::Refused(kept_from(owner, &part, uid)));
        }
    }
    if let (true, false, Some(w)) = (multi, *told, &options.warnings) {
        *told = true;
        w(&Warning {
            part: target.path.clone(),
            offset: None,
            message: format!(
                "{why}: the parts before the active one are compacted into {} in place (§19.8), where readers that take no lock may see it rewritten",
                name(&target.path)
            ),
        });
    }
    let mut copy = None;
    if s.damaged() || s.stopped().is_some() {
        copy = match backed_up.last() {
            Some((at, path)) if *at == before[before.len() - 1] => Some(path.clone()),
            last => {
                let path = read!(backup(&target.path, options), Step::Again);
                // A copy of the part as it was before (appends since) holds nothing more.
                if let Some((_, old)) = last {
                    if starts_with_file(&path, old) {
                        let _ = std::fs::remove_file(old);
                        backed_up.pop();
                    }
                }
                backed_up.push((before[before.len() - 1], path.clone()));
                Some(path)
            }
        };
    }
    // The parts left before the one compacted into, read whole: what it must undo.
    let earlier = if others.is_empty() {
        None
    } else {
        let earlier = read!(
            Store::open_prefix(store, whole(options), others.len()),
            Step::Again
        );
        if !read_parts(&earlier, others) {
            return Ok(Step::Again(None)); // the parts changed under the read
        }
        Some(earlier)
    };
    let content = match &earlier {
        None => snapshot(&s),
        Some(earlier) => match read!(
            live_after_any_prefix(store, options, others, earlier),
            Step::Again
        ) {
            Some(deleted) => restating_snapshot(&s, &deleted),
            None => return Ok(Step::Again(None)), // the parts changed under the read
        },
    };
    // A one-part scrub with no room for the copy (it is how a full disk gets room back)
    // rewrites in place without one, as TSVZ.py does, where the file system overwrites in
    // place: the rewrite reserves its length first, so it still changes nothing when even
    // that does not fit. A copy-on-write one gives no such promise: refused there.
    let recovery = match write_recovery(store, &target.path, content.as_bytes()) {
        Ok(path) => Some(path),
        Err(e) if !multi && no_room(&e) && overwrites_in_place(&target.path) => {
            if let Some(w) = &options.warnings {
                w(&Warning {
                    part: target.path.clone(),
                    offset: None,
                    message: "no room for a recovery copy of the new content; rewriting the part in place without one (§19.8)".into(),
                });
            }
            None
        }
        Err(e) if gone(&e) => return Ok(Step::Again(Some(e))),
        Err(e) => return Err(e),
    };
    let drop_copy = || {
        if let Some(copy) = &recovery {
            let _ = std::fs::remove_file(copy);
        }
    };
    if let (true, Some(copy)) = (multi, &recovery) {
        if !(reads_as(copy, &s) && earlier.is_none_or(|e| reads_as_after(e, copy, &s))) {
            drop_copy();
            return Ok(Step::Refused(
                "the compacted content did not read back as the parts it replaces (please report this)".into(),
            ));
        }
    }
    if !unchanged(others, &before) {
        drop_copy();
        return Ok(Step::Again(None)); // another process changed a part being compacted
    }
    // Narrowed only now that the rows are about to go in, and put back if they do not.
    let narrowed = if multi {
        match narrow_mode(&target.path, prefix) {
            Ok(old) => old,
            Err(e) => {
                drop_copy();
                return if gone(&e) {
                    Ok(Step::Again(Some(e)))
                } else {
                    Err(e)
                };
            }
        }
    } else {
        None
    };
    let written = rewrite(
        &target.path,
        content.as_bytes(),
        before[before.len() - 1],
        target.codec,
        s.markers().member_size,
        recovery.as_deref(),
    );
    if let (Some(old), true) = (narrowed, put_mode_back(&written)) {
        let _ = std::fs::set_permissions(&target.path, std::fs::Permissions::from_mode(old));
    }
    // The recovery copy goes, unless the part was left damaged: then it is all there is.
    if !written.as_ref().is_err_and(damaged) {
        drop_copy();
    }
    // Missing before a byte was written (a rewrite that damaged the part is never `gone`).
    let written = read!(written, Step::Again);
    let Some(written) = written else {
        return Ok(Step::Again(None)); // another writer committed after the read
    };
    if multi {
        // A part whose damage was recovered is never deleted: rename instead (§19.5).
        let action = match s.markers().rotate {
            "delete" if s.damaged() => "rename",
            action => action,
        };
        rotate(others, action)?;
    }
    Ok(Step::Done(if s.stopped().is_some() {
        Outcome::Partial { backup: copy }
    } else if written {
        Outcome::Written
    } else {
        Outcome::Unchanged
    }))
}

/// The metadata of `path`.
fn stamp_meta(path: &Path) -> Result<std::fs::Metadata, Error> {
    std::fs::metadata(path).map_err(|e| Error::io(path, e))
}

/// Whether the file `whole` begins with the bytes of the file `start` (an older copy of a part
/// that was only appended to since).
fn starts_with_file(whole: &Path, start: &Path) -> bool {
    use std::io::Read;
    let (Ok(mut a), Ok(mut b)) = (File::open(whole), File::open(start)) else {
        return false;
    };
    let (mut x, mut y) = (vec![0u8; 1 << 16], vec![0u8; 1 << 16]);
    loop {
        let n = match b.read(&mut y) {
            Ok(0) => return true,
            Ok(n) => n,
            Err(_) => return false,
        };
        if a.read_exact(&mut x[..n]).is_err() || x[..n] != y[..n] {
            return false;
        }
    }
}

/// Write the snapshot of `prefix` (read whole as `s`) as the new part `ordinal`, between it and
/// the active part (§19.2.3), then apply `#_rotate_#` to `prefix` (§19.5), newest first. No
/// part is rewritten, and the new one appears whole under its name or not at all, so readers,
/// who take no lock, see the parts before or after the snapshot. Its content is the plain
/// snapshot when every part of `prefix` stays (`keep`); the restating one when a rotation cut
/// short may leave only the first of them.
fn snapshot_part(
    store: &Path,
    options: &Options,
    prefix: &[parts::Part],
    ordinal: &str,
    s: &Store,
    before: &[Stamp],
) -> Result<Snapshotted, Error> {
    let last = &prefix[prefix.len() - 1];
    let content = if s.markers().rotate == "keep" {
        snapshot(s)
    } else {
        match read!(
            live_after_any_prefix(store, options, prefix, s),
            Snapshotted::Again
        ) {
            Some(deleted) => restating_snapshot(s, &deleted),
            None => return Ok(Snapshotted::Again(None)),
        }
    };
    let bytes = match last.codec {
        Some(c) if !content.is_empty() => {
            codec::encode_members(c, content.as_bytes(), s.markers().member_size as usize)
                .ok_or_else(|| Error::UnsupportedCodec {
                    path: last.path.clone(),
                    codec: c.name(),
                })?
        }
        _ => content.into_bytes(),
    };
    // A part is added only when it gains something: when the last part already reads alone
    // as the whole prefix and is no larger, there is nothing to compact (a scrub run again,
    // by cron say, would add a full copy each time), only a rotation cut short to finish.
    let action = match s.markers().rotate {
        "delete" if s.damaged() => "rename", // a part whose damage was recovered is kept
        action => action,
    };
    let size = std::fs::metadata(&last.path).map(|m| m.len());
    if size.is_ok_and(|len| len <= bytes.len() as u64) && reads_as(&last.path, s) {
        let others = &prefix[..prefix.len() - 1];
        if others.is_empty() || action == "keep" {
            return Ok(Snapshotted::Done(Outcome::Unchanged));
        }
        rotate(others, action)?;
        return Ok(Snapshotted::Done(Outcome::Written));
    }
    let suffix = parts::codec_suffix(&last.path);
    let target = parts::numbered_path(store, ordinal, suffix.as_deref())?;
    let tmp = snapshot_temp(store, &target, suffix.as_deref());
    let metas = read!(metas(prefix), Snapshotted::Again);
    // Its owner is judged while it is empty: one who could not read every part gets no row.
    let (file, owner) = new_snapshot(&tmp, &metas[metas.len() - 1])?;
    #[cfg(test)]
    owners_checked(&tmp);
    if let Some((part, uid)) = unreadable_to(owner, prefix, &metas) {
        drop(file);
        let _ = std::fs::remove_file(&tmp);
        return Ok(Snapshotted::Refused(kept_from(owner, &part, uid)));
    }
    if let Err(e) = fill_snapshot(file, &tmp, &bytes, &metas) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    // A guard before it is in the store: alone, and after every part before it, it reads as
    // those parts did.
    let all = Store::open_prefix(store, whole(options), prefix.len());
    let same_parts = all.as_ref().is_ok_and(|a| read_parts(a, prefix));
    if !same_parts || !unchanged(prefix, before) {
        let _ = std::fs::remove_file(&tmp);
        return Ok(Snapshotted::Again(None));
    }
    if !(reads_as(&tmp, s) && all.is_ok_and(|a| reads_as_after(a, &tmp, s))) {
        let _ = std::fs::remove_file(&tmp);
        return Ok(Snapshotted::Refused(
            "the snapshot did not read back as the parts it replaces (please report this)".into(),
        ));
    }
    // A link never replaces a file: a name taken meanwhile (another scrub) means try again;
    // a file system without links (FAT, some network and FUSE mounts) means in place.
    let linked = std::fs::hard_link(&tmp, &target);
    let _ = std::fs::remove_file(&tmp);
    match linked {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => return Ok(Snapshotted::Again(None)),
        // The temporary file was removed meanwhile: write it again.
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Ok(Snapshotted::Again(Some(Error::io(&tmp, e))));
        }
        Err(e) => {
            return Ok(Snapshotted::InPlace(format!(
                "a new part cannot be linked in here ({e})"
            )));
        }
    }
    sync_dir(&target);
    rotate(prefix, action)?;
    Ok(Snapshotted::Done(Outcome::Written))
}

/// What `snapshot_part` did: the scrub is done; the parts changed under the reads, so try
/// again; it is refused; or the parts are to be compacted in place instead, for the reason
/// given.
enum Snapshotted {
    Done(Outcome),
    Again(Option<Error>),
    Refused(String),
    InPlace(String),
}

/// Where a snapshot part is written before it is linked under its name: a store of its own,
/// `<stem>.snapshot-<time>-<pid>-<n>.<ext>[.<codec>]` (`n` counts this process's snapshots),
/// beside the parts but none of them.
fn snapshot_temp(store: &Path, target: &Path, suffix: Option<&str>) -> PathBuf {
    static MADE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = MADE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let (stem, ext) = stem_and_ext(store);
    let mut temp = format!(
        "{stem}.snapshot-{}-{}-{n}.{ext}",
        local_timestamp(),
        std::process::id()
    );
    if let Some(s) = suffix {
        temp.push('.');
        temp.push_str(s);
    }
    target.with_file_name(temp)
}

/// `mode` narrowed for a file of group `gid` that holds the rows of parts with `metas`: every
/// bit but its owner's read and write (they let the owner rewrite it, and scrub again, even
/// when old parts were made read-only) is no more than in every part; group bits go when the
/// parts are not all of its group. Group and others can neither read nor write the rows more
/// than in every part they come from.
fn closed_mode(mode: u32, metas: &[std::fs::Metadata], gid: u32) -> u32 {
    use std::os::unix::fs::MetadataExt;
    let mut shared = metas.iter().fold(mode & 0o7177, |m, meta| m & meta.mode());
    if metas.iter().any(|m| m.gid() != gid) {
        shared &= !0o070;
    }
    (mode & 0o600) | shared
}

/// Make the new, empty file `path` (0600), owned like the part with `like` when this process
/// may give it away: the file and its owner. A file it made is removed if it fails after.
fn new_snapshot(path: &Path, like: &std::fs::Metadata) -> Result<(File, u32), Error> {
    use std::os::unix::fs::MetadataExt;
    let io = |e: io::Error| Error::io(path, e);
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(io)?;
    match match_owner(&file, like).and_then(|()| file.metadata()) {
        Ok(meta) => Ok((file, meta.uid())),
        Err(e) => {
            drop(file);
            let _ = std::fs::remove_file(path);
            Err(io(e))
        }
    }
}

/// Write `bytes` to the new snapshot `file` (at `path`), give it a mode no more open than any
/// of the parts with `metas` (its owner reads and writes it), and sync it.
fn fill_snapshot(
    mut file: File,
    path: &Path,
    bytes: &[u8],
    metas: &[std::fs::Metadata],
) -> Result<(), Error> {
    use std::os::unix::fs::MetadataExt;
    let io = |e: io::Error| Error::io(path, e);
    file.write_all(bytes).map_err(io)?;
    let gid = file.metadata().map_err(io)?.gid();
    file.set_permissions(std::fs::Permissions::from_mode(closed_mode(
        0o666, metas, gid,
    )))
    .map_err(io)?;
    file.sync_all().map_err(io)
}

/// The variant extension of a store path as it is spelled (`tsvz`, `CSV`, …).
fn variant_ext(store: &Path) -> String {
    stem_and_ext(store).1
}

/// A store path's name without its codec suffix, split at its variant extension: `s.tsvz.gz`
/// → (`s`, `tsvz`).
fn stem_and_ext(store: &Path) -> (String, String) {
    let name = store
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let base = crate::name::split_codec(&name).0;
    let (stem, ext) = base.rsplit_once('.').unwrap_or((base, "tsvz"));
    (stem.to_string(), ext.to_string())
}

/// Write the new content of `part` to a recovery copy beside it, private and synced, before the
/// part is rewritten in place (§19.8 is not crash-atomic): `<part>.scrub-<time>-<pid>-<n>.<ext>`
/// (`n` counts this process's copies), a plain store of its own (no part of this store) that a
/// crash would leave behind.
fn write_recovery(store: &Path, part: &Path, content: &[u8]) -> Result<PathBuf, Error> {
    static MADE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let (mut file, path) = loop {
        let n = MADE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = PathBuf::from(format!(
            "{}.scrub-{}-{}-{n}.{}",
            part.display(),
            local_timestamp(),
            std::process::id(),
            variant_ext(store)
        ));
        // Taken (another host's scrub, same second and pid): the next number.
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(file) => break (file, path),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(Error::io(&path, e)),
        }
    };
    let io = |e: io::Error| Error::io(&path, e);
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
    !r.incomplete() && same_markers(r.markers(), s.markers()) && trimmed_rows(r) == trimmed_rows(s)
}

/// Markers equal but for empty trailing defaults, which read as no default at all (§14, and
/// §3.6 lets a reader pad): a snapshot leaves a #_defaults_# of empty values out, as TSVZ.py does.
fn same_markers(a: &Markers, b: &Markers) -> bool {
    let trim = |m: &Markers| {
        let mut m = m.clone();
        let kept = m
            .defaults
            .iter()
            .rposition(|d| !d.is_empty())
            .map_or(0, |i| i + 1);
        m.defaults = std::sync::Arc::new(m.defaults[..kept].to_vec());
        m
    };
    trim(a) == trim(b)
}

/// Whether a part narrowed for a rewrite gets its mode back: only when it surely holds its old
/// bytes (nothing was written, or the old bytes went back), never when it may hold new rows.
fn put_mode_back(written: &Result<Option<bool>, Error>) -> bool {
    match written {
        Ok(None) => true,
        Ok(Some(_)) => false,
        Err(e) => !damaged(e),
    }
}

/// The keys live after any first ones of `others` (all of them read as `earlier`), in order:
/// what a rotation cut short leaves before the compacted part, read from the start (rotate
/// removes the newest first). None when the parts changed under the reads.
fn live_after_any_prefix(
    store: &Path,
    options: &Options,
    others: &[parts::Part],
    earlier: &Store,
) -> Result<Option<Vec<String>>, Error> {
    let mut seen = std::collections::HashSet::new();
    let mut keys = Vec::new();
    let mut add = |s: &Store| {
        for (k, _) in s.iter() {
            if seen.insert(k.to_string()) {
                keys.push(k.to_string());
            }
        }
    };
    let keys_only = Options {
        scope: Scope::KeysOnly,
        ..whole(options)
    };
    // One read, extended a part at a time: the keys live at each part boundary.
    if others.len() > 1 {
        let mut first = Store::open_prefix(store, keys_only, 1)?;
        if first.parts().first().map(|p| &p.path) != Some(&others[0].path) {
            return Ok(None);
        }
        add(&first);
        for part in &others[1..others.len() - 1] {
            first = first.read_after(&part.path)?;
            add(&first);
        }
    }
    add(earlier);
    Ok(Some(keys))
}

/// Whether the file system holding `path` overwrites a file's blocks in place, so that the
/// space `reserve` takes for a rewrite is really there (ext2/3/4, XFS, f2fs, tmpfs). Not a
/// copy-on-write one (btrfs, ZFS, bcachefs, APFS), which needs new blocks to overwrite, and not
/// one it cannot tell.
#[cfg(target_os = "linux")]
fn overwrites_in_place(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: statfs reads the NUL-terminated path and writes the struct it is given.
    let mut fs: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(c.as_ptr(), &mut fs) } != 0 {
        return false;
    }
    const EXT: i64 = 0xEF53;
    const XFS: i64 = 0x5846_5342;
    const F2FS: i64 = 0xF2F5_2010;
    const TMPFS: i64 = 0x0102_1994;
    matches!(fs.f_type as i64, EXT | XFS | F2FS | TMPFS)
}

#[cfg(not(target_os = "linux"))]
fn overwrites_in_place(_path: &Path) -> bool {
    false
}

/// Whether `e` is a full disk or quota.
fn no_room(e: &Error) -> bool {
    matches!(e, Error::Io { source, .. }
        if matches!(source.raw_os_error(), Some(libc::ENOSPC | libc::EDQUOT)))
}

/// Whether `s` read exactly `parts`: else the parts changed under the read.
fn read_parts(s: &Store, parts: &[parts::Part]) -> bool {
    s.parts()
        .iter()
        .map(|p| &p.path)
        .eq(parts.iter().map(|p| &p.path))
}

/// Whether each of `parts` still has its stamp in `before` (as many as `parts`, or more).
fn unchanged(parts: &[parts::Part], before: &[Stamp]) -> bool {
    parts
        .iter()
        .zip(before)
        .all(|(p, b)| stamp(&p.path).is_ok_and(|now| now == *b))
}

/// Whether two paths name the same file.
fn same_file(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(a), Ok(b)) => (a.dev(), a.ino()) == (b.dev(), b.ino()),
        _ => false,
    }
}

/// Whether user `uid`, a member of `groups`, may read a file with `meta`, by its mode: its
/// owner (who can always change the mode) and root may; a member of its group by the group
/// bits; anyone else by the others bits.
fn may_read(uid: u32, groups: &[u32], meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    if uid == 0 || uid == meta.uid() {
        return true;
    }
    let bit = if groups.contains(&meta.gid()) {
        0o040
    } else {
        0o004
    };
    meta.mode() & bit != 0
}

/// The groups of user `uid`, its primary one first; None for a user the system does not know.
/// Its entry is read into `room` bytes first, more when it does not fit.
fn groups_of(uid: u32, room: usize) -> Option<Vec<u32>> {
    // SAFETY: an all-zero passwd is valid; getpwuid_r fills it with pointers into `buf`.
    let mut pw: libc::passwd = unsafe { std::mem::zeroed() };
    let mut buf = vec![0 as libc::c_char; room.max(1)];
    loop {
        let mut found = std::ptr::null_mut();
        // SAFETY: every pointer is valid for the sizes given.
        let r = unsafe { libc::getpwuid_r(uid, &mut pw, buf.as_mut_ptr(), buf.len(), &mut found) };
        if r == libc::ERANGE && buf.len() < 1 << 20 {
            buf.resize(buf.len() * 2, 0); // the entry does not fit: room for it
            continue;
        }
        if r != 0 || found.is_null() {
            return None;
        }
        break;
    }
    let mut groups = vec![pw.pw_gid];
    let mut room: libc::c_int = 64;
    while room <= 1 << 16 {
        let mut n = room;
        #[cfg(target_vendor = "apple")]
        let (got, list) = {
            let mut list = vec![0 as libc::c_int; room as usize];
            // SAFETY: `list` has room for `n` entries; pw_name points into `buf`.
            let got = unsafe {
                libc::getgrouplist(
                    pw.pw_name,
                    pw.pw_gid as libc::c_int,
                    list.as_mut_ptr(),
                    &mut n,
                )
            };
            (
                got,
                list.into_iter().map(|g| g as u32).collect::<Vec<u32>>(),
            )
        };
        #[cfg(not(target_vendor = "apple"))]
        let (got, list) = {
            let mut list = vec![0 as libc::gid_t; room as usize];
            // SAFETY: as above.
            let got =
                unsafe { libc::getgrouplist(pw.pw_name, pw.pw_gid, list.as_mut_ptr(), &mut n) };
            (got, list)
        };
        if got >= 0 {
            let n = usize::try_from(n).unwrap_or(0).min(list.len());
            groups.extend_from_slice(&list[..n]);
            break;
        }
        room = room.saturating_mul(2).max(n.saturating_add(1)); // too small: n may say how many
    }
    Some(groups)
}

/// The first of `parts` (with `metas`) that user `owner` could not read, by its mode, and that
/// part's owner: rows a scrub must not compact into a part of `owner`'s, who would then read
/// them. None when `owner` is the user scrubbing, who has just read them all.
fn unreadable_to(
    owner: u32,
    parts: &[parts::Part],
    metas: &[std::fs::Metadata],
) -> Option<(PathBuf, u32)> {
    use std::os::unix::fs::MetadataExt;
    // SAFETY: geteuid only reads the process's user id.
    if owner == unsafe { libc::geteuid() } {
        return None;
    }
    let mut groups = None;
    parts.iter().zip(metas).find_map(|(p, meta)| {
        let groups = groups.get_or_insert_with(|| groups_of(owner, 1 << 14).unwrap_or_default());
        (!may_read(owner, groups, meta)).then(|| (p.path.clone(), meta.uid()))
    })
}

/// Why rows of `part` (owned by `uid`) stay out of a part of `owner`'s.
fn kept_from(owner: u32, part: &Path, uid: u32) -> String {
    format!(
        "the rows of {} (uid {uid}) would go into a part of uid {owner}, who cannot read them now; give the parts one owner, or let uid {owner} read them, first",
        part.display()
    )
}

/// The size of each snapshot file as its owner was judged: for tests.
#[cfg(test)]
static OWNERS_CHECKED: std::sync::Mutex<Vec<(PathBuf, u64)>> = std::sync::Mutex::new(Vec::new());

#[cfg(test)]
fn owners_checked(tmp: &Path) {
    let len = std::fs::metadata(tmp).map_or(u64::MAX, |m| m.len());
    OWNERS_CHECKED
        .lock()
        .unwrap()
        .push((tmp.to_path_buf(), len));
}

/// Make `target` no more open than any of `prefix`, whose rows it is about to hold: the
/// intersection of their modes, without group access unless they all share its group, except
/// that its owner keeps reading and writing it. The mode before, when it changed.
fn narrow_mode(target: &Path, prefix: &[parts::Part]) -> Result<Option<u32>, Error> {
    use std::os::unix::fs::MetadataExt;
    let metas = metas(prefix)?;
    let own = std::fs::metadata(target).map_err(|e| Error::io(target, e))?;
    let old = own.mode() & 0o7777;
    let mode = closed_mode(old, &metas, own.gid());
    if mode == old {
        return Ok(None);
    }
    std::fs::set_permissions(target, std::fs::Permissions::from_mode(mode))
        .map_err(|e| Error::io(target, e))?;
    Ok(Some(old))
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
    // Newest first: cut short, it leaves the oldest parts, read from the start as written. A
    // part already gone was rotated by another scrub.
    let not_found = |e: &io::Error| e.kind() == io::ErrorKind::NotFound;
    for part in parts.iter().rev() {
        let io = |e: io::Error| Error::io(&part.path, e);
        match action {
            "rename" => {
                let rotated = parts::rotated_path(&part.path, part.ordinal.is_none());
                match std::fs::hard_link(&part.path, &rotated) {
                    Ok(()) => {}
                    // A rotation cut short between the link and the unlink: finish it.
                    Err(e)
                        if e.kind() == io::ErrorKind::AlreadyExists
                            && same_file(&part.path, &rotated) => {}
                    Err(e) if not_found(&e) => continue,
                    Err(e) => return Err(io(e)),
                }
                match std::fs::remove_file(&part.path) {
                    Err(e) if !not_found(&e) => return Err(io(e)),
                    _ => {}
                }
            }
            "delete" => match std::fs::remove_file(&part.path) {
                Err(e) if !not_found(&e) => return Err(io(e)),
                _ => {}
            },
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
        if s.incomplete() {
            let why = if s.stopped().is_some() {
                "the read stopped at damaged data (§4.10)"
            } else {
                "the store could not be read whole (a part kept changing while it was read)"
            };
            return Err(Error::Refused(format!(
                "{}: clear skipped: {why}; nothing was written",
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
            // Synced as a one-part clear is (rewritten and synced): a clear that answers is
            // durable, whatever #_write_ack_# says (§19.8, §21.9.2).
            Writer::open(store)?
                .member_size(s.markers().member_size as usize)
                .fsync(true)
                .warnings(options.warnings.clone())
                .delimiter(options.delimiter)
                .append(&tombstones)?;
            return Ok(Outcome::Written);
        }
        let Some(before) = before else { continue };
        if let Some(version) = newer_version(&s) {
            return Err(Error::Refused(format!(
                "{}: clear skipped: it declares {version} (#_version_#), newer than this tsvz implements, whose markers it cannot keep (§19.8); nothing was written",
                store.display()
            )));
        }
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

    /// Every file of `dir`: name, bytes, mode and owner.
    fn files_of(dir: &Path) -> Vec<(String, Vec<u8>, u32, u32)> {
        use std::os::unix::fs::MetadataExt;
        let mut all: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .map(|e| {
                let e = e.unwrap();
                let m = e.metadata().unwrap();
                let name = e.file_name().to_string_lossy().into_owned();
                (name, fs::read(e.path()).unwrap(), m.mode(), m.uid())
            })
            .collect();
        all.sort();
        all
    }

    fn give(p: &Path, uid: u32, gid: u32, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
        std::os::unix::fs::chown(p, Some(uid), Some(gid)).unwrap();
        fs::set_permissions(p, fs::Permissions::from_mode(mode)).unwrap();
    }

    #[test]
    fn a_scrub_never_moves_rows_into_a_part_whose_owner_could_not_read_them() {
        // SAFETY: geteuid only reads the process's user id.
        if unsafe { libc::geteuid() } != 0 {
            eprintln!("skipped: only root can give files to other owners");
            return;
        }
        // Consecutive ordinals compact in place into .1; sparse ones write a new part .5. Both
        // would belong to uid 12345, who cannot read the private part of uid 65534.
        for (middle, active) in [("s.tsvz.1", "s.tsvz.2"), ("s.tsvz.3", "s.tsvz.8")] {
            let dir = tempfile::tempdir().unwrap();
            let p = dir.path().join("s.tsvz");
            fs::write(&p, "secret\tB-only\n").unwrap();
            give(&p, 65534, 65534, 0o600);
            let m = dir.path().join(middle);
            fs::write(&m, "x\t1\n").unwrap();
            give(&m, 12345, 12345, 0o644);
            fs::write(dir.path().join(active), "y\t2\n").unwrap();
            let before = files_of(dir.path());
            let err = scrub(&p, &opts()).unwrap_err().to_string();
            assert!(
                err.contains("12345") && err.contains("nothing was written"),
                "{err}"
            );
            assert_eq!(files_of(dir.path()), before, "{middle}");
        }
    }

    #[test]
    fn group_membership_counts_the_primary_group_and_the_others() {
        let root = groups_of(0, 1 << 14).expect("root is known");
        assert_eq!(root[0], 0, "root's primary group first");
        assert!(!root.contains(&4_000_000_000));
        assert_eq!(
            groups_of(4_000_000_000, 1 << 14),
            None,
            "a user the system does not know"
        );
        // A member listed in /etc/group whose primary group is another one, if any.
        let users: std::collections::HashMap<String, (u32, u32)> =
            fs::read_to_string("/etc/passwd")
                .unwrap_or_default()
                .lines()
                .filter_map(|l| {
                    let f: Vec<&str> = l.split(':').collect();
                    Some((
                        f.first()?.to_string(),
                        (f.get(2)?.parse().ok()?, f.get(3)?.parse().ok()?),
                    ))
                })
                .collect();
        let other = fs::read_to_string("/etc/group")
            .unwrap_or_default()
            .lines()
            .find_map(|l| {
                let f: Vec<&str> = l.split(':').collect();
                let gid: u32 = f.get(2)?.parse().ok()?;
                f.get(3)?.split(',').find_map(|m| {
                    let &(uid, primary) = users.get(m)?;
                    (primary != gid).then_some((uid, gid))
                })
            });
        if let Some((uid, gid)) = other {
            let groups = groups_of(uid, 1 << 14).unwrap();
            assert!(groups.contains(&gid), "uid {uid}, group {gid}");
        }
    }

    #[test]
    fn a_new_part_s_owner_is_judged_before_any_row_is_in_it() {
        // SAFETY: geteuid only reads the process's user id.
        if unsafe { libc::geteuid() } != 0 {
            eprintln!("skipped: only root can give files to other owners");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.tsvz");
        fs::write(&p, "secret\tB-only\n").unwrap();
        give(&p, 65534, 65534, 0o600);
        let m = dir.path().join("s.tsvz.3");
        fs::write(&m, "x\t1\n").unwrap();
        give(&m, 12345, 12345, 0o644);
        fs::write(dir.path().join("s.tsvz.8"), "y\t2\n").unwrap();
        assert!(scrub(&p, &opts()).is_err());
        let checked: Vec<u64> = OWNERS_CHECKED
            .lock()
            .unwrap()
            .iter()
            .filter(|(tmp, _)| tmp.starts_with(dir.path()))
            .map(|(_, len)| *len)
            .collect();
        assert_eq!(checked, [0], "the file uid 12345 owned held no row yet");
    }

    #[test]
    fn a_users_groups_are_found_however_long_their_entry() {
        // A buffer too small for the entry (ERANGE) is grown, not taken for an unknown user.
        let groups = groups_of(0, 8).expect("root is known");
        assert!(groups.contains(&0), "{groups:?}");
        assert_eq!(groups_of(4_000_000_000, 8), None);
    }

    #[test]
    fn two_snapshots_of_one_process_never_share_a_temporary_name() {
        let store = Path::new("/d/s.tsvz");
        let target = Path::new("/d/s.tsvz.5.gz");
        let a = snapshot_temp(store, target, Some("gz"));
        let b = snapshot_temp(store, target, Some("gz"));
        assert_ne!(a, b);
        assert!(a.to_string_lossy().ends_with(".tsvz.gz"), "{}", a.display());
    }

    #[test]
    fn a_snapshot_never_removes_a_file_it_did_not_make() {
        let dir = tempfile::tempdir().unwrap();
        let theirs = dir.path().join("s.snapshot-x.tsvz");
        fs::write(&theirs, "another scrub's\n").unwrap();
        let like = fs::metadata(&theirs).unwrap();
        assert!(new_snapshot(&theirs, &like).is_err());
        assert_eq!(fs::read(&theirs).unwrap(), b"another scrub's\n");
    }

    #[test]
    fn a_copy_of_a_part_appended_to_since_holds_nothing_more() {
        let dir = tempfile::tempdir().unwrap();
        let (old, new) = (dir.path().join("old"), dir.path().join("new"));
        fs::write(&old, "a\t1\n\0\0").unwrap();
        fs::write(&new, "a\t1\n\0\0b\t2\n").unwrap();
        assert!(starts_with_file(&new, &old));
        fs::write(&new, "c\t3\n").unwrap(); // rewritten in place, not appended to
        assert!(!starts_with_file(&new, &old));
    }

    #[test]
    fn a_part_that_stays_missing_is_reported_by_name() {
        // A numbered part that is a dangling link: every attempt finds it missing, and the
        // scrub says so, naming it, rather than that the store kept changing.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.tsvz");
        fs::write(&p, "a\t1\n").unwrap();
        std::os::unix::fs::symlink(dir.path().join("nowhere"), dir.path().join("s.tsvz.1"))
            .unwrap();
        fs::write(dir.path().join("s.tsvz.2"), "b\t2\n").unwrap();
        let err = scrub(&p, &opts()).unwrap_err().to_string();
        assert!(
            err.contains("s.tsvz.1") && !err.contains("kept changing"),
            "{err}"
        );
    }

    #[test]
    fn two_recovery_copies_of_one_part_never_share_a_name() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.tsvz");
        fs::write(&p, "a\t1\n").unwrap();
        let a = write_recovery(&p, &p, b"a\t1\n").unwrap();
        let b = write_recovery(&p, &p, b"a\t1\n").unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn a_rewrite_that_left_a_part_damaged_is_never_taken_for_a_missing_file() {
        let e = Error::io(
            Path::new("s.tsvz"),
            io::Error::new(
                io::ErrorKind::NotFound,
                Damaged("the part is damaged".into()),
            ),
        );
        assert!(damaged(&e) && !gone(&e));
    }

    #[test]
    fn a_part_another_scrub_rotated_already_counts_as_rotated() {
        for action in ["delete", "rename"] {
            let dir = tempfile::tempdir().unwrap();
            let gone = dir.path().join("s.tsvz.1");
            let here = dir.path().join("s.tsvz.2");
            fs::write(&here, "a\t1\n").unwrap();
            let parts = [
                parts::Part {
                    path: gone,
                    ordinal: Some("1".into()),
                    codec: None,
                },
                parts::Part {
                    path: here.clone(),
                    ordinal: Some("2".into()),
                    codec: None,
                },
            ];
            rotate(&parts, action).unwrap();
            assert!(!here.exists(), "{action}");
        }
    }

    #[test]
    fn a_part_gone_under_a_scrub_means_the_parts_changed() {
        let dir = tempfile::tempdir().unwrap();
        let gone_part = parts::Part {
            path: dir.path().join("s.tsvz.1"),
            ordinal: Some("1".into()),
            codec: None,
        };
        let err = metas(&[gone_part]).unwrap_err();
        assert!(gone(&err), "{err}");
        assert!(!gone(&Error::Refused("no".into())));
    }

    #[test]
    fn a_scrub_moves_rows_into_a_part_whose_owner_could_read_them() {
        // SAFETY: geteuid only reads the process's user id.
        if unsafe { libc::geteuid() } != 0 {
            eprintln!("skipped: only root can give files to other owners");
            return;
        }
        // Part 0 belongs to uid 12345 but its group, nobody's own, may read it: nobody, who
        // owns the part the rows go to, could read them before.
        for (middle, active) in [("s.tsvz.1", "s.tsvz.2"), ("s.tsvz.3", "s.tsvz.8")] {
            let dir = tempfile::tempdir().unwrap();
            let p = dir.path().join("s.tsvz");
            fs::write(&p, "shared\tby group\n").unwrap();
            give(&p, 12345, 65534, 0o640);
            let m = dir.path().join(middle);
            fs::write(&m, "x\t1\n").unwrap();
            give(&m, 65534, 65534, 0o644);
            fs::write(dir.path().join(active), "y\t2\n").unwrap();
            assert!(
                matches!(scrub(&p, &opts()), Ok(Outcome::Written)),
                "{middle}"
            );
        }
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
    fn the_rotate_marker_renames_deletes_or_keeps_the_parts_a_snapshot_part_holds() {
        // An ordinal fits between .3 and the active .8: the snapshot is the new part .5, in the
        // codec of the part before it, holding every row of the parts before it.
        let cases = [
            (
                "rename",
                &[
                    "s.tsvz.0.rotated",
                    "s.tsvz.3.rotated.gz",
                    "s.tsvz.5.gz",
                    "s.tsvz.8",
                ][..],
            ),
            ("delete", &["s.tsvz.5.gz", "s.tsvz.8"][..]),
            (
                "keep",
                &["s.tsvz", "s.tsvz.3.gz", "s.tsvz.5.gz", "s.tsvz.8"][..],
            ),
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
            let gz = fs::read(dir.path().join("s.tsvz.5.gz")).unwrap();
            assert_eq!(&gz[..2], b"\x1f\x8b", "{action}");
            let alone = Store::open(dir.path().join("s.tsvz.5.gz"), opts()).unwrap();
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
    fn narrowing_keeps_the_owner_able_to_rewrite_and_others_no_freer() {
        use std::os::unix::fs::PermissionsExt;
        // The part compacted into holds every prefix part's rows: no one but its owner may read
        // or write them more than before; its owner keeps reading and writing it (to rewrite it,
        // and to scrub again), even when old parts were made read-only.
        for (part0, target, after) in [
            (0o444, 0o664, 0o644),
            (0o644, 0o666, 0o644),
            (0o600, 0o644, 0o600),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let p = dir.path().join("s.tsvz");
            fs::write(&p, "a\t1\n").unwrap();
            fs::set_permissions(&p, fs::Permissions::from_mode(part0)).unwrap();
            let t = dir.path().join("s.tsvz.1");
            fs::write(&t, "a\t2\nb\t3\n").unwrap();
            fs::set_permissions(&t, fs::Permissions::from_mode(target)).unwrap();
            fs::write(dir.path().join("s.tsvz.2"), "c\t3\n").unwrap();
            assert_eq!(scrub(&p, &opts()).unwrap(), Outcome::Written);
            let mode = fs::metadata(&t).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, after, "{part0:o} + {target:o} -> {mode:o}");
        }
    }

    #[test]
    fn a_narrowed_mode_is_put_back_only_over_the_old_bytes() {
        // Widening again a part that a failed rewrite left holding new rows would show them.
        let full = || io::Error::new(io::ErrorKind::StorageFull, "no space left");
        let p = Path::new("s.tsvz");
        let kept = replace(p, b"new", b"old", None, |b| {
            if b == b"new" { Err(full()) } else { Ok(()) }
        });
        let lost = replace(p, b"new", b"old", None, |_| Err(full()));
        assert!(put_mode_back(&Ok(None)), "nothing written");
        assert!(
            put_mode_back(&kept.map(|()| Some(true))),
            "the old bytes went back"
        );
        assert!(
            !put_mode_back(&lost.map(|()| Some(true))),
            "damaged: new rows may be there"
        );
        assert!(!put_mode_back(&Ok(Some(true))), "written");
    }

    #[test]
    fn a_part_holding_the_rows_of_a_private_part_is_as_private() {
        use std::os::unix::fs::PermissionsExt;
        // Part 0 is private; the part that ends holding its rows is too: the part compacted
        // into (consecutive ordinals), or the new snapshot part (one fits: .5).
        for (second, active, holder) in [("1", "2", "1"), ("2", "8", "5")] {
            let dir = tempfile::tempdir().unwrap();
            let p = dir.path().join("s.tsvz");
            fs::write(&p, "secret\t1\n").unwrap();
            fs::set_permissions(&p, fs::Permissions::from_mode(0o600)).unwrap();
            let open = dir.path().join(format!("s.tsvz.{second}"));
            fs::write(&open, "b\t2\n").unwrap();
            fs::set_permissions(&open, fs::Permissions::from_mode(0o644)).unwrap();
            fs::write(dir.path().join(format!("s.tsvz.{active}")), "c\t3\n").unwrap();
            scrub(&p, &opts()).unwrap();
            let holder = dir.path().join(format!("s.tsvz.{holder}"));
            let mode = fs::metadata(&holder).unwrap().permissions().mode();
            assert_eq!(
                mode & 0o777,
                0o600,
                "{second}/{active}: mode {:o}",
                mode & 0o777
            );
            assert!(fs::read_to_string(&holder).unwrap().contains("secret"));
        }
    }

    #[test]
    fn a_damaged_prefix_is_backed_up_and_never_deleted() {
        // Recovered damage (a corrupt member under `recover`): `delete` is downgraded to
        // `rename` (§19.5), and a part rewritten in place is copied first, so no damaged byte
        // is destroyed. With a free ordinal (active .5) the snapshot is a new part, .3, and the
        // damaged part is only renamed; with none (active .3), it is compacted into, backed up.
        for (active, snapshot, backup) in [("5", Some("s.tsvz.3"), false), ("3", None, true)] {
            let dir = tempfile::tempdir().unwrap();
            let p = dir.path().join("s.tsvz");
            fs::write(&p, "#_rotate_#\tdelete\na\t1\n").unwrap();
            let mut bad = crate::codec::encode_member(crate::name::Codec::Gzip, b"b\t2\n").unwrap();
            let n = bad.len();
            bad[n - 8] ^= 1; // a CRC mismatch: corrupt, but what it decoded is used
            fs::write(dir.path().join("s.tsvz.2.gz"), &bad).unwrap();
            fs::write(dir.path().join(format!("s.tsvz.{active}")), "c\t3\n").unwrap();
            let before = rows(&p);
            assert_eq!(scrub(&p, &opts()).unwrap(), Outcome::Written, "{active}");
            let names = files(dir.path());
            assert!(names.contains(&"s.tsvz.0.rotated".to_string()), "{names:?}");
            if let Some(part) = snapshot {
                assert!(names.iter().any(|n| n.starts_with(part)), "{names:?}");
                let renamed = dir.path().join("s.tsvz.2.rotated.gz");
                assert_eq!(fs::read(renamed).unwrap(), bad, "kept, renamed");
            }
            let copy = names.iter().find(|n| n.contains(".damaged-"));
            assert_eq!(copy.is_some(), backup, "{names:?}");
            if let Some(copy) = copy {
                assert_eq!(fs::read(dir.path().join(copy)).unwrap(), bad);
            }
            assert_eq!(rows(&p), before, "{active}");
        }
    }

    #[test]
    fn no_recovery_copy_is_left_after_a_scrub() {
        // Nor a snapshot's temporary file: in place (no ordinal free between .4 and .5), as a
        // new part (.6 between .4 and .9).
        for (active, left) in [
            ("5", &["s.tsvz", "s.tsvz.4", "s.tsvz.5"][..]),
            ("9", &["s.tsvz", "s.tsvz.4", "s.tsvz.6", "s.tsvz.9"][..]),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let p = dir.path().join("s.tsvz");
            fs::write(&p, "a\t1\na\t2\n").unwrap();
            scrub(&p, &opts()).unwrap();
            fs::write(dir.path().join("s.tsvz.4"), "b\t2\nb\t3\n").unwrap();
            fs::write(dir.path().join(format!("s.tsvz.{active}")), "c\t3\n").unwrap();
            scrub(&p, &opts()).unwrap();
            assert_eq!(files(dir.path()), left);
        }
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
    fn clear_in_place_refuses_a_part_that_declares_a_newer_spec_version() {
        // Its active markers cannot be kept for a version this tsvz does not know (§19.8).
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.tsvz");
        let long = "9".repeat(5000);
        for text in [
            "#hdr\n#_version_#\t2\na\tx\n".to_string(),
            "#_version_#\t2\na\tx\n#_version_#\t1\nb\ty\n".to_string(),
            format!("#_version_#\t{long}\na\tx\n"),
        ] {
            fs::write(&p, &text).unwrap();
            let e = clear(&p, &opts()).unwrap_err();
            assert!(matches!(e, Error::Refused(_)), "{e}");
            assert_eq!(fs::read_to_string(&p).unwrap(), text, "nothing written");
            assert!(
                matches!(scrub(&p, &opts()), Err(Error::Refused(_))),
                "nor scrubbed"
            );
        }
        // Several parts are cleared by appending tombstones, not in place.
        fs::write(&p, "#_version_#\t2\na\tx\n").unwrap();
        fs::write(dir.path().join("s.tsvz.1"), "b\ty\n").unwrap();
        assert_eq!(clear(&p, &opts()).unwrap(), Outcome::Written);
        assert!(rows(&p).is_empty());
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
    fn every_state_a_rotation_cut_short_can_leave_reads_as_the_store() {
        // The newest earlier part goes first, so what a crash (or a failed unlink) leaves is the
        // oldest parts, read from the start as written, then the compacted part: it must undo
        // what each such prefix leaves live, not only what all of them leave (p1 deletes a).
        // Also: part 0 turns stripping off, so "k " is a key of its own only after it.
        for (p0, p1) in [
            ("a\t1\nb\t2\n", "a\n"),
            ("#_strip_trailing_whites_#\tfalse\n", "k \tv\n"),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let p = dir.path().join("s.tsvz");
            fs::write(&p, p0).unwrap();
            fs::write(dir.path().join("s.tsvz.1"), p1).unwrap();
            fs::write(dir.path().join("s.tsvz.2"), "c\t3\n").unwrap();
            fs::write(dir.path().join("s.tsvz.3"), "z\t9\n").unwrap();
            let before = rows(&p);
            assert_eq!(scrub(&p, &opts()).unwrap(), Outcome::Written);
            for kept in 0..=2 {
                let state = tempfile::tempdir().unwrap();
                let earlier = ["s.tsvz", "s.tsvz.1"];
                for name in earlier[..kept].iter().chain(&["s.tsvz.2", "s.tsvz.3"]) {
                    fs::copy(dir.path().join(name), state.path().join(name)).unwrap();
                }
                let after = rows(&state.path().join("s.tsvz"));
                assert_eq!(after, before, "{p1:?}: {kept} earlier parts kept");
            }
        }
    }

    #[test]
    fn a_rotation_removes_the_newest_earlier_part_first() {
        let dir = tempfile::tempdir().unwrap();
        let part = |name: &str, ordinal: Option<&str>| parts::Part {
            path: dir.path().join(name),
            ordinal: ordinal.map(String::from),
            codec: None,
        };
        fs::create_dir(dir.path().join("s.tsvz")).unwrap(); // cannot be removed as a file
        fs::write(dir.path().join("s.tsvz.1"), "a\n").unwrap();
        let earlier = [part("s.tsvz", None), part("s.tsvz.1", Some("1"))];
        assert!(rotate(&earlier, "delete").is_err());
        assert!(
            !dir.path().join("s.tsvz.1").exists(),
            "the newest went first"
        );
    }

    #[test]
    fn clear_refuses_a_read_torn_by_a_part_rewritten_under_it() {
        // A part that keeps changing while it is read (an in-place scrub) makes the read torn;
        // tombstones for the keys of that read could leave rows live.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.tsvz");
        fs::write(&p, "a\t1\n#_version_#\tx\nb\t2\n").unwrap();
        fs::write(dir.path().join("s.tsvz.1"), "z\t9\n").unwrap();
        let mut options = opts();
        let part = p.clone();
        options.warnings = Some(std::sync::Arc::new(move |w: &Warning| {
            if w.message.contains("#_version_#") {
                let mut f = fs::OpenOptions::new().append(true).open(&part).unwrap();
                std::io::Write::write_all(&mut f, b"#x\n").unwrap(); // changes on every read
            }
        }));
        let e = clear(&p, &options).unwrap_err();
        assert!(matches!(e, Error::Refused(_)), "{e}");
        assert_eq!(
            fs::read_to_string(dir.path().join("s.tsvz.1")).unwrap(),
            "z\t9\n"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_rewrite_without_a_copy_is_only_for_file_systems_that_overwrite_in_place() {
        // A copy-on-write file system (btrfs, ZFS, APFS) needs new blocks even to overwrite:
        // there a full disk can leave a rewrite half done, so the copy is not skipped.
        let dir = tempfile::tempdir().unwrap();
        let tmpfs = std::process::Command::new("stat")
            .args(["-f", "-c", "%T"])
            .arg(dir.path())
            .output();
        let kind = tmpfs.map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
        if let Ok(kind) = kind {
            let expected = matches!(kind.as_str(), "tmpfs" | "ext2/ext3" | "xfs" | "f2fs");
            assert_eq!(overwrites_in_place(dir.path()), expected, "{kind}");
        }
        assert!(!overwrites_in_place(Path::new("/proc")), "procfs");
        assert!(!overwrites_in_place(Path::new("/no/such/dir")), "unknown");
    }

    #[test]
    fn a_key_live_only_after_a_middle_part_is_undone_too() {
        // p1 adds b, p2 deletes it: a rotation cut short after p1 would show b, unless the
        // snapshot part deletes it; the keys of every prefix count, not only the first and all.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.tsvz");
        let earlier = [
            ("s.tsvz", "#_rotate_#\tdelete\na\t1\n"),
            ("s.tsvz.1", "b\t2\n"),
            ("s.tsvz.2", "b\n"),
        ];
        for (name, text) in earlier {
            fs::write(dir.path().join(name), text).unwrap();
        }
        fs::write(dir.path().join("s.tsvz.8"), "z\t9\n").unwrap();
        let before = rows(&p);
        assert_eq!(scrub(&p, &opts()).unwrap(), Outcome::Written);
        assert_eq!(files(dir.path()), ["s.tsvz.5", "s.tsvz.8"]);
        for kept in 0..=3 {
            let state = tempfile::tempdir().unwrap();
            for (name, text) in &earlier[..kept] {
                fs::write(state.path().join(name), text).unwrap();
            }
            for name in ["s.tsvz.5", "s.tsvz.8"] {
                fs::copy(dir.path().join(name), state.path().join(name)).unwrap();
            }
            assert_eq!(rows(&state.path().join("s.tsvz")), before, "{kept} kept");
        }
    }

    #[test]
    fn scrubbing_again_adds_no_copy() {
        // A snapshot part already holding the whole prefix, no larger than a new one would be:
        // a scrub adds nothing (cron could otherwise add a full copy each time). A part with
        // superseded rows is still compacted.
        for action in ["keep", "rename", "delete"] {
            let dir = tempfile::tempdir().unwrap();
            let p = dir.path().join("s.tsvz");
            fs::write(&p, format!("#_rotate_#\t{action}\na\t1\na\t2\nb\t3\nb\n")).unwrap();
            fs::write(dir.path().join("s.tsvz.ff"), "z\t9\n").unwrap();
            let before = rows(&p);
            assert_eq!(scrub(&p, &opts()).unwrap(), Outcome::Written, "{action}");
            let mut outcome = scrub(&p, &opts()).unwrap();
            if outcome == Outcome::Written {
                outcome = scrub(&p, &opts()).unwrap(); // a rotation's restating part shrinks once
            }
            assert_eq!(outcome, Outcome::Unchanged, "{action}");
            let names = files(dir.path());
            let parts = names.iter().filter(|n| !n.contains("rotated")).count();
            assert!(parts <= 3, "{action}: {names:?}");
            assert_eq!(
                scrub(&p, &opts()).unwrap(),
                Outcome::Unchanged,
                "{action}: {names:?}"
            );
            assert_eq!(rows(&p), before, "{action}");
        }
    }

    #[test]
    fn compacting_in_place_for_want_of_an_ordinal_says_so() {
        // Readers that take no lock (TSVZ.py's) can see a part being rewritten in place: the
        // fallback is named, with the way to avoid it; a new snapshot part needs no word.
        for (active, warned) in [("2", true), ("8", false)] {
            let dir = tempfile::tempdir().unwrap();
            let p = dir.path().join("s.tsvz");
            fs::write(&p, "a\t1\n").unwrap();
            fs::write(dir.path().join("s.tsvz.1"), "b\t2\n").unwrap();
            fs::write(dir.path().join(format!("s.tsvz.{active}")), "c\t3\n").unwrap();
            let said = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let log = said.clone();
            let mut options = opts();
            options.warnings = Some(std::sync::Arc::new(move |w: &Warning| {
                log.lock().unwrap().push(w.message.clone())
            }));
            assert_eq!(scrub(&p, &options).unwrap(), Outcome::Written);
            let said = said.lock().unwrap();
            let note = said.iter().find(|m| m.contains("in place"));
            assert_eq!(note.is_some(), warned, "{active}: {said:?}");
            if let Some(note) = note {
                assert!(
                    note.contains("s.tsvz.1") && note.contains("UUIDv7"),
                    "{note}"
                );
            }
        }
    }

    #[test]
    fn a_snapshot_goes_into_a_new_part_when_an_ordinal_fits() {
        // Readers take no lock: the parts they may be reading are never rewritten, the snapshot
        // appears whole under its name (§19.2), or not at all.
        use std::os::unix::fs::MetadataExt;
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.tsvz");
        fs::write(&p, "#h\na\t1\nb\t2\n").unwrap();
        fs::write(dir.path().join("s.tsvz.2"), "a\nc\t3\n").unwrap();
        fs::write(dir.path().join("s.tsvz.8"), "z\t9\n").unwrap();
        let kept = |name: &str| {
            let path = dir.path().join(name);
            (fs::metadata(&path).unwrap().ino(), fs::read(&path).unwrap())
        };
        let (zero, two) = (kept("s.tsvz"), kept("s.tsvz.2"));
        let before = rows(&p);
        assert_eq!(scrub(&p, &opts()).unwrap(), Outcome::Written);
        assert_eq!(
            files(dir.path()),
            ["s.tsvz", "s.tsvz.2", "s.tsvz.5", "s.tsvz.8"]
        );
        assert_eq!(
            (kept("s.tsvz"), kept("s.tsvz.2")),
            (zero, two),
            "nothing rewritten"
        );
        assert_eq!(rows(&p), before);
        let alone = Store::open(dir.path().join("s.tsvz.5"), opts()).unwrap();
        assert_eq!(
            trimmed_rows(&alone),
            trimmed_rows(&Store::open_prefix(&p, opts(), 2).unwrap())
        );
        assert_eq!(alone.header(), Some("#h"));
        // After part 0 alone, an ordinal from 1 up: .1 between it and .2.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.tsvz");
        fs::write(&p, "a\t1\n").unwrap();
        fs::write(dir.path().join("s.tsvz.2"), "z\t9\n").unwrap();
        assert_eq!(scrub(&p, &opts()).unwrap(), Outcome::Written);
        assert_eq!(files(dir.path()), ["s.tsvz", "s.tsvz.1", "s.tsvz.2"]);
    }

    #[test]
    fn every_state_a_rotation_after_a_snapshot_part_can_leave_reads_as_the_store() {
        // Cut short (newest first), a rotation leaves the oldest parts, then the snapshot part.
        for (p0, p2) in [
            ("#_rotate_#\tdelete\na\t1\nb\t2\n", "a\n"),
            (
                "#_rotate_#\trename\n#_strip_trailing_whites_#\tfalse\n",
                "k \tv\n",
            ),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let p = dir.path().join("s.tsvz");
            fs::write(&p, p0).unwrap();
            fs::write(dir.path().join("s.tsvz.2"), p2).unwrap();
            fs::write(dir.path().join("s.tsvz.8"), "z\t9\n").unwrap();
            let originals = [
                fs::read(&p).unwrap(),
                fs::read(dir.path().join("s.tsvz.2")).unwrap(),
            ];
            let before = rows(&p);
            assert_eq!(scrub(&p, &opts()).unwrap(), Outcome::Written);
            for kept in 0..=2 {
                let state = tempfile::tempdir().unwrap();
                for (name, bytes) in ["s.tsvz", "s.tsvz.2"].iter().zip(&originals).take(kept) {
                    fs::write(state.path().join(name), bytes).unwrap();
                }
                for name in ["s.tsvz.5", "s.tsvz.8"] {
                    fs::copy(dir.path().join(name), state.path().join(name)).unwrap();
                }
                let after = rows(&state.path().join("s.tsvz"));
                assert_eq!(after, before, "{p2:?}: {kept} earlier parts kept");
            }
        }
    }

    #[test]
    fn a_rename_rotation_cut_short_is_finished_by_the_next_scrub() {
        // A crash between the link and the unlink leaves both names: the next scrub finishes it
        // rather than fail on "File exists" forever.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.tsvz");
        fs::write(&p, "#_rotate_#\trename\na\t1\n").unwrap();
        fs::write(dir.path().join("s.tsvz.1"), "b\t2\n").unwrap();
        fs::write(dir.path().join("s.tsvz.2"), "c\t3\n").unwrap();
        let before = rows(&p);
        assert_eq!(scrub(&p, &opts()).unwrap(), Outcome::Written);
        fs::hard_link(dir.path().join("s.tsvz.0.rotated"), &p).unwrap();
        assert_eq!(rows(&p), before, "the cut-short state reads the same");
        scrub(&p, &opts()).unwrap();
        assert_eq!(
            files(dir.path()),
            ["s.tsvz.0.rotated", "s.tsvz.1", "s.tsvz.2"]
        );
        assert_eq!(rows(&p), before);
    }

    #[test]
    fn defaults_of_empty_values_only_do_not_refuse_a_two_part_scrub() {
        // The snapshot leaves out a #_defaults_# of empty values (it reads the same: §3.6, §14),
        // so the guard must not take that for a difference (difftest seed 610).
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.tsvz");
        fs::write(&p, "#_defaults_#\t\t\na\t1\n").unwrap();
        fs::write(dir.path().join("s.tsvz.1"), "b\t2\n").unwrap();
        let before = rows(&p);
        assert_eq!(scrub(&p, &opts()).unwrap(), Outcome::Written);
        assert_eq!(rows(&p), before);
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
        let deleted: Vec<String> = earlier().iter().map(|(k, _)| k.to_string()).collect();
        fs::write(&copy, restating_snapshot(&s, &deleted)).unwrap();
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
