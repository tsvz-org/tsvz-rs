//! The pointer file of a store (§21.2): how a handler announces itself, and how a client decides
//! whether to use it (§21.2.4).

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::handler::protocol::{decode, field};
use crate::parts;

/// The first line of a pointer file: its format and protocol version (§21.2.2).
pub const MAGIC: &str = "tsvz-handler\t1";
/// A larger file is not a pointer file.
const SIZE_LIMIT: u64 = 64 << 10;

/// The pointer file of `store`: the store path as given, plus `.serve` (§21.2.1).
pub fn path_of(store: &Path) -> PathBuf {
    let mut path = store.as_os_str().to_owned();
    path.push(".serve");
    PathBuf::from(path)
}

/// This host's name, as pointer files name it (§21.2.2).
pub fn hostname() -> String {
    let mut buf = [0u8; 256];
    // SAFETY: gethostname writes at most buf.len() bytes into buf.
    let rc = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) };
    if rc != 0 {
        return String::new();
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end]).into_owned()
}

/// Take `flock(LOCK_EX|LOCK_NB)` on `file`: false when another open file holds it.
fn try_flock(file: &File) -> io::Result<bool> {
    // SAFETY: flock only operates on the descriptor, which `file` keeps open.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        return Ok(true);
    }
    let e = io::Error::last_os_error();
    if e.kind() == io::ErrorKind::WouldBlock {
        Ok(false)
    } else {
        Err(e)
    }
}

/// Whether `path` still names the open `file` (no link is followed).
fn names(path: &Path, file: &File) -> bool {
    match (std::fs::symlink_metadata(path), file.metadata()) {
        (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
        _ => false,
    }
}

/// Open a pointer file for reading without following a link or blocking on a FIFO.
fn open_pointer(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
}

/// Read a pointer file: Ok(None) when there is none, its fields (None when it is not a pointer
/// file of protocol 1) and metadata, or why the file there cannot be one.
#[allow(clippy::type_complexity)]
fn read(
    path: &Path,
) -> Result<Option<(Option<HashMap<String, String>>, std::fs::Metadata)>, String> {
    let mut file = match open_pointer(path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{} is not a regular file ({e})", path.display())),
    };
    let meta = file
        .metadata()
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if !meta.is_file() {
        return Err(format!("{} is not a regular file", path.display()));
    }
    let mut data = Vec::new();
    Read::by_ref(&mut file)
        .take(SIZE_LIMIT + 1)
        .read_to_end(&mut data)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if data.len() as u64 > SIZE_LIMIT {
        return Err(format!(
            "{} is too large to be a pointer file",
            path.display()
        ));
    }
    let text = String::from_utf8_lossy(&data);
    let mut lines = text.split('\n');
    if lines.next() != Some(MAGIC) {
        return Ok(Some((None, meta)));
    }
    let fields = lines
        .filter_map(|line| line.split_once('\t'))
        .map(|(name, value)| (name.to_string(), decode(value)))
        .collect();
    Ok(Some((Some(fields), meta)))
}

/// What a trusted pointer file says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Info {
    /// `unix:PATH`.
    pub address: String,
    pub host: String,
    pub pid: String,
    /// The uid owning the pointer file: the socket and the handler must run as it.
    pub owner: u32,
}

/// The uid owning `store` (its first part), if it has one.
fn store_owner(store: &Path) -> Option<u32> {
    let list = parts::list(store).ok()?;
    list.iter()
        .find_map(|p| std::fs::metadata(&p.path).ok().map(|m| m.uid()))
}

/// The pointer file of `store`, checked as §21.2.4 says: Ok(None) when there is no usable
/// pointer file and nothing to report (none, not of this protocol, or incomplete); Err with why
/// one that exists is not trusted (a client warns, §20.8.2).
pub fn lookup(store: &Path) -> Result<Option<Info>, String> {
    let path = path_of(store);
    let mut found = None;
    for attempt in 0..2 {
        if attempt > 0 {
            std::thread::sleep(Duration::from_millis(50)); // a handler may be writing it
        }
        match read(&path)? {
            None => return Ok(None),
            Some((Some(fields), meta))
                if fields.contains_key("address") && fields.contains_key("host") =>
            {
                found = Some((fields, meta));
                break;
            }
            Some(_) => continue,
        }
    }
    let Some((fields, meta)) = found else {
        return Ok(None);
    };
    // SAFETY: geteuid only reads the process's user id.
    let me = unsafe { libc::geteuid() };
    if meta.uid() != me && Some(meta.uid()) != store_owner(store) {
        return Err(format!(
            "{} is owned by uid {}, neither you nor the owner of the store",
            path.display(),
            meta.uid()
        ));
    }
    if meta.mode() & 0o022 != 0 {
        return Err(format!(
            "{} is writable by others than its owner",
            path.display()
        ));
    }
    if meta.nlink() != 1 {
        // Its owner wrote it, but not necessarily here: anyone may link a file they can see.
        return Err(format!(
            "{} has another name (a hard link), so its owner may not have put it there",
            path.display()
        ));
    }
    if let Some(name) = fields
        .iter()
        .find(|(_, value)| value.chars().any(|c| (c as u32) < 32))
        .map(|(name, _)| name)
    {
        return Err(format!(
            "{} has a control character in its {name}",
            path.display()
        ));
    }
    let address = fields["address"].clone();
    if !address.starts_with("unix:") {
        return Err(format!(
            "{} names {address}, which is not a Unix-domain socket",
            path.display()
        ));
    }
    Ok(Some(Info {
        address,
        host: fields["host"].clone(),
        pid: fields.get("pid").cloned().unwrap_or_default(),
        owner: meta.uid(),
    }))
}

/// Why a trusted pointer's handler cannot be used; `quiet` when that is expected (another host,
/// no permission to connect), so a client need not warn.
#[derive(Debug)]
pub struct Unreachable {
    pub quiet: bool,
    pub message: String,
}

fn unreachable(message: String) -> Unreachable {
    Unreachable {
        quiet: false,
        message,
    }
}

/// Connect to the socket at `path`, waiting at most `patience` while its listen queue is full: a
/// handler that does not accept (stuck, or stopped by a signal) is given up on, not waited for
/// forever, as a blocking `connect` would.
fn connect_within(path: &Path, patience: Duration) -> io::Result<UnixStream> {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;
    let bytes = path.as_os_str().as_bytes();
    // SAFETY: sockaddr_un is plain data, for which all zeroes is a valid value.
    let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    if bytes.len() >= addr.sun_path.len() || bytes.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the path is too long for a socket",
        ));
    }
    addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (to, from) in addr.sun_path.iter_mut().zip(bytes) {
        *to = *from as libc::c_char;
    }
    let len = std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t;
    #[cfg(target_vendor = "apple")]
    {
        addr.sun_len = len as u8;
    }
    // Close-on-exec as the socket is made, where the system can (a child that another thread
    // spawns meanwhile would keep the connection open); else right after, checked.
    #[cfg(not(target_vendor = "apple"))]
    let kind = libc::SOCK_STREAM | libc::SOCK_CLOEXEC;
    #[cfg(target_vendor = "apple")]
    let kind = libc::SOCK_STREAM;
    // SAFETY: socket makes a new descriptor, owned below.
    let fd = unsafe { libc::socket(libc::AF_UNIX, kind, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fd is a new descriptor that nothing else owns.
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    let raw = fd.as_raw_fd();
    // SAFETY: fcntl sets the descriptor flags of the descriptor `fd` owns.
    #[cfg(target_vendor = "apple")]
    if unsafe { libc::fcntl(raw, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fcntl reads the file status flags of the descriptor `fd` owns.
    let flags = unsafe { libc::fcntl(raw, libc::F_GETFL) };
    // SAFETY: as above.
    if flags < 0 || unsafe { libc::fcntl(raw, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    let deadline = Instant::now() + patience;
    loop {
        // SAFETY: connect reads `len` bytes of `addr`, a sockaddr_un.
        let r = unsafe { libc::connect(raw, (&raw const addr).cast::<libc::sockaddr>(), len) };
        if r == 0 {
            break;
        }
        let e = io::Error::last_os_error();
        match e.raw_os_error() {
            Some(libc::EINTR) => {}
            Some(libc::EAGAIN) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10)); // the listen queue is full
            }
            Some(libc::EAGAIN) => {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "the handler is not accepting connections",
                ));
            }
            Some(libc::EINPROGRESS) => {
                let mut poll = libc::pollfd {
                    fd: raw,
                    events: libc::POLLOUT,
                    revents: 0,
                };
                let wait = deadline.saturating_duration_since(Instant::now());
                // SAFETY: poll reads and writes the one pollfd entry.
                if unsafe { libc::poll(&mut poll, 1, wait.as_millis() as libc::c_int) } != 1 {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "the handler is not accepting connections",
                    ));
                }
                let mut error: libc::c_int = 0;
                let mut size = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
                // SAFETY: getsockopt writes one c_int and its size.
                unsafe {
                    libc::getsockopt(
                        raw,
                        libc::SOL_SOCKET,
                        libc::SO_ERROR,
                        (&raw mut error).cast(),
                        &mut size,
                    )
                };
                if error != 0 {
                    return Err(io::Error::from_raw_os_error(error));
                }
                break;
            }
            _ => return Err(e),
        }
    }
    // SAFETY: as above: the connection blocks again, as callers expect.
    if unsafe { libc::fcntl(raw, libc::F_SETFL, flags) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(UnixStream::from(fd))
}

/// The uid of the process on the other end of `stream`.
pub fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
    #[cfg(target_os = "linux")]
    {
        let mut cred = libc::ucred {
            pid: 0,
            uid: 0,
            gid: 0,
        };
        let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        // SAFETY: getsockopt writes at most `len` bytes into `cred`.
        let rc = unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&mut cred as *mut libc::ucred).cast(),
                &mut len,
            )
        };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(cred.uid)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let (mut uid, mut gid) = (0, 0);
        // SAFETY: getpeereid writes the two ids.
        if unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(uid)
    }
}

/// Connect to the handler a trusted pointer names (§21.2.4): on this host, through a socket
/// owned by the pointer's owner, to a process running as that user. `timeout` limits each
/// answer.
pub fn connect(info: &Info, timeout: Option<Duration>) -> Result<UnixStream, Unreachable> {
    if info.host != hostname() {
        return Err(Unreachable {
            quiet: true,
            message: format!("it runs on host {}", info.host),
        });
    }
    let path = Path::new(&info.address["unix:".len()..]);
    let meta = std::fs::symlink_metadata(path)
        .map_err(|e| unreachable(format!("{}: {e}", path.display())))?;
    if !meta.file_type().is_socket() {
        return Err(unreachable(format!("{} is not a socket", path.display())));
    }
    if meta.uid() != info.owner {
        return Err(unreachable(format!(
            "the socket {} is owned by uid {}, not by the owner of the pointer file",
            path.display(),
            meta.uid()
        )));
    }
    let stream = connect_within(path, Duration::from_millis(500)).map_err(|e| Unreachable {
        quiet: e.kind() == io::ErrorKind::PermissionDenied,
        message: format!("{}: {e}", path.display()),
    })?;
    let uid = peer_uid(&stream).map_err(|e| unreachable(format!("{}: {e}", path.display())))?;
    if uid != info.owner {
        return Err(unreachable(format!(
            "the process behind {} runs as uid {uid}, not as the owner of the pointer file",
            path.display()
        )));
    }
    let _ = stream.set_read_timeout(timeout);
    Ok(stream)
}

/// Why a handler could not take the pointer file.
#[derive(Debug)]
pub enum Take {
    /// Another handler holds it; what its pointer file says (pid, host, …).
    Busy(HashMap<String, String>),
    /// Anything else: a path that is not a regular file, a file system without locks.
    Other(String),
}

fn busy(path: &Path) -> Take {
    Take::Busy(
        read(path)
            .ok()
            .flatten()
            .and_then(|r| r.0)
            .unwrap_or_default(),
    )
}

/// The locked pointer file of a running handler (§21.2.3).
pub struct Held {
    file: File,
    path: PathBuf,
}

/// Take over the pointer file of `store` (§21.2.3): busy when another handler holds its lock;
/// a stale one is removed, never written into, and a new one created exclusively without
/// following a link, mode 0600 until it is published.
pub fn take(store: &Path) -> Result<Held, Take> {
    let path = path_of(store);
    let other = |e: io::Error| Take::Other(format!("{}: {e}", path.display()));
    let old = match open_pointer(&path) {
        Ok(file) => Some(file),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => {
            return Err(Take::Other(format!(
                "{} is not a regular file ({e}); remove it",
                path.display()
            )));
        }
    };
    if let Some(old) = &old {
        if !old.metadata().map_err(other)?.is_file() {
            return Err(Take::Other(format!(
                "{} is not a regular file; remove it",
                path.display()
            )));
        }
        if !try_flock(old).map_err(other)? {
            return Err(busy(&path));
        }
        if names(&path, old) {
            std::fs::remove_file(&path).map_err(other)?;
        }
    }
    let file = match OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path)
    {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => return Err(busy(&path)),
        Err(e) => return Err(other(e)),
    };
    drop(old);
    match try_flock(&file) {
        Ok(true) if names(&path, &file) => Ok(Held { file, path }),
        Ok(_) => Err(busy(&path)),
        Err(e) => {
            let _ = std::fs::remove_file(&path);
            Err(other(e))
        }
    }
}

impl Held {
    /// Write the pointer for a handler at `address` (`unix:PATH`): readable by all, writable
    /// by its owner only (§21.2.3), and synced.
    pub fn publish(&mut self, address: &str) -> io::Result<()> {
        let text = format!(
            "{MAGIC}\naddress\t{}\nhost\t{}\npid\t{}\n",
            field(address, false),
            field(&hostname(), false),
            std::process::id()
        );
        self.file
            .set_permissions(std::fs::Permissions::from_mode(0o644))?;
        self.file.set_len(0)?;
        self.file.rewind()?;
        self.file.write_all(text.as_bytes())?;
        self.file.sync_all()
    }

    /// Remove the pointer file if its path still names this handler's file, and unlock it.
    pub fn release(self) {
        if names(&self.path, &self.file) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::net::UnixListener;

    /// A socket at `path` listening with a backlog of 1 and never accepting: a stuck handler.
    /// (Not 0: macOS reads a backlog of 0 as the system maximum.)
    fn listen_without_room(path: &Path) -> std::os::fd::OwnedFd {
        use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
        let std = UnixListener::bind(path).unwrap();
        // SAFETY: listen only changes the backlog of the socket that `std` owns.
        assert_eq!(unsafe { libc::listen(std.as_raw_fd(), 1) }, 0);
        // SAFETY: into_raw_fd hands over the descriptor, which the OwnedFd now owns.
        unsafe { OwnedFd::from_raw_fd(std::os::fd::IntoRawFd::into_raw_fd(std)) }
    }

    #[test]
    fn a_handler_that_does_not_accept_is_given_up_on_not_waited_for() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s");
        let _listener = listen_without_room(&path);
        for _ in 0..8 {
            // Fill the queue (Linux takes the backlog + 1, macOS up to 3/2 of it): a filler keeps
            // its connection queued, or blocks (Linux) or is refused (macOS), and is left behind.
            let path = path.clone();
            std::thread::spawn(move || {
                let _queued = UnixStream::connect(path);
                loop {
                    std::thread::park();
                }
            });
        }
        std::thread::sleep(Duration::from_millis(200));
        let info = Info {
            address: format!("unix:{}", path.display()),
            host: hostname(),
            pid: String::new(),
            // SAFETY: geteuid only reads the process's user id.
            owner: unsafe { libc::geteuid() },
        };
        let (done, outcome) = std::sync::mpsc::channel();
        let started = Instant::now();
        std::thread::spawn(move || {
            let _ = done.send(connect(&info, None).map(|_| ()).map_err(|e| e.message));
        });
        let result = outcome
            .recv_timeout(Duration::from_secs(3))
            .expect("connect hung on a handler that does not accept");
        assert!(result.is_err(), "nobody accepted: {result:?}");
        assert!(started.elapsed() >= Duration::from_millis(400) || cfg!(target_os = "macos"));
    }

    fn root() -> bool {
        // SAFETY: geteuid only reads the process's user id.
        unsafe { libc::geteuid() == 0 }
    }

    fn store(dir: &Path) -> PathBuf {
        let p = dir.join("s.tsvz");
        fs::write(&p, "k\tv\n").unwrap();
        p
    }

    fn plant(store: &Path, text: &str, mode: u32) {
        let p = path_of(store);
        fs::write(&p, text).unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(mode)).unwrap();
    }

    fn text(address: &str, host: &str) -> String {
        format!("tsvz-handler\t1\naddress\t{address}\nhost\t{host}\npid\t1\n")
    }

    #[test]
    fn the_pointer_is_the_store_path_as_given_plus_serve() {
        assert_eq!(
            path_of(Path::new("d/s.tsvz.gz")),
            Path::new("d/s.tsvz.gz.serve")
        );
        assert!(!hostname().is_empty());
    }

    #[test]
    fn one_handler_holds_the_pointer_and_a_clean_release_removes_it() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        let mut held = take(&s).unwrap();
        held.publish("unix:/nowhere/s").unwrap();
        let meta = fs::metadata(path_of(&s)).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o644);
        match take(&s) {
            Err(Take::Busy(fields)) => {
                assert_eq!(fields.get("pid"), Some(&std::process::id().to_string()))
            }
            other => panic!("second take: {:?}", other.err()),
        }
        let content = fs::read_to_string(path_of(&s)).unwrap();
        assert_eq!(
            content,
            format!(
                "tsvz-handler\t1\naddress\tunix:/nowhere/s\nhost\t{}\npid\t{}\n",
                field(&hostname(), false),
                std::process::id()
            )
        );
        held.release();
        assert!(!path_of(&s).exists());
        take(&s).unwrap().release();
    }

    #[test]
    fn a_stale_pointer_is_replaced_never_written_into() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        plant(&s, &text("unix:/gone", &hostname()), 0o600);
        let before = fs::metadata(path_of(&s)).unwrap().ino();
        // A hard link sees whether the old file was written into.
        let link = dir.path().join("old.serve");
        fs::hard_link(path_of(&s), &link).unwrap();
        let mut held = take(&s).unwrap();
        held.publish("unix:/new").unwrap();
        assert_ne!(fs::metadata(path_of(&s)).unwrap().ino(), before);
        assert_eq!(
            fs::read_to_string(&link).unwrap(),
            text("unix:/gone", &hostname())
        );
        held.release();
    }

    #[test]
    fn a_pointer_path_that_is_a_link_or_a_directory_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        let victim = dir.path().join("victim.txt");
        fs::write(&victim, "do not touch\n").unwrap();
        std::os::unix::fs::symlink(&victim, path_of(&s)).unwrap();
        match take(&s) {
            Err(Take::Other(why)) => assert!(why.contains("not a regular file"), "{why}"),
            other => panic!("symlink: {:?}", other.err()),
        }
        assert_eq!(fs::read_to_string(&victim).unwrap(), "do not touch\n");
        assert!(lookup(&s).unwrap_err().contains("not a regular file"));
        fs::remove_file(path_of(&s)).unwrap();
        fs::create_dir(path_of(&s)).unwrap();
        assert!(matches!(take(&s), Err(Take::Other(_))));
    }

    #[test]
    fn clients_trust_only_a_safe_pointer() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        let host = hostname();
        assert_eq!(lookup(&s), Ok(None), "no pointer file");
        plant(&s, &text("unix:/x/s", &host), 0o644);
        let info = lookup(&s).unwrap().unwrap();
        assert_eq!(
            (info.address.as_str(), info.host.as_str()),
            ("unix:/x/s", host.as_str())
        );
        assert_eq!(info.pid, "1");
        plant(&s, &text("unix:/x/s", &host), 0o620);
        assert!(lookup(&s).unwrap_err().contains("writable by others"));
        plant(
            &s,
            &text("unix:/x/s", &host).replace("handler\t1", "handler\t2"),
            0o600,
        );
        assert_eq!(lookup(&s), Ok(None), "not a pointer file of this protocol");
        plant(
            &s,
            &(text("unix:/x/s", &host) + "x-note\tbad\x07value\n"),
            0o600,
        );
        assert!(lookup(&s).unwrap_err().contains("control character"));
        plant(&s, &text("tcp:127.0.0.1:9", &host), 0o600);
        assert!(lookup(&s).unwrap_err().contains("not a Unix-domain socket"));
        plant(&s, "tsvz-handler\t1\npid\t1\n", 0o600);
        assert_eq!(lookup(&s), Ok(None), "an incomplete pointer is no handler");
        if root() {
            plant(&s, &text("unix:/x/s", &host), 0o600);
            std::os::unix::fs::chown(path_of(&s), Some(65534), Some(65534)).unwrap();
            assert!(lookup(&s).unwrap_err().contains("uid 65534"));
        }
        // A second name for the file: whoever linked it there need not be its owner.
        let _ = fs::remove_file(path_of(&s));
        plant(&s, &text("unix:/x/s", &host), 0o644);
        let other = dir.path().join("elsewhere");
        fs::hard_link(path_of(&s), &other).unwrap();
        assert!(lookup(&s).unwrap_err().contains("hard link"));
        fs::remove_file(&other).unwrap();
        assert!(lookup(&s).unwrap().is_some());
    }

    #[test]
    fn connections_go_only_to_a_socket_of_the_pointers_owner_on_this_host() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("h.sock");
        let listener = UnixListener::bind(&sock).unwrap();
        // SAFETY: geteuid only reads the process's user id.
        let me = unsafe { libc::geteuid() };
        let info = |address: String, host: String| Info {
            address,
            host,
            pid: "1".into(),
            owner: me,
        };
        let address = format!("unix:{}", sock.display());
        let stream = connect(&info(address.clone(), hostname()), None).unwrap();
        assert_eq!(peer_uid(&stream).unwrap(), me);
        let elsewhere = connect(&info(address.clone(), "other-host.invalid".into()), None);
        assert!(elsewhere.unwrap_err().quiet);
        let file = dir.path().join("plain");
        fs::write(&file, "").unwrap();
        let not_a_socket = connect(&info(format!("unix:{}", file.display()), hostname()), None);
        assert!(not_a_socket.unwrap_err().message.contains("not a socket"));
        if root() {
            listener.set_nonblocking(true).unwrap();
            while listener.accept().is_ok() {}
            std::os::unix::fs::chown(&sock, Some(65534), Some(65534)).unwrap();
            let theirs = connect(&info(address, hostname()), None);
            assert!(theirs.unwrap_err().message.contains("uid 65534"));
            assert!(listener.accept().is_err(), "it must not even connect");
        }
    }
}
