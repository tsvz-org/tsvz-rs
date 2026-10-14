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
fn a_quiet_handler_still_reports_writes_that_fail() {
    // -q prints errors only, and a write that fails is one.
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path(), "a\t1\n");
    let mut served = Served::start(&s, &["-q"]);
    fs::remove_file(&s).unwrap();
    fs::create_dir(&s).unwrap(); // every append fails, even as root
    assert_eq!(ask(&s, "--x-written\tset\tb\t2").status, 1);
    fs::remove_dir(&s).unwrap();
    let stop = run(dir.path(), &["stop", "s.tsvz"], b"");
    assert_eq!(stop.status.code(), Some(0), "{}", text(&stop.stderr));
    assert_eq!(served.wait(), 0, "{}", served.stderr());
    let said = served.stderr();
    assert!(said.contains("a batch of 1 writes failed"), "{said:?}");
    assert!(!said.contains("serving"), "and nothing else: {said:?}");
}

#[test]
fn a_routed_pop_whose_row_cannot_be_printed_puts_it_back() {
    // The handler deletes the row before the command prints it: a row that cannot be printed
    // (here to a pipe nobody reads) is put back, not lost.
    use std::os::fd::{FromRawFd, OwnedFd};
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path(), "a\t1\nb\t2\n");
    let _served = Served::start(&s, &[]);
    let mut fds = [0; 2];
    // SAFETY: pipe writes two new descriptors into fds.
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
    // SAFETY: both descriptors are new and owned here alone.
    let (read, write) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    drop(read);
    for op in [&["pop", "s.tsvz", "a"][..], &["popitem", "s.tsvz"][..]] {
        let out = tsvz()
            .current_dir(dir.path())
            .args(op)
            .stdin(Stdio::null())
            .stdout(Stdio::from(write.try_clone().unwrap()))
            .stderr(Stdio::piped())
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(1), "{op:?}: {}", text(&out.stderr));
        assert!(
            text(&out.stderr).contains("it was put back"),
            "{}",
            text(&out.stderr)
        );
        let mut keys = ask(&s, "keys").lines;
        keys.sort();
        assert_eq!(keys, ["a", "b"], "{op:?}: {}", text(&out.stderr));
    }
    assert_eq!(ask(&s, "get\ta").lines, ["a\t1"]);
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

fn strings(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

#[test]
fn the_rust_client_speaks_to_a_handler() {
    use tsvz::handler::Client;
    use tsvz::{Error, Record};
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path(), "a\t1\n#_defaults_#\td\n");
    let mut served = Served::start(&s, &[]);
    let mut c = Client::connect(&s).unwrap().sync(true);
    let data = |k: &str, v: &str| Record::Data {
        key: k.into(),
        values: vec![v.into()],
    };
    c.set(&[data("#k", "x\ty")]).unwrap();
    assert_eq!(c.get("#k").unwrap(), Some(strings(&["#k", "x\ty"])));
    assert_eq!(
        c.get("zz").unwrap(),
        None,
        "a missing key, even with defaults"
    );
    assert!(c.has("a").unwrap() && !c.has("zz").unwrap());
    assert_eq!(c.len().unwrap(), 2);
    assert_eq!(c.keys().unwrap(), strings(&["a", "#k"]));
    c.set(&[data("b", "2"), data("c", "3")]).unwrap();
    assert_eq!(c.pop("b").unwrap(), Some(strings(&["b", "2"])));
    assert_eq!(c.pop("b").unwrap(), None);
    assert_eq!(c.popitem(true).unwrap(), Some(strings(&["c", "3"])), "last");
    assert_eq!(
        c.popitem(false).unwrap(),
        Some(strings(&["a", "1"])),
        "first"
    );
    assert_eq!(c.setdefault("n", &["v"]).unwrap(), strings(&["n", "v"]));
    c.delete(&["n"]).unwrap();
    assert_eq!(c.read().unwrap(), [strings(&["#k", "x\ty"])]);
    assert!(c.verify().unwrap().is_empty());
    c.scrub().unwrap();
    assert_eq!(
        c.request(&["get"], None).unwrap().status,
        2,
        "raw requests keep their status"
    );
    match c.setdefault("k", &[]) {
        Err(Error::Handler { status: 2, .. }) => {}
        other => panic!("setdefault without a value: {other:?}"),
    }
    c.clear().unwrap();
    assert_eq!(c.len().unwrap(), 0);
    c.stop().unwrap();
    assert_eq!(served.wait(), 0);
    assert!(matches!(Client::connect(&s), Err(Error::NoHandler(_))));
}

#[test]
fn many_clients_at_once_all_get_their_answers() {
    use tsvz::handler::Client;
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path(), "");
    let _served = Served::start(&s, &[]);
    let threads: Vec<_> = (0..32)
        .map(|t| {
            let s = s.clone();
            std::thread::spawn(move || {
                let mut c = Client::connect(&s).unwrap();
                for i in 0..50 {
                    let key = format!("k{t}-{i}");
                    c.set(&[tsvz::Record::Data {
                        key: key.clone(),
                        values: vec![i.to_string()],
                    }])
                    .unwrap();
                    assert_eq!(c.get(&key).unwrap(), Some(vec![key, i.to_string()]));
                }
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    assert_eq!(Client::connect(&s).unwrap().len().unwrap(), 32 * 50);
}

#[test]
fn a_client_whose_handler_dies_reports_the_request_as_lost() {
    use tsvz::handler::Client;
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path(), "a\t1\n");
    let mut served = Served::start(&s, &[]);
    let mut c = Client::connect(&s).unwrap();
    assert_eq!(c.len().unwrap(), 1);
    served.child.kill().unwrap();
    served.child.wait().unwrap();
    let err = c.len().unwrap_err();
    assert!(matches!(err, tsvz::Error::HandlerLost(_)), "{err:?}");
    assert!(err.to_string().contains("may or may not"), "{err}");
}

/// (exit status, stdout, stderr) of a command line run in `dir`.
fn cli(dir: &Path, args: &[&str], stdin: &[u8]) -> (i32, String, String) {
    let out = run(dir, args, stdin);
    (
        out.status.code().unwrap_or(-1),
        text(&out.stdout),
        text(&out.stderr),
    )
}

#[test]
fn routed_commands_print_what_the_files_give() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    fs::write(
        d.join("s.csvz"),
        "#_defaults_#,d1,d2\n<#>h,v<lt>,x<sep>y\na,1\nb,2\n",
    )
    .unwrap();
    let _served = Served::start(&d.join("s.csvz"), &[]);
    let reads: &[&[&str]] = &[
        &["--format", "records", "read", "s.csvz"],
        &["--format", "table", "read", "s.csvz"],
        &["--format", "records", "get", "s.csvz", "a  ", "zz", "#h"],
        &["--format", "records", "has", "s.csvz", "b"],
        &["--format", "records", "has", "s.csvz", "zz"],
        &["--format", "records", "len", "s.csvz"],
        &["--format", "records", "keys", "s.csvz"],
    ];
    let same = |stage: &str| {
        for args in reads {
            let routed = cli(d, args, b"");
            let mut direct_args = vec!["--x-direct"];
            direct_args.extend_from_slice(args);
            let direct = cli(d, &direct_args, b"");
            assert_eq!(
                (routed.0, &routed.1),
                (direct.0, &direct.1),
                "{stage}: {args:?}: {}",
                routed.2
            );
        }
    };
    same("start");
    let (code, _, err) = cli(d, &["-v", "set", "s.csvz", "c", "3,4"], b"");
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("via the handler"), "{err}");
    same("set");
    assert_eq!(
        cli(d, &["set", "s.csvz", "-"], b"e,5\n#_defaults_#,n\nf,6\n").0,
        0
    );
    same("set -");
    assert_eq!(cli(d, &["delete", "s.csvz", "a", "zz"], b"").0, 0);
    assert_eq!(cli(d, &["delete", "s.csvz", "-"], b"e\n").0, 0);
    same("delete");
    let pop = cli(d, &["--format", "records", "pop", "s.csvz", "b"], b"");
    assert_eq!((pop.0, pop.1.as_str()), (0, "b,2,d2\n"));
    assert_eq!(cli(d, &["pop", "s.csvz", "b"], b"").0, 3);
    let item = cli(
        d,
        &["--format", "records", "popitem", "s.csvz", "first"],
        b"",
    );
    assert_eq!((item.0, item.1.as_str()), (0, "<#>h,v<lt>,x<sep>y\n"));
    let sd = cli(
        d,
        &["--format", "records", "setdefault", "s.csvz", "g", "7"],
        b"",
    );
    assert_eq!((sd.0, sd.1.as_str()), (0, "g,7,\n"));
    assert_eq!(
        cli(d, &["setdefault", "s.csvz", "#_defaults_#", "x"], b"").0,
        2
    );
    same("pop, popitem, setdefault");
    assert_eq!(cli(d, &["clear", "s.csvz"], b"").0, 0);
    same("clear");
    assert_eq!(cli(d, &["len", "s.csvz"], b"").1, "0\n");
}

#[test]
fn an_unreadable_part_reads_the_same_through_the_handler() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let s = store(d, "a\t1\n");
    fs::create_dir(d.join("s.tsvz.1")).unwrap();
    fs::write(d.join("s.tsvz.2"), "b\t2\n").unwrap();
    let _served = Served::start(&s, &[]);
    let routed = cli(d, &["--format", "records", "read", "s.tsvz"], b"");
    assert_eq!((routed.0, routed.1.as_str()), (1, "a\t1\nb\t2\n"));
    assert!(routed.2.contains("s.tsvz.1"), "{}", routed.2);
    assert_eq!(cli(d, &["pop", "s.tsvz", "b"], b"").0, 1);
    assert_eq!(fs::read_to_string(d.join("s.tsvz.2")).unwrap(), "b\t2\n");
}

/// A trusted pointer to a socket of this test, whose "handler" reads one line and hangs up;
/// the number of connections it took.
fn rude_handler(store: &Path) -> std::sync::Arc<std::sync::atomic::AtomicUsize> {
    use std::io::BufRead;
    use std::os::unix::net::UnixListener;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let dir = tempfile::Builder::new()
        .prefix("rude")
        .tempdir_in("/tmp")
        .unwrap();
    let path = dir.path().join("s");
    let listener = UnixListener::bind(&path).unwrap();
    let p = pointer::path_of(store);
    fs::write(
        &p,
        format!(
            "tsvz-handler\t1\naddress\tunix:{}\nhost\t{}\npid\t1\n",
            path.display(),
            pointer::hostname()
        ),
    )
    .unwrap();
    fs::set_permissions(&p, fs::Permissions::from_mode(0o644)).unwrap();
    let count = std::sync::Arc::new(AtomicUsize::new(0));
    let seen = count.clone();
    std::thread::spawn(move || {
        let _keep = dir;
        for conn in listener.incoming() {
            seen.fetch_add(1, Ordering::SeqCst);
            let mut reader = std::io::BufReader::new(conn.unwrap());
            let mut line = String::new();
            let _ = reader.read_line(&mut line);
        }
    });
    count
}

#[test]
fn a_handler_that_hangs_up_falls_back_for_reads_and_fails_writes() {
    use std::sync::atomic::Ordering;
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let s = store(d, "k\tv\n");
    let contacts = rude_handler(&s);
    let (code, out, err) = cli(d, &["--format", "records", "get", "s.tsvz", "k"], b"");
    assert_eq!((code, out.as_str()), (0, "k\tv\n"), "{err}");
    assert!(err.contains("lost the handler"), "{err}");
    let (code, _, err) = cli(d, &["set", "s.tsvz", "n", "w"], b"");
    assert_eq!(code, 1);
    assert!(err.contains("may or may not"), "{err}");
    assert_eq!(contacts.load(Ordering::SeqCst), 2);
    // --x-direct, verify and parts never contact a handler; neither does a loose file.
    assert_eq!(cli(d, &["--x-direct", "get", "s.tsvz", "k"], b"").0, 0);
    assert_eq!(cli(d, &["verify", "s.tsvz"], b"").0, 0);
    assert_eq!(cli(d, &["parts", "s.tsvz"], b"").0, 0);
    fs::write(d.join("l.tsv"), "k\tv\n").unwrap();
    fs::copy(pointer::path_of(&s), d.join("l.tsv.serve")).unwrap();
    assert_eq!(cli(d, &["get", "l.tsv", "k"], b"").0, 0);
    assert_eq!(contacts.load(Ordering::SeqCst), 2);
}

#[test]
fn an_untrusted_pointer_is_ignored_with_a_warning() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let s = store(d, "k\tv\n");
    let p = pointer::path_of(&s);
    fs::write(&p, "tsvz-handler\t1\naddress\ttcp:127.0.0.1:9\nhost\tx\n").unwrap();
    fs::set_permissions(&p, fs::Permissions::from_mode(0o600)).unwrap();
    let (code, out, err) = cli(d, &["--format", "records", "get", "s.tsvz", "k"], b"");
    assert_eq!((code, out.as_str()), (0, "k\tv\n"));
    assert!(err.contains("ignoring the pointer file"), "{err}");
    let (_, _, quiet) = cli(d, &["-q", "get", "s.tsvz", "k"], b"");
    assert_eq!(quiet, "");
}

#[test]
fn a_bulk_request_with_an_unknown_option_still_reads_its_records_as_records() {
    // Its record lines must not run as requests (a tombstone `clear` would empty the store).
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path(), "a\t1\n");
    let _served = Served::start(&s, &[]);
    let mut conn = connect(&s).unwrap();
    let r = send(&mut conn, b"--x-future\tset\t-\nclear\nb\t2\n#\n");
    assert_eq!(r.status, 2, "{r:?}");
    assert_eq!(
        send(&mut conn, b"keys\n").lines,
        ["a"],
        "the connection stays in step"
    );
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(fs::read_to_string(&s).unwrap(), "a\t1\n");
}

#[test]
fn a_handler_whose_standard_error_is_closed_keeps_serving() {
    // `tsvz serve s 2>&1 | tee log` whose tee dies: every request's -v line hits a closed pipe.
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path(), "a\t1\n");
    let mut child = tsvz()
        .args(["-v", "serve"])
        .arg(&s)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while connect(&s).is_none() {
        assert!(Instant::now() < deadline, "no handler");
        std::thread::sleep(Duration::from_millis(20));
    }
    drop(child.stderr.take());
    for _ in 0..5 {
        assert_eq!(ask(&s, "len").lines, ["1"]);
    }
    assert!(child.try_wait().unwrap().is_none(), "the handler died");
    let stop = run(dir.path(), &["stop", "s.tsvz"], b"");
    assert_eq!(stop.status.code(), Some(0));
    assert_eq!(child.wait().unwrap().code(), Some(0));
    assert!(!pointer::path_of(&s).exists());
}

#[test]
fn a_handler_whose_socket_directory_was_removed_still_stops() {
    // As /run/user/UID is removed at logout: stopping must not need the socket path.
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path(), "a\t1\n");
    let tmp = tempfile::tempdir().unwrap();
    let mut child = tsvz()
        .arg("serve")
        .arg(&s)
        .env_remove("XDG_RUNTIME_DIR")
        .env("TMPDIR", tmp.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while connect(&s).is_none() {
        assert!(Instant::now() < deadline, "no handler");
        std::thread::sleep(Duration::from_millis(20));
    }
    let info = pointer::lookup(&s).unwrap().unwrap();
    let socket = PathBuf::from(&info.address["unix:".len()..]);
    assert!(socket.starts_with(tmp.path()), "{}", socket.display());
    fs::remove_dir_all(socket.parent().unwrap()).unwrap();
    // SAFETY: kill only sends a signal to the child, which is still ours to reap.
    unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("serve did not exit after SIGTERM");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(status.code(), Some(0));
    assert!(!pointer::path_of(&s).exists());
}

#[test]
fn a_client_never_reads_an_answer_meant_for_a_request_it_gave_up_on() {
    use tsvz::handler::Client;
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path(), "a\t1\nb\t2\n");
    let served = Served::start(&s, &[]);
    let c = Client::connect(&s)
        .unwrap()
        .timeout(Some(Duration::from_millis(100)));
    served.signal(libc::SIGSTOP);
    let mut c = c;
    let lost = c.get("a");
    served.signal(libc::SIGCONT);
    assert!(matches!(lost, Err(tsvz::Error::HandlerLost(_))), "{lost:?}");
    std::thread::sleep(Duration::from_millis(200)); // the late answer for `a` arrives
    let mut c = c.timeout(None);
    match c.get("b") {
        Err(tsvz::Error::HandlerLost(_)) => {}
        Ok(Some(row)) => assert_eq!(row, ["b", "2"], "a stale answer"),
        other => panic!("{other:?}"),
    }
}
