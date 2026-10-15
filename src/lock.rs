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
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};

type Gate = Arc<(Mutex<bool>, Condvar)>;

/// The in-process gate for a file: POSIX record locks do not exclude two handles of one process.
fn gate(path: &Path) -> Gate {
    static GATES: OnceLock<Mutex<HashMap<PathBuf, Gate>>> = OnceLock::new();
    let key = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let mut gates = GATES
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    gates.entry(key).or_default().clone()
}

/// What this process holds on one file, by device and inode: its classic POSIX locks (and the
/// locks being taken, which may turn out classic), and the descriptors whose closing waits for
/// them. A file is listed only while it has a lock.
#[derive(Default)]
struct Held {
    locks: usize,
    waiting: Vec<File>,
}

fn held() -> MutexGuard<'static, HashMap<(u64, u64), Held>> {
    static HELD: OnceLock<Mutex<HashMap<(u64, u64), Held>>> = OnceLock::new();
    HELD.get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// One lock of this process on file `id` is gone: once none is left, close what waited for it.
fn settle(id: (u64, u64)) {
    let waiting = {
        let mut held = held();
        let Some(h) = held.get_mut(&id) else {
            return;
        };
        h.locks -= 1;
        if h.locks > 0 {
            return;
        }
        held.remove(&id).map(|h| h.waiting).unwrap_or_default()
    };
    drop(waiting); // closed outside the table's mutex
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
        let meta = file.metadata()?;
        Ok(Handle {
            id: (meta.dev(), meta.ino()),
            file: Some(file),
        })
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
        let mut held = held();
        match held.get_mut(&self.id) {
            Some(h) => h.waiting.push(file),
            None => {
                drop(held);
                drop(file);
            }
        }
    }
}

fn release(gate: &Gate) {
    let (held, cv) = &**gate;
    *held.lock().unwrap_or_else(|e| e.into_inner()) = false;
    cv.notify_one();
}

/// The write lock on one file, held until dropped.
pub struct Guard<'f> {
    file: &'f File,
    gate: Gate,
    ofd: bool,
    id: (u64, u64),
}

/// Take the write lock on `file` (open for writing), whose path is `path`; waits as long as
/// another handle holds it.
pub fn lock<'f>(file: &'f File, path: &Path) -> io::Result<Guard<'f>> {
    #[cfg(test)]
    if classic_everywhere() {
        return lock_classic(file, path);
    }
    lock_as(file, path, None)
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
pub(crate) fn lock_classic<'f>(file: &'f File, path: &Path) -> io::Result<Guard<'f>> {
    lock_as(file, path, Some(false))
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

/// [`lock`]; `ofd` Some(false) takes a classic POSIX lock.
fn lock_as<'f>(file: &'f File, path: &Path, ofd: Option<bool>) -> io::Result<Guard<'f>> {
    let gate = gate(path);
    {
        let (held, cv) = &*gate;
        let mut held = held.lock().unwrap_or_else(|e| e.into_inner());
        while *held {
            held = cv.wait(held).unwrap_or_else(|e| e.into_inner());
        }
        *held = true;
    }
    let id = match file.metadata() {
        Ok(meta) => (meta.dev(), meta.ino()),
        Err(e) => {
            release(&gate);
            return Err(e);
        }
    };
    // Until the lock turns out an open file description lock, or is released, the handles of
    // the file that this process drops are closed only after it.
    held().entry(id).or_default().locks += 1;
    match file_lock(file, true, ofd) {
        Ok(ofd) => {
            if ofd {
                settle(id);
            }
            Ok(Guard {
                file,
                gate,
                ofd,
                id,
            })
        }
        Err(e) => {
            settle(id);
            release(&gate);
            Err(e)
        }
    }
}

impl Drop for Guard<'_> {
    fn drop(&mut self) {
        let _ = file_lock(self.file, false, Some(self.ofd));
        if !self.ofd {
            settle(self.id);
        }
        release(&self.gate);
    }
}

#[cfg(target_os = "linux")]
const F_OFD_SETLK: libc::c_int = 37;
#[cfg(target_os = "linux")]
const F_OFD_SETLKW: libc::c_int = 38;

/// Lock (`on`) or unlock the whole file. `ofd` says which kind an unlock releases; the result
/// says whether an open file description lock was used.
fn file_lock(file: &File, on: bool, ofd: Option<bool>) -> io::Result<bool> {
    // SAFETY: an all-zero `flock` is valid; the fields set below describe the whole file.
    let mut fl: libc::flock = unsafe { std::mem::zeroed() };
    fl.l_type = (if on { libc::F_WRLCK } else { libc::F_UNLCK }) as _;
    fl.l_whence = libc::SEEK_SET as _;
    let fd = file.as_raw_fd();
    #[cfg(target_os = "linux")]
    if ofd != Some(false) {
        match fcntl(fd, if on { F_OFD_SETLKW } else { F_OFD_SETLK }, &fl) {
            Ok(()) => return Ok(true),
            Err(e) if on && e.raw_os_error() == Some(libc::EINVAL) => {} // a kernel or file system without them
            Err(e) => return Err(e),
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = ofd;
    fcntl(fd, if on { libc::F_SETLKW } else { libc::F_SETLK }, &fl).map(|()| false)
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
        held()
            .get(&(meta.dev(), meta.ino()))
            .map(|h| h.waiting.len())
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
        assert_eq!(waiting(&path), None);
        assert!(locked_elsewhere(&path));
        drop(guard);
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
