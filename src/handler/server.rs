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
/// `popitem` is in it, whose row is deleted once its answer is made: a stop does not cut it off
/// before that answer is written, until [`FINISH`] has passed after the queue was written (a
/// client that still reads nothing then is cut off, as it is at the first window).
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

/// SIGINT, SIGTERM and SIGHUP blocked in the thread that serves and in every thread it starts, so
/// that one thread of their own receives them with `sigwait` ([`Signals::wait`]), every one that
/// comes until `serve` returns (the first stops the handler; a later one, during the stop, changes
/// nothing: left pending, it would end the process by its default action once the mask is given
/// back); a signal ignored when `serve` starts (`nohup`) stays ignored. Dropped as `serve`
/// returns, it ends that thread and gives the serving thread its signal mask back, so a program
/// that embeds `serve` gets those signals again; the command line's ([`serve_to_exit`]) keeps
/// them blocked to its exit, as 0.2.1's did.
struct Signals {
    set: libc::sigset_t,
    old: libc::sigset_t,
    waiter: Option<std::thread::JoinHandle<()>>,
    /// Whether the drop gives the mask back (`serve`, for an embedder) or leaves the three blocked
    /// ([`serve_to_exit`], for the command line, which goes on for a moment and then exits: a signal
    /// that came meanwhile would end it by its default action once unblocked).
    restore: bool,
    /// Set by the drop before it wakes the waiter, which then ends.
    done: Arc<AtomicBool>,
}

impl Signals {
    fn block(restore: bool) -> Signals {
        // SAFETY: the sets are initialized by sigemptyset and pthread_sigmask before use;
        // sigaction with a null new action only reads the current one; pthread_sigmask changes
        // this thread's mask.
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
            let mut old: libc::sigset_t = std::mem::zeroed();
            libc::pthread_sigmask(libc::SIG_BLOCK, &set, &mut old);
            Signals {
                set,
                old,
                waiter: None,
                restore,
                done: Arc::new(AtomicBool::new(false)),
            }
        }
    }

    /// The first of the signals blocked; None when all three are ignored.
    fn first(&self) -> Option<i32> {
        [libc::SIGINT, libc::SIGTERM, libc::SIGHUP]
            .into_iter()
            // SAFETY: sigismember only reads the set.
            .find(|&s| unsafe { libc::sigismember(&self.set, s) } == 1)
    }

    /// Start the thread that waits for the signals and stops the handler when one comes.
    fn wait(&mut self, shared: Arc<Shared>) -> std::io::Result<()> {
        if self.first().is_some() {
            let (set, done) = (self.set, self.done.clone());
            let waiter = std::thread::Builder::new()
                .name("tsvz-signals".into())
                .spawn(move || wait_for_signals(set, shared, done))?;
            self.waiter = Some(waiter);
        }
        Ok(())
    }
}

impl Drop for Signals {
    fn drop(&mut self) {
        if let (Some(waiter), Some(signal)) = (self.waiter.take(), self.first()) {
            use std::os::unix::thread::JoinHandleExt;
            // Sent to the waiter alone, which takes it in `sigwait` (or is making a claim, and
            // takes it next), sees `done` and ends: it blocks it, as every thread of the handler
            // does.
            self.done.store(true, Ordering::SeqCst);
            // SAFETY: a thread not yet joined keeps its pthread_t valid. std hands it over as an
            // integer; libc's `pthread_t` is an integer on glibc and macOS and a pointer on musl,
            // so `as _` takes whichever type libc wants on each target.
            unsafe { libc::pthread_kill(waiter.as_pthread_t() as _, signal) };
            let _ = waiter.join();
        }
        if self.restore {
            // SAFETY: pthread_sigmask sets this thread's mask to the one `block` saved.
            unsafe { libc::pthread_sigmask(libc::SIG_SETMASK, &self.old, std::ptr::null_mut()) };
        }
    }
}

/// Take every signal of `set` until `done`: each is a claim of the stop, which changes nothing
/// once the handler is stopping.
fn wait_for_signals(set: libc::sigset_t, shared: Arc<Shared>, done: Arc<AtomicBool>) {
    loop {
        let mut signal = 0;
        // SAFETY: sigwait reads the set and writes the signal number.
        if unsafe { libc::sigwait(&set, &mut signal) } != 0 || done.load(Ordering::SeqCst) {
            return;
        }
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
    run(store, config, true)
}

/// [`serve`] for the command line, which gives no signal mask back: the three signals,
/// blocked once the pointer file is taken (so that a signal still ends a start whose first
/// access to it hangs), stay blocked to the exit, where a pending one is dropped, as in 0.2.1.
#[cfg(any(feature = "cli", test))] // the command line's, and a lib test's
pub(crate) fn serve_to_exit(store: &Path, config: Config) -> Result<(), Error> {
    run(store, config, false)
}

fn run(store: &Path, config: Config, restore: bool) -> Result<(), Error> {
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
    let mut signals = Signals::block(restore);
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
    let mut helpers = signals.wait(shared.clone());
    if let (Some(idle), Ok(())) = (config.idle, &helpers) {
        let shared = shared.clone();
        // Let go of at once (detached): it sleeps at least 10 ms before it can end, and a thread
        // that ends while it is detached is the case `start_connection` keeps clear of.
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
    let mut finisher = None;
    let mut threads = Vec::new();
    loop {
        let ready = wait_for(&listener, &wakened);
        drain(&wakened);
        if shared.stopping.load(Ordering::SeqCst) && !finishing {
            finishing = true;
            let why = lock(&shared.why).take();
            if let Some(why) = why {
                (config.info)(&why);
            }
            let started = {
                let (shared, engine) = (shared.clone(), engine.clone());
                std::thread::Builder::new()
                    .name("tsvz-stop".into())
                    .spawn(move || finish(&shared, &engine))
            };
            match started {
                Ok(thread) => finisher = Some(thread),
                // No thread to spare: finished here, with the connections that come meanwhile
                // left in the listener's queue.
                Err(_) => finish(&shared, &engine),
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
        let thread = start_connection(
            std::thread::Builder::new().name("tsvz-connection".into()),
            stream,
            engine.clone(),
            shared.clone(),
            config.info.clone(),
            config.verbose,
        );
        threads.extend(thread);
        reap(&mut threads);
    }
    // Every queued write is written: the pointer file goes first, so that a command that finds the
    // listener closed finds no handler (`tsvz stop` waits for this).
    held.release();
    drop(listener); // the clients still in its queue find their connection closed
    let _ = std::fs::remove_dir_all(&dir);
    // The stop cut every connection off: their threads end, once a row whose answer it cut off is
    // put back.
    for thread in threads.into_iter().chain(finisher) {
        let _ = thread.join();
    }
    (config.info)(&format!("stopped serving {}", store.display()));
    // A write it acknowledged before it was made, and lost: the stop said so, and so does serve.
    match engine.lost() {
        Some(lost) => Err(Error::Refused(format!(
            "{}: stopped serving, but {lost}",
            store.display()
        ))),
        None => Ok(()),
    }
}

/// Join the connection threads that have ended. A connection's thread is joined, never let go
/// of (detached) when it is started: glibc 2.41's `pthread_detach` reads the thread's descriptor
/// once more after it has made the thread detached, and a thread that ends in that moment frees
/// its stack, which holds that descriptor (a SIGSEGV in `pthread_detach`, seen under a flood of
/// connections that close at once; docs/limitations.md). A joined thread never frees its own.
fn reap(threads: &mut Vec<std::thread::JoinHandle<()>>) {
    let mut i = 0;
    while i < threads.len() {
        if threads[i].is_finished() {
            let _ = threads.swap_remove(i).join();
        } else {
            i += 1;
        }
    }
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
/// [`FINISH`] more to be written, then the rest is cut off: a row whose answer is cut off is put
/// back on the files by its connection's thread ([`give_back`]), which `serve` joins.
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
/// release build aborts on panic. The thread, for the caller to join ([`reap`]).
fn start_connection(
    builder: std::thread::Builder,
    stream: UnixStream,
    engine: Arc<Engine>,
    shared: Arc<Shared>,
    info: InfoFn,
    verbose: bool,
) -> Option<std::thread::JoinHandle<()>> {
    {
        let mut activity = lock(&shared.activity);
        if shared.stopping.load(Ordering::SeqCst) {
            return None;
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
            return None;
        }
    }
    let report = info.clone();
    let mine = shared.clone();
    match builder.spawn(move || serve_connection(id, stream, &engine, &shared, &info, verbose)) {
        Ok(thread) => Some(thread),
        Err(e) => {
            end_connection(&mine, id);
            report(&format!(
                "cannot start a thread for a connection ({e}); closed it"
            ));
            None
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
            Err(message) => engine.refuse(message),
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
            if owes && response.status == 0 {
                give_back(engine, &response, info);
            }
            return;
        }
    }
}

/// Put back the row a `pop` or `popitem` took, when its answer, the row's only copy, could not be
/// written (its client hung up, gave up waiting, or was cut off by a stop): `setdefault` of the
/// row, which keeps a row written for its key since, as the command line puts back a row it could
/// not print; on the files once a stop has closed the engine ([`Engine::put_back`]), and `serve`
/// joins this connection's thread before it returns. Said on `serve`'s standard error: that it was
/// put back as a notice (`-q` hides it), and the row taken as an error when it is not in its place
/// (a row written since stays; the files could not be written), which `-q` shows: else the row is
/// in no file and in no log.
fn give_back(engine: &Engine, response: &Response, info: &InfoFn) {
    let Some(row) = response.lines.first() else {
        return;
    };
    let line = protocol::setdefault_line(&protocol::decode_fields(row));
    let back = protocol::parse(&line).map(|request| engine.put_back(&request));
    let said = "the answer to a pop could not be written (its client went away)";
    match back {
        Ok(r) if r.status == 0 && r.lines.first() == Some(row) => {
            info(&format!("{}: {said}; it was put back", engine.path().display()));
        }
        // setdefault wrote nothing: the key reads another row.
        Ok(r) if r.status == 0 => engine.error(format!(
            "{said}; it was not put back, its key reads {} (written since, or stripped as the store now strips); the row taken was: {row}",
            r.lines.first().map_or("", String::as_str)
        )),
        Ok(r) => engine.error(format!(
            "{said}; it could not be put back ({}): {row}",
            r.message
        )),
        Err(e) => engine.error(format!("{said}; it could not be put back ({e}): {row}")),
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
/// no longer names it, which it removes once every acknowledged write is written, or no handler
/// holds it (its process ended); also when another stop, a signal or its idle timeout stopped it
/// first. An error when there is no handler, when it does not answer, when it is still there
/// after [`STOP_WAIT`], or, once it is gone, when it answered other than `#0`.
pub fn stop(store: &Path) -> Result<(), Error> {
    stop_within(store, STOP_WAIT)
}

/// How long a stop whose connection was closed unread waits before it asks again: ten times the
/// accept loop's pause when it has no descriptor for a connection, the one wait of a handler that
/// closes a connection for a reason of its own (no descriptor, no thread to start).
const ASK_AGAIN: Duration = Duration::from_millis(100);

/// What one `stop` request came to.
enum Asked {
    /// Answered: the handler stops. With what it says it could not do, when it answered other
    /// than `#0` (a write it acknowledged was lost): it stops all the same.
    Stopping(Option<String>),
    /// Closed unread: the handler is stopping already, or closed it for a reason of its own.
    Closed,
    /// Not answered in time, or answered otherwise: why.
    Failed(String),
    /// Not reached.
    Unreachable(String),
}

/// Send one `stop` request to the handler `info` names, waiting at most `wait` for its answer.
fn ask_stop(info: &pointer::Info, wait: Duration) -> Asked {
    let mut conn = match pointer::connect(info, None) {
        Ok(conn) => conn,
        Err(e) => return Asked::Unreachable(e.message),
    };
    let _ = conn.set_read_timeout(Some(wait.max(Duration::from_millis(1))));
    let answer = conn
        .write_all(b"stop\n")
        .and_then(|()| read_response(&mut BufReader::new(&conn)));
    match answer {
        Ok(response) if response.status != 0 && response.message.is_empty() => {
            Asked::Stopping(Some(format!("it answered #{}", response.status)))
        }
        Ok(response) if response.status != 0 => Asked::Stopping(Some(response.message)),
        Ok(_) => Asked::Stopping(None),
        Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => Asked::Failed(
            format!("the handler did not answer within {} s", wait.as_secs()),
        ),
        Err(_) => Asked::Closed,
    }
}

/// [`stop`], waiting at most `wait`. A connection closed unread is asked again once, after
/// [`ASK_AGAIN`]: a handler that answers then was serving, and stops; one that closes it again
/// is stopping, and is waited for. A handler that answers other than `#0` stops as well, and is
/// waited for the same; once it is gone, the stop fails with what it answered (as TSVZ.py's does
/// by its status).
fn stop_within(store: &Path, wait: Duration) -> Result<(), Error> {
    let refused = |why: String| Error::Refused(format!("{}: {why}", store.display()));
    let info = match pointer::lookup(store) {
        Ok(Some(info)) if pointer::held(store) => info,
        Ok(_) => return Err(refused("no handler is running".into())),
        Err(why) => return Err(refused(why)),
    };
    let deadline = Instant::now() + wait;
    let there = || pointer::names_handler(store, &info);
    let mut answered = None;
    for again in [false, true] {
        match ask_stop(&info, deadline.saturating_duration_since(Instant::now())) {
            Asked::Stopping(said) => {
                answered = said;
                break;
            }
            Asked::Failed(why) => return Err(refused(why)),
            // Closed again, or not reached once seen closing: it is stopping.
            _ if again => break,
            Asked::Closed => {
                std::thread::sleep(ASK_AGAIN);
                if !there() {
                    return Ok(()); // it stopped meanwhile
                }
            }
            Asked::Unreachable(_) if !there() => return Ok(()), // it stopped meanwhile
            Asked::Unreachable(why) => {
                return Err(refused(format!("the handler does not answer ({why})")));
            }
        }
    }
    if pointer::wait_gone(store, &info, deadline) {
        match answered {
            None => Ok(()),
            Some(why) => Err(refused(format!("the handler stopped, but {why}"))),
        }
    } else {
        // A stop whose flush took most of the wait can be answered with a lost write late, and the
        // handler needs longer than what is left: what it answered is said too (0.2.1 failed with
        // it at once).
        let said = answered.map_or_else(String::new, |why| format!("; {why}"));
        Err(refused(format!(
            "the handler (pid {}) has not stopped within {} s{said}",
            info.pid,
            wait.as_secs()
        )))
    }
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

    /// A pointer file of `store` naming a socket this test listens on: a stand-in handler, and
    /// the pointer file, locked as a handler holds it (dropped, the handler's process ended).
    fn stand_in(dir: &Path, store: &Path) -> (UnixListener, std::fs::File) {
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
        let held = std::fs::File::open(&at).unwrap();
        use std::os::fd::AsRawFd;
        // SAFETY: flock only operates on the descriptor, which `held` keeps open.
        assert_eq!(unsafe { libc::flock(held.as_raw_fd(), libc::LOCK_EX) }, 0);
        (listener, held)
    }

    /// Answer one `stop` on `listener` with `#0`; the connection, still open.
    fn answer_stop(listener: &UnixListener) -> UnixStream {
        use std::io::BufRead;
        let (mut conn, _) = listener.accept().unwrap();
        let mut line = String::new();
        BufReader::new(&conn).read_line(&mut line).unwrap();
        assert_eq!(line, "stop\n");
        conn.write_all(b"#0\n").unwrap();
        conn
    }

    #[test]
    fn a_stop_whose_connection_was_closed_for_another_reason_asks_again() {
        // A handler that closes a connection for a reason of its own (no descriptor, no thread to
        // start) is serving: the stop asked again is answered, and the handler stops, at once
        // (0.2.1 took the close for a stop under way and waited 30 s for it).
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("s.tsvz");
        let (listener, held) = stand_in(dir.path(), &store);
        let at = pointer::path_of(&store);
        let handler = std::thread::spawn(move || {
            drop(listener.accept().unwrap()); // closed unread, and serving on
            let conn = answer_stop(&listener);
            std::fs::remove_file(at).unwrap(); // gone
            drop((conn, held));
        });
        let started = Instant::now();
        stop_within(&store, Duration::from_secs(20)).unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
        handler.join().unwrap();
    }

    #[test]
    fn a_stop_the_handler_answers_with_a_failure_waits_for_it_to_go_and_says_what_it_answered() {
        // A handler that lost a write it acknowledged answers `stop` with #1 and stops all the same:
        // the stop waits until it is gone, as after #0, then fails saying what it answered
        // (0.2.1 failed at once with "stop failed", as if it were still running).
        use std::io::BufRead;
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("s.tsvz");
        let (listener, held) = stand_in(dir.path(), &store);
        let at = pointer::path_of(&store);
        let handler = std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().unwrap();
            let mut line = String::new();
            BufReader::new(&conn).read_line(&mut line).unwrap();
            let lost = Response::failed(1, "writes answered before they were made were lost");
            conn.write_all(&lost.encode()).unwrap();
            std::thread::sleep(Duration::from_millis(400)); // writing its queue
            std::fs::remove_file(at).unwrap(); // gone
            drop((conn, held));
        });
        let started = Instant::now();
        let e = stop_within(&store, Duration::from_secs(20)).unwrap_err();
        assert!(started.elapsed() >= Duration::from_millis(350), "{e}");
        let said = e.to_string();
        assert!(
            said.contains("the handler stopped, but writes answered"),
            "{said}"
        );
        assert!(!said.contains("stop failed"), "{said}");
        handler.join().unwrap();
    }

    #[test]
    fn a_stop_of_a_handler_that_died_after_its_answer_ends_at_once() {
        // Killed after it answered and before it removed its pointer file: the file still names
        // it, but no process holds its lock, so the handler is gone (0.2.1 waited 30 s, then
        // failed); and a pointer file that no handler holds is no handler to stop.
        let _quiet = crate::lock::quiet(); // no child holds the lock dropped below
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("s.tsvz");
        let (listener, held) = stand_in(dir.path(), &store);
        let handler = std::thread::spawn(move || {
            let conn = answer_stop(&listener);
            drop(held); // its process ended: the system lets go of its lock
            conn
        });
        let started = Instant::now();
        stop_within(&store, Duration::from_secs(20)).unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
        drop(handler.join());
        assert!(pointer::path_of(&store).exists(), "left as it was");
        let e = stop_within(&store, Duration::from_secs(20)).unwrap_err();
        assert!(e.to_string().contains("no handler is running"), "{e}");
    }

    #[test]
    fn a_stop_that_finds_the_handler_stopping_waits_until_it_is_gone() {
        // Another stop, or a signal, came first: the handler closes this one's connection
        // unread, and this stop waits for it to be gone as the first does, and succeeds.
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("s.tsvz");
        let (listener, _held) = stand_in(dir.path(), &store);
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
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("s.tsvz");
        let (listener, _held) = stand_in(dir.path(), &store);
        let handler = std::thread::spawn(move || answer_stop(&listener));
        let e = stop_within(&store, Duration::from_millis(500)).unwrap_err();
        assert!(e.to_string().contains("has not stopped within"), "{e}");
        drop(handler.join());
    }

    #[test]
    fn a_stop_that_runs_out_its_wait_says_what_the_handler_answered() {
        // A handler that lost a write it acknowledged answers #1, and is still there when the
        // stop's wait ends (the flush took most of it): the stop says what it answered, as
        // 0.2.1's "stop failed: ..." did, not only that the handler is still there.
        use std::io::BufRead;
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("s.tsvz");
        let (listener, _held) = stand_in(dir.path(), &store);
        let handler = std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().unwrap();
            let mut line = String::new();
            BufReader::new(&conn).read_line(&mut line).unwrap();
            let lost = Response::failed(1, "writes answered before they were made were lost");
            conn.write_all(&lost.encode()).unwrap();
            conn // never gone: its pointer file stays, locked
        });
        let said = stop_within(&store, Duration::from_millis(500))
            .unwrap_err()
            .to_string();
        assert!(said.contains("has not stopped within"), "{said}");
        assert!(
            said.ends_with("; writes answered before they were made were lost"),
            "{said}"
        );
        drop(handler.join());
    }

    #[test]
    fn a_stop_gives_up_on_a_handler_that_does_not_answer() {
        use std::io::BufRead;
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("s.tsvz");
        let (listener, _held) = stand_in(dir.path(), &store);
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

    /// Whether SIGTERM is blocked in this thread.
    fn blocks_sigterm() -> bool {
        // SAFETY: pthread_sigmask with no new set only reads this thread's mask into `mask`, and
        // sigismember only reads it.
        unsafe {
            let mut mask: libc::sigset_t = std::mem::zeroed();
            libc::pthread_sigmask(libc::SIG_BLOCK, std::ptr::null(), &mut mask);
            libc::sigismember(&mask, libc::SIGTERM) == 1
        }
    }

    /// A config with no logging, which idles out after `idle`.
    fn quiet_config(idle: Option<Duration>) -> Config {
        Config {
            idle,
            group: None,
            mode: None,
            log: None,
            errors: None,
            info: Arc::new(|_: &str| {}),
            verbose: false,
        }
    }

    #[test]
    fn a_serve_that_does_not_get_the_pointer_file_leaves_the_signals_unblocked() {
        // The signals are blocked once `serve` holds the pointer file, never before: a start
        // whose first access to it hangs (a hard-mounted file system whose server is down) then
        // ends on a signal, as 0.2.1's did, also on the command line's path, which gives no mask
        // back (`serve_to_exit`: a block taken before the file would stay).
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("s.tsvz");
        let (_listener, _held) = stand_in(dir.path(), &store);
        let outcome = std::thread::spawn(move || {
            let before = blocks_sigterm();
            let a = serve(&store, quiet_config(None)).unwrap_err().to_string();
            let after_serve = blocks_sigterm();
            let b = serve_to_exit(&store, quiet_config(None))
                .unwrap_err()
                .to_string();
            (before, after_serve, blocks_sigterm(), a, b)
        })
        .join()
        .unwrap();
        let (before, after_serve, after_to_exit, a, b) = outcome;
        assert!(a.contains("already served by pid 4242"), "{a}");
        assert!(b.contains("already served by pid 4242"), "{b}");
        assert_eq!((before, after_serve, after_to_exit), (false, false, false));
    }

    #[test]
    fn serve_gives_its_thread_the_signal_mask_back_and_ends_its_signal_thread() {
        // A program that embeds `serve` gets SIGINT, SIGTERM and SIGHUP again once it returns, and
        // keeps no thread waiting for them (0.2.1 left them blocked and the thread in `sigwait`).
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("s.tsvz");
        let serving = std::thread::spawn(move || {
            let before = blocks_sigterm();
            serve(&store, quiet_config(Some(Duration::from_millis(200)))).unwrap();
            (before, blocks_sigterm())
        });
        assert_eq!(serving.join().unwrap(), (false, false));
        #[cfg(target_os = "linux")]
        {
            let named = |name: &str| {
                std::fs::read_dir("/proc/self/task").unwrap().any(|t| {
                    let comm = t.unwrap().path().join("comm");
                    std::fs::read_to_string(comm).is_ok_and(|c| c.trim() == name)
                })
            };
            // A thread that was joined can stay listed for a moment: the kernel wakes its joiner
            // before it takes the thread's entry away. Gone within milliseconds; 5 s is for a host
            // busy enough to starve it.
            let deadline = Instant::now() + Duration::from_secs(5);
            while named("tsvz-signals") && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(1));
            }
            assert!(!named("tsvz-signals"), "the signal thread is left");
        }
    }

    #[test]
    fn a_pop_s_row_is_put_back_or_printed_by_what_became_of_it() {
        // The row taken is said to have been put back only when its key reads it again; else it
        // is printed as an error (which `-q` does not hide), with the row its key reads: a row
        // written since stays. Once a stop has closed the engine, it is put back on the files
        // (0.2.3 put back nothing then), and printed when they cannot be written.
        use crate::Warning;
        let dir = tempfile::tempdir().unwrap();
        let notices = Arc::new(Mutex::new(Vec::new()));
        let errors = Arc::new(Mutex::new(Vec::new()));
        let (n, e) = (notices.clone(), errors.clone());
        let info: InfoFn = Arc::new(move |m: &str| lock(&n).push(m.to_string()));
        let on_error: crate::WarnFn = Arc::new(move |w: &Warning| lock(&e).push(w.message.clone()));
        let path = dir.path().join("s.tsvz");
        std::fs::write(&path, "a\t1\nb\t2\nc\t3\nd\t4\n").unwrap();
        let engine = Engine::open(&path, None, Some(on_error)).unwrap();
        let ask = |line: &str| engine.handle(&protocol::parse(line).unwrap(), None);
        let taken = |key: &str| {
            let popped = ask(&format!("pop\t{key}"));
            assert_eq!((popped.status, popped.lines.len()), (0, 1), "{popped:?}");
            popped
        };
        // Nothing written since: put back, as a notice only.
        give_back(&engine, &taken("a"), &info);
        assert_eq!(ask("get\ta").lines, ["a\t1"]);
        // (Copies, not guards: a failing assertion that locks again, to say what it saw, would wait
        // for itself.)
        let notes = lock(&notices).clone();
        assert!(notes[0].ends_with("; it was put back"), "{notes:?}");
        let said = lock(&errors).clone();
        assert!(said.is_empty(), "{said:?}");
        // A row written since stays: the row taken is printed, and not said to be put back.
        let second = taken("b");
        assert_eq!(ask("set\tb\t9").status, 0);
        give_back(&engine, &second, &info);
        assert_eq!(ask("get\tb").lines, ["b\t9"]);
        let notes = lock(&notices).clone();
        assert_eq!(notes.len(), 1, "no second notice: {notes:?}");
        let said = lock(&errors).clone();
        assert_eq!(said.len(), 1, "{said:?}");
        assert!(
            said[0].contains("it was not put back, its key reads b\t9"),
            "{said:?}"
        );
        assert!(said[0].ends_with("the row taken was: b\t2"), "{said:?}");
        // A closed engine (a stop has written its queue): put back on the files, synced.
        let (third, fourth) = (taken("c"), taken("d"));
        engine.close();
        give_back(&engine, &third, &info);
        let notes = lock(&notices).clone();
        assert_eq!(notes.len(), 2, "{notes:?}");
        assert!(notes[1].ends_with("; it was put back"), "{notes:?}");
        let left = crate::Store::open(&path, crate::Options::default()).unwrap();
        assert_eq!(left.get("c").map(|r| r.to_vec()), Some(vec!["3".into()]));
        assert!(left.get("d").is_none());
        // Files that cannot be written (a directory where the store was): the row is printed.
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        give_back(&engine, &fourth, &info);
        let said = lock(&errors).clone();
        assert_eq!(said.len(), 2, "{said:?}");
        assert!(said[1].contains("it could not be put back ("), "{said:?}");
        assert!(said[1].ends_with("): d\t4"), "{said:?}");
        assert_eq!(lock(&notices).len(), 2);
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
        assert!(started.join().unwrap().is_some());
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
        assert!(
            start_connection(
                std::thread::Builder::new(),
                end,
                engine.clone(),
                shared.clone(),
                info,
                false
            )
            .is_none()
        );
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
        assert!(
            start_connection(
                impossible,
                end,
                engine.clone(),
                shared.clone(),
                info.clone(),
                false
            )
            .is_none()
        );
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
        let thread = start_connection(
            std::thread::Builder::new(),
            end,
            engine,
            shared,
            info,
            false,
        );
        assert!(thread.is_some());
        client.write_all(b"len\n").unwrap();
        let mut answer = String::new();
        client.take(5).read_to_string(&mut answer).unwrap();
        assert_eq!(answer, "0\n#0\n");
    }
}
