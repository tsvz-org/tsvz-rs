//! `serve` and `stop` (§21.13): the socket, its connections, and stopping cleanly.

use std::collections::HashMap;
use std::ffi::{CString, OsString};
use std::io::{BufReader, Read, Write};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crate::handler::engine::Engine;
use crate::handler::pointer::{self, Take};
use crate::handler::protocol::{self, LINE_LIMIT, Response, read_line, read_response};
use crate::{Error, WarnFn, name};

/// Where `serve` reports what it does (one line each): starting, stopping, and with `verbose`
/// every request.
pub type InfoFn = Arc<dyn Fn(&str) + Send + Sync>;

/// How to serve.
pub struct Config {
    /// Stop after this long with no open connection (`--x-idle-timeout`).
    pub idle: Option<Duration>,
    /// Let this group connect (`--x-group`): the socket becomes 0660 unless `mode` says else.
    pub group: Option<u32>,
    /// The socket's mode (`--x-mode`).
    pub mode: Option<u32>,
    /// Warnings.
    pub log: Option<WarnFn>,
    /// Failed writes and fsyncs: errors, which `-q` does not hide.
    pub errors: Option<WarnFn>,
    pub info: InfoFn,
    /// Report every request and its status.
    pub verbose: bool,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn io(path: &Path) -> impl Fn(std::io::Error) -> Error + '_ {
    move |e| Error::io(path, e)
}

/// What the connections and the stoppers share.
struct Shared {
    stopping: AtomicBool,
    /// The idle timeout stopped the handler (its thread says so, and is waited for).
    idled: AtomicBool,
    /// The write end of the accept loop's wake-up pair.
    wake: UnixStream,
    /// Open connections (for the idle timeout) and when the last one closed.
    activity: Mutex<(usize, Instant)>,
    /// A handle on each open connection, to end it when the handler stops.
    open: Mutex<HashMap<u64, UnixStream>>,
    next: AtomicU64,
}

impl Shared {
    /// Make the accept loop return: set the flag, then wake it through its own pair of sockets,
    /// which needs no path (the socket's directory may be gone, as `/run/user/UID` is after a
    /// logout).
    fn stop(&self) {
        if !self.stopping.swap(true, Ordering::SeqCst) {
            self.wake();
        }
    }

    fn wake(&self) {
        let _ = (&self.wake).write_all(b"x");
    }
}

/// Block SIGINT, SIGTERM and SIGHUP in this thread and every thread it starts, so that one
/// thread receives them with `sigwait`; a signal ignored when `serve` starts (`nohup`) stays
/// ignored. The blocked set; empty when all three are ignored.
fn block_signals() -> libc::sigset_t {
    // SAFETY: the set is initialized by sigemptyset before use; sigaction with a null new
    // action only reads the current one; pthread_sigmask changes this thread's mask.
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
            let mut old: libc::sigaction = std::mem::zeroed();
            libc::sigaction(signal, std::ptr::null(), &mut old);
            if old.sa_sigaction != libc::SIG_IGN {
                libc::sigaddset(&mut set, signal);
            }
        }
        libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
        set
    }
}

fn wait_for_signal(set: libc::sigset_t, shared: Arc<Shared>, info: InfoFn) {
    let mut signal = 0;
    // SAFETY: sigwait reads the set and writes the signal number.
    if unsafe { libc::sigwait(&set, &mut signal) } == 0 {
        info(&format!("signal {signal}; stopping"));
        shared.stop();
    }
}

/// A new directory for the socket, under [`socket_base`].
fn socket_dir(shared: bool) -> Result<PathBuf, Error> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from);
    let base = socket_base(runtime, std::env::temp_dir(), shared);
    let template = base.join("tsvz-XXXXXX");
    let mut bytes = template.as_os_str().as_bytes().to_vec();
    bytes.push(0);
    // SAFETY: mkdtemp rewrites the trailing XXXXXX of the NUL-terminated template in place.
    let made = unsafe { libc::mkdtemp(bytes.as_mut_ptr().cast()) };
    if made.is_null() {
        return Err(Error::io(&template, std::io::Error::last_os_error()));
    }
    bytes.pop();
    Ok(PathBuf::from(OsString::from_vec(bytes)))
}

/// Where socket directories go: the runtime directory when it is an absolute, writable directory
/// and the socket is private (others cannot enter a private runtime directory), else the
/// temporary directory, made absolute: the published address must mean the same to every client.
fn socket_base(runtime: Option<PathBuf>, temp: PathBuf, shared: bool) -> PathBuf {
    match runtime.filter(|d| !shared && d.is_absolute() && d.is_dir() && writable(d)) {
        Some(dir) => dir,
        None => std::path::absolute(&temp).unwrap_or_else(|_| PathBuf::from("/tmp")),
    }
}

fn writable(dir: &Path) -> bool {
    let Ok(c) = CString::new(dir.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: access only reads the NUL-terminated path.
    unsafe { libc::access(c.as_ptr(), libc::W_OK) == 0 }
}

/// Bind the handler's socket in `dir` with the access `config` asks for (§21.11).
fn bind(dir: &Path, config: &Config) -> Result<(UnixListener, PathBuf), Error> {
    let path = dir.join("s");
    let listener = UnixListener::bind(&path).map_err(io(&path))?;
    if let Some(gid) = config.group {
        std::os::unix::fs::chown(dir, None, Some(gid)).map_err(io(dir))?;
        std::os::unix::fs::chown(&path, None, Some(gid)).map_err(io(&path))?;
    }
    let mode = config
        .mode
        .unwrap_or(if config.group.is_some() { 0o660 } else { 0o600 });
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).map_err(io(&path))?;
    let search = 0o700
        | if mode & 0o070 != 0 { 0o010 } else { 0 }
        | if mode & 0o007 != 0 { 0o001 } else { 0 };
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(search)).map_err(io(dir))?;
    Ok((listener, path))
}

/// Run a handler for `store` in this thread until it is stopped (§21.13.1): by a `stop`
/// request, a signal, or the idle timeout. Errors: another handler serves the store (its pid
/// and host are in the message), the store is loose, or the files or socket cannot be set up.
pub fn serve(store: &Path, config: Config) -> Result<(), Error> {
    if !name::format_of(store).is_some_and(|f| f.strict) {
        return Err(Error::Refused(format!(
            "{}: serve takes a strict store (.tsvz .csvz .nsvz .psvz)",
            store.display()
        )));
    }
    let mut held = match pointer::take(store) {
        Ok(held) => held,
        Err(Take::Busy(fields)) => {
            let say = |name: &str| fields.get(name).cloned().unwrap_or_else(|| "?".into());
            return Err(Error::Refused(format!(
                "{}: already served by pid {} on host {}",
                store.display(),
                say("pid"),
                say("host")
            )));
        }
        Err(Take::Other(why)) => {
            return Err(Error::Refused(format!(
                "{}: cannot take the pointer file: {why}",
                store.display()
            )));
        }
    };
    // A handler outlives its readers: a closed log pipe or a client that hangs up must not end
    // it (the command line restores SIGPIPE's default action for its own output).
    // SAFETY: setting a signal's disposition to SIG_IGN has no other effect.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }
    let signals = block_signals();
    let engine = match Engine::open(store, config.log.clone(), config.errors.clone()) {
        Ok(engine) => Arc::new(engine),
        Err(e) => {
            held.release();
            return Err(e);
        }
    };
    let shared_access = config.group.is_some() || config.mode.is_some();
    let started = socket_dir(shared_access).and_then(|dir| match bind(&dir, &config) {
        Ok((listener, path)) => Ok((dir, listener, path)),
        Err(e) => {
            let _ = std::fs::remove_dir_all(&dir);
            Err(e)
        }
    });
    let (dir, listener, socket) = match started {
        Ok(started) => started,
        Err(e) => {
            engine.close();
            held.release();
            return Err(e);
        }
    };
    let address = format!("unix:{}", socket.display());
    let woken = UnixStream::pair().and_then(|pair| {
        listener.set_nonblocking(true)?;
        pair.1.set_nonblocking(true)?;
        Ok(pair)
    });
    let (wake, wakened) = match woken {
        Ok(pair) => pair,
        Err(e) => {
            engine.close();
            let _ = std::fs::remove_dir_all(&dir);
            held.release();
            return Err(Error::io(&socket, e));
        }
    };
    if let Err(e) = held.publish(&address) {
        engine.close();
        let _ = std::fs::remove_dir_all(&dir);
        held.release();
        return Err(Error::io(&pointer::path_of(store), e));
    }
    let shared = Arc::new(Shared {
        stopping: AtomicBool::new(false),
        idled: AtomicBool::new(false),
        wake,
        activity: Mutex::new((0, Instant::now())),
        open: Mutex::new(HashMap::new()),
        next: AtomicU64::new(0),
    });
    // SAFETY: sigismember only reads the set.
    let any = [libc::SIGINT, libc::SIGTERM, libc::SIGHUP]
        .iter()
        .any(|&s| unsafe { libc::sigismember(&signals, s) } == 1);
    let mut helpers = Ok(());
    if any {
        let (shared, info) = (shared.clone(), config.info.clone());
        helpers = std::thread::Builder::new()
            .name("tsvz-signals".into())
            .spawn(move || wait_for_signal(signals, shared, info))
            .map(drop);
    }
    let mut idler = None;
    if let (Some(idle), Ok(())) = (config.idle, &helpers) {
        let (shared, info) = (shared.clone(), config.info.clone());
        match std::thread::Builder::new()
            .name("tsvz-idle".into())
            .spawn(move || watch_idle(idle, shared, info))
        {
            Ok(thread) => idler = Some(thread),
            Err(e) => helpers = Err(e),
        }
    }
    if let Err(e) = helpers {
        engine.close();
        let _ = std::fs::remove_dir_all(&dir);
        held.release();
        return Err(Error::Refused(format!(
            "{}: serve cannot start its threads ({e})",
            store.display()
        )));
    }
    (config.info)(&format!(
        "serving {} at {address} (pid {})",
        store.display(),
        std::process::id()
    ));
    loop {
        let ready = wait_for(&listener, &wakened);
        if shared.stopping.load(Ordering::SeqCst) {
            break;
        }
        if !ready {
            continue; // interrupted
        }
        let stream = match listener.accept() {
            Ok((stream, _)) => stream,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
            Err(_) => {
                std::thread::sleep(Duration::from_millis(10)); // out of descriptors, for a moment
                continue;
            }
        };
        // An accepted socket inherits non-blocking mode on some systems (BSD, macOS).
        if stream.set_nonblocking(false).is_err() {
            continue;
        }
        start_connection(
            std::thread::Builder::new().name("tsvz-connection".into()),
            stream,
            engine.clone(),
            shared.clone(),
            config.info.clone(),
            config.verbose,
        );
    }
    refuse_queued(&listener);
    drop(listener);
    finish_connections(&engine, &shared);
    engine.close();
    let _ = std::fs::remove_dir_all(&dir);
    held.release();
    if let (true, Some(thread)) = (shared.idled.load(Ordering::SeqCst), idler) {
        let _ = thread.join(); // it says why it stopped the handler first
    }
    (config.info)(&format!("stopped serving {}", store.display()));
    Ok(())
}

/// Tell a client the handler is stopping, before reading anything of its request: nothing of it
/// is made, and the client knows so.
fn refuse(mut stream: UnixStream) {
    refuse_within(&mut stream, Duration::from_millis(200));
}

/// [`refuse`], waiting at most `patience` in all for the client to close (a client that sends
/// a byte now and then gets no more time), and reading at most 64 KiB of its request.
fn refuse_within(stream: &mut UnixStream, patience: Duration) {
    let _ = stream.write_all(&Response::failed(1, "the handler is stopping").encode());
    let _ = stream.shutdown(std::net::Shutdown::Write);
    // Closing with the request unread would reset the connection, and the answer with it: read
    // and drop what the client sent, for a moment, until it closes.
    let deadline = Instant::now() + patience;
    let mut sink = [0u8; 4096];
    for _ in 0..16 {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() || stream.set_read_timeout(Some(left)).is_err() {
            break;
        }
        if !matches!(stream.read(&mut sink), Ok(n) if n > 0) {
            break;
        }
    }
}

/// Refuse the clients still waiting in the listen queue, rather than reset them as closing the
/// listener would: a reset leaves a client unsure whether its request was made. At most
/// [`QUEUED`] clients, within [`REFUSING`]: clients that keep connecting cannot hold a stop.
fn refuse_queued(listener: &UnixListener) {
    if listener.set_nonblocking(true).is_err() {
        return;
    }
    let deadline = Instant::now() + REFUSING;
    for _ in 0..QUEUED {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        let Ok((mut stream, _)) = listener.accept() else {
            break;
        };
        if stream.set_nonblocking(false).is_ok() {
            refuse_within(&mut stream, left.min(Duration::from_millis(200)));
        }
    }
}

/// The most queued clients a stop refuses, and how long it spends on them.
const QUEUED: usize = 128;
const REFUSING: Duration = Duration::from_secs(1);

/// How long a stop waits for the requests already read to be answered.
const FINISH: Duration = Duration::from_secs(5);

/// End the open connections as a stop does: requests from now on are refused; the ones already
/// read finish and are answered (a pop in flight has written its tombstone: its row must reach
/// its client); then each connection ends when it reads its next line. A connection still open
/// after [`FINISH`] (a client not reading its answer) is cut off.
fn finish_connections(engine: &Engine, shared: &Shared) {
    engine.refuse();
    for conn in lock(&shared.open).values() {
        let _ = conn.shutdown(std::net::Shutdown::Read);
    }
    let deadline = Instant::now() + FINISH;
    while lock(&shared.activity).0 > 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    for (_, conn) in lock(&shared.open).drain() {
        let _ = conn.shutdown(std::net::Shutdown::Both);
    }
}

/// Wait until a connection can be accepted on `listener` or the stop pair `wakened` is
/// written to; false when the wait failed (and should be retried).
fn wait_for(listener: &UnixListener, wakened: &UnixStream) -> bool {
    use std::os::fd::AsRawFd;
    let mut fds = [
        libc::pollfd {
            fd: listener.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: wakened.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    // SAFETY: poll reads and writes the two pollfd entries, whose descriptors stay open.
    unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, -1) > 0 }
}

fn watch_idle(idle: Duration, shared: Arc<Shared>, info: InfoFn) {
    let tick = (idle / 4).clamp(Duration::from_millis(10), Duration::from_secs(1));
    while !shared.stopping.load(Ordering::SeqCst) {
        std::thread::sleep(tick);
        // Decided, and the stop set, under the lock that counts new connections: one counted
        // before is served to its end, one after is refused (start_connection). Said after it
        // (no output under the lock), unless something else stopped the handler first; serve
        // waits for this thread before it ends.
        let idled = {
            let activity = lock(&shared.activity);
            let idled = activity.0 == 0
                && activity.1.elapsed() >= idle
                && !shared.stopping.load(Ordering::SeqCst);
            if idled {
                shared.idled.store(true, Ordering::SeqCst);
                shared.stopping.store(true, Ordering::SeqCst);
            }
            idled
        };
        if idled {
            info(&format!("idle for {} s; stopping", idle.as_secs_f64()));
            shared.wake();
        }
    }
}

/// Serve `stream` on a thread of its own, made by `builder`. The connection is counted and
/// registered here, as it is accepted: the idle timeout never sees an accepted connection as
/// none, and a stop always ends it. When no thread can be made (a task limit, no memory), the
/// connection is closed, the fact is reported, and the handler keeps serving:
/// `std::thread::spawn` would panic, and the release build aborts on panic.
fn start_connection(
    builder: std::thread::Builder,
    stream: UnixStream,
    engine: Arc<Engine>,
    shared: Arc<Shared>,
    info: InfoFn,
    verbose: bool,
) -> bool {
    {
        let mut activity = lock(&shared.activity);
        if shared.stopping.load(Ordering::SeqCst) {
            drop(activity);
            refuse(stream);
            return false;
        }
        activity.0 += 1;
    }
    let id = shared.next.fetch_add(1, Ordering::SeqCst);
    if let Ok(handle) = stream.try_clone() {
        lock(&shared.open).insert(id, handle);
    }
    let report = info.clone();
    let mine = shared.clone();
    match builder.spawn(move || serve_connection(id, stream, &engine, &shared, &info, verbose)) {
        Ok(_) => true,
        Err(e) => {
            end_connection(&mine, id);
            report(&format!(
                "cannot start a thread for a connection ({e}); closed it"
            ));
            false
        }
    }
}

fn end_connection(shared: &Shared, id: u64) {
    lock(&shared.open).remove(&id);
    let mut activity = lock(&shared.activity);
    activity.0 -= 1;
    activity.1 = Instant::now();
}

/// Answer the requests of connection `id`, one at a time (§21.3.2), until it ends.
fn serve_connection(
    id: u64,
    stream: UnixStream,
    engine: &Engine,
    shared: &Shared,
    info: &InfoFn,
    verbose: bool,
) {
    if let Ok(reader) = stream.try_clone() {
        answer(
            BufReader::new(reader),
            stream,
            engine,
            shared,
            info,
            verbose,
        );
    }
    end_connection(shared, id);
}

fn answer(
    mut reader: BufReader<UnixStream>,
    mut writer: UnixStream,
    engine: &Engine,
    shared: &Shared,
    info: &InfoFn,
    verbose: bool,
) {
    // A read error, a line over the limit or the end of the connection ends it (§21.10.2).
    while let Ok(Some(line)) = read_line(&mut reader, LINE_LIMIT) {
        let request = protocol::parse(&line);
        let mut bulk = None;
        if protocol::needs_bulk(&line) {
            let mut records = Vec::new();
            loop {
                match read_line(&mut reader, LINE_LIMIT) {
                    Ok(Some(l)) if l == "#" => break,
                    Ok(Some(l)) => records.push(l),
                    _ => return, // cut off before its end: nothing is written (§21.7.2)
                }
            }
            bulk = Some(records);
        }
        let response = match &request {
            Ok(r) if r.op == "stop" => {
                let response = match engine.flush() {
                    Ok(()) => Response::status(0),
                    Err(e) => Response::failed(1, e.to_string()),
                };
                let _ = writer.write_all(&response.encode());
                shared.stop();
                return;
            }
            Ok(r) => engine.handle(r, bulk.as_deref()),
            Err(message) => Response::failed(2, message.clone()),
        };
        if verbose {
            let op = request.as_ref().map_or("?", |r| r.op.as_str());
            info(&format!(
                "{}: {op} answered #{}",
                engine.path().display(),
                response.status
            ));
        }
        if writer.write_all(&response.encode()).is_err() {
            return;
        }
    }
}

/// Ask the handler of `store` to stop and wait until it has (§21.13.2).
pub fn stop(store: &Path) -> Result<(), Error> {
    let refused = |why: String| Error::Refused(format!("{}: {why}", store.display()));
    let info = match pointer::lookup(store) {
        Ok(Some(info)) => info,
        Ok(None) => return Err(refused("no handler is running".into())),
        Err(why) => return Err(refused(why)),
    };
    let mut conn = pointer::connect(&info, None)
        .map_err(|e| refused(format!("the handler does not answer ({})", e.message)))?;
    conn.write_all(b"stop\n")
        .map_err(|e| refused(format!("the handler does not answer ({e})")))?;
    let mut reader = BufReader::new(&conn);
    let response =
        read_response(&mut reader).map_err(|e| refused(format!("the handler went away ({e})")))?;
    if response.status != 0 {
        return Err(refused(format!("stop failed: {}", response.message)));
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline
        && pointer::lookup(store)
            .ok()
            .flatten()
            .is_some_and(|now| now.pid == info.pid)
    {
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn the_socket_directory_is_always_absolute() {
        // Clients resolve the published address from their own working directory.
        let base = socket_base(Some("run".into()), "tmp".into(), false);
        assert!(base.is_absolute() && base.ends_with("tmp"), "{base:?}");
        let runtime = tempfile::tempdir().unwrap();
        let own = Some(runtime.path().to_path_buf());
        assert_eq!(socket_base(own.clone(), "/t".into(), false), runtime.path());
        assert_eq!(
            socket_base(own, "/t".into(), true),
            Path::new("/t"),
            "shared: not private"
        );
    }

    fn shared() -> Arc<Shared> {
        let (wake, _) = UnixStream::pair().unwrap();
        Arc::new(Shared {
            stopping: AtomicBool::new(false),
            idled: AtomicBool::new(false),
            wake,
            activity: Mutex::new((0, Instant::now())),
            open: Mutex::new(HashMap::new()),
            next: AtomicU64::new(0),
        })
    }

    #[test]
    fn a_connection_is_counted_when_it_is_accepted_not_when_its_thread_runs() {
        // Else the idle timeout can stop the handler under a client it has just accepted.
        let dir = tempfile::tempdir().unwrap();
        let engine = Arc::new(Engine::open(&dir.path().join("s.tsvz"), None, None).unwrap());
        let shared = shared();
        let info: InfoFn = Arc::new(|_: &str| {});
        let (_client, end) = UnixStream::pair().unwrap();
        let held = lock(&shared.open); // stalls whatever registers the connection next
        let started = {
            let shared = shared.clone();
            std::thread::spawn(move || {
                start_connection(
                    std::thread::Builder::new(),
                    end,
                    engine,
                    shared,
                    info,
                    false,
                )
            })
        };
        let deadline = Instant::now() + Duration::from_secs(2);
        while lock(&shared.activity).0 == 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(lock(&shared.activity).0, 1, "counted before anything else");
        drop(held);
        assert!(started.join().unwrap());
        assert_eq!(
            lock(&shared.open).len(),
            1,
            "registered for a stop by the accept loop"
        );
    }

    #[test]
    fn a_connection_accepted_once_the_handler_is_stopping_is_told_so_not_served() {
        // The stop decision and the count of a new connection are made under one lock: a
        // connection is served to the end, or refused before anything of it is read.
        let dir = tempfile::tempdir().unwrap();
        let engine = Arc::new(Engine::open(&dir.path().join("s.tsvz"), None, None).unwrap());
        let shared = shared();
        shared.stopping.store(true, Ordering::SeqCst);
        let info: InfoFn = Arc::new(|_: &str| {});
        let (mut client, end) = UnixStream::pair().unwrap();
        client.write_all(b"set\tk\tv\n").unwrap();
        assert!(!start_connection(
            std::thread::Builder::new(),
            end,
            engine.clone(),
            shared.clone(),
            info,
            false
        ));
        let mut answer = String::new();
        client.read_to_string(&mut answer).unwrap();
        assert_eq!(answer, "#1\tthe handler is stopping\n");
        assert_eq!(lock(&shared.activity).0, 0, "never counted");
        assert!(lock(&shared.open).is_empty());
        engine.close();
        assert_eq!(
            std::fs::read(dir.path().join("s.tsvz")).unwrap(),
            b"",
            "nothing written"
        );
    }

    #[test]
    fn a_client_sending_slowly_cannot_hold_a_refusal() {
        // The wait is one deadline, not a timeout per read that a trickle of bytes renews.
        let (mut client, mut end) = UnixStream::pair().unwrap();
        let trickle = std::thread::spawn(move || {
            for _ in 0..40 {
                if client.write_all(b"x").is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        });
        let started = Instant::now();
        refuse_within(&mut end, Duration::from_millis(200));
        let took = started.elapsed();
        drop(end);
        trickle.join().unwrap();
        assert!(took < Duration::from_millis(600), "held for {took:?}");
    }

    #[test]
    fn refusing_the_queued_clients_ends_even_when_they_keep_coming() {
        // A client connecting again and again must not keep a handler from stopping.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s");
        let listener = UnixListener::bind(&path).unwrap();
        let flooding = Arc::new(AtomicBool::new(true));
        let flood = {
            let (path, flooding) = (path.clone(), flooding.clone());
            std::thread::spawn(move || {
                while flooding.load(Ordering::SeqCst) {
                    if let Ok(mut c) = UnixStream::connect(&path) {
                        let _ = c.write_all(b"len\n"); // and keep it open, unread
                        std::mem::forget(c);
                    }
                }
            })
        };
        std::thread::sleep(Duration::from_millis(50));
        let (done, finished) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            refuse_queued(&listener);
            let _ = done.send(());
        });
        let ended = finished.recv_timeout(Duration::from_secs(3));
        flooding.store(false, Ordering::SeqCst);
        flood.join().unwrap();
        assert!(ended.is_ok(), "refusing queued clients never ended");
    }

    #[test]
    fn a_connection_that_gets_no_thread_is_closed_and_serving_goes_on() {
        let dir = tempfile::tempdir().unwrap();
        let engine = Arc::new(Engine::open(&dir.path().join("s.tsvz"), None, None).unwrap());
        let (wake, _wakened) = UnixStream::pair().unwrap();
        let shared = Arc::new(Shared {
            stopping: AtomicBool::new(false),
            idled: AtomicBool::new(false),
            wake,
            activity: Mutex::new((0, Instant::now())),
            open: Mutex::new(HashMap::new()),
            next: AtomicU64::new(0),
        });
        let said = Arc::new(Mutex::new(Vec::new()));
        let log = said.clone();
        let info: InfoFn = Arc::new(move |m: &str| lock(&log).push(m.to_string()));
        // No system can map a stack this large: the thread cannot be made.
        let impossible = std::thread::Builder::new().stack_size(usize::MAX / 2);
        let (mut client, end) = UnixStream::pair().unwrap();
        assert!(!start_connection(
            impossible,
            end,
            engine.clone(),
            shared.clone(),
            info.clone(),
            false
        ));
        let mut byte = [0u8; 1];
        assert_eq!(
            client.read(&mut byte).unwrap(),
            0,
            "the connection is closed"
        );
        assert!(lock(&said)[0].contains("thread"), "{:?}", lock(&said));
        assert_eq!(lock(&shared.activity).0, 0, "no longer counted");
        assert!(lock(&shared.open).is_empty(), "nor registered");
        let (mut client, end) = UnixStream::pair().unwrap();
        assert!(start_connection(
            std::thread::Builder::new(),
            end,
            engine,
            shared,
            info,
            false
        ));
        client.write_all(b"len\n").unwrap();
        let mut answer = String::new();
        client.take(5).read_to_string(&mut answer).unwrap();
        assert_eq!(answer, "0\n#0\n");
    }
}
