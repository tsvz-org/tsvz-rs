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

/// What `op` of each tool prints for the loose file `file`: (tsvz-rs, TSVZ.py).
fn both(py: &Path, op: &str, file: &Path) -> (String, String) {
    let f = file.to_str().unwrap();
    let text = |o: Output| {
        assert!(o.status.success(), "{op} {f}: {:?}", o.status);
        String::from_utf8_lossy(&o.stdout).into_owned()
    };
    (text(rust(&[op, f], b"")), text(python(py, &[op, f], b"")))
}

/// Rows of records output with the empty cells a reader pads them with removed (§3.6).
fn trimmed(out: &str) -> Vec<Vec<u8>> {
    rows(out.as_bytes(), b'\t')
}

/// Loose files written as TSVZ.py writes and reads them (header lines, `#_defaults_#` lines,
/// one-column files), and the limitations entry's examples.
const LOOSE: &[&str] = &[
    "a\nb\nc\n",
    "#h\na\nb\na\n",
    "a\nb\t1\nc\n",
    "#h\tx\ty\na\t1\t\nb\t\t2\na\t\t\n",
    "#h\tx\ty\n#_defaults_#\tP\tQ\na\t1\t\nb\t\t2\n#c\na\t3\t\n#_defaults_#\tR\nb\t\t4\nc\t\t\n",
    // The entry's second example, as TSVZ.py 5.10's scrub wrote it, then before that scrub.
    "id\nb\t1\tX\tX\na\nd\tX\tX\t2\na\t\n#_defaults_#\tP\t\tQ\n",
    "id\nb\t1\tX\tX\na\na\t2\nd\tX\tX\t2\na\t\n#_defaults_#\tP\t\tQ\n",
    // Its first example, after and before a TSVZed commit.
    "d\tX\tW\nd\ne\t1\nd\t\n",
    "d\tX\tW\na\nd\t\ne\t1\nd\t\n",
    "x\t0\nb\t1\ny\t5\nb\t\nb\t2\n",
    "#h\nk\t1\nk\t \t\nk\nz\t2\n",
    "\na\t1\nb\t2\na\t3\n",
    // A byte-order mark is part of line 1, to TSVZ.py: its key, whatever it holds.
    "\u{feff}a\nb\n",
    "\u{feff}#h\na\t1\n",
    "\u{feff}name\tvalue\nx\t1\n#c\nx\t2\n",
    "\u{feff}a\t1\nb\t\nb\t2\n",
    // TSVZ.py strips the NULs at the start of a line: a line of only NULs is empty, and a key
    // never begins with one.
    "\0\na\tb\nc\t\n",
    "\0\0\na\nb\n",
    "\0a\t1\na\t2\n",
    "#h\n\0\0\na\t1\n\0b\t\n\0\t3\n",
    // CRLF lines.
    "#h\r\na\t1\r\nb\r\nb\t2\r\na\t3\r\n",
    "a\r\nb\r\n\r\na\r\n",
    // Only a first cell that is `#_defaults_#` makes a defaults line, to TSVZ.py.
    "#_defaults_#x\t1\na\t\tq\n",
    "#h\n#_defaults_#x\na\nb\n#_defaults_#x\t2\n",
];

/// Loose files that are rewritten alike, but not read alike (docs/limitations.md): a key with
/// whitespace at its end is a key of another name to TSVZ.py.
const REWRITTEN: &[&str] = &[
    "#h\na \t1\nb\t2\na\t3\n",
    "a \nb\na\nb \n",
    "#h\nk\t1\nk \t2\nk\t3\nz\t\n",
];

/// A loose file drawn from `seed`: keys a to d, with values, blank cells, keys alone, comments,
/// empty lines, empty keys and `#_defaults_#` lines; one in three keeps only the keys.
fn random_loose(seed: u64) -> String {
    let mut x = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
    let mut next = |n: u64| {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x % n
    };
    let shapes = [
        "K",
        "K\t1",
        "K\t",
        "K\t\t",
        "K\tX\tY",
        "K\t\tZ",
        "#c",
        "#_defaults_#\tP\t\tQ",
        "#_defaults_#",
        "",
        "\tx",
        "K\t2",
        "K\t \t",
        // What TSVZ.py reads differently from tsvz-rs: a key with whitespace at its end, a CR.
        "K \t1",
        "K\t1\r",
    ];
    let one = next(3) == 0;
    let mut text = ["", "#h\n", "\n", ""][next(4) as usize].to_string();
    for _ in 0..1 + next(11) {
        let key = ["a", "b", "c", "d"][next(4) as usize];
        let line = shapes[next(shapes.len() as u64) as usize].replace('K', key);
        let line = match line.split_once('\t') {
            Some((k, _)) if one && !line.starts_with('#') => k.to_string(),
            _ => line,
        };
        text.push_str(&line);
        text.push('\n');
    }
    text
}

/// Whether `text` has a `#_defaults_#` line, whose defaults TSVZ.py applies to the rows after it
/// and tsvz-rs does not.
fn has_defaults_line(text: &str) -> bool {
    text.lines().any(|l| {
        l.split('\t')
            .next()
            .is_some_and(|k| k.trim_end() == "#_defaults_#")
    })
}

#[test]
fn loose_files_list_the_keys_tsvz_py_lists() {
    let py = need_tsvz_py!();
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("f.tsv");
    for text in LOOSE {
        std::fs::write(&file, text).unwrap();
        let (rs, py_keys) = both(&py, "keys", &file);
        assert_eq!(rs, py_keys, "{text:?}");
        if !has_defaults_line(text) {
            let (rs, py_rows) = both(&py, "read", &file);
            assert_eq!(trimmed(&rs), trimmed(&py_rows), "{text:?}");
        }
    }
}

#[test]
fn loose_rows_are_padded_to_the_width_of_the_first_keyed_line() {
    let py = need_tsvz_py!();
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("f.tsv");
    // The first keyed line sets the width, a deletion's blank cells included, where the first
    // data row is a narrower one. No row has empty cells at its end past the width, which
    // TSVZ.py cuts and tsvz-rs keeps (docs/limitations.md).
    for text in [
        "k\t\t\na\t1\n",
        "#h\nk\t \t\na\t1\nb\t2\t3\nc\t\t\tx\n",
        "x\t\t\t\ny\t1\nz\t2\t3\n",
        "a\t1\nb\t2\t3\nc\t4\n",
        "#h\n\nk\t\na\t1\t2\nb\t3\n",
    ] {
        std::fs::write(&file, text).unwrap();
        let (rs, py_rows) = both(&py, "read", &file);
        assert_eq!(rs, py_rows, "{text:?}");
    }
}

/// Whether tsvz-rs lists the keys of `text` as TSVZ.py does: not with a bare `#_defaults_#` line
/// (a row to TSVZ.py in a one-column file), a key with whitespace at its end, or a CR (TSVZ.py
/// strips them, docs/limitations.md).
fn keys_agree(text: &str) -> bool {
    let spaced = |l: &str| l.split('\t').next().is_some_and(|k| k.trim_end() != k);
    !text.contains('\r') && !text.lines().any(|l| l == "#_defaults_#" || spaced(l))
}

#[test]
fn a_loose_scrub_changes_nothing_tsvz_py_reads() {
    let py = need_tsvz_py!();
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("f.tsv");
    let texts = LOOSE
        .iter()
        .chain(REWRITTEN)
        .map(|t| t.to_string())
        .chain((0..60).map(random_loose));
    let mut compacted = 0;
    for text in texts {
        std::fs::write(&file, &text).unwrap();
        let (rs, py_rows) = both(&py, "read", &file);
        let (rs_keys, py_keys) = both(&py, "keys", &file);
        if keys_agree(&text) {
            assert_eq!(rs_keys, py_keys, "{text:?}");
        }
        let scrubbed = rust(&["scrub", file.to_str().unwrap()], b"");
        assert!(scrubbed.status.success(), "{text:?}");
        let (rs_after, py_after) = both(&py, "read", &file);
        let after = std::fs::read_to_string(&file).unwrap();
        compacted += (after.len() < text.len()) as usize;
        // TSVZ.py reads exactly the same rows, its defaults applied; tsvz-rs the same rows,
        // however it pads them, and the same keys.
        assert_eq!(py_after, py_rows, "{text:?} scrubbed to {after:?}");
        assert_eq!(
            trimmed(&rs_after),
            trimmed(&rs),
            "{text:?} scrubbed to {after:?}"
        );
        assert_eq!(
            both(&py, "keys", &file).0,
            rs_keys,
            "{text:?} scrubbed to {after:?}"
        );
        if keys_agree(&text) && !has_defaults_line(&text) {
            assert_eq!(
                trimmed(&rs_after),
                trimmed(&py_after),
                "{text:?} scrubbed to {after:?}"
            );
        }
    }
    // A scrub that wrote every line back would pass all of the above.
    assert!(compacted >= 40, "only {compacted} files shrank");
    // TSVZ.py's own scrub of the entry's second example: tsvz-rs lists the keys it listed.
    std::fs::write(&file, LOOSE[6]).unwrap();
    let before = both(&py, "keys", &file);
    assert!(
        python(&py, &["scrub", file.to_str().unwrap()], b"")
            .status
            .success()
    );
    assert_eq!(both(&py, "keys", &file), before);
    assert_eq!(before.0, "id\nb\na\nd\n");
}

#[test]
fn rows_written_after_a_loose_clear_read_to_tsvz_py_as_before() {
    let py = need_tsvz_py!();
    let dir = tempfile::tempdir().unwrap();
    let text = "#h\n#_defaults_#\tP\t\tQ\na\t1\t\t\n#_defaults_#\n#_defaults_#\tR\t\tS\nb\t2\t\t\n";
    let (kept, cleared) = (dir.path().join("kept.tsv"), dir.path().join("cleared.tsv"));
    for f in [&kept, &cleared] {
        std::fs::write(f, text).unwrap();
    }
    assert!(
        rust(&["clear", cleared.to_str().unwrap()], b"")
            .status
            .success()
    );
    assert_eq!(both(&py, "keys", &cleared), (String::new(), String::new()));
    for f in [&kept, &cleared] {
        let probe = rust(&["set", f.to_str().unwrap(), "z", "", "q", ""], b"");
        assert!(probe.status.success());
    }
    let z = |f: &Path| {
        let out = python(&py, &["get", f.to_str().unwrap(), "z"], b"");
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    assert_eq!(z(&cleared), z(&kept));
    assert_eq!(z(&kept), "z\tR\tq\tS\n");
}

#[test]
fn a_delete_in_a_one_column_loose_file_reads_alike() {
    let py = need_tsvz_py!();
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("f.tsv");
    for (text, key, keys) in [
        ("#h\na\nb\t1\nc\nb\n", "b", "a\nc\n"),
        ("a\nb\t1\nc\n", "a", "b\nc\n"),
        ("a\nb\nc\n", "c", "a\nb\n"),
    ] {
        std::fs::write(&file, text).unwrap();
        assert!(
            rust(&["delete", file.to_str().unwrap(), key], b"")
                .status
                .success()
        );
        assert_eq!(
            both(&py, "keys", &file),
            (keys.into(), keys.into()),
            "{text:?}"
        );
    }
}

/// Keys of the loose file `file` as TSVZ.py lists them, sorted (a delete may move a key).
fn py_keys_sorted(py: &Path, file: &Path) -> Vec<String> {
    let (_, keys) = both(py, "keys", file);
    let mut keys: Vec<String> = keys.lines().map(str::to_string).collect();
    keys.sort();
    keys
}

#[test]
fn a_loose_delete_writes_what_tsvz_py_writes() {
    let py = need_tsvz_py!();
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = (dir.path().join("rs.tsv"), dir.path().join("py.tsv"));
    // More than one column, or no keyed line: a key alone for every key asked, live or not;
    // none for a reset of the defaults it finds; nothing where no line is keyed. The bytes are
    // TSVZ.py's, but for the whitespace at the end of a key, which tsvz-rs leaves off, since it
    // does not strip it when it reads. Both read the file alike: tsvz-rs the keys TSVZ.py
    // lists, and TSVZ.py the same keys after either tool's delete.
    for (text, keys) in [
        ("b\t1\nc\t3\n", &["b "][..]),
        ("b\t1\nc\t3\n", &["zz", "c"]),
        ("#_defaults_#\tX\tY\na\t1\nb\t2\n", &["#_defaults_#", "a"]),
        ("#_defaults_#\tX\tY\na\t1\nb\t2\n", &["#_defaults_#x"]),
        ("#h\n\n#c\n", &["k"]),
        ("a\t\nb\n", &["a"]),
    ] {
        for f in [&a, &b] {
            std::fs::write(f, text).unwrap();
        }
        let mut args = vec!["delete", a.to_str().unwrap()];
        args.extend_from_slice(keys);
        assert!(rust(&args, b"").status.success(), "{text:?} {keys:?}");
        args[1] = b.to_str().unwrap();
        assert!(python(&py, &args, b"").status.success());
        if keys.iter().all(|k| k.trim_end() == *k) {
            assert_eq!(
                std::fs::read(&a).unwrap(),
                std::fs::read(&b).unwrap(),
                "{text:?} {keys:?}"
            );
        }
        let (rs_keys, py_keys) = both(&py, "keys", &a);
        assert_eq!(rs_keys, py_keys, "{text:?} {keys:?}");
        assert_eq!(
            py_keys_sorted(&py, &a),
            py_keys_sorted(&py, &b),
            "{text:?} {keys:?}"
        );
    }
    // One column: the same keys, in whatever order (TSVZ.py may move one first), whether or
    // not the key as given has whitespace at its end.
    for (text, keys) in [
        ("a\nb\nc\n", &["b"][..]),
        ("#h\na\nb \nc\nb\n", &["b"]),
        ("#h\na\nb\nc\nb\n", &["b\t", "zz"]),
        ("a\nb\t1\nc\n", &["a"]),
        ("a\n#_defaults_#\tP\nb\nc\n", &["a", "c"]),
    ] {
        for f in [&a, &b] {
            std::fs::write(f, text).unwrap();
        }
        let mut args = vec!["delete", a.to_str().unwrap()];
        args.extend_from_slice(keys);
        assert!(rust(&args, b"").status.success(), "{text:?} {keys:?}");
        args[1] = b.to_str().unwrap();
        assert!(python(&py, &args, b"").status.success());
        assert_eq!(
            py_keys_sorted(&py, &a),
            py_keys_sorted(&py, &b),
            "{text:?} {keys:?}: {:?} against {:?}",
            std::fs::read_to_string(&a).unwrap(),
            std::fs::read_to_string(&b).unwrap()
        );
    }
}

#[test]
fn a_delete_of_a_loose_files_marked_line_1_takes_the_mark_with_it() {
    let py = need_tsvz_py!();
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = (dir.path().join("rs.tsv"), dir.path().join("py.tsv"));
    // The mark is part of line 1's key, so `a` is not the key of line 1, and the line that
    // follows a deleted line 1 never takes the mark.
    for (text, key) in [
        ("\u{feff}a\nb\n", "\u{feff}a"),
        ("\u{feff}a\nb\n", "a"),
        ("\u{feff}zz\n", "\u{feff}zz"),
        ("\u{feff}a\t1\nb\t2\n", "\u{feff}a"),
        ("\u{feff}#h\na\nb\n", "\u{feff}#h"),
    ] {
        for f in [&a, &b] {
            std::fs::write(f, text).unwrap();
        }
        assert!(
            rust(&["delete", a.to_str().unwrap(), key], b"")
                .status
                .success()
        );
        assert!(
            python(&py, &["delete", b.to_str().unwrap(), key], b"")
                .status
                .success()
        );
        let after = std::fs::read_to_string(&a).unwrap();
        assert_eq!(
            py_keys_sorted(&py, &a),
            py_keys_sorted(&py, &b),
            "{text:?} {key:?}: {after:?}"
        );
        // And tsvz-rs reads the file as TSVZ.py does.
        let (rs, py_keys) = both(&py, "keys", &a);
        assert_eq!(rs, py_keys, "{text:?} {key:?}: {after:?}");
    }
}

#[test]
fn a_loose_clear_keeps_the_column_count_its_defaults_were_read_at() {
    let py = need_tsvz_py!();
    let dir = tempfile::tempdir().unwrap();
    let (kept, cleared) = (dir.path().join("kept.tsv"), dir.path().join("cleared.tsv"));
    // The rows TSVZ.py reads for the probe key, as it pads them to the file's width.
    let z = |f: &Path| {
        let out = python(&py, &["read", f.to_str().unwrap()], b"");
        let out = String::from_utf8_lossy(&out.stdout).into_owned();
        out.lines()
            .filter(|l| l.starts_with("zz\t") || *l == "zz")
            .map(|l| l.trim_end_matches('\t').to_string())
            .collect::<Vec<_>>()
    };
    // (a file, whether it has one column)
    for (text, one) in [
        // Defaults of more cells than the file has columns, and of fewer.
        ("a\t1\n#_defaults_#\tP\tQ\tS\n", false),
        ("a\t1\t2\t3\n#_defaults_#\tP\tQ\n", false),
        ("#h\na\t1\n#_defaults_#\tP\tQ\n#_defaults_#\t\t\tR\n", false),
        (
            "#_defaults_#\tP\tQ\na\t1\t\n#_defaults_#\n#_defaults_#\tR\tS\tT\n",
            false,
        ),
        // One column: a key alone stays a row, whatever defaults the file had.
        ("a\n#_defaults_#\tP\n", true),
        ("#h\na\nb\n#_defaults_#\tP\tQ\tS\n", true),
    ] {
        for f in [&kept, &cleared] {
            std::fs::write(f, text).unwrap();
        }
        assert!(
            rust(&["clear", cleared.to_str().unwrap()], b"")
                .status
                .success()
        );
        let after = std::fs::read_to_string(&cleared).unwrap();
        assert_eq!(both(&py, "keys", &cleared), (String::new(), String::new()));
        // Rows with blank cells read with the defaults they had, but at one column, which
        // keeps none: a defaults line of more cells would make a key alone a deletion.
        for probe in ["zz\t\tq\tr\n", "zz\t5\n", "zz\t\t\n"] {
            let (mut want, mut got) = (text.to_string(), after.clone());
            want.push_str(probe);
            got.push_str(probe);
            std::fs::write(&kept, want).unwrap();
            std::fs::write(&cleared, got).unwrap();
            if !one {
                assert_eq!(
                    z(&cleared),
                    z(&kept),
                    "{text:?} cleared to {after:?}, then {probe:?}"
                );
            }
        }
        for f in [&kept, &cleared] {
            std::fs::write(f, if f == &kept { text } else { &after }).unwrap();
        }
        let bare = |f: &Path| {
            assert!(
                rust(&["set", f.to_str().unwrap(), "zz"], b"")
                    .status
                    .success()
            );
            let keys = python(&py, &["keys", f.to_str().unwrap()], b"").stdout;
            String::from_utf8_lossy(&keys).lines().any(|k| k == "zz")
        };
        assert_eq!(bare(&cleared), bare(&kept), "{text:?} cleared to {after:?}");
        assert_eq!(bare(&kept), one, "{text:?}");
    }
}

#[test]
fn a_loose_delete_leaves_the_rows_tsvz_py_read_but_the_key() {
    let py = need_tsvz_py!();
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("f.tsv");
    // In a file of one column a `#_defaults_#` line with no value is a row to TSVZ.py, and so
    // is a bare one: they stay rows when the key that set the column count goes.
    for (text, key) in [
        ("c\n#_defaults_#\t\n", "c"),
        ("c\n#_defaults_#\tP\n#_defaults_#\n", "c"),
        ("b\nb\t\t\t\n#_defaults_#\t\t\n", "b"),
        ("x\n#_defaults_#\tP\n", "x"),
        ("a\nb\t1\nc\n#_defaults_#x\t1\n", "a"),
        ("#h\na\nb\n#_defaults_#\tP\nc\n", "b"),
    ] {
        std::fs::write(&file, text).unwrap();
        let before = py_keys_sorted(&py, &file);
        assert!(
            rust(&["delete", file.to_str().unwrap(), key], b"")
                .status
                .success()
        );
        let want: Vec<String> = before.into_iter().filter(|k| k != key).collect();
        let after = std::fs::read_to_string(&file).unwrap();
        assert_eq!(
            py_keys_sorted(&py, &file),
            want,
            "{text:?} {key:?}: {after:?}"
        );
    }
}
