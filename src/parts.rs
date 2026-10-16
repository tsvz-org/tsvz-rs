//! A store's parts in replay order (§17).

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use crate::Error;
use crate::name::{self, Codec};

/// One part of a store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Part {
    pub path: PathBuf,
    /// The ordinal as written in the file name; None for the unnumbered part 0 (§17.1).
    pub ordinal: Option<String>,
    pub codec: Option<Codec>,
}

/// What identifies a part's bytes between a read and a later check: inode, size, mtime (ns).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stamp {
    pub ino: u64,
    pub size: u64,
    pub mtime: i128,
}

impl Stamp {
    pub fn of(meta: &std::fs::Metadata) -> Stamp {
        Stamp {
            ino: meta.ino(),
            size: meta.size(),
            mtime: meta.mtime() as i128 * 1_000_000_000 + meta.mtime_nsec() as i128,
        }
    }
}

/// The parts of the store at `store`, in replay order (§17.3): the unnumbered file when it
/// exists, then the numbered parts `<store-without-codec>.<ordinal>[.<codec>]` by ordinal
/// value. `.rotated` parts and unknown codec suffixes are skipped (§19.5). A missing
/// directory gives no parts; a store path that is a directory, or a directory that cannot be
/// listed, is an error.
pub fn list(store: &Path) -> Result<Vec<Part>, Error> {
    let text = store
        .to_str()
        .ok_or_else(|| Error::BadPath(store.to_path_buf()))?;
    let (base, codec) = name::split_codec(text);
    if store.is_dir() {
        let is_a_directory = std::io::Error::from(std::io::ErrorKind::IsADirectory);
        return Err(Error::io(store, is_a_directory));
    }
    if name::format_of(store).is_some_and(|f| !f.strict) {
        // A loose file is one file (§20.2.4): numbered siblings such as rotated logs are not parts.
        return Ok(if store.is_file() {
            vec![Part {
                path: store.to_path_buf(),
                ordinal: None,
                codec,
            }]
        } else {
            Vec::new()
        });
    }
    let base = Path::new(base);
    let file_name = base
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| Error::BadPath(store.to_path_buf()))?;
    let prefix = format!("{file_name}.");
    let parent = base.parent().filter(|p| !p.as_os_str().is_empty());
    let mut numbered = Vec::new();
    let dir = parent.unwrap_or(Path::new("."));
    // A missing directory has no parts; one that cannot be listed is an error, or its numbered
    // parts would be skipped and appends would land in part 0, behind them.
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => Some(entries),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(Error::io(dir, e)),
    };
    if let Some(entries) = entries {
        for entry in entries {
            let entry = entry.map_err(|e| Error::io(dir, e))?;
            let entry_name = entry.file_name();
            let Some(entry_name) = entry_name.to_str() else {
                continue;
            };
            let Some(rest) = entry_name.strip_prefix(&prefix) else {
                continue;
            };
            let Some(n) = name::parse_numbered(rest) else {
                continue;
            };
            if n.rotated {
                continue;
            }
            numbered.push((name::ordinal_key(&n.ordinal), entry_name.to_string(), n));
        }
    }
    numbered.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
    let mut parts = Vec::with_capacity(numbered.len() + 1);
    if store.is_file() {
        parts.push(Part {
            path: store.to_path_buf(),
            ordinal: None,
            codec,
        });
    }
    for (_, entry_name, n) in numbered {
        let path = match parent {
            Some(dir) => dir.join(&entry_name),
            None => PathBuf::from(&entry_name),
        };
        parts.push(Part {
            path,
            ordinal: Some(n.ordinal),
            codec: n.codec,
        });
    }
    Ok(parts)
}

/// The part appends go to: the store's last part in replay order, or the store path itself
/// when the store has no part yet (§17.7, §20.2.2).
pub fn active(store: &Path) -> Result<Part, Error> {
    match list(store)?.pop() {
        Some(last) => Ok(last),
        None => unnumbered(store),
    }
}

/// The unnumbered part 0 of `store`, the store's own path (§17.1).
pub fn unnumbered(store: &Path) -> Result<Part, Error> {
    let text = store
        .to_str()
        .ok_or_else(|| Error::BadPath(store.to_path_buf()))?;
    Ok(Part {
        path: store.to_path_buf(),
        ordinal: None,
        codec: name::split_codec(text).1,
    })
}

/// The codec suffix of a part's file name as it is spelled (`gz`, `GZ`, `zst`, …), if any.
pub fn codec_suffix(part: &Path) -> Option<String> {
    let name = part.file_name()?.to_str()?;
    match name::split_codec(name) {
        (root, Some(_)) => Some(name[root.len() + 1..].to_string()),
        _ => None,
    }
}

/// An ordinal's value (§17.3: hexadecimal, leading zeros insignificant); None past 128 bits.
fn ordinal_value(ordinal: &str) -> Option<u128> {
    let digits = ordinal.trim_start_matches('0');
    if digits.is_empty() {
        return Some(0);
    }
    if digits.len() > 32 {
        return None;
    }
    u128::from_str_radix(digits, 16).ok()
}

/// An ordinal strictly between the last part of a prefix (None: the unnumbered part 0) and the
/// active part, for a snapshot part (§19.2.3): the midpoint, spelled with the active ordinal's
/// width and letter case; None when none is free. After part 0 alone, 0 is never taken: §17.1
/// names a rotated part 0 `<store>.0.rotated`, the name a rotated part `.0` would take too.
pub fn snapshot_ordinal(prefix_last: Option<&str>, active: &str) -> Option<String> {
    let low = match prefix_last {
        Some(last) => ordinal_value(last)?.checked_add(1)?,
        None => 1,
    };
    let high = ordinal_value(active)?.checked_sub(1)?;
    if low > high {
        return None;
    }
    let value = low + (high - low) / 2;
    let width = active.len();
    Some(if active.bytes().any(|b| b.is_ascii_uppercase()) {
        format!("{value:0width$X}")
    } else {
        format!("{value:0width$x}")
    })
}

/// The path of the part `ordinal` of `store`, with the codec `suffix` (§17.2).
pub fn numbered_path(store: &Path, ordinal: &str, suffix: Option<&str>) -> Result<PathBuf, Error> {
    let text = store
        .to_str()
        .ok_or_else(|| Error::BadPath(store.to_path_buf()))?;
    let base = name::split_codec(text).0;
    Ok(PathBuf::from(match suffix {
        Some(s) => format!("{base}.{ordinal}.{s}"),
        None => format!("{base}.{ordinal}"),
    }))
}

/// The path a part takes when it is rotated (§19.5): `.rotated` before its codec suffix, and
/// `.0.rotated` for part 0 (§17.1: `s.tsvz` → `s.tsvz.0.rotated`). Neither is a part of the store.
pub fn rotated_path(part: &Path, part0: bool) -> PathBuf {
    let name = part
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let rotated = if part0 { "0.rotated" } else { "rotated" };
    let renamed = match codec_suffix(part) {
        Some(suffix) => format!(
            "{}.{rotated}.{suffix}",
            &name[..name.len() - suffix.len() - 1]
        ),
        None => format!("{name}.{rotated}"),
    };
    part.with_file_name(renamed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn names(parts: &[Part]) -> Vec<String> {
        parts
            .iter()
            .map(|p| p.path.file_name().unwrap().to_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn a_snapshot_ordinal_lies_between_the_prefix_and_the_active_part() {
        // After the unnumbered part 0 alone, 0 is never taken: §17.1 names a rotated part 0
        // s.tsvz.0.rotated, the name a rotated part .0 would take too.
        assert_eq!(snapshot_ordinal(None, "1"), None);
        assert_eq!(snapshot_ordinal(None, "2").as_deref(), Some("1"));
        assert_eq!(snapshot_ordinal(None, "10").as_deref(), Some("08"));
        assert_eq!(snapshot_ordinal(Some("1"), "5").as_deref(), Some("3"));
        assert_eq!(
            snapshot_ordinal(Some("0"), "10").as_deref(),
            Some("08"),
            "the active width"
        );
        assert_eq!(
            snapshot_ordinal(Some("9"), "B").as_deref(),
            Some("A"),
            "its letter case"
        );
        assert_eq!(snapshot_ordinal(Some("a"), "c").as_deref(), Some("b"));
        for (prefix, active) in [
            (Some("0f"), "10"),
            (Some("1"), "2"),
            (None, "0"),
            (Some("5"), "3"),
        ] {
            assert_eq!(
                snapshot_ordinal(prefix, active),
                None,
                "{prefix:?} {active}"
            );
        }
        // UUIDv7 ordinals: 32 hex digits, 128 bits (§17.6).
        let a = "0192f3c0000070008000000000000000";
        let b = "0192f3c0000070008000000000000004";
        assert_eq!(
            snapshot_ordinal(Some(a), b).as_deref(),
            Some("0192f3c0000070008000000000000002")
        );
        assert_eq!(
            snapshot_ordinal(Some("1"), &format!("1{}", "0".repeat(32))),
            None,
            "33 digits"
        );
        assert_eq!(
            snapshot_ordinal(Some("00000000000000000000000000000000001"), "0003").as_deref(),
            Some("0002")
        );
        let store = std::path::Path::new("d/s.tsvz");
        assert_eq!(
            numbered_path(store, "5", None).unwrap(),
            std::path::Path::new("d/s.tsvz.5")
        );
        assert_eq!(
            numbered_path(std::path::Path::new("d/s.tsvz.gz"), "5", Some("gz")).unwrap(),
            std::path::Path::new("d/s.tsvz.5.gz")
        );
    }

    #[test]
    fn unnumbered_file_first_then_hex_order() {
        let dir = tempfile::tempdir().unwrap();
        for name in [
            "s.tsvz",
            "s.tsvz.10",
            "s.tsvz.f",
            "s.tsvz.002",
            "s.tsvz.B",
            "s.tsvz.1",
        ] {
            fs::write(dir.path().join(name), "").unwrap();
        }
        let parts = list(&dir.path().join("s.tsvz")).unwrap();
        assert_eq!(
            names(&parts),
            [
                "s.tsvz",
                "s.tsvz.1",
                "s.tsvz.002",
                "s.tsvz.B",
                "s.tsvz.f",
                "s.tsvz.10"
            ]
        );
        assert_eq!(parts[0].ordinal, None);
        assert_eq!(parts[2].ordinal.as_deref(), Some("002"));
    }

    #[test]
    fn rotated_unknown_and_other_files_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        for name in [
            "s.tsvz.1",
            "s.tsvz.2.rotated",
            "s.tsvz.3.rotated.gz",
            "s.tsvz.4.GZ",
            "s.tsvz.5.zip",
            "s.tsvz.serve",
            "s.tsvz.damaged-20261012T000000",
            "s.tsvzx.6",
            "t.tsvz.7",
            "s.csvz.8",
        ] {
            fs::write(dir.path().join(name), "").unwrap();
        }
        let parts = list(&dir.path().join("s.tsvz")).unwrap();
        assert_eq!(names(&parts), ["s.tsvz.1", "s.tsvz.4.GZ"]);
        assert_eq!(parts[1].codec, Some(Codec::Gzip));
    }

    #[test]
    fn compressed_store_path_and_its_parts() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["s.tsvz.gz", "s.tsvz.1", "s.tsvz.2.xz"] {
            fs::write(dir.path().join(name), "").unwrap();
        }
        let parts = list(&dir.path().join("s.tsvz.gz")).unwrap();
        assert_eq!(names(&parts), ["s.tsvz.gz", "s.tsvz.1", "s.tsvz.2.xz"]);
        assert_eq!(parts[0].codec, Some(Codec::Gzip));
        assert_eq!(parts[2].codec, Some(Codec::Xz));
    }

    #[test]
    fn missing_store_and_active_part() {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("s.tsvz");
        assert!(list(&store).unwrap().is_empty());
        assert_eq!(active(&store).unwrap().path, store);
        fs::write(dir.path().join("s.tsvz.a"), "").unwrap();
        fs::write(dir.path().join("s.tsvz.9"), "").unwrap();
        assert_eq!(active(&store).unwrap().path, dir.path().join("s.tsvz.a"));
    }

    #[test]
    fn relative_store_path_keeps_relative_part_paths() {
        let parts = list(std::path::Path::new("no-such-dir-xyz/s.tsvz")).unwrap();
        assert!(parts.is_empty());
    }

    #[test]
    fn a_directory_is_not_a_store() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("d.tsvz")).unwrap();
        let err = list(&dir.path().join("d.tsvz")).unwrap_err();
        assert!(err.to_string().contains("directory"), "{err}");
    }

    #[test]
    fn a_directory_that_cannot_be_listed_is_an_error() {
        use std::os::unix::fs::PermissionsExt;
        // SAFETY: geteuid only reads the process's user id.
        if unsafe { libc::geteuid() } == 0 {
            eprintln!("skipped: root can list any directory");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let shut = dir.path().join("shut");
        fs::create_dir(&shut).unwrap();
        fs::write(shut.join("s.tsvz"), "a\t1\n").unwrap();
        fs::write(shut.join("s.tsvz.1"), "b\t2\n").unwrap();
        fs::set_permissions(&shut, fs::Permissions::from_mode(0o300)).unwrap();
        let result = list(&shut.join("s.tsvz"));
        fs::set_permissions(&shut, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(result.is_err(), "{result:?}");
    }

    #[test]
    fn stamp_changes_with_content() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.tsvz");
        fs::write(&p, "a\tx\n").unwrap();
        let one = Stamp::of(&fs::metadata(&p).unwrap());
        fs::write(&p, "a\tx\nb\ty\n").unwrap();
        let two = Stamp::of(&fs::metadata(&p).unwrap());
        assert_eq!(one.ino, two.ino);
        assert_ne!(one, two);
    }

    #[test]
    fn rotated_part_names() {
        let p = |s: &str| std::path::PathBuf::from(s);
        assert_eq!(
            rotated_path(&p("d/s.tsvz.1"), false),
            p("d/s.tsvz.1.rotated")
        );
        assert_eq!(
            rotated_path(&p("d/s.tsvz.1.gz"), false),
            p("d/s.tsvz.1.rotated.gz")
        );
        assert_eq!(
            rotated_path(&p("s.tsvz.1.GZ"), false),
            p("s.tsvz.1.rotated.GZ")
        );
        // Part 0 as §17.1 names it: <store>.0.rotated[.<compression>].
        assert_eq!(rotated_path(&p("s.tsvz"), true), p("s.tsvz.0.rotated"));
        assert_eq!(
            rotated_path(&p("s.tsvz.zst"), true),
            p("s.tsvz.0.rotated.zst")
        );
        assert_eq!(codec_suffix(&p("d/s.tsvz.1.GZ")).as_deref(), Some("GZ"));
        assert_eq!(codec_suffix(&p("d/s.tsvz.1")), None);
        // A rotated part, or a rotated part 0, is no part of the store (§19.5).
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("s.tsvz");
        fs::write(&store, "").unwrap();
        let part = dir.path().join("s.tsvz.0.gz");
        fs::write(&part, "").unwrap();
        assert_eq!(list(&store).unwrap()[1].path, part);
        fs::rename(&part, rotated_path(&part, false)).unwrap();
        assert_eq!(
            list(&store).unwrap().len(),
            1,
            "a rotated part is not loaded"
        );
        fs::rename(&store, rotated_path(&store, true)).unwrap();
        assert!(list(&store).unwrap().is_empty(), "nor is a rotated part 0");
    }
}
