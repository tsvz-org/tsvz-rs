//! Working on the same stores as TSVZ.py: writers exclude each other, and reads agree.
//! Needs python3 and TSVZ.py (`TSVZ_PY`, default `../TSVZ/TSVZ.py`); skipped without them,
//! unless `TSVZ_REQUIRE_INTEROP` is set (CI), which makes their absence a failure.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

fn tsvz_py() -> Option<PathBuf> {
    let path = std::env::var_os("TSVZ_PY")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../TSVZ/TSVZ.py"));
    let python = Command::new("python3")
        .arg("-V")
        .output()
        .is_ok_and(|o| o.status.success());
    (python && path.is_file()).then_some(path)
}

macro_rules! need_tsvz_py {
    () => {
        match tsvz_py() {
            Some(path) => path,
            None => {
                // CI sets TSVZ_REQUIRE_INTEROP so a missing TSVZ.py fails instead of passing silently.
                assert!(
                    std::env::var_os("TSVZ_REQUIRE_INTEROP").is_none(),
                    "TSVZ_REQUIRE_INTEROP is set but python3 or TSVZ.py was not found (set TSVZ_PY)"
                );
                eprintln!("skipped: python3 or TSVZ.py not found (set TSVZ_PY)");
                return;
            }
        }
    };
}

fn run(mut command: Command, stdin: &[u8]) -> Output {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin).unwrap();
    child.wait_with_output().unwrap()
}

fn rust(args: &[&str], stdin: &[u8]) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_tsvz"));
    c.args(["--format", "records", "-q"]).args(args);
    run(c, stdin)
}

fn python(py: &Path, args: &[&str], stdin: &[u8]) -> Output {
    let mut c = Command::new("python3");
    c.arg(py).args(["--format", "records", "-q"]).args(args);
    run(c, stdin)
}

/// Lines with trailing empty cells removed (§3.6: a reader may pad).
fn rows(out: &[u8], d: u8) -> Vec<Vec<u8>> {
    out.split(|&b| b == b'\n')
        .map(|l| {
            let mut l = l.to_vec();
            while l.last() == Some(&d) {
                l.pop();
            }
            l
        })
        .collect()
}

#[test]
fn rust_and_python_writers_never_interleave() {
    let py = need_tsvz_py!();
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("s.tsvz");
    let value = "v".repeat(3000);
    let threads: Vec<_> = (0..8)
        .map(|w| {
            let (py, store, value) = (py.clone(), store.clone(), value.clone());
            std::thread::spawn(move || {
                let who = if w % 2 == 0 { "rs" } else { "py" };
                for round in 0..5 {
                    let batch: String = (0..20)
                        .map(|i| format!("{who}{w}-{round}-{i}\t{value}\n"))
                        .collect();
                    let s = store.to_str().unwrap();
                    let out = if who == "rs" {
                        rust(&["set", s, "-"], batch.as_bytes())
                    } else {
                        python(&py, &["set", s, "-"], batch.as_bytes())
                    };
                    assert!(
                        out.status.success(),
                        "{who}: {}",
                        String::from_utf8_lossy(&out.stderr)
                    );
                }
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    let text = std::fs::read_to_string(&store).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 8 * 5 * 20);
    for line in lines {
        let (key, v) = line
            .split_once('\t')
            .unwrap_or_else(|| panic!("torn line: {line:.80}"));
        assert!(
            (key.starts_with("rs") || key.starts_with("py")) && v == value,
            "torn line: {line:.80}"
        );
    }
}

#[test]
fn rust_waits_while_python_holds_the_lock() {
    let _py = need_tsvz_py!();
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("s.tsvz");
    std::fs::write(&store, "a\t1\n").unwrap();
    let mut holder = Command::new("python3")
        .arg("-c")
        .arg("import fcntl, sys, time\nf = open(sys.argv[1], 'ab')\nfcntl.lockf(f, fcntl.LOCK_EX)\nprint('locked', flush=True)\ntime.sleep(1.5)\n")
        .arg(&store)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(holder.stdout.as_mut().unwrap())
        .read_line(&mut line)
        .unwrap();
    assert_eq!(line.trim(), "locked");
    let started = Instant::now();
    let out = rust(&["set", store.to_str().unwrap(), "b", "2"], b"");
    assert!(out.status.success());
    assert!(
        started.elapsed() >= Duration::from_millis(1000),
        "set did not wait for the lock: {:?}",
        started.elapsed()
    );
    holder.wait().unwrap();
    assert_eq!(std::fs::read_to_string(&store).unwrap(), "a\t1\nb\t2\n");
}

#[test]
fn stores_written_by_one_read_the_same_in_the_other() {
    let py = need_tsvz_py!();
    let dir = tempfile::tempdir().unwrap();
    let input = "#_defaults_#\td1\td2\n<#>h\tv<lt>w\ta<sep>b\nk\t\tx\nk2\nz\t1\n#_fill_empty_with_default_#\ttrue\ne\t\t\n";
    for (writer, name) in [
        ("rs", "a.tsvz"),
        ("py", "b.tsvz"),
        ("rs", "c.tsvz.gz"),
        ("py", "d.tsvz.gz"),
        ("py", "e.csvz"),
    ] {
        let path = dir.path().join(name);
        let s = path.to_str().unwrap();
        let (input, d) = if name.ends_with(".csvz") {
            (input.replace('\t', ","), b',')
        } else {
            (input.to_string(), b'\t')
        };
        let w = if writer == "rs" {
            rust(&["set", s, "-"], input.as_bytes())
        } else {
            python(&py, &["set", s, "-"], input.as_bytes())
        };
        assert!(
            w.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&w.stderr)
        );
        for op in [
            &["read"][..],
            &["get", "k", "missing", "e"],
            &["len"],
            &["keys"],
        ] {
            let mut args = vec![op[0], s];
            args.extend(&op[1..]);
            let (a, b) = (rust(&args, b""), python(&py, &args, b""));
            assert_eq!(
                (a.status.code(), rows(&a.stdout, d)),
                (b.status.code(), rows(&b.stdout, d)),
                "{name}: {op:?}"
            );
        }
    }
}

/// A handler of either tool, started in `dir`; killed if a test leaves it running.
struct Handler(std::process::Child);

impl Handler {
    fn start(mut command: Command, store: &Path) -> Handler {
        let child = command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let handler = Handler(child);
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let info = tsvz::handler::pointer::lookup(store).ok().flatten();
            if info.is_some_and(|i| tsvz::handler::pointer::connect(&i, None).is_ok()) {
                return handler;
            }
            assert!(
                Instant::now() < deadline,
                "no handler for {}",
                store.display()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn rust(store: &Path, extra: &[&str]) -> Handler {
        let mut c = Command::new(env!("CARGO_BIN_EXE_tsvz"));
        c.arg("serve").arg(store).args(extra);
        Handler::start(c, store)
    }

    fn python(py: &Path, store: &Path) -> Handler {
        let mut c = Command::new("python3");
        c.arg(py).arg("serve").arg(store);
        Handler::start(c, store)
    }

    /// Wait for the handler to exit: its status and standard error.
    fn finish(mut self) -> (Option<i32>, String) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.0.try_wait().unwrap().is_none() {
            assert!(Instant::now() < deadline, "the handler did not exit");
            std::thread::sleep(Duration::from_millis(20));
        }
        let mut err = String::new();
        use std::io::Read;
        self.0
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut err)
            .unwrap();
        (self.0.wait().unwrap().code(), err)
    }
}

impl Drop for Handler {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn out(o: &Output) -> (Option<i32>, String) {
    (
        o.status.code(),
        String::from_utf8_lossy(&o.stdout).into_owned(),
    )
}

#[test]
fn the_rust_cli_goes_through_a_python_handler() {
    let py = need_tsvz_py!();
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("s.tsvz");
    std::fs::write(&store, "a\t1\n").unwrap();
    let s = store.to_str().unwrap();
    let handler = Handler::python(&py, &store);
    let mut verbose = Command::new(env!("CARGO_BIN_EXE_tsvz"));
    verbose.args(["-v", "set", s, "k", "v"]);
    let set = run(verbose, b"");
    assert!(set.status.success());
    assert!(String::from_utf8_lossy(&set.stderr).contains("via the handler"));
    assert_eq!(
        out(&rust(&["get", s, "k", "zz"], b"")),
        (Some(3), "k\tv\nzz\t\n".into())
    );
    assert_eq!(
        rust(&["set", s, "-"], b"b\t2\nc\t3\n").status.code(),
        Some(0)
    );
    assert_eq!(
        out(&rust(&["pop", s, "b"], b"")),
        (Some(0), "b\t2\n".into())
    );
    assert_eq!(
        out(&rust(&["popitem", s, "first"], b"")),
        (Some(0), "a\t1\n".into())
    );
    assert_eq!(
        out(&rust(&["setdefault", s, "n", "w"], b"")),
        (Some(0), "n\tw\n".into())
    );
    assert_eq!(rust(&["delete", s, "c"], b"").status.code(), Some(0));
    assert_eq!(out(&rust(&["keys", s], b"")), (Some(0), "k\nn\n".into()));
    assert_eq!(out(&rust(&["len", s], b"")), (Some(0), "2\n".into()));
    assert_eq!(rust(&["stop", s], b"").status.code(), Some(0));
    let (code, err) = handler.finish();
    assert_eq!(code, Some(0), "{err}");
    assert_eq!(
        std::fs::read_to_string(&store).unwrap().lines().last(),
        Some("c")
    );
}

#[test]
fn python_commands_and_its_client_go_through_a_rust_handler() {
    let py = need_tsvz_py!();
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("s.tsvz");
    let s = store.to_str().unwrap();
    let handler = Handler::rust(&store, &["-v"]);
    assert_eq!(
        python(&py, &["set", s, "k", "v"], b"").status.code(),
        Some(0)
    );
    assert_eq!(
        out(&python(&py, &["get", s, "k"], b"")),
        (Some(0), "k\tv\n".into())
    );
    let script = "import sys\nsys.path.insert(0, sys.argv[1])\nimport TSVZ\n\
        c = TSVZ.TSVZClient(sys.argv[2])\nc['a'] = 'x'\nprint(c['a'])\nprint(len(c))\n\
        c.update({'b': 'y', 'c': 'z'})\nprint(c.pop('b'))\nprint(c.popitem())\n\
        print(c.setdefault('d', 'w'))\ndel c['a']\nprint(c.keys())\nc.close()\n";
    let mut client = Command::new("python3");
    client
        .arg("-c")
        .arg(script)
        .arg(py.parent().unwrap())
        .arg(s);
    let client = run(client, b"");
    let stderr = String::from_utf8_lossy(&client.stderr);
    assert!(client.status.success(), "{stderr}");
    assert!(!stderr.contains("working on the files"), "{stderr}");
    assert_eq!(
        String::from_utf8_lossy(&client.stdout),
        "['a', 'x']\n2\n['b', 'y']\n('c', ['c', 'z'])\n['d', 'w']\n['k', 'd']\n"
    );
    assert_eq!(python(&py, &["stop", s], b"").status.code(), Some(0));
    let (code, err) = handler.finish();
    assert_eq!(code, Some(0), "{err}");
    assert!(
        err.contains("set answered #0"),
        "the requests came through the handler: {err}"
    );
}

#[test]
fn neither_tool_serves_a_store_the_other_serves() {
    let py = need_tsvz_py!();
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("s.tsvz");
    std::fs::write(&store, "").unwrap();
    let s = store.to_str().unwrap();
    let handler = Handler::rust(&store, &[]);
    assert_eq!(python(&py, &["serve", s], b"").status.code(), Some(1));
    assert_eq!(python(&py, &["stop", s], b"").status.code(), Some(0));
    assert_eq!(handler.finish().0, Some(0));
    let handler = Handler::python(&py, &store);
    let second = rust(&["serve", s], b"");
    assert_eq!(second.status.code(), Some(1));
    assert_eq!(rust(&["stop", s], b"").status.code(), Some(0));
    assert_eq!(handler.finish().0, Some(0));
}
