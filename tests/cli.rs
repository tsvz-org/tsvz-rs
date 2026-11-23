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
    // A multi-part store: the parts before the active one are compacted into the last of them.
    fs::write(d.path().join("s.tsvz"), "#hdr\na\t1\na\n").unwrap();
    assert_eq!(records(d.path(), &["scrub", "s.tsvz"]).0, 0);
    assert_eq!(file(d.path(), "s.tsvz"), "#hdr\n#_version_#\t1\n");
    assert_eq!(records(d.path(), &["read", "s.tsvz"]).1, "z\t1\n");
    assert_eq!(records(d.path(), &["scrub", "s.tsvz"]).0, 0, "and again");
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
fn loose_files_as_tsvz_py_reads_and_writes_them() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    fs::write(
        dir.join("s.csv"),
        "#name,age\nalice,30\nbob,40\nbob\ncarol,<lt>\n",
    )
    .unwrap();
    assert_eq!(records(dir, &["read", "s.csv"]).1, "alice,30\ncarol,<lt>\n");
    // A cell that holds the delimiter is written `<sep>`, as TSVZ.py writes it, and reads back.
    assert_eq!(records(dir, &["set", "s.csv", "dave", "a,b"]).0, 0);
    assert_eq!(records(dir, &["get", "s.csv", "dave"]).1, "dave,a<sep>b\n");
    let (code, _, err) = records(dir, &["set", "s.csv", "eve", "x</sep/>"]);
    assert!(code == 1 && err.contains("reads back as another"), "{err}");
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

/// A loose file is read as TSVZ.py's command line reads it (`_processLine`): keys and cells
/// without the whitespace at their end and decoded (`<sep>`, `<LF>`), `#_defaults_#` lines that
/// fill the empty cells after them, a row wider than the column count without the empty cells at
/// its end, the unterminated last line as a line, and no row for a missing key.
#[test]
fn loose_files_are_read_as_tsvz_py_reads_them() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    fs::write(
        dir.join("f.tsv"),
        "#_defaults_#\tP\tQ\na \t\t1 \r\r\nb\t2\nc\tx<sep>y</sep/><LF>\t\t\nw\t1\t2\t\t\n  \t9\nlast\t5",
    )
    .unwrap();
    assert_eq!(
        records(dir, &["read", "f.tsv"]).1,
        "a\tP\t1\nb\t2\tQ\nc\tx<sep>y</sep/><LF>\tQ\nw\t1\t2\nlast\t5\tQ\n"
    );
    assert_eq!(
        records(dir, &["get", "f.tsv", "a  "]),
        (0, "a\tP\t1\n".into(), String::new())
    );
    assert_eq!(
        records(dir, &["get", "f.tsv", "zz"]),
        (3, String::new(), String::new())
    );
    // A write ends the unterminated last line first, as TSVZ.py's does.
    assert_eq!(records(dir, &["set", "f.tsv", "n", "7"]).0, 0);
    assert!(file(dir, "f.tsv").ends_with("last\t5\nn\t7\n"));
    assert_eq!(
        records(dir, &["get", "f.tsv", "last", "n"]).1,
        "last\t5\tQ\nn\t7\tQ\n"
    );
    // Lines on standard input are in the file's own encoding, as TSVZ.py takes them.
    let bulk = run_in(dir, &["set", "f.tsv", "-"], b"m\tx<sep>y\t1\n");
    assert_eq!(bulk.0, 0, "{}", bulk.2);
    assert!(file(dir, "f.tsv").ends_with("m\tx<sep>y\t1\n"));
    assert_eq!(records(dir, &["get", "f.tsv", "m"]).1, "m\tx<sep>y\t1\n");
}

#[test]
fn a_one_column_loose_file_holds_keys_alone() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    // As TSVZ.py reads it: the first keyed line sets the column count; at one column a key
    // alone is a row, and nothing can delete, so a delete rewrites the file.
    fs::write(dir.join("hosts.tsv"), "#hosts\nh1\nh2\nh3\nh2\n").unwrap();
    assert_eq!(records(dir, &["keys", "hosts.tsv"]).1, "h1\nh2\nh3\n");
    assert_eq!(records(dir, &["delete", "hosts.tsv", "h2", "h9"]).0, 0);
    assert_eq!(file(dir, "hosts.tsv"), "#hosts\nh1\nh3\n");
    assert_eq!(
        records(dir, &["pop", "hosts.tsv", "h1"]),
        (0, "h1\n".into(), String::new())
    );
    assert_eq!(file(dir, "hosts.tsv"), "#hosts\nh3\n");
    assert_eq!(records(dir, &["set", "hosts.tsv", "h4"]).0, 0);
    assert_eq!(records(dir, &["scrub", "hosts.tsv"]).0, 0);
    assert_eq!(file(dir, "hosts.tsv"), "#hosts\nh3\nh4\n");
    // A missing file is made, and tsvz-rs holds nothing in it (TSVZ.py writes one empty line; the
    // two read alike): in a file with no keyed line yet, a key alone would make it one column,
    // and a row.
    assert_eq!(records(dir, &["delete", "new.tsv", "k"]).0, 0);
    assert_eq!(file(dir, "new.tsv"), "");
    assert_eq!(records(dir, &["keys", "new.tsv"]).1, "");
}

#[test]
fn a_loose_delete_decides_from_the_column_count_alone() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    // More than one column: a key alone deletes, and one is appended for each key asked, live
    // or not, as a reader names it: without the whitespace at its end.
    fs::write(dir.join("m.tsv"), "b\t1\nc\t3\n").unwrap();
    let (code, _, err) = records(dir, &["-v", "delete", "m.tsv", "b ", "zz"]);
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("appended 2 tombstones"), "{err}");
    assert_eq!(file(dir, "m.tsv"), "b\t1\nc\t3\nb\nzz\n");
    // `b` is gone, and a line whose key is `b ` is `b` too.
    assert_eq!(records(dir, &["keys", "m.tsv"]).1, "c\n");
    fs::write(dir.join("w.tsv"), "b \t1\nc\t3\n").unwrap();
    assert_eq!(records(dir, &["delete", "w.tsv", "b "]).0, 0);
    assert_eq!(records(dir, &["keys", "w.tsv"]).1, "c\n");
    // A reset of the defaults is a line there too.
    fs::write(dir.join("d.tsv"), "#_defaults_#\tX\tY\na\t1\nb\t2\n").unwrap();
    assert_eq!(records(dir, &["delete", "d.tsv", "#_defaults_#"]).0, 0);
    assert_eq!(
        file(dir, "d.tsv"),
        "#_defaults_#\tX\tY\na\t1\nb\t2\n#_defaults_#\n"
    );
    // pop and popitem delete the same way.
    fs::write(dir.join("p.tsv"), "a\t1\nb\t2\n").unwrap();
    assert_eq!(records(dir, &["pop", "p.tsv", "a"]).1, "a\t1\n");
    assert_eq!(records(dir, &["popitem", "p.tsv"]).1, "b\t2\n");
    assert_eq!(file(dir, "p.tsv"), "a\t1\nb\t2\na\nb\n");
    // No keyed line yet: nothing is written, and an empty file stays empty.
    for (name, text) in [("e.tsv", ""), ("c.tsv", "#h\n\n#c\n")] {
        fs::write(dir.join(name), text).unwrap();
        assert_eq!(records(dir, &["delete", name, "k", "#_defaults_#"]).0, 0);
        assert_eq!(file(dir, name), text);
    }
    // One column: the lines of the keys go, whether or not the key as given has whitespace at
    // its end.
    fs::write(dir.join("o.tsv"), "#h\na\nb \nc\nb\n").unwrap();
    assert_eq!(records(dir, &["delete", "o.tsv", "b\t"]).0, 0);
    assert_eq!(file(dir, "o.tsv"), "#h\na\nc\n");
    // A reset of the defaults there would be a row, to TSVZ.py too: none is written.
    assert_eq!(records(dir, &["delete", "o.tsv", "#_defaults_#"]).0, 0);
    assert_eq!(file(dir, "o.tsv"), "#h\na\nc\n");
}

#[test]
fn a_loose_delete_of_a_key_of_only_whitespace_writes_nothing() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    // A key of only whitespace names nothing: a reader skips a line whose key is only
    // whitespace, as TSVZ.py does. A tab is such a key too, cut as a reader cuts a key's end.
    fs::write(dir.join("m.tsv"), "b\t1\n \t2\nc\t3\n").unwrap();
    assert_eq!(records(dir, &["keys", "m.tsv"]).1, "b\nc\n");
    for key in ["\t", " "] {
        let (code, _, err) = records(dir, &["-v", "delete", "m.tsv", key]);
        assert_eq!((code, err.as_str()), (0, ""), "{key:?}");
    }
    let (code, _, err) = records(dir, &["-v", "delete", "m.tsv", " ", "zz"]);
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("appended 1 tombstones"), "{err}");
    assert_eq!(file(dir, "m.tsv"), "b\t1\n \t2\nc\t3\nzz\n");
    // At one column too.
    fs::write(dir.join("o.tsv"), "a\n \nb\n").unwrap();
    assert_eq!(records(dir, &["delete", "o.tsv", " "]).0, 0);
    assert_eq!(file(dir, "o.tsv"), "a\n \nb\n");
    assert_eq!(records(dir, &["keys", "o.tsv"]).1, "a\nb\n");
}

/// What the read of the head warns of is said once, though a file of one column is read again
/// from the start.
#[test]
fn a_loose_delete_says_what_the_head_warns_of_once() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    let warned = |name: &str, bytes: &[u8]| {
        fs::write(dir.join(name), bytes).unwrap();
        records(dir, &["delete", name, "zz"])
            .2
            .matches("line 1 is not valid UTF-8")
            .count()
    };
    assert_eq!(warned("w1.tsv", b"#\xff\na\nb\n"), 1);
    assert_eq!(warned("w2.tsv", b"#\xff\na\t1\n"), 1);
    assert_eq!(warned("w3.tsv", b"#\xff\n"), 1);
}

/// What the read of a compressed file's head says is said once, with the file's name, whether
/// the head finds a keyed line (and the read stops there) or not (and the whole file is read).
#[test]
fn a_compressed_loose_delete_says_its_warnings_once_and_names_the_file() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    let gz = |t: &[u8]| tsvz::codec::encode_member(tsvz::name::Codec::Gzip, t).unwrap();
    for (name, bytes) in [
        ("w4.tsv.gz", gz(b"#\xff\na\t1\n")),
        ("w5.tsv.gz", [gz(b"#\xff\n"), gz(b"a\t1\n")].concat()),
        ("w6.tsv.gz", gz(b"#\xff\n")),
        ("w7.tsv.gz", gz(b"#\xff\na\nb\n")),
    ] {
        fs::write(dir.join(name), bytes).unwrap();
        let (code, _, err) = records(dir, &["delete", name, "zz"]);
        assert_eq!(code, 0, "{name}: {err}");
        assert_eq!(err.lines().count(), 1, "{name}: {err}");
        assert!(
            err.contains(&format!("{name}: line 1 is not valid UTF-8")),
            "{name}: {err}"
        );
    }
}

/// A file whose only lines have a key of only whitespace has no keyed line and no row (a reader
/// skips such lines, as TSVZ.py does): `popitem` finds nothing, and a delete writes nothing there,
/// since a key alone would set one column and be a row.
#[test]
fn a_file_of_blank_keys_has_no_row_and_a_delete_writes_nothing_there() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    for text in [" \t5\n", "#h\n \n\x0b\t2\n"] {
        fs::write(dir.join("n.tsv"), text).unwrap();
        let rows = records(dir, &["len", "n.tsv"]).1;
        let rows: usize = rows.trim().parse().unwrap();
        for _ in 0..rows {
            assert_eq!(records(dir, &["popitem", "n.tsv"]).0, 0, "{text:?}");
        }
        assert_eq!(records(dir, &["popitem", "n.tsv"]).0, 3, "{text:?}");
        assert_eq!(records(dir, &["keys", "n.tsv"]).1, "", "{text:?}");
    }
    fs::write(dir.join("z.tsv"), " \t5\n").unwrap();
    assert_eq!(records(dir, &["delete", "z.tsv", "zz"]).0, 0);
    assert_eq!(file(dir, "z.tsv"), " \t5\n");
}

/// `popitem` until the file is empty: it ends within as many calls as there are keys, so a loop
/// that drains a file ends. A line whose key is only whitespace is no row.
#[test]
fn popitem_drains_a_loose_file_with_a_blank_key() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    for (name, text, rows) in [
        ("m.tsv", " \t2\na\t1\nb\t3\n", 2),
        ("o.tsv", "a\n \nb\n", 2),
    ] {
        fs::write(dir.join(name), text).unwrap();
        let mut popped = Vec::new();
        for _ in 0..rows {
            let (code, out, err) = records(dir, &["popitem", name]);
            assert_eq!((code, err.as_str()), (0, ""), "{name}");
            popped.push(out);
        }
        popped.sort();
        popped.dedup();
        assert_eq!(popped.len(), rows, "{name}: each call pops another row");
        assert_eq!(records(dir, &["popitem", name]).0, 3, "{name}: then empty");
        assert_eq!(records(dir, &["keys", name]).1, "", "{name}");
    }
}

#[test]
fn a_loose_delete_of_more_than_one_column_reads_no_further_than_its_first_keyed_line() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    // Line 2 holds bytes that are no UTF-8: a read of it warns, and one that stops at line 1 does
    // not get there.
    fs::write(dir.join("m.tsv"), b"a\t1\n\xff\t2\n").unwrap();
    let (code, _, err) = records(dir, &["delete", "m.tsv", "zz"]);
    assert_eq!((code, err.as_str()), (0, ""));
    assert!(
        records(dir, &["len", "m.tsv"])
            .2
            .contains("line 2 is not valid UTF-8")
    );
}

#[test]
fn a_loose_row_of_blank_cells_is_a_deletion_but_at_one_column() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    fs::write(dir.join("t.tsv"), "a\t1\nb\t2\nb\t\t \n").unwrap();
    assert_eq!(records(dir, &["keys", "t.tsv"]).1, "a\n");
    assert_eq!(records(dir, &["set", "t.tsv", "a", ""]).0, 0);
    assert_eq!(records(dir, &["len", "t.tsv"]).1, "0\n");
    fs::write(dir.join("one.tsv"), "a\nb\t\n").unwrap();
    assert_eq!(records(dir, &["read", "one.tsv"]).1, "a\nb\t\n");
}

#[test]
fn a_loose_scrub_and_clear_keep_what_tsvz_py_reads() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    // #_defaults_# lines stay in place (TSVZ.py fills the empty cells after them); a key's
    // last line takes its first one's place only where no such line lies between them.
    let text = "#h\tx\ty\n#_defaults_#\tP\tQ\na\t1\t\nb\t\t2\n#c\na\t3\t\n#_defaults_#\tR\nb\t\t4\nc\t\t\n";
    fs::write(dir.join("t.tsv"), text).unwrap();
    assert_eq!(records(dir, &["scrub", "t.tsv"]).0, 0);
    assert_eq!(
        file(dir, "t.tsv"),
        "#h\tx\ty\n#_defaults_#\tP\tQ\na\t3\t\nb\t\t2\n#_defaults_#\tR\nb\t\t4\n"
    );
    assert_eq!(records(dir, &["clear", "t.tsv"]).0, 0);
    assert_eq!(
        file(dir, "t.tsv"),
        "#h\tx\ty\n#_defaults_#\tP\tQ\n#_defaults_#\tR\n"
    );
}

#[test]
fn a_loose_rewrite_keeps_the_bytes_after_the_last_newline() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    // Both read that line as a row; a scrub and a one-column delete leave it where it is.
    fs::write(dir.join("s.tsv"), "#h\na\t1\na\t2\nb\t9").unwrap();
    assert_eq!(records(dir, &["scrub", "s.tsv"]).0, 0);
    assert_eq!(file(dir, "s.tsv"), "#h\na\t2\nb\t9");
    fs::write(dir.join("o.tsv"), "a\nb\nc\nd").unwrap();
    assert_eq!(records(dir, &["delete", "o.tsv", "b"]).0, 0);
    assert_eq!(file(dir, "o.tsv"), "a\nc\nd");
    // A key with whitespace at its end names the key without it, as TSVZ.py reads it.
    fs::write(dir.join("w.tsv"), "a \nb\nc\n").unwrap();
    assert_eq!(records(dir, &["delete", "w.tsv", "a "]).0, 0);
    assert_eq!(file(dir, "w.tsv"), "b\nc\n");
}

#[test]
fn a_loose_rewrite_keeps_the_lines_it_keeps_as_they_are() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    // CRLF lines stay CRLF, in a scrub, a delete and a clear alike.
    fs::write(dir.join("a.tsv"), "#h\r\na\t1\r\nb\t2\r\n\r\na\t3\r\n").unwrap();
    assert_eq!(records(dir, &["scrub", "a.tsv"]).0, 0);
    assert_eq!(file(dir, "a.tsv"), "#h\r\na\t3\r\nb\t2\r\n");
    fs::write(dir.join("o.tsv"), "#h\r\na\r\nb\r\nc\r\n").unwrap();
    assert_eq!(records(dir, &["delete", "o.tsv", "b"]).0, 0);
    assert_eq!(file(dir, "o.tsv"), "#h\r\na\r\nc\r\n");
    assert_eq!(records(dir, &["clear", "a.tsv"]).0, 0);
    assert_eq!(file(dir, "a.tsv"), "#h\r\n");
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

/// A loose `set` writes nothing that a reader could not read back as it was set: not a cell whose
/// encoding holds the delimiter (`-d e`: `<sep>` has an `e`), not a key that begins with `#` or a
/// NUL, or is only whitespace. Each is refused, as 0.2.0 refused the first, with nothing written.
#[test]
fn a_loose_set_that_could_not_be_read_back_is_refused() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    fs::write(dir.join("t.tsv"), "a\t1\n").unwrap();
    for args in [
        &["set", "t.tsv", "#tag", "1"][..],
        &["set", "t.tsv", " ", "1"],
        &["set", "t.tsv", "#_rotate_#", "1"],
    ] {
        let (code, _, err) = records(dir, args);
        assert!(
            code == 1 && err.contains("never be read back"),
            "{args:?} {err}"
        );
        assert_eq!(file(dir, "t.tsv"), "a\t1\n", "{args:?}");
    }
    // A comment is no key, and a delete of one is no failure.
    assert_eq!(records(dir, &["delete", "t.tsv", "#tag"]).0, 0);
    let (code, _, err) = records(dir, &["-d", "e", "set", "e.tsv", "k", "hello"]);
    assert!(
        code == 1 && err.contains("delimiter 'e' inside it"),
        "{err}"
    );
    assert!(fs::read(dir.join("e.tsv")).is_ok_and(|f| f.is_empty()) || !dir.join("e.tsv").exists());
    assert_eq!(records(dir, &["-d", "e", "set", "e.tsv", "k", "hllo"]).0, 0);
    assert_eq!(file(dir, "e.tsv"), "kehllo\n");
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

/// A loose `scrub` and `clear` work on the lines they keep and read no row. On this file (11.7
/// MiB, the shape of a 228 MiB one measured the same way, 2026-11-16) the test build peaked at
/// 3.0 and 5.8 times its size for `clear` and `scrub`; with every row also built, at 8.2 and 11.5
/// times. So 7 and 9 times are between the two.
#[cfg(target_os = "linux")]
#[test]
fn a_loose_scrub_and_clear_read_no_row() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    let mut text = String::from("#key\tv1\tv2\tv3\n");
    for i in 0..200_000u64 {
        let x = i
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        text.push_str(&format!("key{i:07}\t{x:016x}\t{x:016x}\tsome text here\n"));
    }
    let size = text.len() as i64 / 1024;
    let mut over = Vec::new();
    for (op, times) in [("clear", 7), ("scrub", 9)] {
        fs::write(dir.join("big.tsv"), &text).unwrap();
        let (_, kib) = peak_rss_kib(dir, &[op, "big.tsv"]);
        if kib >= times * size {
            over.push(format!("a loose {op} of {size} KiB peaked at {kib} KiB"));
        }
    }
    assert!(over.is_empty(), "{over:?}");
}

/// A delete of a compressed loose file of more than one column decodes it as far as the end of the
/// member that holds its first keyed line: the damage of that member is found and said, once, with
/// the file's name. A first member with no keyed line, or only a header, is no matter: the read
/// goes on into the next.
#[test]
fn a_compressed_loose_delete_says_the_damage_of_the_member_with_its_first_key_once() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    let mut text = String::from("#key\tv1\tv2\tv3\n");
    for i in 0..20_000u64 {
        let x = i
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        text.push_str(&format!("key{i:07}\t{x:016x}\t{x:016x}\tsome text here\n"));
    }
    let mut member = tsvz::codec::encode_member(tsvz::name::Codec::Gzip, text.as_bytes()).unwrap();
    assert!(member.len() > 1 << 16, "{} bytes", member.len());
    let whole = member.clone();
    fs::write(dir.join("ok.tsv.gz"), &whole).unwrap();
    let middle = member.len() / 2;
    member[middle] ^= 0xff;
    fs::write(dir.join("big.tsv.gz"), &member).unwrap();
    assert!(
        records(dir, &["len", "big.tsv.gz"]).2.contains("corrupt"),
        "a read of the whole damaged member says so"
    );
    let says_once = |name: &str| {
        let (code, _, err) = records(dir, &["delete", name, "key0000005"]);
        assert_eq!(code, 0, "{name}: {err}");
        assert_eq!(err.lines().count(), 1, "{name}: {err}");
        assert!(err.contains(&format!("{name}: ")), "{name}: {err}");
        assert!(err.contains("corrupt"), "{name}: {err}");
    };
    says_once("big.tsv.gz");
    // The same delete of the undamaged file: the key is gone, and the others are not.
    assert_eq!(records(dir, &["delete", "ok.tsv.gz", "key0000005"]).0, 0);
    assert_eq!(records(dir, &["has", "ok.tsv.gz", "key0000005"]).0, 3);
    assert_eq!(records(dir, &["has", "ok.tsv.gz", "key0000006"]).0, 0);
    // A header that a member holds alone, as a `clear` and a later write leave a file, and rows in a
    // damaged member.
    let gz = |t: &str| tsvz::codec::encode_member(tsvz::name::Codec::Gzip, t.as_bytes()).unwrap();
    let rows = &text[text.find('\n').unwrap() + 1..];
    let mut second = gz(rows);
    let middle = second.len() / 2;
    second[middle] ^= 0xff;
    fs::write(
        dir.join("hdr.tsv.gz"),
        [gz("#key\tv1\tv2\tv3\n"), second].concat(),
    )
    .unwrap();
    says_once("hdr.tsv.gz");
    // A first member with no keyed line: the read goes on into the next.
    fs::write(
        dir.join("two.tsv.gz"),
        [gz("#h\n"), gz("a\t1\nb\t2\n")].concat(),
    )
    .unwrap();
    assert_eq!(records(dir, &["delete", "two.tsv.gz", "a"]).0, 0);
    assert_eq!(records(dir, &["keys", "two.tsv.gz"]).1, "b\n");
}

/// A compressed loose delete decides from the file as the whole read sees it, also when a member
/// that holds only comment lines is cut short and members of rows follow. A look at the first
/// lines alone fed the cut member's decoder the next member's bytes, and took the garbage that
/// came out for a keyed line, so that it gave a wrong column count: the delete then failed with
/// "the file kept changing", or decided from a wrong column count (in the files that found it, a key
/// alone written at one column, so that a `popitem` loop never ended). The tiny files below: a gzip member cut inside its data and a
/// member `z`, and the same in zstd.
#[test]
fn a_compressed_loose_delete_after_a_cut_comment_member_decides_as_the_whole_read_does() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    let gz = |t: &[u8]| tsvz::codec::encode_member(tsvz::name::Codec::Gzip, t).unwrap();
    // 600 comment lines of random words (about half with a tab), so that the member is a few KiB of
    // dynamic Huffman codes, cut at 63 points: the look failed at 5 of them at two columns and
    // at 1 at one (counted on the build before the look was removed, 0e0e128).
    let mut x = 0x1234_5678_9abc_def1u64;
    let mut next = move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    let words = [
        "alpha", "beta", "gamma", "delta", "x", "yy", "zzz", "key", "\t", "0", "17",
    ];
    let mut comments = String::new();
    for _ in 0..600 {
        comments.push('#');
        for _ in 0..3 + next() % 12 {
            comments.push_str(words[(next() % words.len() as u64) as usize]);
            comments.push(' ');
        }
        comments.push('\n');
    }
    let member = gz(comments.as_bytes());
    for one in [false, true] {
        let rows: String = (0..200)
            .map(|i| match one {
                true => format!("k{i}x{}\n", next() % 1000),
                false => format!("k{i}\t{:016x}\n", next()),
            })
            .collect();
        let tail = gz(rows.as_bytes());
        let name = "c.tsv.gz";
        for i in 1..64 {
            let cut = member.len() * i / 64;
            fs::write(dir.join(name), [&member[..cut], &tail[..]].concat()).unwrap();
            let why = format!("one column {one}, cut at {cut} of {} bytes", member.len());
            let keys = records(dir, &["keys", name]).1;
            assert_eq!(keys.lines().count(), 200, "{why}");
            let key = keys.lines().nth(7).unwrap().to_string();
            assert!(key.starts_with("k7"), "{why}: {key}");
            let (code, _, err) = records(dir, &["delete", name, &key]);
            assert_eq!(code, 0, "{why}: {err}");
            assert_eq!(err.matches("cut short").count(), 1, "{why}: {err}");
            assert_eq!(
                records(dir, &["has", name, &key]).0,
                3,
                "{why}: still there"
            );
            let keys = records(dir, &["keys", name]).1;
            assert_eq!(keys.lines().count(), 199, "{why}: a key alone is now a row");
            // A `popitem` loop ends: it empties the file in as many calls as it has keys.
            if one && i % 16 == 0 {
                for n in 0..199 {
                    let (code, out, _) = records(dir, &["popitem", name]);
                    assert_eq!(code, 0, "{why}: popitem {n}");
                    assert!(!out.is_empty(), "{why}: popitem {n}");
                }
                assert_eq!(records(dir, &["popitem", name]).0, 3, "{why}: then empty");
            }
            for e in fs::read_dir(dir).unwrap() {
                let e = e.unwrap().path();
                if e.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .contains("damaged-")
                {
                    fs::remove_file(e).unwrap();
                }
            }
        }
    }
    // The tiny files: a member cut inside its data, then a member `z`.
    let hex = |s: &str| -> Vec<u8> {
        (0..s.len() / 2)
            .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap())
            .collect()
    };
    let mut tiny = vec![(
        "t.tsv.gz",
        hex(concat!(
            "1f8b08000000000000ff05c16d9381400000e0cffc8bb45d87a9bb3aed184d3b8d954aa7a1440c66bd2c",
            "75251179f9f5f73c1f8b08000000000000ffabe202009d6adc7402000000"
        )),
    )];
    if tsvz::name::Codec::Zstd.supported() {
        tiny.push((
            "t.tsv.zst",
            hex("28b52ffd2419c900006b300928b52ffd24021100007a0a10007838"),
        ));
    }
    for (name, bytes) in tiny {
        fs::write(dir.join(name), &bytes).unwrap();
        // (0e0e128 read the zstd one with a byte-order mark before `z`: the cut member's dropped
        // bytes were three, and the next line began at offset 3)
        let before = records(dir, &["keys", name]).1;
        assert_eq!(before, "z\n", "{name}");
        let (code, _, err) = records(dir, &["delete", name, "k0"]);
        assert_eq!(code, 0, "{name}: {err}");
        assert_eq!(
            records(dir, &["keys", name]).1,
            before,
            "{name}: k0 became a row"
        );
    }
}

/// A delete of a key that is not there changes nothing: a compressed file of one column too,
/// whose bytes a rewrite would change (here it holds two members, which one member replaces).
#[test]
fn a_one_column_delete_that_removes_no_line_writes_and_says_nothing() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    fs::write(dir.join("o.tsv"), "#h\na\nb \nc\nb\n").unwrap();
    let (code, _, err) = records(dir, &["-v", "delete", "o.tsv", "zz"]);
    assert_eq!((code, err.as_str()), (0, ""), "nothing was rewritten");
    assert_eq!(file(dir, "o.tsv"), "#h\na\nb \nc\nb\n");
    let gz = |t: &str| tsvz::codec::encode_member(tsvz::name::Codec::Gzip, t.as_bytes()).unwrap();
    let bytes = [gz("#h\na\n"), gz("b\n")].concat();
    fs::write(dir.join("c.tsv.gz"), &bytes).unwrap();
    let (code, _, err) = records(dir, &["-v", "delete", "c.tsv.gz", "zz"]);
    assert_eq!((code, err.as_str()), (0, ""));
    assert_eq!(fs::read(dir.join("c.tsv.gz")).unwrap(), bytes);
    assert_eq!(records(dir, &["delete", "c.tsv.gz", "a"]).0, 0);
    assert_eq!(records(dir, &["keys", "c.tsv.gz"]).1, "b\n");
}

/// A delete of a key that is not there writes nothing in a damaged compressed file either: the
/// rewrite that would drop the key's lines has none to drop, and would only leave a copy of the
/// damaged bytes (`.damaged-`) and replace them by the recovered lines.
#[test]
fn a_one_column_delete_copies_a_damaged_file_only_when_it_removes_a_line() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    let gz = |t: &str| tsvz::codec::encode_member(tsvz::name::Codec::Gzip, t.as_bytes()).unwrap();
    let mut second = gz(&"b\nc\n".repeat(20));
    let middle = second.len() / 2;
    second[middle] ^= 0xff;
    let bytes = [gz("#h\na\n"), second].concat();
    fs::write(dir.join("d.tsv.gz"), &bytes).unwrap();
    assert!(
        records(dir, &["keys", "d.tsv.gz"]).2.contains("corrupt"),
        "the file is damaged"
    );
    let (code, _, _) = records(dir, &["delete", "d.tsv.gz", "zz"]);
    assert_eq!(code, 0);
    assert_eq!(fs::read(dir.join("d.tsv.gz")).unwrap(), bytes);
    let names: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names, ["d.tsv.gz"], "no copy of the damaged part");
    // A key that is there is deleted, and the damaged bytes are kept beside the rewrite.
    let (code, _, err) = records(dir, &["delete", "d.tsv.gz", "a"]);
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("damaged-"), "{err}");
    assert_eq!(fs::read_dir(dir).unwrap().count(), 2);
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

#[test]
fn a_multi_part_clear_is_synced_before_it_answers() {
    // An acknowledged clear is durable, as a one-part clear (rewritten and synced) is: the
    // tombstones it appends to the active part are fdatasync'd before it exits.
    let dir = store("a\t1\n");
    fs::write(dir.path().join("s.tsvz.1"), "b\t2\n").unwrap();
    let traced = Command::new("strace")
        .current_dir(dir.path())
        .args([
            "-f",
            "-e",
            "trace=openat,fsync,fdatasync",
            "-o",
            "trace.txt",
        ])
        .arg(env!("CARGO_BIN_EXE_tsvz"))
        .args(["clear", "s.tsvz"])
        .output();
    let Ok(out) = traced else {
        eprintln!("no strace here: skipped");
        return;
    };
    let Ok(trace) = fs::read_to_string(dir.path().join("trace.txt")) else {
        eprintln!("strace cannot trace here: skipped");
        return;
    };
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let fd = trace
        .lines()
        .filter(|l| l.contains("\"s.tsvz.1\"") && l.contains("O_APPEND"))
        .filter_map(|l| l.rsplit("= ").next()?.trim().parse::<u32>().ok())
        .next()
        .unwrap_or_else(|| panic!("no append to s.tsvz.1:\n{trace}"));
    assert!(
        trace
            .lines()
            .any(|l| l.contains(&format!("fdatasync({fd})")) || l.contains(&format!("fsync({fd})"))),
        "the tombstones were never synced:\n{trace}"
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("s.tsvz.1")).unwrap(),
        "b\t2\na\nb\n"
    );
}

#[test]
fn a_write_that_fails_part_way_leaves_nothing_of_its_batch() {
    // A batch answered as failed must not be partly in the file: a refused bulk set would
    // leave some of its rows, and a pop whose batch failed would lose its row.
    use std::os::unix::process::CommandExt;
    let dir = store("k\tv\n");
    let mut input = String::new();
    for i in 0..100 {
        input.push_str(&format!("r{i}\t{}\n", "x".repeat(20)));
    }
    let mut child = tsvz();
    child
        .current_dir(dir.path())
        .args(["set", "s.tsvz", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // SAFETY: between fork and exec only async-signal-safe calls: signal and setrlimit.
    unsafe {
        child.pre_exec(|| {
            libc::signal(libc::SIGXFSZ, libc::SIG_IGN);
            let limit = libc::rlimit {
                rlim_cur: 200,
                rlim_max: 200,
            };
            if libc::setrlimit(libc::RLIMIT_FSIZE, &limit) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = child.spawn().unwrap();
    use std::io::Write;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(
        out.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("s.tsvz")).unwrap(),
        "k\tv\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn a_scrub_on_a_disk_too_full_for_its_recovery_copy_still_compacts() {
    // Scrubbing is how a full disk gets room back: with no room for the copy of the new
    // content, a part that shrinks is rewritten in place without one, as TSVZ.py does.
    struct Mounted(std::path::PathBuf);
    impl Drop for Mounted {
        fn drop(&mut self) {
            let _ = Command::new("umount").arg(&self.0).status();
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let mounted = Command::new("mount")
        .args(["-t", "tmpfs", "-o", "size=1m", "tmpfs"])
        .arg(dir.path())
        .output();
    if !mounted.is_ok_and(|o| o.status.success()) {
        eprintln!("cannot mount a small tmpfs here: skipped");
        return;
    }
    let _mounted = Mounted(dir.path().to_path_buf());
    let mut content = String::new();
    for pass in ["a", "b", "c"] {
        for i in 0..10_000 {
            content.push_str(&format!("k{i:05}\t{}\n", pass.repeat(20)));
        }
    }
    fs::write(dir.path().join("s.tsvz"), &content).unwrap();
    let (code, _, err) = run_in(dir.path(), &["scrub", "s.tsvz"], b"");
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("recovery copy"), "{err}");
    let (code, out, _) = run_in(dir.path(), &["len", "s.tsvz"], b"");
    assert_eq!((code, out.trim()), (0, "10000"));
    let (_, out, _) = records(dir.path(), &["get", "s.tsvz", "k00042"]);
    assert_eq!(out, format!("k00042\t{}\n", "c".repeat(20)));
    let left: Vec<_> = fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(left, ["s.tsvz"], "no recovery file left behind");
}

#[test]
fn a_store_on_a_file_system_without_hard_links_is_still_scrubbed() {
    // A new snapshot part is linked in under its name; where links do not exist (FAT, some
    // network and FUSE mounts), the parts are compacted in place instead, with a word why.
    struct Mounted(std::path::PathBuf);
    impl Drop for Mounted {
        fn drop(&mut self) {
            let _ = Command::new("umount").arg(&self.0).status();
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let image = dir.path().join("fat.img");
    fs::write(&image, vec![0u8; 8 << 20]).unwrap();
    let mnt = dir.path().join("mnt");
    fs::create_dir(&mnt).unwrap();
    let made = Command::new("mkfs.vfat").arg(&image).output();
    let mounted = made.is_ok_and(|o| o.status.success())
        && Command::new("mount")
            .args(["-o", "loop,quiet"])
            .arg(&image)
            .arg(&mnt)
            .output()
            .is_ok_and(|o| o.status.success());
    if !mounted {
        eprintln!("cannot make and mount a FAT image here: skipped");
        return;
    }
    let _mounted = Mounted(mnt.clone());
    fs::write(mnt.join("s.tsvz"), "a\t1\na\t2\n").unwrap();
    fs::write(mnt.join("s.tsvz.2"), "b\t2\n").unwrap();
    fs::write(mnt.join("s.tsvz.8"), "c\t3\n").unwrap();
    let (code, _, err) = run_in(&mnt, &["scrub", "s.tsvz"], b"");
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("in place"), "{err}");
    let (_, out, _) = records(&mnt, &["read", "s.tsvz"]);
    assert_eq!(out, "a\t2\nb\t2\nc\t3\n");
    let mut left: Vec<_> = fs::read_dir(&mnt)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    left.sort();
    assert_eq!(
        left,
        ["s.tsvz", "s.tsvz.2", "s.tsvz.8"],
        "no temporary file left"
    );
}
