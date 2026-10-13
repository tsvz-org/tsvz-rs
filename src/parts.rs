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
/// directory gives no parts.
pub fn list(store: &Path) -> Result<Vec<Part>, Error> {
    let text = store
        .to_str()
        .ok_or_else(|| Error::BadPath(store.to_path_buf()))?;
    let (base, codec) = name::split_codec(text);
    let base = Path::new(base);
    let file_name = base
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| Error::BadPath(store.to_path_buf()))?;
    let prefix = format!("{file_name}.");
    let parent = base.parent().filter(|p| !p.as_os_str().is_empty());
    let mut numbered = Vec::new();
    if let Ok(entries) = std::fs::read_dir(parent.unwrap_or(Path::new("."))) {
        for entry in entries.flatten() {
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
    let mut parts = list(store)?;
    Ok(match parts.pop() {
        Some(last) => last,
        None => {
            let text = store
                .to_str()
                .ok_or_else(|| Error::BadPath(store.to_path_buf()))?;
            Part {
                path: store.to_path_buf(),
                ordinal: None,
                codec: name::split_codec(text).1,
            }
        }
    })
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
}
