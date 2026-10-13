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
    let Output {
        status,
        stdout,
        stderr,
    } = child.wait_with_output().unwrap();
    (
        status.code().unwrap_or(-1),
        String::from_utf8_lossy(&stdout).into(),
        String::from_utf8_lossy(&stderr).into(),
    )
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
    assert_eq!(
        records(d.path(), &["read", "s.tsvz"]),
        (0, "<#>h\tv<lt>\td2\na\tx\td2\n".into(), String::new())
    );
    assert_eq!(
        records(d.path(), &["get", "s.tsvz", "a  ", "zz"]),
        (3, "a\tx\td2\nzz\td1\td2\n".into(), String::new())
    );
    assert_eq!(records(d.path(), &["has", "s.tsvz", "a"]).0, 0);
    assert_eq!(records(d.path(), &["has", "s.tsvz", "zz"]).0, 3);
    assert_eq!(records(d.path(), &["len", "s.tsvz"]).1, "2\n");
    assert_eq!(records(d.path(), &["keys", "s.tsvz"]).1, "<#>h\na\n");
    assert_eq!(
        records(d.path(), &["s.tsvz"]).1,
        "<#>h\tv<lt>\td2\na\tx\td2\n"
    );
}

#[test]
fn rows_print_at_the_store_width() {
    let d = store("a\t1\t2\t3\na\nb\tx\n");
    assert_eq!(records(d.path(), &["read", "s.tsvz"]).1, "b\tx\t\t\n");
    assert_eq!(
        records(d.path(), &["get", "s.tsvz", "zz"]),
        (3, "zz\t\t\t\n".into(), String::new())
    );
    assert_eq!(
        records(d.path(), &["setdefault", "s.tsvz", "n", "v"]).1,
        "n\tv\t\t\n"
    );
    assert_eq!(records(d.path(), &["pop", "s.tsvz", "b"]).1, "b\tx\t\t\n");
    // A loose file pads the same way, and keeps the cells of a longer row.
    fs::write(d.path().join("l.tsv"), "a\t1\t2\nb\tx\nc\tp\tq\tr\n").unwrap();
    assert_eq!(
        records(d.path(), &["read", "l.tsv"]).1,
        "a\t1\t2\nb\tx\t\nc\tp\tq\tr\n"
    );
}

#[test]
fn verify_and_parts() {
    let d = store("#_checksum_crc32_#\na\tx\n#_checksum_crc32_#\tDEADBEEF\n");
    let (code, out, _) = records(d.path(), &["verify", "s.tsvz"]);
    assert_eq!(code, 4);
    assert_eq!(
        out,
        format!(
            "s.tsvz\t3\tcrc32\tdeadbeef\t{:08x}\n",
            crc32fast::hash(b"a\tx\n")
        )
    );
    fs::write(d.path().join("s.tsvz.a.gz"), b"").unwrap();
    assert_eq!(
        records(d.path(), &["parts", "s.tsvz"]).1,
        "0\t\ts.tsvz\t\n1\ta\ts.tsvz.a.gz\tactive,gzip\n"
    );
}

#[test]
fn version_help_and_usage_errors() {
    let d = tempfile::tempdir().unwrap();
    assert_eq!(
        run_in(d.path(), &["-V"], b"").1,
        format!("tsvz-rs {}\n", env!("CARGO_PKG_VERSION"))
    );
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
    assert!(
        code == 1 && err.contains("dir.tsvz") && !err.contains("panicked"),
        "{err}"
    );
    fs::write(d.path().join("p.tsvz"), "a\tx\n").unwrap();
    fs::create_dir(d.path().join("p.tsvz.1")).unwrap();
    let (code, _, err) = records(d.path(), &["read", "p.tsvz"]);
    assert!(
        code == 1 && err.contains("p.tsvz.1") && !err.contains("panicked"),
        "{err}"
    );
    let (code, _, err) = records(d.path(), &["read", "notes.txt"]);
    assert!(code == 1 && err.contains("not a TSVZ store path"), "{err}");
}

#[test]
fn a_part_that_cannot_be_read_is_skipped_and_the_read_fails() {
    // §21.8.5 and §4.10: print what could be read, exit 1; change nothing on such a read.
    let d = store("a\t1\n");
    fs::create_dir(d.path().join("s.tsvz.1")).unwrap();
    let last = d.path().join("s.tsvz.2");
    fs::write(&last, "b\t2\n").unwrap();
    let (code, out, err) = records(d.path(), &["read", "s.tsvz"]);
    assert_eq!((code, out.as_str()), (1, "a\t1\nb\t2\n"));
    assert!(err.contains("s.tsvz.1") && err.contains("skipped"), "{err}");
    assert_eq!(
        records(d.path(), &["get", "s.tsvz", "b"]),
        (1, "b\t2\n".into(), err.clone())
    );
    assert_eq!(records(d.path(), &["len", "s.tsvz"]).1, "2\n");
    assert_eq!(records(d.path(), &["len", "s.tsvz"]).0, 1);
    assert_eq!(records(d.path(), &["has", "s.tsvz", "b"]).0, 1);
    for change in [
        &["pop", "s.tsvz", "b"][..],
        &["popitem", "s.tsvz"],
        &["setdefault", "s.tsvz", "c", "3"],
        &["clear", "s.tsvz"],
    ] {
        let (code, out, _) = records(d.path(), change);
        assert_eq!((code, out.as_str()), (1, ""), "{change:?}");
        assert_eq!(fs::read_to_string(&last).unwrap(), "b\t2\n", "{change:?}");
    }
}

#[test]
fn awkward_paths() {
    let d = tempfile::tempdir().unwrap();
    fs::write(d.path().join("my store ü.tsvz"), "k\tv\n").unwrap();
    assert_eq!(
        records(d.path(), &["get", "my store ü.tsvz", "k"]).1,
        "k\tv\n"
    );
    fs::write(d.path().join("-dash.tsvz"), "k\tv\n").unwrap();
    assert_eq!(
        records(d.path(), &["--", "-dash.tsvz", "get", "k"]).1,
        "k\tv\n"
    );
    use std::os::unix::ffi::OsStringExt;
    let out = tsvz()
        .current_dir(d.path())
        .arg("read")
        .arg(std::ffi::OsString::from_vec(b"bad\xff.tsvz".to_vec()))
        .output()
        .unwrap();
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
    child
        .stdout
        .as_mut()
        .unwrap()
        .read_exact(&mut first)
        .unwrap();
    drop(child.stdout.take());
    let out = child.wait_with_output().unwrap();
    assert!(
        !String::from_utf8_lossy(&out.stderr).contains("panicked"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn a_stopped_read_prints_what_it_read_and_exits_1() {
    let d = store("#_on_corruption_#\tstop\na\t1\n\0\0c\t3\nd\t4\n");
    let (code, out, _) = records(d.path(), &["read", "s.tsvz"]);
    assert_eq!((code, out.as_str()), (1, "a\t1\n"));
}

fn file(dir: &Path, name: &str) -> String {
    String::from_utf8(fs::read(dir.join(name)).unwrap()).unwrap()
}

#[test]
fn set_append_delete_write_records() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    assert_eq!(records(dir, &["set", "s.tsvz", "#k", "a\tb", "<", ""]).0, 0);
    assert_eq!(
        records(dir, &["append", "s.tsvz", "#_defaults_#", "d1"]).0,
        0
    );
    assert_eq!(records(dir, &["set", "s.tsvz", "gone"]).0, 0);
    assert_eq!(
        records(dir, &["delete", "s.tsvz", "x", "#_defaults_#"]).0,
        0
    );
    assert_eq!(
        file(dir, "s.tsvz"),
        "<#>k\ta<sep>b\t<lt>\t\n#_defaults_#\td1\ngone\nx\n#_defaults_#\n"
    );
    assert_eq!(
        records(dir, &["set", "-v", "s.tsvz", "--", "carol", "-5"]).0,
        0
    );
    // Padded to the width of the first data row, `#k` (§3.6), as TSVZ.py pads it.
    assert_eq!(
        records(dir, &["get", "s.tsvz", "carol"]).1,
        "carol\t-5\t\t\n"
    );
}

#[test]
fn bulk_input() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    let input =
        b"\xef\xbb\xbfa\tx<LF>y\r\n# comment\n\n#_defaults_#\td\nb\na\nc\t\nunterminated\tz";
    let (code, _, err) = run_in(dir, &["set", "s.tsvz", "-"], input);
    assert_eq!(code, 0);
    assert!(err.contains("unterminated"), "{err}");
    assert_eq!(
        file(dir, "s.tsvz"),
        "a\tx<LF>y\n#_defaults_#\td\nb\na\nc\t\n"
    );
    let (code, _, _) = run_in(
        dir,
        &["delete", "s.tsvz", "-"],
        b"c\textra\n#_defaults_#\n# skip\n<#>h\n",
    );
    assert_eq!(code, 0);
    assert!(file(dir, "s.tsvz").ends_with("c\t\nc\n#_defaults_#\n<#>h\n"));
    fs::write(dir.join("c.csvz"), "").unwrap();
    run_in(dir, &["set", "c.csvz", "-"], b"k,a<sep>b\tc\n");
    assert_eq!(records(dir, &["get", "c.csvz", "k"]).1, "k,a<sep>b\tc\n");
}

#[test]
fn pop_popitem_setdefault() {
    let d = store("#_defaults_#\td1\td2\na\t1\nb\t2\nc\t3\n");
    let dir = d.path();
    assert_eq!(
        records(dir, &["pop", "s.tsvz", "b"]),
        (0, "b\t2\td2\n".into(), String::new())
    );
    assert_eq!(records(dir, &["pop", "s.tsvz", "b"]).0, 3);
    assert_eq!(
        records(dir, &["popitem", "s.tsvz", "first"]).1,
        "a\t1\td2\n"
    );
    assert_eq!(records(dir, &["popitem", "s.tsvz"]).1, "c\t3\td2\n");
    assert_eq!(records(dir, &["popitem", "s.tsvz"]).0, 3);
    assert_eq!(
        records(dir, &["setdefault", "s.tsvz", "n", "v "]).1,
        "n\tv\td2\n"
    );
    assert_eq!(
        records(dir, &["setdefault", "s.tsvz", "n", "other"]).1,
        "n\tv\td2\n"
    );
    assert_eq!(
        records(dir, &["setdefault", "s.tsvz", "#_defaults_#", "x"]).0,
        2
    );
    assert!(
        file(dir, "s.tsvz").starts_with("#_defaults_#\td1\td2\na\t1\nb\t2\nc\t3\nb\na\nc\nn\tv \n")
    );
}

#[test]
fn writes_create_missing_stores_and_reads_do_not() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    for (op, args) in [
        ("set", vec!["k", "v"]),
        ("append", vec!["k", "v"]),
        ("delete", vec!["k"]),
        ("setdefault", vec!["k", "v"]),
    ] {
        let name = format!("{op}.tsvz");
        let mut all = vec![op, name.as_str()];
        all.extend(args);
        assert_eq!(records(dir, &all).0, 0, "{op}");
        assert!(dir.join(&name).exists(), "{op}");
    }
    for op in ["pop", "popitem"] {
        let name = format!("{op}.tsvz");
        let mut all = vec![op, name.as_str()];
        if op == "pop" {
            all.push("k");
        }
        assert_eq!(records(dir, &all).0, 1, "{op}");
        assert!(!dir.join(&name).exists(), "{op}");
    }
}

#[test]
fn a_stopped_store_is_not_changed_by_pop() {
    let d = store("#_on_corruption_#\tstop\na\t1\n\0\0c\t3\n");
    let before = fs::read(d.path().join("s.tsvz")).unwrap();
    assert_eq!(records(d.path(), &["pop", "s.tsvz", "a"]).0, 1);
    assert_eq!(fs::read(d.path().join("s.tsvz")).unwrap(), before);
}

#[test]
fn clear_and_scrub_from_the_command_line() {
    let d = store("#hdr\na\tx\nb\na\ty\n");
    assert_eq!(
        records(d.path(), &["scrub", "s.tsvz"]),
        (0, String::new(), String::new())
    );
    assert_eq!(file(d.path(), "s.tsvz"), "#hdr\n#_version_#\t1\na\ty\n");
    assert_eq!(records(d.path(), &["clear", "s.tsvz"]).0, 0);
    assert_eq!(file(d.path(), "s.tsvz"), "#hdr\n");
    fs::write(d.path().join("s.tsvz.1"), "z\t1\n").unwrap();
    // A multi-part store: the parts before the active one become a snapshot part (§19.2).
    assert_eq!(records(d.path(), &["scrub", "s.tsvz"]).0, 0);
    assert_eq!(file(d.path(), "s.tsvz.0"), "#hdr\n#_version_#\t1\n");
    assert_eq!(records(d.path(), &["read", "s.tsvz"]).1, "z\t1\n");
    // No ordinal is free between .0 and .1 for another one.
    let (code, _, err) = records(d.path(), &["scrub", "s.tsvz"]);
    assert!(code == 1 && err.contains("no ordinal is free"), "{err}");
    assert_eq!(records(d.path(), &["scrub", "none.tsvz"]).0, 1);
    let damaged = store("#_on_corruption_#\tstop\na\t1\n\0\0c\t3\n");
    let (code, _, err) = records(damaged.path(), &["scrub", "s.tsvz"]);
    assert!(code == 1 && err.contains("damaged-"), "{err}");
}

#[test]
fn table_format_on_request() {
    let d = store("alpha\tA\t1\nbeta\tB<sep>x\t2\n");
    let (code, out, _) = run_in(d.path(), &["--format", "table", "read", "s.tsvz"], b"");
    assert_eq!(
        (code, out.as_str()),
        (0, "alpha | A    | 1\nbeta  | B\\tx | 2\n\n")
    );
    let (_, empty, _) = run_in(d.path(), &["--format", "table", "verify", "s.tsvz"], b"");
    assert_eq!(empty, "");
}

#[test]
fn loose_files_by_the_simple_rule() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    fs::write(
        dir.join("s.csv"),
        "#name,age\nalice,30\nbob,40\nbob\ncarol,<lt>\n",
    )
    .unwrap();
    assert_eq!(records(dir, &["read", "s.csv"]).1, "alice,30\ncarol,<lt>\n");
    let (code, _, err) = records(dir, &["set", "s.csv", "dave", "a,b"]);
    assert!(code == 1 && err.contains("loose file"), "{err}");
    assert_eq!(records(dir, &["set", "s.csv", "dave", "41"]).0, 0);
    assert_eq!(
        records(dir, &["verify", "s.csv"]),
        (0, String::new(), String::new())
    );
    assert_eq!(records(dir, &["parts", "s.csv"]).1, "0\t\ts.csv\tactive\n");
    assert_eq!(records(dir, &["scrub", "s.csv"]).0, 0);
    assert_eq!(
        file(dir, "s.csv"),
        "#name,age\nalice,30\ncarol,<lt>\ndave,41\n"
    );
    assert_eq!(records(dir, &["clear", "s.csv"]).0, 0);
    assert_eq!(file(dir, "s.csv"), "#name,age\n");
}

#[test]
fn the_delimiter_option() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    fs::write(dir.join("t.tsv"), "k;v\n").unwrap();
    assert_eq!(records(dir, &["-d", ";", "get", "t.tsv", "k"]).1, "k;v\n");
    assert_eq!(records(dir, &["-d", ";", "set", "t.tsv", "j", "w\tx"]).0, 0);
    assert_eq!(file(dir, "t.tsv"), "k;v\nj;w\tx\n");
    fs::write(dir.join("s.tsvz"), "k\tv\n").unwrap();
    let (code, out, err) = records(dir, &["-d", ";", "get", "s.tsvz", "k"]);
    assert_eq!((code, out.as_str()), (0, "k\tv\n"));
    assert!(err.contains("-d ignored"), "{err}");
    let (_, _, quiet) = records(dir, &["-q", "-d", ";", "get", "s.tsvz", "k"]);
    assert_eq!(quiet, "");
}

/// Peak RSS in KiB of `tsvz ARGS` run in `dir`, read with wait4 so no other child counts.
#[cfg(target_os = "linux")]
#[allow(clippy::zombie_processes)] // reaped by wait4 below, which also reads its peak RSS
fn peak_rss_kib(dir: &Path, args: &[&str]) -> (String, i64) {
    use std::io::Read;
    let mut child = tsvz()
        .current_dir(dir)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut out = String::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut out)
        .unwrap();
    let mut status = 0;
    // SAFETY: an all-zero rusage is valid; wait4 reaps exactly this child and fills it.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    assert_eq!(
        unsafe { libc::wait4(child.id() as i32, &mut status, 0, &mut usage) },
        child.id() as i32
    );
    (out, usage.ru_maxrss)
}

#[cfg(target_os = "linux")]
#[test]
fn many_small_compressed_members_read_in_bounded_memory() {
    let d = tempfile::tempdir().unwrap();
    let mut part = Vec::new();
    for i in 0..10_000 {
        part.extend(
            tsvz::codec::encode_member(tsvz::name::Codec::Gzip, format!("k{i}\tv\n").as_bytes())
                .unwrap(),
        );
    }
    fs::write(d.path().join("s.tsvz.gz"), &part).unwrap();
    let (out, kib) = peak_rss_kib(d.path(), &["--format", "records", "len", "s.tsvz.gz"]);
    assert_eq!(out, "10000\n");
    assert!(
        kib < 64 * 1024,
        "reading a {} KiB part of 10,000 members peaked at {kib} KiB",
        part.len() / 1024
    );
}

#[test]
fn clear_and_scrub_refuse_a_path_that_names_one_part() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    fs::write(dir.join("s.tsvz"), "a\t1\nb\t2\nc\t3\n").unwrap();
    fs::write(dir.join("s.tsvz.1"), "a\nd\t4\n").unwrap();
    for op in ["scrub", "clear"] {
        let (code, _, err) = records(dir, &[op, "s.tsvz.1"]);
        assert!(code == 1 && err.contains("one part"), "{op}: {code} {err}");
        assert_eq!(file(dir, "s.tsvz.1"), "a\nd\t4\n", "{op}");
        assert_eq!(file(dir, "s.tsvz"), "a\t1\nb\t2\nc\t3\n", "{op}");
    }
}

#[test]
fn a_loose_file_is_one_file_even_with_numbered_siblings() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    fs::write(dir.join("app.tsv"), "a\tnew\n").unwrap();
    fs::write(dir.join("app.tsv.1"), "a\told\nb\told\n").unwrap();
    assert_eq!(
        records(dir, &["parts", "app.tsv"]).1,
        "0\t\tapp.tsv\tactive\n"
    );
    assert_eq!(records(dir, &["read", "app.tsv"]).1, "a\tnew\n");
    assert_eq!(records(dir, &["set", "app.tsv", "c", "3"]).0, 0);
    assert_eq!(file(dir, "app.tsv"), "a\tnew\nc\t3\n");
    assert_eq!(file(dir, "app.tsv.1"), "a\told\nb\told\n");
}

#[cfg(target_os = "linux")]
#[test]
fn a_rewrite_that_cannot_get_its_space_leaves_the_part_as_it_was() {
    use std::os::unix::process::CommandExt;
    let d = tempfile::tempdir().unwrap();
    // Resolving the defaults makes every scrubbed row longer than its written form.
    let original: String = std::iter::once("#_defaults_#\tdefault-one\tdefault-two\n".to_string())
        .chain((0..2000).map(|i| format!("k{i}\tv\n")))
        .collect();
    fs::write(d.path().join("s.tsvz"), &original).unwrap();
    let limit = original.len() as u64 + 100;
    let mut command = tsvz();
    command.current_dir(d.path()).args(["scrub", "s.tsvz"]);
    // SAFETY: only async-signal-safe calls between fork and exec.
    unsafe {
        command.pre_exec(move || {
            libc::signal(libc::SIGXFSZ, libc::SIG_IGN);
            let rlimit = libc::rlimit {
                rlim_cur: limit,
                rlim_max: limit,
            };
            if libc::setrlimit(libc::RLIMIT_FSIZE, &rlimit) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let out = command.output().unwrap();
    assert_eq!(
        out.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        file(d.path(), "s.tsvz") == original,
        "the part was changed by a rewrite that could not finish"
    );
}

#[test]
fn tombstones_for_keys_ending_in_space_delete_that_key() {
    let stored =
        "#_strip_trailing_whites_#\tfalse\nk \tv1\n#_strip_trailing_whites_#\ttrue\nk\tv2\n";
    let d = store(stored);
    assert_eq!(
        records(d.path(), &["popitem", "s.tsvz", "first"]).1,
        "k \tv1\n"
    );
    assert_eq!(records(d.path(), &["read", "s.tsvz"]).1, "k\tv2\n");
    let m = tempfile::tempdir().unwrap();
    fs::write(
        m.path().join("s.tsvz"),
        "#_strip_trailing_whites_#\tfalse\nk \tv1\n#_strip_trailing_whites_#\ttrue\n",
    )
    .unwrap();
    fs::write(m.path().join("s.tsvz.1"), "k\tv2\n").unwrap();
    assert_eq!(records(m.path(), &["clear", "s.tsvz"]).0, 0);
    assert_eq!(records(m.path(), &["read", "s.tsvz"]).1, "");
}

#[cfg(target_os = "linux")]
fn run_full(
    dir: &Path,
    args: &[&str],
    stdout_full: bool,
    stderr_full: bool,
) -> (Option<i32>, String) {
    let full = || {
        Stdio::from(
            fs::OpenOptions::new()
                .write(true)
                .open("/dev/full")
                .unwrap(),
        )
    };
    let out = tsvz()
        .current_dir(dir)
        .args(args)
        .stdout(if stdout_full { full() } else { Stdio::null() })
        .stderr(if stderr_full { full() } else { Stdio::piped() })
        .output()
        .unwrap();
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stderr).into(),
    )
}

#[cfg(target_os = "linux")]
#[test]
fn unwritable_output_never_panics() {
    let d = store("a\tx\ntorn");
    for (args, stdout_full, stderr_full, want) in [
        (&["-V"][..], true, false, 1),
        (&["--help"][..], true, false, 1),
        (
            &["--format", "records", "read", "s.tsvz"][..],
            true,
            false,
            1,
        ),
        (
            &["--format", "records", "read", "none.tsvz"][..],
            false,
            true,
            1,
        ),
        (
            &["--format", "records", "read", "s.tsvz"][..],
            false,
            true,
            0,
        ),
        (&["frobnicate", "s.tsvz"][..], false, true, 2),
    ] {
        let (code, err) = run_full(d.path(), args, stdout_full, stderr_full);
        assert_eq!(code, Some(want), "{args:?}: {err}");
        assert!(!err.contains("panicked"), "{args:?}: {err}");
    }
}

#[cfg(target_os = "linux")]
#[test]
fn a_pop_that_cannot_print_deletes_nothing() {
    let d = store("a\t1\nb\t2\n");
    for args in [
        &["--format", "records", "pop", "s.tsvz", "a"][..],
        &["--format", "records", "popitem", "s.tsvz"],
    ] {
        let (code, _) = run_full(d.path(), args, true, false);
        assert_eq!(code, Some(1), "{args:?}");
        assert_eq!(file(d.path(), "s.tsvz"), "a\t1\nb\t2\n", "{args:?}");
    }
}

#[test]
fn a_backup_left_in_another_group_loses_its_group_bits() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    // Needs a user who may give the copy neither the part's owner nor its group: root runs
    // the scrub as nobody, on a part of root's that nobody can write.
    // SAFETY: geteuid only reads the process's user id.
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipped: needs root to run the scrub as nobody");
        return;
    }
    let d = store("#_on_corruption_#\tstop\na\t1\n\0\0c\t3\n");
    fs::set_permissions(d.path(), fs::Permissions::from_mode(0o777)).unwrap();
    let part = d.path().join("s.tsvz");
    fs::set_permissions(&part, fs::Permissions::from_mode(0o666)).unwrap();
    let ran = Command::new("setpriv")
        .args(["--reuid=65534", "--regid=65534", "--clear-groups", "--"])
        .arg(env!("CARGO_BIN_EXE_tsvz"))
        .args(["-q", "scrub"])
        .arg(&part)
        .output();
    let Ok(out) = ran else {
        eprintln!("skipped: no setpriv");
        return;
    };
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let copy = fs::read_dir(d.path())
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.to_string_lossy().contains(".damaged-"))
        .expect("no backup");
    let meta = fs::metadata(&copy).unwrap();
    assert_ne!(meta.gid(), 0, "nobody cannot give the copy root's group");
    assert_eq!(meta.mode() & 0o070, 0, "mode {:o}", meta.mode() & 0o7777);
}

#[test]
fn an_unreadable_part_exits_1_naming_it() {
    use std::os::unix::fs::PermissionsExt;
    // SAFETY: geteuid only reads the process's user id.
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("skipped: root can read any file");
        return;
    }
    let d = store("a\tx\n");
    let part = d.path().join("s.tsvz.1");
    fs::write(&part, "b\ty\n").unwrap();
    fs::set_permissions(&part, fs::Permissions::from_mode(0o000)).unwrap();
    let (code, out, err) = records(d.path(), &["read", "s.tsvz"]);
    fs::set_permissions(&part, fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!((code, out.as_str()), (1, "a\tx\n"), "{err}");
    assert!(
        err.contains("s.tsvz.1") && err.contains("ermission") && !err.contains("panicked"),
        "{err}"
    );
}

#[test]
fn a_key_ending_in_white_space_names_the_live_key_equal_to_it() {
    // §20.2.7: k<space> was written while stripping was off; stripping is on at the end.
    let both = "#_strip_trailing_whites_#\tfalse\nk \tv1\n#_strip_trailing_whites_#\ttrue\nk\tv2\n";
    let d = store(both);
    assert_eq!(records(d.path(), &["get", "s.tsvz", "k "]).1, "k \tv1\n");
    assert_eq!(records(d.path(), &["get", "s.tsvz", "k"]).1, "k\tv2\n");
    assert_eq!(
        records(d.path(), &["setdefault", "s.tsvz", "k ", "z"]).1,
        "k \tv1\n"
    );
    assert_eq!(records(d.path(), &["pop", "s.tsvz", "k "]).1, "k \tv1\n");
    assert_eq!(records(d.path(), &["read", "s.tsvz"]).1, "k\tv2\n");
    let d = store(both);
    assert_eq!(records(d.path(), &["delete", "s.tsvz", "k ", "k "]).0, 0);
    assert_eq!(records(d.path(), &["read", "s.tsvz"]).1, "k\tv2\n");
    let d = store(both);
    let keys = records(d.path(), &["keys", "s.tsvz"]).1;
    assert_eq!(keys, "k \nk\n");
    assert_eq!(
        run_in(d.path(), &["delete", "s.tsvz", "-"], keys.as_bytes()).0,
        0
    );
    assert_eq!(records(d.path(), &["len", "s.tsvz"]).1, "0\n");
    let d = store("k\tplain\nk<sep>\ttab\n");
    assert_eq!(
        records(d.path(), &["get", "s.tsvz", "k\t"]).1,
        "k<sep>\ttab\n"
    );
    assert_eq!(records(d.path(), &["delete", "s.tsvz", "k\t"]).0, 0);
    assert_eq!(records(d.path(), &["read", "s.tsvz"]).1, "k\tplain\n");
}

#[test]
fn a_delete_that_must_read_the_store_writes_nothing_when_it_cannot() {
    let content = "#_on_corruption_#\tstop\nk\t1\n\0\0c\t3\n";
    let d = store(content);
    let (code, out, err) = records(d.path(), &["delete", "s.tsvz", "k "]);
    assert_eq!((code, out.as_str()), (1, ""), "{err}");
    assert_eq!(file(d.path(), "s.tsvz"), content);
    assert_eq!(
        records(d.path(), &["delete", "s.tsvz", "k"]).0,
        0,
        "k names itself: no read"
    );
}
