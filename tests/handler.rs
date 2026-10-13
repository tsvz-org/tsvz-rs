//! A real `tsvz serve`, driven over its socket and through the command line (§20.8, §21).

use std::fs;
use std::io::{BufReader, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use tsvz::handler::Response;
use tsvz::handler::pointer;
use tsvz::handler::protocol::read_response;

fn tsvz() -> Command {
    Command::new(env!("CARGO_BIN_EXE_tsvz"))
}

/// A running `tsvz serve`; killed if a test leaves it running.
struct Served {
    child: Child,
    stderr: PathBuf,
}

impl Served {
    fn start(store: &Path, extra: &[&str]) -> Served {
        let stderr = store.with_extension("serve-stderr");
        let child = tsvz()
            .arg("serve")
            .arg(store)
            .args(extra)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(fs::File::create(&stderr).unwrap())
            .spawn()
            .unwrap();
        let mut served = Served { child, stderr };
        let deadline = Instant::now() + Duration::from_secs(10);
        while connect(store).is_none() {
            if let Some(status) = served.child.try_wait().unwrap() {
                panic!("serve exited {status}: {}", served.stderr());
            }
            assert!(Instant::now() < deadline, "no handler: {}", served.stderr());
            std::thread::sleep(Duration::from_millis(20));
        }
        served
    }

    fn stderr(&self) -> String {
        fs::read_to_string(&self.stderr).unwrap_or_default()
    }

    /// Wait for the handler to exit; its exit status.
    fn wait(&mut self) -> i32 {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status.code().unwrap_or(-1);
            }
            assert!(
                Instant::now() < deadline,
                "serve did not exit: {}",
                self.stderr()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn signal(&self, signal: i32) {
        // SAFETY: kill only sends a signal to the child, which is still ours to reap.
        unsafe { libc::kill(self.child.id() as i32, signal) };
    }
}

impl Drop for Served {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A connection to the store's handler, through a trusted pointer file.
fn connect(store: &Path) -> Option<UnixStream> {
    let info = pointer::lookup(store).ok()??;
    pointer::connect(&info, Some(Duration::from_secs(10))).ok()
}

/// Send raw request bytes on `conn` and read one response.
fn send(conn: &mut UnixStream, bytes: &[u8]) -> Response {
    conn.write_all(bytes).unwrap();
    read_response(&mut BufReader::new(conn.try_clone().unwrap())).unwrap()
}

fn ask(store: &Path, line: &str) -> Response {
    let mut conn = connect(store).expect("a handler");
    send(&mut conn, format!("{line}\n").as_bytes())
}

fn run(dir: &Path, args: &[&str], stdin: &[u8]) -> Output {
    let mut child = tsvz()
        .current_dir(dir)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin).unwrap();
    child.wait_with_output().unwrap()
}

fn text(out: &[u8]) -> String {
    String::from_utf8_lossy(out).into_owned()
}

fn store(dir: &Path, content: &str) -> PathBuf {
    let p = dir.join("s.tsvz");
    fs::write(&p, content).unwrap();
    p
}

#[test]
fn a_client_that_hangs_up_before_its_answer_leaves_the_handler_serving() {
    let dir = tempfile::tempdir().unwrap();
    let rows: String = (0..50_000)
        .map(|i| format!("k{i}\t{}\n", "v".repeat(40)))
        .collect();
    let s = store(dir.path(), &rows);
    let _served = Served::start(&s, &[]);
    for _ in 0..5 {
        let mut conn = connect(&s).unwrap();
        conn.write_all(b"read\n").unwrap();
    }
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(ask(&s, "len").lines, ["50000"]);
}

#[test]
fn stop_ends_the_connections_still_open() {
    use std::io::Read;
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path(), "k\tv\n");
    let mut served = Served::start(&s, &[]);
    let mut idle = connect(&s).unwrap();
    idle.write_all(b"len").unwrap(); // a request never finished
    let stop = run(dir.path(), &["stop", "s.tsvz"], b"");
    assert_eq!(stop.status.code(), Some(0), "{}", text(&stop.stderr));
    assert_eq!(served.wait(), 0);
    idle.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut byte = [0u8; 1];
    assert_eq!(idle.read(&mut byte).unwrap_or(0), 0, "closed");
}

#[test]
fn a_socket_path_too_long_to_bind_fails_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path(), "k\tv\n");
    let deep = dir.path().join("d".repeat(120));
    fs::create_dir(&deep).unwrap();
    // --x-mode puts the socket under the temporary directory, here one too deep to bind in.
    let out = tsvz()
        .current_dir(dir.path())
        .env("TMPDIR", &deep)
        .args(["serve", "s.tsvz", "--x-mode", "600"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    assert!(!text(&out.stderr).is_empty());
    assert!(
        !pointer::path_of(&s).exists(),
        "no pointer file is left behind"
    );
    assert_eq!(
        fs::read_dir(&deep).unwrap().count(),
        0,
        "nor a socket directory"
    );
}

#[test]
fn serve_answers_until_stop_and_leaves_nothing_behind() {
    let dir = tempfile::tempdir().unwrap();
    let s = dir.path().join("s.tsvz");
    let mut served = Served::start(&s, &[]);
    assert_eq!(fs::read(&s).unwrap(), b"", "serve creates a missing store");
    assert_eq!(ask(&s, "set\tk\tv").status, 0);
    assert_eq!(ask(&s, "get\tk").lines, ["k\tv"]);
    let info = pointer::lookup(&s).unwrap().unwrap();
    let socket = PathBuf::from(&info.address["unix:".len()..]);
    let stop = run(dir.path(), &["stop", "s.tsvz"], b"");
    assert_eq!(stop.status.code(), Some(0), "{}", text(&stop.stderr));
    assert_eq!(served.wait(), 0, "{}", served.stderr());
    assert!(
        !pointer::path_of(&s).exists(),
        "the pointer file is removed"
    );
    assert!(
        !socket.parent().unwrap().exists(),
        "the socket directory is removed"
    );
    assert_eq!(fs::read_to_string(&s).unwrap(), "k\tv\n");
    assert!(served.stderr().contains("serving"), "{}", served.stderr());
}

#[test]
fn a_second_serve_exits_1_and_the_first_keeps_serving() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path(), "k\tv\n");
    let _served = Served::start(&s, &[]);
    let second = run(dir.path(), &["serve", "s.tsvz"], b"");
    assert_eq!(second.status.code(), Some(1));
    assert!(
        text(&second.stderr).contains("already served by pid"),
        "{}",
        text(&second.stderr)
    );
    assert_eq!(ask(&s, "len").lines, ["1"]);
}

#[test]
fn signals_stop_the_handler_cleanly() {
    for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), "");
        let mut served = Served::start(&s, &[]);
        let mut conn = connect(&s).unwrap();
        for i in 0..50 {
            assert_eq!(
                send(&mut conn, format!("set\tk{i}\tv\n").as_bytes()).status,
                0
            );
        }
        served.signal(signal);
        assert_eq!(served.wait(), 0, "signal {signal}: {}", served.stderr());
        assert!(!pointer::path_of(&s).exists(), "signal {signal}");
        assert_eq!(
            fs::read_to_string(&s).unwrap().lines().count(),
            50,
            "signal {signal}"
        );
    }
}

#[test]
fn an_idle_handler_stops_after_its_timeout() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path(), "");
    let mut served = Served::start(&s, &["--x-idle-timeout", "0.5"]);
    assert_eq!(served.wait(), 0);
    assert!(served.stderr().contains("idle"), "{}", served.stderr());
}

#[test]
fn the_socket_is_private_unless_widened() {
    let mode_of = |s: &Path| {
        let info = pointer::lookup(s).unwrap().unwrap();
        let socket = PathBuf::from(&info.address["unix:".len()..]);
        let dir = fs::metadata(socket.parent().unwrap()).unwrap();
        let sock = fs::metadata(&socket).unwrap();
        (sock.mode() & 0o777, dir.mode() & 0o777, sock.gid())
    };
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path(), "");
    {
        let _served = Served::start(&s, &[]);
        let (sock, d, _) = mode_of(&s);
        assert_eq!((sock, d), (0o600, 0o700));
    }
    {
        let _served = Served::start(&s, &["--x-mode", "666"]);
        let (sock, d, _) = mode_of(&s);
        assert_eq!((sock, d), (0o666, 0o711));
    }
    // SAFETY: getegid only reads the process's group id.
    let gid = unsafe { libc::getegid() };
    {
        let _served = Served::start(&s, &["--x-group", &gid.to_string()]);
        let (sock, d, g) = mode_of(&s);
        assert_eq!((sock, d, g), (0o660, 0o710, gid));
    }
    let bad = run(dir.path(), &["serve", "s.tsvz", "--x-mode", "999"], b"");
    assert_eq!(bad.status.code(), Some(2));
}

#[test]
fn serve_takes_a_strict_store_and_stop_needs_a_handler() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("s.tsv"), "k\tv\n").unwrap();
    let loose = run(dir.path(), &["serve", "s.tsv"], b"");
    assert_eq!(loose.status.code(), Some(1));
    assert!(
        text(&loose.stderr).contains("strict"),
        "{}",
        text(&loose.stderr)
    );
    assert!(!dir.path().join("s.tsv.serve").exists());
    store(dir.path(), "");
    let stop = run(dir.path(), &["stop", "s.tsvz"], b"");
    assert_eq!(stop.status.code(), Some(1));
    assert!(!text(&stop.stderr).is_empty());
}

#[test]
fn a_bulk_request_cut_off_before_its_end_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path(), "");
    let _served = Served::start(&s, &[]);
    let mut conn = connect(&s).unwrap();
    conn.write_all(b"set\t-\na\t1\nb\t2\n").unwrap();
    drop(conn);
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(ask(&s, "len").lines, ["0"]);
    assert_eq!(fs::read(&s).unwrap(), b"");
}

#[test]
fn one_connection_carries_many_requests_including_bad_ones() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path(), "a\tx\n");
    let _served = Served::start(&s, &[]);
    let mut conn = connect(&s).unwrap();
    assert_eq!(send(&mut conn, b"get\n").status, 2);
    assert_eq!(send(&mut conn, b"bogus\tk\n").status, 2);
    assert_eq!(send(&mut conn, b"len\r\n").lines, ["1"]);
    let big = "x".repeat((1 << 20) - 16);
    assert_eq!(
        send(&mut conn, format!("set\tbig\t{big}\n").as_bytes()).status,
        0
    );
    let r = send(&mut conn, b"get\tbig\n");
    assert_eq!(r.lines, [format!("big\t{big}")]);
    assert_eq!(send(&mut conn, b"set\t-\nb\t2\n#\n").status, 0);
    assert_eq!(send(&mut conn, b"keys\n").lines, ["a", "big", "b"]);
}

#[test]
fn a_stale_pointer_is_taken_over() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path(), "k\tv\n");
    let p = pointer::path_of(&s);
    fs::write(
        &p,
        format!(
            "tsvz-handler\t1\naddress\tunix:{}\nhost\t{}\npid\t999999999\n",
            dir.path().join("gone.sock").display(),
            pointer::hostname()
        ),
    )
    .unwrap();
    fs::set_permissions(&p, fs::Permissions::from_mode(0o600)).unwrap();
    let _served = Served::start(&s, &[]);
    assert_eq!(ask(&s, "len").lines, ["1"]);
}
