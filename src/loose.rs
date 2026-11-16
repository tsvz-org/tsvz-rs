//! Loose files (§5.2, §20.2.4) as two readers read them: tsvz-rs by its own rule (a loose file's
//! data semantics are implementation-defined), and TSVZ.py by its 3.39 rules (TSVZ.py
//! `_processLine`). Both drop the NULs at the start of a line, take the column count from the
//! first keyed line ([`keyed`]), and read a line whose cells after the key are none or all blank
//! as a deletion ([`deletes`]), but in a one-column file, where it is a row. Line 1 begins with
//! its byte-order mark, which is part of its key. tsvz-rs reads every `#` line as a comment except
//! a key that begins with `#_defaults_#`; TSVZ.py reads a `#_defaults_#` line as defaults for the
//! rows after it; and only TSVZ.py strips each cell's trailing whitespace and decodes `<sep>` and
//! `<LF>`, and skips a line whose key is only whitespace, which tsvz-rs reads as a row.
//!
//! A rewrite keeps each line it keeps as the file has it, `\r` included (a byte-order mark an
//! unterminated first line loses is the one exception), and changes what a reader reads only
//! where both readers read the result alike: [`scrub`] (which moves a key's
//! last line into its first line's place where that is so), [`cleared`] and, in a one-column
//! file, where no line can delete, [`without`] (which puts a key's line in the place of a width
//! line that goes).

use std::borrow::Cow;
use std::collections::HashMap;
#[cfg(feature = "cli")]
use std::collections::HashSet;

const DEFAULTS: &str = "#_defaults_#";

/// Whether `c` is whitespace to Python's `str.rstrip()`, which TSVZ.py applies to every cell.
pub(crate) fn py_space(c: char) -> bool {
    c.is_whitespace() || ('\x1c'..='\x1f').contains(&c)
}

fn blank(s: &str) -> bool {
    s.chars().all(py_space)
}

fn first_field(line: &str, d: u8) -> (&str, Option<&str>) {
    match memchr::memchr(d, line.as_bytes()) {
        Some(i) => (&line[..i], Some(&line[i + 1..])),
        None => (line, None),
    }
}

/// Whether the cells after a key, `rest` (None for a key alone), make its line a deletion: in a
/// file of more than one column, none or all blank, as TSVZ.py reads them.
pub(crate) fn deletes(rest: Option<&str>, d: u8, one: bool) -> bool {
    // The first cell begins with a visible ASCII character in nearly every row: no deletion.
    let visible = |r: &str| {
        r.bytes()
            .next()
            .is_some_and(|b| b > b' ' && b < 0x7f && b != d)
    };
    !one && rest.is_none_or(|r| !visible(r) && r.split(d as char).all(blank))
}

/// A line as TSVZ.py reads it: without the NULs at its start (`line.strip('\x00')`), so that a
/// line of only NULs is empty and a key never begins with one.
#[inline]
pub(crate) fn body(line: &str) -> &str {
    if line.as_bytes().first() == Some(&0) {
        line.trim_start_matches('\0')
    } else {
        line
    }
}

/// Whether `line` is a `#_defaults_#` line to TSVZ.py: its first cell, without the whitespace
/// at its end, is that and no more (`#_defaults_#x` is a key).
pub(crate) fn is_defaults(line: &str, d: u8) -> bool {
    first_field(body(line), d).0.trim_end_matches(py_space) == DEFAULTS
}

/// Whether tsvz-rs reads `line` as a comment: one that begins with `#`, a `#_defaults_#` line
/// included (TSVZ.py reads it as defaults), but not a key that only begins with `#_defaults_#`.
pub(crate) fn is_comment(line: &str, d: u8) -> bool {
    let line = body(line);
    line.starts_with('#') && !(line.starts_with(DEFAULTS) && !is_defaults(line, d))
}

/// Whether TSVZ.py takes `line` as a keyed line, the first of which sets the column count: not
/// empty, no comment (a `#_defaults_#` line counts), and a key that is not blank.
pub(crate) fn keyed(line: &str, d: u8) -> bool {
    let line = body(line);
    (!is_comment(line, d) || is_defaults(line, d)) && !blank(first_field(line, d).0)
}

/// What a line is to both readers, in a file whose column count is one or not (`one`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// Read by neither: empty, a comment, or an empty key.
    Skip,
    /// A `#_defaults_#` line: a comment to tsvz-rs, defaults to TSVZ.py.
    Defaults,
    /// A row to both.
    Row,
    /// A deletion to both ([`deletes`]).
    Delete,
    /// A key the two read differently (TSVZ.py strips its trailing whitespace and decodes
    /// `<…>` tokens): the file is then rewritten without changing any line.
    Odd,
}

fn kind(line: &str, d: u8, one: bool) -> (Kind, &str) {
    let line = body(line);
    if is_defaults(line, d) {
        return (Kind::Defaults, "");
    }
    let (key, rest) = first_field(line, d);
    if is_comment(line, d) || key.is_empty() {
        return (Kind::Skip, "");
    }
    // A NUL is the delimiter of a `.nsv` file, no odd byte there.
    if key.ends_with(py_space)
        || key.contains('<')
        || line.contains('\r')
        || (d != 0 && line.contains('\0'))
    {
        return (Kind::Odd, key);
    }
    let kind = if deletes(rest, d, one) {
        Kind::Delete
    } else {
        Kind::Row
    };
    (kind, key)
}

/// A loose file's committed lines (without their `\n`), as a rewrite works on them.
pub(crate) struct Lines<'a> {
    raw: &'a [Vec<u8>],
    text: Vec<Cow<'a, str>>,
    d: u8,
    /// The line that sets the column count ([`keyed`]), if any.
    width_line: Option<usize>,
    one: bool,
}

impl<'a> Lines<'a> {
    pub(crate) fn new(raw: &'a [Vec<u8>], d: u8) -> Lines<'a> {
        // A line as the framer reads it: one `\r` before the `\n` is no part of it.
        let text: Vec<Cow<'a, str>> = raw
            .iter()
            .map(|l| String::from_utf8_lossy(l.strip_suffix(b"\r").unwrap_or(l)))
            .collect();
        let width_line = text.iter().position(|t| keyed(t, d));
        let one = width_line.is_some_and(|i| !body(&text[i]).as_bytes().contains(&d));
        Lines {
            raw,
            text,
            d,
            width_line,
            one,
        }
    }

    /// The lines `keep` says, each as `put` has it, else as it was.
    fn write(&self, keep: &[bool], put: &[Option<&[u8]>]) -> Vec<u8> {
        let mut out = Vec::new();
        for i in (0..keep.len()).filter(|&i| keep[i]) {
            out.extend_from_slice(put[i].unwrap_or(&self.raw[i]));
            out.push(b'\n');
        }
        out
    }

    /// How many lines there are.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.raw.len()
    }

    fn fields(&self, i: usize) -> usize {
        memchr::memchr_iter(self.d, body(&self.text[i]).as_bytes()).count()
    }
}

/// A compacted `lines`: comments, empty lines, superseded lines and dead keys' lines go; line 1,
/// every `#_defaults_#` line, the lines that set the column count, and for each live key the
/// line that places it and the one that gives its value stay. A key's value
/// line takes the place of its first line when no `#_defaults_#` line (which changes how TSVZ.py
/// reads a row) and no other line of the key kept lies between them, and that place is not line
/// 1 (unless the two lines are the same) nor a line that sets the column count (unless the two
/// have as many cells). Both readers (TSVZ.py as its command line reads, given no header) read
/// the result as they read `lines`; line 1, and the first keyed line after it, stay for a TSVZ.py
/// program that takes line 1 for its header.
pub(crate) fn scrub(lines: &Lines<'_>) -> Vec<u8> {
    let n = lines.raw.len();
    let d = lines.d;
    let kinds: Vec<(Kind, &str)> = lines.text.iter().map(|t| kind(t, d, lines.one)).collect();
    // The lines that set the column count: the first keyed line, and the first after line 1,
    // which a reader that takes line 1 for its header counts from.
    let after_first = (1..n).find(|&i| keyed(&lines.text[i], d));
    let pinned = |i: usize| i == 0 || Some(i) == lines.width_line || Some(i) == after_first;
    let mut keep: Vec<bool> = (0..n)
        .map(|i| pinned(i) || kinds[i].0 != Kind::Skip)
        .collect();
    let mut put = vec![None; n];
    if kinds.iter().any(|(k, _)| *k == Kind::Odd) {
        return lines.write(&keep, &put);
    }
    let mut by_key: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, (k, key)) in kinds.iter().enumerate() {
        if matches!(k, Kind::Row | Kind::Delete) {
            by_key.entry(key).or_default().push(i);
            keep[i] = pinned(i);
        }
    }
    // How many `#_defaults_#` lines come before each line: whether one lies between two is then
    // a subtraction, not a scan per key (a scan made a file of many keys, each on several lines,
    // take time in the square of its length).
    let mut defaults_before = vec![0usize; n + 1];
    for (i, (k, _)) in kinds.iter().enumerate() {
        defaults_before[i + 1] = defaults_before[i] + usize::from(*k == Kind::Defaults);
    }
    let defaults_between = |a: usize, b: usize| defaults_before[b] > defaults_before[a + 1];
    for at in by_key.values() {
        let last_delete = at.iter().rposition(|&i| kinds[i].0 == Kind::Delete);
        let run = &at[last_delete.map_or(0, |p| p + 1)..];
        // A pinned line of the key before its last deletion is undone by that deletion.
        if let Some(p) = last_delete {
            if at[..p].iter().any(|&i| pinned(i)) {
                keep[at[p]] = true;
            }
        }
        let (Some(&first), Some(&last)) = (run.first(), run.last()) else {
            continue; // dead to both
        };
        keep[first] = true;
        keep[last] = true;
        let fits = if first == 0 {
            lines.raw[first] == lines.raw[last]
        } else {
            !pinned(first) || lines.fields(first) == lines.fields(last)
        };
        let pinned_between = run.iter().any(|&i| i > first && pinned(i));
        if first != last && fits && !pinned_between && !defaults_between(first, last) {
            put[first] = Some(&lines.raw[last][..]);
            keep[last] = false;
        }
    }
    lines.write(&keep, &put)
}

/// What `clear` leaves of `lines`: line 1 when it is a comment or empty (TSVZ.py writes an empty
/// line where a file has no header), and, in a file of more than one column, the `#_defaults_#`
/// lines that hold a value after the last one that holds none (which resets the defaults), so
/// that rows written later read to TSVZ.py with the defaults they had before. They keep the
/// file's column count too, which the first of them may not have: a `#_defaults_#` line that
/// holds no value, at that count, goes before it (it sets the count, and changes nothing else).
/// A one-column file keeps none: its first keyed line would be a defaults line of more cells,
/// and a key alone, a row there, would be a deletion.
pub(crate) fn cleared(lines: &Lines<'_>) -> Vec<u8> {
    let d = lines.d;
    let n = lines.raw.len();
    let defaults = |i: usize| {
        let (key, rest) = first_field(body(&lines.text[i]), d);
        let valued = rest.is_some_and(|r| !r.split(d as char).all(blank));
        (key.trim_end_matches(py_space) == DEFAULTS).then_some(valued)
    };
    let reset = (0..n).rposition(|i| defaults(i) == Some(false));
    let in_force =
        |i: usize| !lines.one && defaults(i) == Some(true) && reset.is_none_or(|r| i > r);
    let keep: Vec<bool> = (0..n)
        .map(|i| {
            let t = &lines.text[i];
            (i == 0 && (body(t).is_empty() || is_comment(t, d)) && !keyed(t, d)) || in_force(i)
        })
        .collect();
    let mut put = vec![None; n];
    // The column count of the file, and the first line kept that is not line 1's header.
    let first = (0..n).find(|&i| in_force(i));
    let count = |i: usize| memchr::memchr_iter(d, body(&lines.text[i]).as_bytes()).count() + 1;
    let width: Vec<u8>;
    if let (Some(first), Some(w)) = (first, lines.width_line) {
        if count(first) != count(w) {
            width = [
                DEFAULTS.as_bytes(),
                &vec![d; count(w) - 1],
                b"\n",
                &lines.raw[first],
            ]
            .concat();
            put[first] = Some(&width[..]);
        }
    }
    lines.write(&keep, &put)
}

/// In a one-column file, where a key alone is a row and no line can delete: `lines` without the
/// lines of `keys` (each without the whitespace at its end, as a line's key is for TSVZ.py), as
/// TSVZ.py deletes there; None in any other file, where a key alone deletes, and Some(None) when
/// no line is one of those, so that the file stays as it is (also a compressed one, whose bytes
/// a rewrite would change).
/// When the line that set the column count goes and the next keyed line has more cells, the first
/// remaining key, alone, takes its place, so that the file keeps one column, as in TSVZ.py.
#[cfg(feature = "cli")]
pub(crate) fn without(lines: &Lines<'_>, keys: &HashSet<&str>) -> Option<Option<Vec<u8>>> {
    if !lines.one {
        return None;
    }
    let d = lines.d;
    let n = lines.raw.len();
    let gone = |i: usize| {
        let (k, key) = kind(&lines.text[i], d, true);
        matches!(k, Kind::Row | Kind::Odd) && keys.contains(key.trim_end_matches(py_space))
    };
    let mut keep: Vec<bool> = (0..n).map(|i| !gone(i)).collect();
    if keep.iter().all(|&k| k) {
        return Some(None);
    }
    let mut put = vec![None; n];
    if let Some(w) = lines.width_line.filter(|&w| !keep[w]) {
        let keyed_at = |i: usize| keep[i] && keyed(&lines.text[i], d);
        let next = (w + 1..n).find(|&i| keyed_at(i));
        let row = (w + 1..n).find(|&i| keyed_at(i) && !is_defaults(&lines.text[i], d));
        // With no key left, a `#_defaults_#` line that holds no value, which TSVZ.py reads as a
        // row here, is kept a row by a bare one in front of it; one that holds a value alone is
        // no row, and the file then has the count of that line.
        let valueless = |i: usize| {
            is_defaults(&lines.text[i], d) && deletes(first_field(&lines.text[i], d).1, d, false)
        };
        let stand_in = match row {
            // the first cell of the line as both readers read it, without the NULs at its start
            // (in a `.nsv` file the first piece of the raw line is empty)
            Some(row) => {
                let raw = &lines.raw[row];
                let start = raw.iter().take_while(|&&b| b == 0).count();
                raw[start..].split(|&b| b == d).next()
            }
            None if (w + 1..n).any(|i| keep[i] && valueless(i)) => Some(DEFAULTS.as_bytes()),
            None => None,
        };
        if let (Some(next), Some(stand_in)) = (next, stand_in) {
            if lines.fields(next) > 0 {
                keep[w] = true;
                put[w] = Some(stand_in);
            }
        }
    }
    Some(Some(lines.write(&keep, &put)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &str) -> Vec<Vec<u8>> {
        text.split_terminator('\n')
            .map(|l| l.as_bytes().to_vec())
            .collect()
    }

    fn with(text: &str, f: impl Fn(&Lines<'_>) -> Vec<u8>) -> String {
        let raw = lines(text);
        String::from_utf8(f(&Lines::new(&raw, b'\t'))).unwrap()
    }

    #[test]
    fn the_column_count_comes_from_the_first_keyed_line() {
        for (text, one) in [
            ("a\nb\t1\n", true),
            ("#h\n\n\tx\na\n", true),
            ("a\t1\nb\n", false),
            ("#_defaults_#\tP\na\n", false),
            ("#_defaults_#\na\n", true),
            ("\u{feff}#h\na\t1\n", true), // the mark makes line 1 keyed, to TSVZ.py
            ("#h\n", false),
            ("#_defaults_#x\na\t1\n", true),
            ("#_defaults_# \tP\na\n", false),
            // A line of only NULs is empty to TSVZ.py, and a key never begins with one.
            ("\0\na\tb\n", false),
            ("\0\0\na\n", true),
            ("\0a\nb\t1\n", true),
        ] {
            let raw = lines(text);
            assert_eq!(Lines::new(&raw, b'\t').one, one, "{text:?}");
        }
        assert!(deletes(None, b'\t', false) && deletes(Some(" \t\u{1c}"), b'\t', false));
        assert!(!deletes(None, b'\t', true) && !deletes(Some("\tx"), b'\t', false));
        // Whatever begins the first cell: a visible character, a delimiter, a blank, no ASCII.
        assert!(!deletes(Some("1\t "), b'\t', false) && !deletes(Some("\u{e9}"), b'\t', false));
        assert!(deletes(Some("\t"), b'\t', false) && deletes(Some(""), b'\t', false));
        assert!(deletes(Some(" \u{a0}\u{3000}\t "), b'\t', false));
        assert!(!deletes(Some("1;"), b';', false) && deletes(Some(";"), b';', false));
    }

    #[test]
    fn a_scrub_keeps_lines_in_their_places() {
        for (input, output) in [
            // As TSVZ.py 5.18's scrub writes them: line 1 stays; a key's value line moves into
            // the place of its first line only where that keeps the column count, and never
            // across a #_defaults_# line.
            ("a\t1\nb\t2\na\t3\n", "a\t1\nb\t2\na\t3\n"),
            ("#h\na\t1\nb\t2\na\t3\n", "#h\na\t3\nb\t2\n"),
            ("#h\na\t1\nb\t2\na\t3\t4\n", "#h\na\t1\nb\t2\na\t3\t4\n"),
            ("\na\t1\nb\t2\na\t3\n", "\na\t3\nb\t2\n"),
            (
                "#h\n#_defaults_#\tP\na\t\tq\nb\t1\t\n#_defaults_#\tR\na\t\tz\n",
                "#h\n#_defaults_#\tP\na\t\tq\nb\t1\t\n#_defaults_#\tR\na\t\tz\n",
            ),
            // Comments, empty lines, superseded and dead keys' lines go; a line that sets the
            // column count stays, with the deletion that undoes it.
            (
                "#h\n#c\n\na\t1\nb\t2\nb\t\nc\t3\na\t4\n",
                "#h\na\t4\nc\t3\n",
            ),
            ("#h\nk\t1\nk\t\nk\nz\t2\n", "#h\nk\t1\nk\nz\t2\n"),
            (
                "x\t0\nb\t1\ny\t5\nb\t\nb\t2\n",
                "x\t0\nb\t1\ny\t5\nb\t\nb\t2\n",
            ),
            // A line that sets the column count and a later one of the key that is pinned
            // between its first and last lines keep their places: `a` still reads 2.
            ("a\t1\na\t2\nb\t0\na\t1\n", "a\t1\na\t2\nb\t0\na\t1\n"),
            // Every line it keeps is as it was, its `\r` too.
            (
                "#h\r\na\t1\r\n\r\nb\t2\r\na\t3\r\n",
                "#h\r\na\t3\r\nb\t2\r\n",
            ),
            // A key that only begins with `#_defaults_#` is a key.
            (
                "#h\n#_defaults_#x\t1\n#_defaults_#x\t2\n",
                "#h\n#_defaults_#x\t2\n",
            ),
            // One column: a key alone is a row.
            ("a\nb\t1\na\n", "a\nb\t1\n"),
            ("#h\na\nb\nc\nb\n", "#h\na\nb\nc\n"),
            // A key TSVZ.py reads differently, or a byte-order mark: only skipped lines go.
            ("#h\n#c\na \t1\na\t2\na\t3\n", "#h\na \t1\na\t2\na\t3\n"),
            ("\u{feff}a\t1\n#c\na\t2\n", "\u{feff}a\t1\na\t2\n"),
        ] {
            assert_eq!(with(input, scrub), output, "{input:?}");
        }
    }

    /// NUL is the delimiter of a `.nsv` file: a line that holds one is no odd line there (0.1.4's
    /// scrub of such a file never compacted it), and is one with another delimiter.
    #[test]
    fn a_scrub_compacts_a_file_whose_delimiter_is_nul() {
        let nul = |text: &str| {
            let raw = lines(text);
            String::from_utf8(scrub(&Lines::new(&raw, 0))).unwrap()
        };
        assert_eq!(
            nul("#h\0v\n#c\na\0\x31\na\0\x32\nb\0\x33\n\nb\0\n"),
            "#h\0v\na\0\x32\n"
        );
        // The NULs at a line's start are no part of it, as TSVZ.py strips them: `a` is `a`, and
        // the last line, as the file has it, takes the first one's place.
        assert_eq!(
            nul("#h\0v\n\0a\0\x31\n\0\0a\0\x32\n"),
            "#h\0v\n\0\0a\0\x32\n"
        );
        // With another delimiter the same bytes are odd, and only the skipped lines go.
        assert_eq!(
            with("#h\n#c\na\0\t1\na\t2\na\t3\n", scrub),
            "#h\na\0\t1\na\t2\na\t3\n"
        );
    }

    /// 400,000 keys, each written twice: a block of them, then the block again, so that every
    /// key's first and last line lie 400,000 lines apart. A scan of the lines between them, per
    /// key, took 50 s in a release build here (2026-11-16; the old test, 200,000 lines of 50,000
    /// keys in random order, took 1.4 s with the scan and so passed in a release build); counted
    /// once, the scrub takes 1.4 s in a debug build and 0.4 s in a release one. The scrub runs on a
    /// thread, so that a scan fails the test after 10 s, 7 times the debug scrub and a fifth of
    /// the release scan, and not when it ends (it takes minutes in a debug build).
    #[test]
    fn a_scrub_takes_time_in_the_length_of_the_file_whatever_the_keys_repeat() {
        let keys = 400_000u64;
        let (done, took) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut text = String::from("#h\tv\n");
            for round in 0..2 {
                for k in 0..keys {
                    text.push_str(&format!("k{k}\t{round}\n"));
                }
            }
            let raw = lines(&text);
            let start = std::time::Instant::now();
            let out = scrub(&Lines::new(&raw, b'\t'));
            let _ = done.send((start.elapsed(), out.len() < text.len()));
        });
        let (time, smaller) = took
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("a scrub of 800,000 lines of 400,000 keys took more than 10 s");
        assert!(smaller, "the scrub took {time:?}");
    }

    #[test]
    fn a_clear_keeps_line_1_and_the_defaults_in_force() {
        for (input, output) in [
            ("#h\n#_defaults_#\tP\na\t1\n", "#h\n#_defaults_#\tP\n"),
            ("\na\t1\n", "\n"),
            ("a\t1\n#_defaults_#\tP\n", "#_defaults_#\tP\n"),
            (
                "#h\r\na\t1\r\n#_defaults_#\tP\r\n",
                "#h\r\n#_defaults_#\tP\r\n",
            ),
            // A #_defaults_# line that holds no value resets the defaults: it goes, with what
            // it reset.
            (
                "#h\n#_defaults_#\tP\tQ\nb\t1\n#_defaults_#\n#_defaults_#\tR\tS\n",
                "#h\n#_defaults_#\tR\tS\n",
            ),
            // The column count stays what it was: a defaults line of another count goes after
            // a blank one of the file's.
            (
                "a\t1\n#_defaults_#\tP\tQ\tS\n",
                "#_defaults_#\t\n#_defaults_#\tP\tQ\tS\n",
            ),
            (
                "a\t1\t2\t3\n#_defaults_#\tP\tQ\n",
                "#_defaults_#\t\t\t\n#_defaults_#\tP\tQ\n",
            ),
            (
                "#h\na\t1\n#_defaults_#\tP\tQ\n#_defaults_#\t\t\tR\n",
                "#h\n#_defaults_#\t\n#_defaults_#\tP\tQ\n#_defaults_#\t\t\tR\n",
            ),
            // A one-column file keeps none: a defaults line of more cells would make it two.
            ("a\n#_defaults_#\tP\n#_defaults_#\n", ""),
            ("#h\na\n#_defaults_#\tP\tQ\n", "#h\n"),
            // With a byte-order mark, line 1 is a row to TSVZ.py.
            ("\u{feff}#_defaults_#\tP\na\t1\n", ""),
            // A line 1 of only NULs is empty to TSVZ.py: kept as the empty line it writes.
            ("\0\na\t1\n", "\0\n"),
        ] {
            assert_eq!(with(input, cleared), output, "{input:?}");
        }
    }

    #[test]
    #[cfg(feature = "cli")]
    fn a_nul_delimiter_leaves_the_nuls_at_a_lines_start_out_of_its_count() {
        // The NULs at the start of a line are no part of it (`body`), the delimiter's too:
        // `\0a` is a key alone, in a file of one column.
        let raw = vec![b"\0a".to_vec(), b"b".to_vec()];
        let lines = Lines::new(&raw, 0);
        let keys: HashSet<&str> = ["a"].into();
        assert_eq!(without(&lines, &keys), Some(Some(b"b\n".to_vec())));
    }

    /// The key that takes the place of a column-count line that goes is a line's first cell
    /// without its leading NULs, which in a `.nsv` file is not the first piece of the raw line:
    /// an empty one there left the file with a `#_defaults_#` line of three cells for its count,
    /// and every row after it a deletion.
    #[test]
    #[cfg(feature = "cli")]
    fn the_key_that_replaces_a_width_line_is_read_without_its_leading_nuls() {
        let raw = lines("0\n\n#_defaults_#\0Q\0\n\0x\n");
        let keys: HashSet<&str> = ["0"].into();
        assert_eq!(
            without(&Lines::new(&raw, 0), &keys),
            Some(Some(b"x\n\n#_defaults_#\0Q\0\n\0x\n".to_vec()))
        );
        let tabs = lines("0\n\n#_defaults_#\tQ\t\n\0x\n");
        assert_eq!(
            without(&Lines::new(&tabs, b'\t'), &keys),
            Some(Some(b"x\n\n#_defaults_#\tQ\t\n\0x\n".to_vec()))
        );
    }

    #[test]
    #[cfg(feature = "cli")]
    fn a_delete_in_a_one_column_file_keeps_one_column() {
        let gone = |text: &str, keys: &[&str]| {
            let keys: HashSet<&str> = keys.iter().copied().collect();
            with(text, |l| {
                // None for a file of more than one column, Some(None) for a file that stays as it is
                without(l, &keys)
                    .unwrap()
                    .unwrap_or_else(|| text.as_bytes().to_vec())
            })
        };
        assert_eq!(gone("#h\na\nb\nc\n", &["b"]), "#h\na\nc\n");
        assert_eq!(gone("a\nb\nc\n", &["a", "c"]), "b\n");
        // The line that set one column goes: the first remaining key, alone, takes its place.
        assert_eq!(gone("a\nb\t1\nc\n", &["a"]), "b\nb\t1\nc\n");
        assert_eq!(
            gone("a\n#_defaults_#\tP\nb\n", &["a"]),
            "b\n#_defaults_#\tP\nb\n"
        );
        assert_eq!(gone("a\r\nb\r\nc\r\n", &["b"]), "a\r\nc\r\n");
        // A byte-order mark is part of line 1's key, as TSVZ.py reads it: `a` is another key
        // than `\u{feff}a`, and the mark goes with its line, never onto another.
        assert_eq!(gone("\u{feff}a\nb\n", &["a"]), "\u{feff}a\nb\n");
        let raw = lines("\u{feff}a\nb\n");
        let none: HashSet<&str> = ["a", "zz"].into();
        assert_eq!(without(&Lines::new(&raw, b'\t'), &none), Some(None));
        let raw = lines("a\tb\n");
        assert_eq!(without(&Lines::new(&raw, b'\t'), &none), None);
        assert_eq!(gone("\u{feff}a\nb\n", &["\u{feff}a"]), "b\n");
        assert_eq!(gone("\u{feff}zz\n", &["\u{feff}zz"]), "");
        assert_eq!(gone("\u{feff}a\nb\n", &["b"]), "\u{feff}a\n");
        // Only a first cell that is `#_defaults_#` makes a defaults line; a key that begins with
        // it is a row, which takes the place of a width line that goes.
        assert_eq!(
            gone("c\n#_defaults_#x\t1\n#c\n", &["c"]),
            "#_defaults_#x\n#_defaults_#x\t1\n#c\n"
        );
        assert_eq!(gone("c\n#_defaults_#x\n", &["#_defaults_#x"]), "c\n");
        // No key left to take its place: a `#_defaults_#` line with no value is a row to TSVZ.py
        // at one column, and a bare one before it keeps it one; one with a value is no row.
        assert_eq!(
            gone("c\n#_defaults_#\tP\n#_defaults_#\t\n", &["c"]),
            "#_defaults_#\n#_defaults_#\tP\n#_defaults_#\t\n"
        );
        assert_eq!(gone("c\n#_defaults_#\n", &["c"]), "#_defaults_#\n");
        assert_eq!(gone("x\n#_defaults_#\tP\n", &["x"]), "#_defaults_#\tP\n");
    }
}
