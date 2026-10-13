//! End-to-end tests of the `tsvz` binary.

use std::fs;
use std::path::Path;
use std::process::{Command, Output, Stdio};

fn tsvz() -> Command {
    Command::new(env!("CARGO_BIN_EXE_tsvz"))
}

fn run_in(dir: &Path, args: &[&str], stdin: &[u8]) -> (i32, String, String) {
    let mut child = tsvz()
        .current_dir(dir)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    child.stdin.take().unwrap().write_all(stdin).unwrap();
    let Output { status, stdout, stderr } = child.wait_with_output().unwrap();
    (status.code().unwrap_or(-1), String::from_utf8_lossy(&stdout).into(), String::from_utf8_lossy(&stderr).into())
}

fn records(dir: &Path, args: &[&str]) -> (i32, String, String) {
    let mut all = vec!["--format", "records"];
    all.extend_from_slice(args);
    run_in(dir, &all, b"")
}

fn store(content: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("s.tsvz"), content).unwrap();
    dir
}

#[test]
fn read_get_has_len_keys() {
    let d = store("#_defaults_#\td1\td2\n<#>h\tv<lt>\na\tx\n");
    assert_eq!(records(d.path(), &["read", "s.tsvz"]), (0, "<#>h\tv<lt>\td2\na\tx\td2\n".into(), String::new()));
    assert_eq!(records(d.path(), &["get", "s.tsvz", "a  ", "zz"]), (3, "a\tx\td2\nzz\td1\td2\n".into(), String::new()));
    assert_eq!(records(d.path(), &["has", "s.tsvz", "a"]).0, 0);
    assert_eq!(records(d.path(), &["has", "s.tsvz", "zz"]).0, 3);
    assert_eq!(records(d.path(), &["len", "s.tsvz"]).1, "2\n");
    assert_eq!(records(d.path(), &["keys", "s.tsvz"]).1, "<#>h\na\n");
    assert_eq!(records(d.path(), &["s.tsvz"]).1, "<#>h\tv<lt>\td2\na\tx\td2\n");
}

#[test]
fn verify_and_parts() {
    let d = store("#_checksum_crc32_#\na\tx\n#_checksum_crc32_#\tDEADBEEF\n");
    let (code, out, _) = records(d.path(), &["verify", "s.tsvz"]);
    assert_eq!(code, 4);
    assert_eq!(out, format!("s.tsvz\t3\tcrc32\tdeadbeef\t{:08x}\n", crc32fast::hash(b"a\tx\n")));
    fs::write(d.path().join("s.tsvz.a.gz"), b"").unwrap();
    assert_eq!(records(d.path(), &["parts", "s.tsvz"]).1, "0\t\ts.tsvz\t\n1\ta\ts.tsvz.a.gz\tactive,gzip\n");
}

#[test]
fn version_help_and_usage_errors() {
    let d = tempfile::tempdir().unwrap();
    assert_eq!(run_in(d.path(), &["-V"], b"").1, format!("tsvz-rs {}\n", env!("CARGO_PKG_VERSION")));
    let (code, out, _) = run_in(d.path(), &["--help"], b"");
    assert!(code == 0 && out.contains("OPERATION STORE"));
    let (code, _, err) = run_in(d.path(), &["frobnicate", "s.tsvz"], b"");
    assert!(code == 2 && err.contains("unknown operation"), "{err}");
}

#[test]
fn missing_and_unreadable_stores_exit_1_naming_the_path() {
    let d = tempfile::tempdir().unwrap();
    let (code, out, err) = records(d.path(), &["read", "none.tsvz"]);
    assert_eq!((code, out.as_str()), (1, ""));
    assert!(err.contains("none.tsvz"), "{err}");
    fs::create_dir(d.path().join("dir.tsvz")).unwrap();
    let (code, _, err) = records(d.path(), &["get", "dir.tsvz", "k"]);
    assert!(code == 1 && err.contains("dir.tsvz") && !err.contains("panicked"), "{err}");
    fs::write(d.path().join("p.tsvz"), "a\tx\n").unwrap();
    fs::create_dir(d.path().join("p.tsvz.1")).unwrap();
    let (code, _, err) = records(d.path(), &["read", "p.tsvz"]);
    assert!(code == 1 && err.contains("p.tsvz.1") && !err.contains("panicked"), "{err}");
    let (code, _, err) = records(d.path(), &["read", "notes.txt"]);
    assert!(code == 1 && err.contains("not a TSVZ store path"), "{err}");
}

#[test]
fn awkward_paths() {
    let d = tempfile::tempdir().unwrap();
    fs::write(d.path().join("my store ü.tsvz"), "k\tv\n").unwrap();
    assert_eq!(records(d.path(), &["get", "my store ü.tsvz", "k"]).1, "k\tv\n");
    fs::write(d.path().join("-dash.tsvz"), "k\tv\n").unwrap();
    assert_eq!(records(d.path(), &["--", "-dash.tsvz", "get", "k"]).1, "k\tv\n");
    use std::os::unix::ffi::OsStringExt;
    let out = tsvz().current_dir(d.path()).arg("read").arg(std::ffi::OsString::from_vec(b"bad\xff.tsvz".to_vec())).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(!String::from_utf8_lossy(&out.stderr).contains("panicked"));
}

#[test]
fn a_closed_stdout_ends_quietly() {
    let d = tempfile::tempdir().unwrap();
    let big: String = (0..200_000).map(|i| format!("k{i}\tvalue\n")).collect();
    fs::write(d.path().join("big.tsvz"), big).unwrap();
    let mut child = tsvz()
        .current_dir(d.path())
        .args(["--format", "records", "read", "big.tsvz"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut first = [0u8; 16];
    use std::io::Read;
    child.stdout.as_mut().unwrap().read_exact(&mut first).unwrap();
    drop(child.stdout.take());
    let out = child.wait_with_output().unwrap();
    assert!(!String::from_utf8_lossy(&out.stderr).contains("panicked"), "{}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn a_stopped_read_prints_what_it_read_and_exits_1() {
    let d = store("#_on_corruption_#\tstop\na\t1\n\0\0c\t3\nd\t4\n");
    let (code, out, _) = records(d.path(), &["read", "s.tsvz"]);
    assert_eq!((code, out.as_str()), (1, "a\t1\n"));
}

#[test]
fn serve_and_stop_are_not_provided() {
    let d = store("a\tx\n");
    assert_eq!(records(d.path(), &["serve", "s.tsvz"]).0, 1);
    assert_eq!(records(d.path(), &["stop", "s.tsvz"]).0, 1);
}
