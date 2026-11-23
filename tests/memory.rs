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

/// Write `rows` rows of the layout the measurements in docs/limitations.md use (`key%07d`, two
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
