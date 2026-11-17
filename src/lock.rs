//! The write lock shared with TSVZ.py: an exclusive lock on the whole file that belongs to the
//! open file description, of the kind TSVZ.py 5.3 and later take: an open file description lock
//! on Linux, `flock` elsewhere. It excludes every lock TSVZ.py takes on the store file (on Linux
//! its appends lock byte 1, its readers byte 0), so an append here also waits for its readers.
//! Closing another descriptor of the file never releases it, and two descriptors exclude each
//! other in one process as in two. Where the kernel or the file system refuses it (Linux before
//! 3.15), a classic POSIX lock, behind an in-process gate: the process loses that one when it
//! closes any descriptor of the file (docs/limitations.md). Readers take no lock.

use std::collections::HashSet;
use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::MetadataExt;
use std::sync::{Condvar, Mutex, MutexGuard, OnceLock};

/// The write lock on one file, held until dropped.
pub struct Guard<'f> {
    file: &'f File,
    /// The file (device and inode) of a classic POSIX lock, in the in-process gate; None for
    /// the open file description's own lock.
    classic: Option<(u64, u64)>,
}

type Held = HashSet<(u64, u64)>;

/// The files this process holds a classic lock on: POSIX record locks do not exclude two
/// descriptors of one process.
fn gate() -> (MutexGuard<'static, Held>, &'static Condvar) {
    static GATE: OnceLock<(Mutex<Held>, Condvar)> = OnceLock::new();
    let (held, cv) = GATE.get_or_init(Default::default);
    (held.lock().unwrap_or_else(|e| e.into_inner()), cv)
}

/// Take the write lock on `file` (open for writing); waits as long as another descriptor holds
/// it, in this process or another. With the open file description lock or `flock`, a second
/// call on the same open file description (a `try_clone` of the file, or one `File` used by two
/// threads) is not another descriptor: the lock is granted at once, and the first guard's drop
/// releases it for both. With the classic fallback the second call waits at the in-process gate
/// until the first guard is dropped. Nothing in the crate does either; each caller opens its own
/// `File`.
pub fn lock(file: &File) -> io::Result<Guard<'_>> {
    #[cfg(test)]
    let _waiting = Waiting::on(file)?;
    if own(file, true)? {
        return Ok(Guard {
            file,
            classic: None,
        });
    }
    lock_classic(file)
}

/// [`lock`] by a classic POSIX lock, behind the in-process gate.
pub(crate) fn lock_classic(file: &File) -> io::Result<Guard<'_>> {
    let meta = file.metadata()?;
    let id = (meta.dev(), meta.ino());
    let (mut held, cv) = gate();
    while held.contains(&id) {
        held = cv.wait(held).unwrap_or_else(|e| e.into_inner());
    }
    held.insert(id);
    drop(held);
    let guard = Guard {
        file,
        classic: Some(id),
    };
    // Dropped on failure: the gate is opened again.
    fcntl(
        file.as_raw_fd(),
        libc::F_SETLKW,
        libc::F_WRLCK as libc::c_int,
    )?;
    Ok(guard)
}

impl Drop for Guard<'_> {
    fn drop(&mut self) {
        let Some(id) = self.classic else {
            let _ = own(self.file, false);
            return;
        };
        let _ = fcntl(
            self.file.as_raw_fd(),
            libc::F_SETLK,
            libc::F_UNLCK as libc::c_int,
        );
        let (mut held, cv) = gate();
        held.remove(&id);
        cv.notify_all();
    }
}

#[cfg(target_os = "linux")]
const F_OFD_SETLK: libc::c_int = 37;
#[cfg(target_os = "linux")]
const F_OFD_SETLKW: libc::c_int = 38;

/// Lock (`on`) or unlock the whole file by its open file description: an open file description
/// lock on Linux, `flock` elsewhere. Ok(false) when the kernel or file system has no such lock.
fn own(file: &File, on: bool) -> io::Result<bool> {
    #[cfg(target_os = "linux")]
    {
        let (cmd, kind) = if on {
            (F_OFD_SETLKW, libc::F_WRLCK as libc::c_int)
        } else {
            (F_OFD_SETLK, libc::F_UNLCK as libc::c_int)
        };
        match fcntl(file.as_raw_fd(), cmd, kind) {
            Ok(()) => Ok(true),
            Err(e) if e.raw_os_error() == Some(libc::EINVAL) => Ok(false), // before Linux 3.15
            Err(e) => Err(e),
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let op = if on { libc::LOCK_EX } else { libc::LOCK_UN };
        loop {
            // SAFETY: flock only operates on the descriptor, which `file` keeps open.
            if unsafe { libc::flock(file.as_raw_fd(), op) } == 0 {
                return Ok(true);
            }
            let e = io::Error::last_os_error();
            match e.raw_os_error() {
                Some(libc::EINTR) => {}
                // A file system without flock, as TSVZ.py tells it.
                Some(n) if n == libc::ENOTSUP || n == libc::EOPNOTSUPP || n == libc::EINVAL => {
                    return Ok(false);
                }
                _ => return Err(e),
            }
        }
    }
}

fn fcntl(fd: libc::c_int, cmd: libc::c_int, kind: libc::c_int) -> io::Result<()> {
    // SAFETY: an all-zero `flock` is valid; the fields set below describe the whole file.
    let mut fl: libc::flock = unsafe { std::mem::zeroed() };
    fl.l_type = kind as _;
    fl.l_whence = libc::SEEK_SET as _;
    loop {
        // SAFETY: `fd` stays open for the call; `fl` is a valid `flock`.
        if unsafe { libc::fcntl(fd, cmd, &fl as *const libc::flock) } == 0 {
            return Ok(());
        }
        let e = io::Error::last_os_error();
        if e.kind() != io::ErrorKind::Interrupted {
            return Err(e);
        }
    }
}

/// The files threads are in [`lock`] for, one entry each: for tests.
#[cfg(test)]
static WAITING: Mutex<Vec<(u64, u64)>> = Mutex::new(Vec::new());

#[cfg(test)]
struct Waiting((u64, u64));

#[cfg(test)]
impl Waiting {
    fn on(file: &File) -> io::Result<Waiting> {
        let meta = file.metadata()?;
        let id = (meta.dev(), meta.ino());
        WAITING.lock().unwrap().push(id);
        Ok(Waiting(id))
    }
}

#[cfg(test)]
impl Drop for Waiting {
    fn drop(&mut self) {
        let mut w = WAITING.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(i) = w.iter().position(|id| *id == self.0) {
            w.swap_remove(i);
        }
    }
}

/// How many threads are inside [`lock`] for `path`, waiting for it (the holder has left it): for
/// tests.
#[cfg(test)]
pub(crate) fn waiters_on(path: &std::path::Path) -> usize {
    let meta = std::fs::metadata(path).unwrap();
    let id = (meta.dev(), meta.ino());
    WAITING.lock().unwrap().iter().filter(|w| **w == id).count()
}

/// Whether another process would find `path` locked now (a child process tries to lock it): for
/// tests. Panics when the child cannot open the file, which says nothing about its lock.
#[cfg(test)]
pub(crate) fn locked_elsewhere(path: &std::path::Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: the child only calls async-signal-safe functions (open, fcntl, _exit).
    let pid = unsafe { libc::fork() };
    assert!(pid >= 0, "fork");
    if pid == 0 {
        // SAFETY: as above; `c` was made before the fork, and `fl` is a plain struct.
        unsafe {
            let fd = libc::open(c.as_ptr(), libc::O_RDWR);
            if fd < 0 {
                libc::_exit(2);
            }
            let mut fl: libc::flock = std::mem::zeroed();
            fl.l_type = libc::F_WRLCK as _;
            fl.l_whence = libc::SEEK_SET as _;
            libc::_exit(if libc::fcntl(fd, libc::F_SETLK, &fl) != 0 {
                1
            } else {
                0
            });
        }
    }
    let mut status = 0;
    // SAFETY: waits for the child made above.
    assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
    assert!(libc::WIFEXITED(status));
    match libc::WEXITSTATUS(status) {
        0 => false,
        1 => true,
        n => panic!("the child could not open {}: {n}", path.display()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::OpenOptions;
    use std::path::Path;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    /// Lock `second` on a thread while `first` holds `guard`: the thread gets it only once the
    /// guard is dropped. Waits until the thread is counted as waiting, not for a time that a
    /// busy host may not give it; a lock that does not exclude lets it through at once.
    fn excludes(
        guard: Guard<'_>,
        second: &Path,
        take: fn(&File) -> io::Result<Guard<'_>>,
    ) -> Vec<&'static str> {
        let order = Arc::new(Mutex::new(Vec::new()));
        let (p2, o2) = (second.to_path_buf(), order.clone());
        let t = std::thread::spawn(move || {
            let file = OpenOptions::new().append(true).open(&p2).unwrap();
            let waiting = Waiting::on(&file).unwrap(); // `lock_classic` does not count itself
            let _g = take(&file).unwrap();
            drop(waiting);
            o2.lock().unwrap().push("second");
        });
        // 10 s: how long the other tests give a thread to get going.
        let deadline = Instant::now() + Duration::from_secs(10);
        while waiters_on(second) == 0 && !t.is_finished() {
            assert!(
                Instant::now() < deadline,
                "the thread never came to the lock"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        // Counted, it is a few instructions from blocking; a lock that let it through would
        // have it done well within this.
        std::thread::sleep(Duration::from_millis(50));
        order.lock().unwrap().push("first");
        drop(guard);
        t.join().unwrap();
        order.lock().unwrap().clone()
    }

    #[test]
    fn two_descriptors_in_one_process_exclude_each_other() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.tsvz");
        std::fs::write(&path, "").unwrap();
        let first = OpenOptions::new().append(true).open(&path).unwrap();
        let guard = lock(&first).unwrap();
        assert_eq!(excludes(guard, &path, lock), ["first", "second"]);
    }

    #[test]
    fn the_lock_outlives_closing_another_descriptor_of_its_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.tsvz");
        std::fs::write(&path, "").unwrap();
        let file = OpenOptions::new().append(true).open(&path).unwrap();
        let guard = lock(&file).unwrap();
        if guard.classic.is_some() {
            return; // no open file description lock here: a classic one goes with any close
        }
        drop(File::open(&path).unwrap());
        assert!(locked_elsewhere(&path), "the lock went with the close");
        drop(guard);
        assert!(!locked_elsewhere(&path));
    }

    #[test]
    fn the_classic_fallback_excludes_other_processes_and_threads_by_file() {
        // Where the kernel refuses the description's own lock: the gate stands in for the
        // exclusion a classic lock does not give within one process, by file, under any name.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.tsvz.1");
        let other = dir.path().join("s.tsvz.1.rotated"); // a rotation's link, before the unlink
        std::fs::write(&path, "").unwrap();
        std::fs::hard_link(&path, &other).unwrap();
        let first = OpenOptions::new().append(true).open(&path).unwrap();
        let guard = lock_classic(&first).unwrap();
        assert!(locked_elsewhere(&path));
        assert_eq!(excludes(guard, &other, lock_classic), ["first", "second"]);
        assert!(!locked_elsewhere(&path), "released");
    }
}
