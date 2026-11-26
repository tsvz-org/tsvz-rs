//! Peak memory of the `tsvz` binary, measured in a test binary of its own: a child's peak RSS
//! counts its parent's highest RSS before the child's `exec` (Linux records the old address
//! space's peak at `exec`), so the files are written a row at a time and the parent stays small.
#![cfg(target_os = "linux")]

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::process::{Command, Stdio};

/// Peak RSS in KiB of `tsvz ARGS` run in `dir`, and its exit status, read with wait4 so no other
/// child counts.
#[allow(clippy::zombie_processes)] // reaped by wait4 below, which also reads its peak RSS
fn peak_rss_kib(dir: &Path, args: &[&str]) -> (i32, i64) {
    let child = Command::new(env!("CARGO_BIN_EXE_tsvz"))
        .current_dir(dir)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut status = 0;
    // SAFETY: an all-zero rusage is valid; wait4 reaps exactly this child and fills it.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    assert_eq!(
        unsafe { libc::wait4(child.id() as i32, &mut status, 0, &mut usage) },
        child.id() as i32
    );
    (status, usage.ru_maxrss)
}

/// Write `rows` rows of the layout the release notes' measurements use (`key%07d`, two
/// cells of 16 hex digits, `some text here`) after a header line, as one gzip member: the bytes
/// of text written.
fn one_gzip_member(path: &Path, rows: u64) -> u64 {
    let file = BufWriter::new(File::create(path).unwrap());
    let mut gz = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
    let mut text = 0;
    let mut put = |line: String| {
        text += line.len() as u64;
        gz.write_all(line.as_bytes()).unwrap();
    };
    put("#key\tv1\tv2\tv3\n".into());
    for i in 0..rows {
        let x = i
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        put(format!("key{i:07}\t{x:016x}\t{x:016x}\tsome text here\n"));
    }
    gz.finish()
        .unwrap()
        .into_inner()
        .unwrap()
        .sync_all()
        .unwrap();
    text
}

/// A delete in a compressed loose file of more than one column holds no more of the member that
/// holds its first keyed line than a member of the default size (1 MiB), however large it is: it
/// decodes the member to its check, then again as far as that line. 0.2.0 held the whole file and
/// the member's whole output: on 400,000 rows (23 MiB of text, one gzip member), its test build
/// peaked 65 MiB above a `len` of a one-row file, and 0.2.1's 2.4 MiB above it (2026-11-23); the
/// test allows 4.
#[test]
fn a_compressed_loose_delete_holds_no_more_of_a_large_member_than_its_default_size() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    std::fs::write(dir.join("tiny.tsv"), "a\t1\n").unwrap();
    let (code, base) = peak_rss_kib(dir, &["len", "tiny.tsv"]);
    assert_eq!(code, 0);
    let text = one_gzip_member(&dir.join("big.tsv.gz"), 400_000) as i64 / 1024;
    let (code, kib) = peak_rss_kib(dir, &["delete", "big.tsv.gz", "key0000005"]);
    assert_eq!(code, 0);
    assert!(
        kib - base < 4 * 1024,
        "a delete in {text} KiB of text in one member peaked at {kib} KiB, {base} KiB for a len of one row"
    );
}

/// Write `rows` lines of `keys` keys in turn, of the layout above, to a plain loose file: the
/// bytes written.
fn loose_rows(path: &Path, rows: u64, keys: u64) -> u64 {
    let mut file = BufWriter::new(File::create(path).unwrap());
    let mut text = 0;
    for i in 0..rows {
        let x = i
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let line = format!("key{:07}\t{x:016x}\t{x:016x}\tsome text here\n", i % keys);
        text += line.len() as u64;
        file.write_all(line.as_bytes()).unwrap();
    }
    file.into_inner().unwrap().sync_all().unwrap();
    text
}

/// A loose scrub reads the file twice: the first read keeps whether each line stays and what each
/// reader read of each key, the second writes the lines kept as it reads them, so memory follows
/// the keys, with the file held once by the in-place rewrite (to put it back should the write
/// fail). 0.2.0 held every line, each in a buffer of its own: on 400,000 lines of 1,000 keys (23
/// MiB), its test build peaked 69 MiB above a `len` of a one-row file, and 0.2.1's 25 MiB above it
/// (2026-11-23); the test allows one and a half times the file.
#[test]
fn a_loose_scrub_holds_the_file_once_and_what_its_keys_keep() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    std::fs::write(dir.join("tiny.tsv"), "a\t1\n").unwrap();
    let (_, base) = peak_rss_kib(dir, &["len", "tiny.tsv"]);
    let text = loose_rows(&dir.join("big.tsv"), 400_000, 1_000) as i64 / 1024;
    let (code, kib) = peak_rss_kib(dir, &["scrub", "big.tsv"]);
    assert_eq!(code, 0);
    assert!(
        kib - base < text * 3 / 2,
        "a scrub of {text} KiB peaked at {kib} KiB, {base} KiB for a len of one row"
    );
}

/// A delete in a one-column loose file reads it twice too: which lines go, then the rest written
/// as they are read, and held once more by the in-place rewrite. 0.2.0 held every line: on
/// 1,000,000 keys alone (9.5 MiB), its test build peaked 106 MiB above a `len` of a one-row file,
/// and 0.2.1's 23 MiB above it (2026-11-23); the test allows three times the file.
#[test]
fn a_one_column_delete_holds_the_file_twice_at_most() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    std::fs::write(dir.join("tiny.tsv"), "a\t1\n").unwrap();
    let (_, base) = peak_rss_kib(dir, &["len", "tiny.tsv"]);
    let mut file = BufWriter::new(File::create(dir.join("one.tsv")).unwrap());
    for i in 0..1_000_000 {
        writeln!(file, "k{i:08}").unwrap();
    }
    file.into_inner().unwrap().sync_all().unwrap();
    let text = std::fs::metadata(dir.join("one.tsv")).unwrap().len() as i64 / 1024;
    let (code, kib) = peak_rss_kib(dir, &["delete", "one.tsv", "k00000005"]);
    assert_eq!(code, 0);
    assert!(
        kib - base < text * 3,
        "a one-column delete in {text} KiB peaked at {kib} KiB, {base} KiB for a len of one row"
    );
    // counted a buffer at a time: the parent's own peak counts in a child's (above)
    let left = std::io::BufReader::new(File::open(dir.join("one.tsv")).unwrap());
    assert_eq!(std::io::BufRead::lines(left).count(), 999_999);
}
