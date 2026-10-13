//! The write lock shared with TSVZ.py: an in-process gate per file, then an exclusive
//! whole-file lock — an open file description lock on Linux (a classic POSIX lock when the
//! kernel refuses it), a classic POSIX lock on macOS. Readers take no lock.

use std::collections::HashMap;
use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, OnceLock};

type Gate = Arc<(Mutex<bool>, Condvar)>;

/// The in-process gate for a file: POSIX record locks do not exclude two handles of one process.
fn gate(path: &Path) -> Gate {
    static GATES: OnceLock<Mutex<HashMap<PathBuf, Gate>>> = OnceLock::new();
    let key = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let mut gates = GATES.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
    gates.entry(key).or_default().clone()
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
}

/// Take the write lock on `file` (open for writing), whose path is `path`; waits as long as
/// another handle holds it.
pub fn lock<'f>(file: &'f File, path: &Path) -> io::Result<Guard<'f>> {
    let gate = gate(path);
    {
        let (held, cv) = &*gate;
        let mut held = held.lock().unwrap_or_else(|e| e.into_inner());
        while *held {
            held = cv.wait(held).unwrap_or_else(|e| e.into_inner());
        }
        *held = true;
    }
    match file_lock(file, true, None) {
        Ok(ofd) => Ok(Guard { file, gate, ofd }),
        Err(e) => {
            release(&gate);
            Err(e)
        }
    }
}

impl Drop for Guard<'_> {
    fn drop(&mut self) {
        let _ = file_lock(self.file, false, Some(self.ofd));
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
