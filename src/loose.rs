//! Loose files (§5.2, §20.2.4) as two readers read them: tsvz-rs by design section 5.6's rule,
//! and TSVZ.py by its 3.39 rules (TSVZ.py `_processLine`). Both take the column count from the
//! first keyed line ([`keyed`]), and both read a line whose cells after the key are none or all
//! blank as a deletion ([`deletes`]), but in a one-column file, where it is a row. tsvz-rs reads
//! every `#` line as a comment, TSVZ.py a `#_defaults_#` line as defaults for the rows after it,
//! and only TSVZ.py strips each cell's trailing whitespace and decodes `<…>` tokens.
//!
//! A rewrite keeps lines as they are and in their places, and changes a line only where both
//! readers read the result alike: [`scrub`], [`cleared`] and, in a one-column file, where no
//! line can delete, [`without`].

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
    !one && rest.is_none_or(|r| r.split(d as char).all(blank))
}

/// A line as TSVZ.py reads it: without the NULs at its start (`line.strip('\x00')`), so that a
/// line of only NULs is empty and a key never begins with one.
pub(crate) fn body(line: &str) -> &str {
    line.trim_start_matches('\0')
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
    if key.ends_with(py_space) || key.contains('<') || line.contains(['\0', '\r']) {
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
    text: Vec<String>,
    d: u8,
    /// The line that sets the column count ([`keyed`]), if any.
    width_line: Option<usize>,
    one: bool,
}

impl<'a> Lines<'a> {
    pub(crate) fn new(raw: &'a [Vec<u8>], d: u8) -> Lines<'a> {
        // A line as the framer reads it: one `\r` before the `\n` is no part of it.
        let text: Vec<String> = raw
            .iter()
            .map(|l| String::from_utf8_lossy(l.strip_suffix(b"\r").unwrap_or(l)).into_owned())
            .collect();
        let width_line = text.iter().position(|t| keyed(t, d));
        let one = width_line.is_some_and(|i| !text[i].as_bytes().contains(&d));
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

    fn fields(&self, i: usize) -> usize {
        memchr::memchr_iter(self.d, self.text[i].as_bytes()).count()
    }
}

/// A compacted `lines`: comments, empty lines, superseded lines and dead keys' lines go; line 1,
/// every `#_defaults_#` line, the lines that set the column count, and for each live key the
/// line that places it and the one that gives its value stay, each where it was. A key's value
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
    let defaults_between = |a: usize, b: usize| (a + 1..b).any(|i| kinds[i].0 == Kind::Defaults);
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
/// line where a file has no header), and the `#_defaults_#` lines that hold a value after the
/// last one that holds none (which resets the defaults, but in a one-column file), so that rows
/// written later read to TSVZ.py as they would have before. A line that holds none is left out:
/// it changes nothing there, and would be a row in a file whose column count it sets.
pub(crate) fn cleared(lines: &Lines<'_>) -> Vec<u8> {
    let d = lines.d;
    let defaults = |t: &str| {
        let (key, rest) = first_field(body(t), d);
        let valued = rest.is_some_and(|r| !r.split(d as char).all(blank));
        (key.trim_end_matches(py_space) == DEFAULTS).then_some(valued)
    };
    let defaults = |i: usize| defaults(&lines.text[i]);
    let reset = if lines.one {
        None
    } else {
        (0..lines.text.len()).rposition(|i| defaults(i) == Some(false))
    };
    let keep: Vec<bool> = lines
        .text
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let header = i == 0 && (t.is_empty() || is_comment(t, d)) && !keyed(t, d);
            header || (defaults(i) == Some(true) && reset.is_none_or(|r| i > r))
        })
        .collect();
    lines.write(&keep, &vec![None; lines.raw.len()])
}

/// In a one-column file, where a key alone is a row and no line can delete: `lines` without the
/// lines of `keys` (each without the whitespace at its end, as a line's key is for TSVZ.py), as
/// TSVZ.py deletes there; None in any other file, where a key alone deletes.
/// When the line that set the column count goes and the next keyed line has more cells, the first
/// remaining key, alone, takes its place, so that the file keeps one column, as in TSVZ.py.
#[cfg(feature = "cli")]
pub(crate) fn without(lines: &Lines<'_>, keys: &HashSet<&str>) -> Option<Vec<u8>> {
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
    let mut put = vec![None; n];
    if let Some(w) = lines.width_line.filter(|&w| !keep[w]) {
        let keyed_at = |i: usize| keep[i] && keyed(&lines.text[i], d);
        let next = (w + 1..n).find(|&i| keyed_at(i));
        let row = (w + 1..n).find(|&i| keyed_at(i) && !is_defaults(&lines.text[i], d));
        if let (Some(next), Some(row)) = (next, row) {
            if lines.fields(next) > 0 {
                keep[w] = true;
                put[w] = lines.raw[row].split(|&b| b == d).next();
            }
        }
    }
    Some(lines.write(&keep, &put))
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
            // A #_defaults_# line that holds no value resets the defaults, but at one column,
            // where it is a row: it goes, with what it reset.
            (
                "#h\n#_defaults_#\tP\nb\t1\n#_defaults_#\n#_defaults_#\tQ\tR\n",
                "#h\n#_defaults_#\tQ\tR\n",
            ),
            ("a\n#_defaults_#\tP\n#_defaults_#\n", "#_defaults_#\tP\n"),
            // With a byte-order mark, line 1 is a row to TSVZ.py.
            ("\u{feff}#_defaults_#\tP\na\t1\n", ""),
        ] {
            assert_eq!(with(input, cleared), output, "{input:?}");
        }
    }

    #[test]
    #[cfg(feature = "cli")]
    fn a_delete_in_a_one_column_file_keeps_one_column() {
        let gone = |text: &str, keys: &[&str]| {
            let keys: HashSet<&str> = keys.iter().copied().collect();
            with(text, |l| without(l, &keys).unwrap())
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
    }
}
