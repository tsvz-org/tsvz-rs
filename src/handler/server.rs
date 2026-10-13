//! `serve` and `stop` (§21.13): the socket, its connections, and stopping cleanly.

use std::collections::HashMap;
use std::ffi::{CString, OsString};
use std::io::{BufReader, Write};
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
    /// Warnings and failed writes.
    pub log: Option<WarnFn>,
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
            let _ = (&self.wake).write_all(b"x");
        }
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

/// A new directory for the socket: under `$XDG_RUNTIME_DIR` when it is a writable directory and
/// the socket is private, else under the temporary directory (others cannot enter a private
/// runtime directory).
fn socket_dir(shared: bool) -> Result<PathBuf, Error> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|d| !shared && d.is_dir() && writable(d));
    let base = runtime.unwrap_or_else(std::env::temp_dir);
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
    let engine = match Engine::open(store, config.log.clone()) {
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
    if let (Some(idle), Ok(())) = (config.idle, &helpers) {
        let (shared, info) = (shared.clone(), config.info.clone());
        helpers = std::thread::Builder::new()
            .name("tsvz-idle".into())
            .spawn(move || watch_idle(idle, shared, info))
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
    drop(listener);
    for (_, conn) in lock(&shared.open).drain() {
        let _ = conn.shutdown(std::net::Shutdown::Both);
    }
    engine.close();
    let _ = std::fs::remove_dir_all(&dir);
    held.release();
    (config.info)(&format!("stopped serving {}", store.display()));
    Ok(())
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
        let (open, since) = *lock(&shared.activity);
        if open == 0 && since.elapsed() >= idle {
            info(&format!("idle for {} s; stopping", idle.as_secs_f64()));
            shared.stop();
        }
    }
}

/// Serve `stream` on a thread of its own, made by `builder`. When no thread can be made (a task
/// limit, no memory), the connection is closed, the fact is reported, and the handler keeps
/// serving: `std::thread::spawn` would panic, and the release build aborts on panic.
fn start_connection(
    builder: std::thread::Builder,
    stream: UnixStream,
    engine: Arc<Engine>,
    shared: Arc<Shared>,
    info: InfoFn,
    verbose: bool,
) -> bool {
    let report = info.clone();
    match builder.spawn(move || serve_connection(stream, &engine, &shared, &info, verbose)) {
        Ok(_) => true,
        Err(e) => {
            report(&format!(
                "cannot start a thread for a connection ({e}); closed it"
            ));
            false
        }
    }
}

/// Answer the requests of one connection, one at a time (§21.3.2), until it ends.
fn serve_connection(
    stream: UnixStream,
    engine: &Engine,
    shared: &Shared,
    info: &InfoFn,
    verbose: bool,
) {
    let id = shared.next.fetch_add(1, Ordering::SeqCst);
    if let Ok(handle) = stream.try_clone() {
        lock(&shared.open).insert(id, handle);
    }
    lock(&shared.activity).0 += 1;
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
    lock(&shared.open).remove(&id);
    let mut activity = lock(&shared.activity);
    activity.0 -= 1;
    activity.1 = Instant::now();
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
    fn a_connection_that_gets_no_thread_is_closed_and_serving_goes_on() {
        let dir = tempfile::tempdir().unwrap();
        let engine = Arc::new(Engine::open(&dir.path().join("s.tsvz"), None).unwrap());
        let (wake, _wakened) = UnixStream::pair().unwrap();
        let shared = Arc::new(Shared {
            stopping: AtomicBool::new(false),
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
