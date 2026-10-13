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
