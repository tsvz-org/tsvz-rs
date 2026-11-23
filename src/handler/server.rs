//! `serve` and `stop` (§21.13): the socket, its connections, and stopping cleanly.

use std::collections::HashMap;
use std::ffi::{CString, OsString};
use std::io::{BufReader, ErrorKind, Write};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::handler::engine::{Engine, lock};
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

fn io(path: &Path) -> impl Fn(std::io::Error) -> Error + '_ {
    move |e| Error::io(path, e)
}

/// An open connection: a handle on it, to end it when the handler stops, and whether a `pop` or
/// `popitem` is in it, whose row is deleted once its answer is made: a stop never cuts it off
/// before that answer is written.
struct Conn {
    stream: UnixStream,
    owes: bool,
}

/// What the connections and the stoppers share.
struct Shared {
    stopping: AtomicBool,
    /// Set once the stop has ended the connections and written every queued write: the accept
    /// loop, which closes every connection it accepts meanwhile, then ends.
    finished: AtomicBool,
    /// Why the handler stops, said by serve once its accept loop ends: set by the one that
    /// claimed the stop (the idle timeout, a signal), under this mutex.
    why: Mutex<Option<String>>,
    /// The write end of the accept loop's wake-up pair.
    wake: UnixStream,
    /// Open connections (for the idle timeout) and when the last one closed.
    activity: Mutex<(usize, Instant)>,
    /// Each open connection that a stop has not cut off.
    open: Mutex<HashMap<u64, Conn>>,
    next: AtomicU64,
}

impl Shared {
    /// Make the accept loop return: set the flag, then wake it through its own pair of sockets,
    /// which needs no path (the socket's directory may be gone, as `/run/user/UID` is after a
    /// logout).
    fn stop(&self) {
        self.claim(None);
    }

    /// Stop the handler, for `why`, unless something stopped it already.
    fn claim(&self, why: Option<String>) {
        let mut said = lock(&self.why);
        if self.stopping.swap(true, Ordering::SeqCst) {
            return;
        }
        *said = why;
        drop(said);
        self.wake();
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

fn wait_for_signal(set: libc::sigset_t, shared: Arc<Shared>) {
    let mut signal = 0;
    // SAFETY: sigwait reads the set and writes the signal number.
    if unsafe { libc::sigwait(&set, &mut signal) } == 0 {
        shared.claim(Some(format!("signal {signal}; stopping")));
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
        finished: AtomicBool::new(false),
        why: Mutex::new(None),
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
        let shared = shared.clone();
        helpers = std::thread::Builder::new()
            .name("tsvz-signals".into())
            .spawn(move || wait_for_signal(signals, shared))
            .map(drop);
    }
    if let (Some(idle), Ok(())) = (config.idle, &helpers) {
        let shared = shared.clone();
        helpers = std::thread::Builder::new()
            .name("tsvz-idle".into())
            .spawn(move || watch_idle(idle, shared))
            .map(drop);
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
    // Once stopping, the loop goes on accepting, and closes each connection unread (as a client
    // finds a handler gone: a write exits 1, a read uses the files), until the stop has written
    // every queued write: a command that found the listener closed would work on the files, where
    // a `pop` could take a row whose tombstone is still queued here.
    let mut finishing = false;
    loop {
        let ready = wait_for(&listener, &wakened);
        drain(&wakened);
        if shared.stopping.load(Ordering::SeqCst) && !finishing {
            finishing = true;
            let why = lock(&shared.why).take();
            if let Some(why) = why {
                (config.info)(&why);
            }
            let finisher = {
                let (shared, engine) = (shared.clone(), engine.clone());
                std::thread::Builder::new()
                    .name("tsvz-stop".into())
                    .spawn(move || finish(&shared, &engine))
            };
            if finisher.is_err() {
                // No thread to spare: finished here, with the connections that come meanwhile
                // left in the listener's queue.
                finish(&shared, &engine);
            }
        }
        if shared.finished.load(Ordering::SeqCst) {
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
    // Every queued write is written: the pointer file goes first, so that a command that finds the
    // listener closed finds no handler (`tsvz stop` waits for this).
    held.release();
    drop(listener); // the clients still in its queue find their connection closed
    let _ = std::fs::remove_dir_all(&dir);
    (config.info)(&format!("stopped serving {}", store.display()));
    Ok(())
}

/// Empty the stop pair `wakened` (non-blocking), so that the next wait waits.
fn drain(wakened: &UnixStream) {
    use std::io::Read;
    let mut buf = [0u8; 64];
    while matches!((&*wakened).read(&mut buf), Ok(n) if n > 0) {}
}

/// How long a stop waits for the requests already read to be answered, and then for the answers
/// of the pops it waited for: a client that does not read its answer is cut off after this.
pub const FINISH: Duration = Duration::from_secs(5);

/// Stop as §21.13 says, then set `finished` and wake the accept loop. No request sent from now on
/// is read; the ones already read finish and are answered, and on Linux so do those already
/// queued on a connection, which the shutdown leaves readable (macOS drops them). After
/// [`FINISH`], every connection is cut off but those a `pop` or `popitem` is in: its row is
/// deleted once its answer is made, so the answer is its only copy. Then the engine closes:
/// requests not yet begun are refused, and every queued write is written and synced (§21.9.3), as
/// long as that takes: the stop waited for it before as well. The pops' answers are given
/// [`FINISH`] more to be written, then the rest is cut off.
fn finish(shared: &Shared, engine: &Engine) {
    for conn in lock(&shared.open).values() {
        let _ = conn.stream.shutdown(std::net::Shutdown::Read);
    }
    settle(shared, |open| open.values().all(|c| c.owes));
    cut(shared, true);
    engine.close();
    settle(shared, HashMap::is_empty);
    cut(shared, false);
    shared.finished.store(true, Ordering::SeqCst);
    shared.wake();
}

/// Wait until `done` holds for the open connections, or [`FINISH`] has passed.
fn settle(shared: &Shared, done: impl Fn(&HashMap<u64, Conn>) -> bool) {
    let deadline = Instant::now() + FINISH;
    while !done(&lock(&shared.open)) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Cut off every open connection, but with `spare_owing` those a pop is in. A connection cut off
/// begins no other request ([`begin`]).
fn cut(shared: &Shared, spare_owing: bool) {
    lock(&shared.open).retain(|_, conn| {
        if spare_owing && conn.owes {
            return true;
        }
        let _ = conn.stream.shutdown(std::net::Shutdown::Both);
        false
    });
}

/// Begin a request on connection `id`: false once a stop has cut it off (a request begun then
/// could take a row it has no way to answer). `owes` marks a `pop` or `popitem` until the next
/// request begins, under the lock the cut takes.
fn begin(shared: &Shared, id: u64, owes: bool) -> bool {
    match lock(&shared.open).get_mut(&id) {
        Some(conn) => {
            conn.owes = owes;
            true
        }
        None => false,
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

fn watch_idle(idle: Duration, shared: Arc<Shared>) {
    let tick = (idle / 4).clamp(Duration::from_millis(10), Duration::from_secs(1));
    while !shared.stopping.load(Ordering::SeqCst) {
        std::thread::sleep(tick);
        // Claimed while holding the lock that counts new connections (start_connection counts
        // under it), so none is counted between the check that none is open and the stop.
        let activity = lock(&shared.activity);
        if activity.0 == 0 && activity.1.elapsed() >= idle {
            shared.claim(Some(format!("idle for {} s; stopping", idle.as_secs_f64())));
        }
    }
}

/// Serve `stream` on a thread of its own, made by `builder`. The connection is counted and
/// registered here, as it is accepted: the idle timeout never sees an accepted connection as
/// none, and a stop always ends it; one accepted once the handler is stopping, or that cannot be
/// registered (no descriptor for its handle), is closed unread, as a client finds a handler
/// gone. When no thread can be made (a task limit, no memory), the connection is closed, the
/// fact is reported, and the handler keeps serving: `std::thread::spawn` would panic, and the
/// release build aborts on panic.
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
            return false;
        }
        activity.0 += 1;
    }
    let id = shared.next.fetch_add(1, Ordering::SeqCst);
    match stream.try_clone() {
        Ok(handle) => {
            let conn = Conn {
                stream: handle,
                owes: false,
            };
            lock(&shared.open).insert(id, conn);
        }
        Err(_) => {
            end_connection(&shared, id);
            return false;
        }
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
            id,
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
    id: u64,
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
        let owes = request
            .as_ref()
            .is_ok_and(|r| matches!(r.op.as_str(), "pop" | "popitem"));
        if !begin(shared, id, owes) {
            return; // cut off by a stop
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

/// How long `tsvz stop` waits, from its request, for the handler to be gone. Once it has
/// answered, a handler stops (`finish`): at most [`FINISH`] for the requests in flight,
/// their writes, and [`FINISH`] more for the pops' answers; the 20 s beyond those two windows are
/// room for the writes and syncs of a slow disk. Measured on a local disk, two overlapping stops a
/// round under 24 command lines: 300 stops took at most 0.15 s (median 0.06 s) with
/// `#_write_ack_# memory`, and 300 at most 0.14 s (median 0.05 s) with `disk`.
pub const STOP_WAIT: Duration = Duration::from_secs(2 * FINISH.as_secs() + 20);

/// Ask the handler of `store` to stop and wait until it has (§21.13.2): Ok once its pointer file
/// no longer names it, which it removes once every acknowledged write is written; also when
/// another stop, a signal or its idle timeout stopped it first. An error when there is no
/// handler, when it does not answer, or when it is still there after [`STOP_WAIT`].
pub fn stop(store: &Path) -> Result<(), Error> {
    stop_within(store, STOP_WAIT)
}

/// [`stop`], waiting at most `wait`.
fn stop_within(store: &Path, wait: Duration) -> Result<(), Error> {
    let refused = |why: String| Error::Refused(format!("{}: {why}", store.display()));
    let info = match pointer::lookup(store) {
        Ok(Some(info)) => info,
        Ok(None) => return Err(refused("no handler is running".into())),
        Err(why) => return Err(refused(why)),
    };
    let deadline = Instant::now() + wait;
    let there = || {
        pointer::lookup(store)
            .ok()
            .flatten()
            .is_some_and(|now| now.pid == info.pid)
    };
    let mut conn = match pointer::connect(&info, None) {
        Ok(conn) => conn,
        Err(_) if !there() => return Ok(()), // it stopped meanwhile
        Err(e) => {
            return Err(refused(format!(
                "the handler does not answer ({})",
                e.message
            )));
        }
    };
    let _ = conn.set_read_timeout(Some(wait.max(Duration::from_millis(1))));
    let answer = conn
        .write_all(b"stop\n")
        .and_then(|()| read_response(&mut BufReader::new(&conn)));
    match answer {
        Ok(response) if response.status != 0 => {
            return Err(refused(format!("stop failed: {}", response.message)));
        }
        Ok(_) => {}
        Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
            return Err(refused(format!(
                "the handler did not answer within {} s",
                wait.as_secs()
            )));
        }
        // Closed unread: the handler is stopping already (another stop, a signal, its idle
        // timeout), and this stop waits for it as the one that asked first does.
        Err(_) => {}
    }
    while there() {
        if Instant::now() >= deadline {
            return Err(refused(format!(
                "the handler (pid {}) has not stopped within {} s",
                info.pid,
                wait.as_secs()
            )));
        }
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
            finished: AtomicBool::new(false),
            why: Mutex::new(None),
            wake,
            activity: Mutex::new((0, Instant::now())),
            open: Mutex::new(HashMap::new()),
            next: AtomicU64::new(0),
        })
    }

    /// A pointer file of `store` naming a socket this test listens on: a stand-in handler.
    fn stand_in(dir: &Path, store: &Path) -> UnixListener {
        let sock = dir.join("sock");
        let listener = UnixListener::bind(&sock).unwrap();
        let at = pointer::path_of(store);
        let text = format!(
            "tsvz-handler\t1\naddress\tunix:{}\nhost\t{}\npid\t4242\n",
            sock.display(),
            pointer::hostname()
        );
        std::fs::write(&at, text).unwrap();
        std::fs::set_permissions(&at, std::fs::Permissions::from_mode(0o644)).unwrap();
        listener
    }

    #[test]
    fn a_stop_that_finds_the_handler_stopping_waits_until_it_is_gone() {
        // Another stop, or a signal, came first: the handler closes this one's connection
        // unread, and this stop waits for it to be gone as the first does, and succeeds.
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("s.tsvz");
        let listener = stand_in(dir.path(), &store);
        let at = pointer::path_of(&store);
        let handler = std::thread::spawn(move || {
            drop(listener.accept().unwrap()); // closed unread
            std::thread::sleep(Duration::from_millis(300));
            std::fs::remove_file(at).unwrap(); // gone
        });
        stop_within(&store, Duration::from_secs(10)).unwrap();
        handler.join().unwrap();
    }

    #[test]
    fn a_stop_fails_when_the_handler_is_still_there_after_its_wait() {
        // A handler that answers and is not gone in time (slow writes, or killed before it
        // removed its pointer file): the stop says so, and does not report it stopped.
        use std::io::BufRead;
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("s.tsvz");
        let listener = stand_in(dir.path(), &store);
        let handler = std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().unwrap();
            let mut line = String::new();
            BufReader::new(&conn).read_line(&mut line).unwrap();
            conn.write_all(b"#0\n").unwrap();
            conn
        });
        let e = stop_within(&store, Duration::from_millis(500)).unwrap_err();
        assert!(e.to_string().contains("has not stopped within"), "{e}");
        drop(handler.join());
    }

    #[test]
    fn a_stop_gives_up_on_a_handler_that_does_not_answer() {
        use std::io::BufRead;
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("s.tsvz");
        let listener = stand_in(dir.path(), &store);
        let handler = std::thread::spawn(move || {
            let (conn, _) = listener.accept().unwrap();
            let mut line = String::new();
            BufReader::new(&conn).read_line(&mut line).unwrap();
            conn // never answered
        });
        let (done, outcome) = std::sync::mpsc::channel();
        let asked = store.clone();
        std::thread::spawn(move || {
            let _ = done.send(stop_within(&asked, Duration::from_millis(500)));
        });
        let e = outcome
            .recv_timeout(Duration::from_secs(10))
            .expect("the stop waited for an answer without a bound")
            .unwrap_err();
        assert!(e.to_string().contains("did not answer within"), "{e}");
        drop(handler.join());
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
    fn a_connection_accepted_once_the_handler_is_stopping_is_closed_unread() {
        // The stop flag is looked at under the lock that counts a new connection: one accepted
        // once it is set is closed before anything of it is read, and its client finds the
        // handler gone (a routed read then runs on the files).
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
        let mut answer = Vec::new();
        let _ = client.read_to_end(&mut answer); // the end of the connection, or a reset
        assert!(answer.is_empty(), "answered {answer:?}");
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
    fn a_connection_that_gets_no_thread_is_closed_and_serving_goes_on() {
        let dir = tempfile::tempdir().unwrap();
        let engine = Arc::new(Engine::open(&dir.path().join("s.tsvz"), None, None).unwrap());
        let (wake, _wakened) = UnixStream::pair().unwrap();
        let shared = Arc::new(Shared {
            stopping: AtomicBool::new(false),
            finished: AtomicBool::new(false),
            why: Mutex::new(None),
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
