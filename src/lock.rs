//! The write lock shared with TSVZ.py: an in-process gate per file, then an exclusive
//! whole-file lock — an open file description lock on Linux (a classic POSIX lock when the
//! kernel refuses it), a classic POSIX lock on macOS. Readers take no lock.
//!
//! A classic POSIX lock belongs to the process and is released when the process closes any
//! descriptor of the file, not only the one it was taken on. So the library opens the files of
//! a store as [`Handle`]s, which this process closes only once it holds no such lock on them;
//! a program embedding the library that opens a part while it appends should do the same.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io;
use std::ops::Deref;
use std::os::fd::AsRawFd;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::sync::{Condvar, Mutex, MutexGuard, OnceLock};

/// What this process does with one file, by device and inode.
#[derive(Default)]
struct Inode {
    /// A thread holds the file's write lock, or is taking it: the in-process gate, since POSIX
    /// record locks do not exclude two descriptors of one process.
    locked: bool,
    /// That lock is, or may become, a classic POSIX lock, which closing any descriptor of the
    /// file releases: the descriptors this process drops meanwhile wait in `waiting`.
    classic: bool,
    waiting: Vec<File>,
}

#[derive(Default)]
struct Table {
    /// The files a thread locks or is locking, listed only meanwhile.
    inodes: HashMap<(u64, u64), Inode>,
    /// Descriptors of no known file (their fstat failed), kept while any lock is classic.
    orphans: Vec<File>,
}

impl Table {
    fn any_classic(&self) -> bool {
        self.inodes.values().any(|i| i.classic)
    }
}

/// The table, and the condition its gates are waited on with. The descriptors of a store's
/// files that this process closes are closed under the table's mutex, and a classic lock is
/// taken only once its file is marked so under the same mutex: a close comes first, or sees
/// the mark and waits for the lock to be released.
fn table() -> (MutexGuard<'static, Table>, &'static Condvar) {
    static TABLE: OnceLock<(Mutex<Table>, Condvar)> = OnceLock::new();
    let (table, cv) = TABLE.get_or_init(Default::default);
    (table.lock().unwrap_or_else(|e| e.into_inner()), cv)
}

/// A file of a store, open in this process. Closing it while the process holds a classic POSIX
/// lock on the file would release that lock, so a handle dropped meanwhile is closed only once
/// the lock is released.
pub struct Handle {
    file: Option<File>,
    id: (u64, u64),
}

impl Handle {
    /// Open `path` with `options`.
    pub fn open(path: &Path, options: &OpenOptions) -> io::Result<Handle> {
        let file = options.open(path)?;
        match file.metadata() {
            Ok(meta) => Ok(Handle {
                id: (meta.dev(), meta.ino()),
                file: Some(file),
            }),
            Err(e) => {
                // Which file it is cannot be told: it waits while any lock is classic.
                let (mut table, _) = table();
                if table.any_classic() {
                    table.orphans.push(file);
                } else {
                    drop(file);
                }
                Err(e)
            }
        }
    }
}

impl Deref for Handle {
    type Target = File;
    fn deref(&self) -> &File {
        self.file
            .as_ref()
            .expect("a handle holds its file until dropped")
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        let Some(file) = self.file.take() else {
            return;
        };
        let (mut table, _) = table();
        match table.inodes.get_mut(&self.id) {
            Some(inode) if inode.classic => inode.waiting.push(file),
            _ => drop(file), // under the mutex: no classic lock is taken meanwhile
        }
    }
}

/// The write lock on one file, held until dropped.
pub struct Guard<'f> {
    file: &'f File,
    ofd: bool,
    id: (u64, u64),
}

/// Take the write lock on `file` (open for writing), whose path is `path`; waits as long as
/// another descriptor holds it, in this process or another.
pub fn lock<'f>(file: &'f File, path: &Path) -> io::Result<Guard<'f>> {
    #[cfg(test)]
    if classic_everywhere() {
        return lock_classic(file, path);
    }
    let _ = path; // the file is told by its device and inode
    lock_as(file, false)
}

/// Whether the tests take classic POSIX locks everywhere, as on macOS
/// (`TSVZ_TEST_CLASSIC_LOCKS`).
#[cfg(test)]
fn classic_everywhere() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("TSVZ_TEST_CLASSIC_LOCKS").is_some())
}

/// [`lock`] with a classic POSIX lock, as macOS takes: for tests.
#[cfg(test)]
pub(crate) fn lock_classic<'f>(file: &'f File, _path: &Path) -> io::Result<Guard<'f>> {
    lock_as(file, true)
}

/// Whether another process would find `path` locked now (a child process tries to lock it).
#[cfg(test)]
pub(crate) fn locked_elsewhere(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: the child only calls async-signal-safe functions (open, fcntl, _exit).
    let pid = unsafe { libc::fork() };
    assert!(pid >= 0, "fork");
    if pid == 0 {
        // SAFETY: as above; `c` and `fl` were made before the fork.
        unsafe {
            let fd = libc::open(c.as_ptr(), libc::O_RDWR);
            let mut fl: libc::flock = std::mem::zeroed();
            fl.l_type = libc::F_WRLCK as _;
            fl.l_whence = libc::SEEK_SET as _;
            let busy = fd < 0 || libc::fcntl(fd, libc::F_SETLK, &fl) != 0;
            libc::_exit(if busy { 1 } else { 0 });
        }
    }
    let mut status = 0;
    // SAFETY: waits for the child made above.
    assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
    assert!(libc::WIFEXITED(status));
    libc::WEXITSTATUS(status) == 1
}

/// [`lock`]: an open file description lock on Linux unless `classic` (or the kernel refuses
/// it), else a classic POSIX lock.
fn lock_as(file: &File, classic: bool) -> io::Result<Guard<'_>> {
    let meta = file.metadata()?;
    let id = (meta.dev(), meta.ino());
    let classic = classic || cfg!(not(target_os = "linux"));
    {
        let (mut table, cv) = table();
        while table.inodes.get(&id).is_some_and(|i| i.locked) {
            table = cv.wait(table).unwrap_or_else(|e| e.into_inner());
        }
        let inode = table.inodes.entry(id).or_default();
        inode.locked = true;
        inode.classic = classic;
    }
    let taken = take(file, id, classic);
    if taken.is_err() {
        unlocked(id);
    }
    taken.map(|ofd| Guard { file, ofd, id })
}

/// Lock `file`, whose entry in the table says `classic`; whether by an open file description
/// lock.
fn take(file: &File, id: (u64, u64), classic: bool) -> io::Result<bool> {
    // SAFETY: an all-zero `flock` is valid; the fields set below describe the whole file.
    let mut fl: libc::flock = unsafe { std::mem::zeroed() };
    fl.l_type = libc::F_WRLCK as _;
    fl.l_whence = libc::SEEK_SET as _;
    #[cfg(target_os = "linux")]
    if !classic {
        match fcntl(file.as_raw_fd(), F_OFD_SETLKW, &fl) {
            Ok(()) => return Ok(true),
            // A kernel or file system without them: a classic lock, marked so first.
            Err(e) if e.raw_os_error() == Some(libc::EINVAL) => {
                if let Some(inode) = table().0.inodes.get_mut(&id) {
                    inode.classic = true;
                }
            }
            Err(e) => return Err(e),
        }
    }
    let _ = (id, classic);
    fcntl(file.as_raw_fd(), libc::F_SETLKW, &fl).map(|()| false)
}

/// The lock on file `id` is released, or was never taken: close what waited for it, under the
/// table's mutex so before another thread can lock the file, and open its gate.
fn unlocked(id: (u64, u64)) {
    let (mut table, cv) = table();
    if let Some(inode) = table.inodes.remove(&id) {
        drop(inode.waiting);
    }
    if !table.any_classic() {
        drop(std::mem::take(&mut table.orphans));
    }
    cv.notify_all();
}

impl Drop for Guard<'_> {
    fn drop(&mut self) {
        let _ = unlock(self.file, self.ofd);
        unlocked(self.id);
    }
}

#[cfg(target_os = "linux")]
const F_OFD_SETLK: libc::c_int = 37;
#[cfg(target_os = "linux")]
const F_OFD_SETLKW: libc::c_int = 38;

/// Release the lock on `file`, an open file description lock when `ofd`.
fn unlock(file: &File, ofd: bool) -> io::Result<()> {
    // SAFETY: an all-zero `flock` is valid; the fields set below describe the whole file.
    let mut fl: libc::flock = unsafe { std::mem::zeroed() };
    fl.l_type = libc::F_UNLCK as _;
    fl.l_whence = libc::SEEK_SET as _;
    #[cfg(target_os = "linux")]
    if ofd {
        return fcntl(file.as_raw_fd(), F_OFD_SETLK, &fl);
    }
    let _ = ofd;
    fcntl(file.as_raw_fd(), libc::F_SETLK, &fl)
}

fn fcntl(fd: libc::c_int, cmd: libc::c_int, fl: &libc::flock) -> io::Result<()> {
    loop {
        // SAFETY: `fd` stays open for the call; `fl` is a valid `flock`.
        if unsafe { libc::fcntl(fd, cmd, fl as *const libc::flock) } == 0 {
            return Ok(());
        }
        let e = io::Error::last_os_error();
        if e.kind() != io::ErrorKind::Interrupted {
            return Err(e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::OpenOptions;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    #[test]
    fn a_classic_lock_outlives_closing_another_descriptor_of_its_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.tsvz");
        std::fs::write(&path, "a\t1\n").unwrap();
        let file = OpenOptions::new().append(true).open(&path).unwrap();
        let guard = lock_classic(&file, &path).unwrap();
        assert!(locked_elsewhere(&path));
        // A reader in this process (a follow) opens the file and is done with it.
        drop(Handle::open(&path, OpenOptions::new().read(true)).unwrap());
        assert!(
            locked_elsewhere(&path),
            "the lock went with the reader's descriptor"
        );
        drop(guard);
        assert!(!locked_elsewhere(&path));
    }

    fn waiting(path: &Path) -> Option<usize> {
        let meta = std::fs::metadata(path).unwrap();
        let id = (meta.dev(), meta.ino());
        table().0.inodes.get(&id).map(|i| i.waiting.len())
    }

    #[test]
    fn handles_dropped_under_a_classic_lock_close_once_it_is_released() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.tsvz");
        std::fs::write(&path, "").unwrap();
        let file = Handle::open(&path, OpenOptions::new().append(true)).unwrap();
        let guard = lock_classic(&file, &path).unwrap();
        for _ in 0..3 {
            drop(Handle::open(&path, OpenOptions::new().read(true)).unwrap());
        }
        assert_eq!(waiting(&path), Some(3));
        drop(guard);
        assert_eq!(
            waiting(&path),
            None,
            "closed, and the file no longer listed"
        );
    }

    #[test]
    fn the_handle_a_lock_was_taken_through_waits_for_the_next_lock_of_its_file() {
        // Thread A releases its lock, thread B locks the file at once, and only then does A
        // drop its handle: B keeps its lock.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.tsvz");
        std::fs::write(&path, "").unwrap();
        let a = Handle::open(&path, OpenOptions::new().append(true)).unwrap();
        drop(lock_classic(&a, &path).unwrap());
        let b = Handle::open(&path, OpenOptions::new().append(true)).unwrap();
        let guard = lock_classic(&b, &path).unwrap();
        drop(a);
        assert!(locked_elsewhere(&path));
        drop(guard);
        assert!(!locked_elsewhere(&path));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn under_an_open_file_description_lock_handles_close_at_once() {
        if classic_everywhere() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.tsvz");
        std::fs::write(&path, "").unwrap();
        let file = Handle::open(&path, OpenOptions::new().append(true)).unwrap();
        let guard = lock(&file, &path).unwrap();
        drop(Handle::open(&path, OpenOptions::new().read(true)).unwrap());
        assert_eq!(waiting(&path), Some(0));
        assert!(locked_elsewhere(&path));
        drop(guard);
    }

    #[test]
    fn a_classic_lock_is_never_lost_to_handles_closed_by_other_threads() {
        // Threads open and drop handles of the file while it is locked and unlocked again: a
        // drop that decided "no lock" just before the lock was taken must not close after it.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.tsvz");
        std::fs::write(&path, "").unwrap();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let droppers: Vec<_> = (0..32)
            .map(|_| {
                let (path, stop) = (path.clone(), stop.clone());
                std::thread::spawn(move || {
                    while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                        drop(Handle::open(&path, OpenOptions::new().read(true)).unwrap());
                    }
                })
            })
            .collect();
        let file = Handle::open(&path, OpenOptions::new().append(true)).unwrap();
        let mut lost = 0;
        for _ in 0..300 {
            let guard = lock_classic(&file, &path).unwrap();
            std::thread::yield_now();
            lost += usize::from(!locked_elsewhere(&path));
            drop(guard);
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        droppers.into_iter().for_each(|t| t.join().unwrap());
        assert_eq!(lost, 0, "locks lost in 300");
    }

    #[test]
    fn two_names_of_one_file_exclude_each_other() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.tsvz.1");
        let other = dir.path().join("s.tsvz.1.rotated"); // a rotation's link, before the unlink
        std::fs::write(&path, "").unwrap();
        std::fs::hard_link(&path, &other).unwrap();
        let order = Arc::new(Mutex::new(Vec::new()));
        let first = Handle::open(&path, OpenOptions::new().append(true)).unwrap();
        let guard = lock_classic(&first, &path).unwrap();
        let (o2, order2) = (other.clone(), order.clone());
        let t = std::thread::spawn(move || {
            let second = Handle::open(&o2, OpenOptions::new().append(true)).unwrap();
            let _g = lock_classic(&second, &o2).unwrap();
            order2.lock().unwrap().push("second");
        });
        std::thread::sleep(Duration::from_millis(150));
        order.lock().unwrap().push("first");
        drop(guard);
        t.join().unwrap();
        assert_eq!(*order.lock().unwrap(), vec!["first", "second"]);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn handles_close_at_once_while_an_open_file_description_lock_is_waited_for() {
        if classic_everywhere() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.tsvz");
        std::fs::write(&path, "").unwrap();
        // Another open file description holds the lock (as another process would).
        let holder = OpenOptions::new().append(true).open(&path).unwrap();
        // SAFETY: an all-zero `flock` is valid; the fields set below describe the whole file.
        let mut fl: libc::flock = unsafe { std::mem::zeroed() };
        fl.l_type = libc::F_WRLCK as _;
        fl.l_whence = libc::SEEK_SET as _;
        fcntl(holder.as_raw_fd(), F_OFD_SETLKW, &fl).unwrap();
        let p = path.clone();
        let waiter = std::thread::spawn(move || {
            let file = Handle::open(&p, OpenOptions::new().append(true)).unwrap();
            drop(lock(&file, &p).unwrap());
        });
        std::thread::sleep(Duration::from_millis(100)); // the waiter is in F_OFD_SETLKW
        drop(Handle::open(&path, OpenOptions::new().read(true)).unwrap());
        assert_eq!(
            waiting(&path),
            Some(0),
            "no descriptor waits for a lock that cannot be lost"
        );
        fl.l_type = libc::F_UNLCK as _;
        fcntl(holder.as_raw_fd(), F_OFD_SETLK, &fl).unwrap();
        waiter.join().unwrap();
    }

    #[test]
    fn two_handles_in_one_process_exclude_each_other() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.tsvz");
        std::fs::write(&path, "").unwrap();
        let order = Arc::new(Mutex::new(Vec::new()));
        let first = OpenOptions::new().append(true).open(&path).unwrap();
        let guard = lock(&first, &path).unwrap();
        let (p2, o2) = (path.clone(), order.clone());
        let t = std::thread::spawn(move || {
            let second = OpenOptions::new().append(true).open(&p2).unwrap();
            let _g = lock(&second, &p2).unwrap();
            o2.lock().unwrap().push("second");
        });
        std::thread::sleep(Duration::from_millis(150));
        order.lock().unwrap().push("first");
        drop(guard);
        t.join().unwrap();
        assert_eq!(*order.lock().unwrap(), vec!["first", "second"]);
    }
}
