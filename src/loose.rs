//! Loose files (§5.2, §20.2.4) as two readers read them: tsvz-rs by its own rule (a loose file's
//! data semantics are implementation-defined), and TSVZ.py by its 3.39 rules (TSVZ.py
//! `_processLine`). Both drop the NULs at the start of a line, take the column count from the
//! first keyed line ([`keyed`]), and read a line whose cells after the key are none or all blank
//! as a deletion ([`deletes`]), but in a one-column file, where it is a row. Line 1 begins with
//! its byte-order mark, which is part of its key. tsvz-rs reads every `#` line as a comment,
//! except a line whose key only begins with `#_defaults_#` (a longer key, such as `#_defaults_#x`,
//! is a key); TSVZ.py reads a line whose key is exactly `#_defaults_#` as defaults for the rows
//! after it; and only TSVZ.py strips each cell's trailing whitespace and decodes `<sep>` and
//! `<LF>`, and skips a line whose key is only whitespace, which tsvz-rs reads as a row when a
//! value follows it (or in a one-column file) and as a deletion of that key when it is alone or
//! its cells are blank.
//!
//! A rewrite keeps each line it keeps as the file has it, `\r` included (a byte-order mark an
//! unterminated first line loses is the one exception), and changes what a reader reads only
//! where both readers read the result alike: [`scrub`] (which moves a key's
//! last line into its first line's place where that is so), [`cleared`] and, in a one-column
//! file, where no line can delete, [`without`] (which puts a key's line in the place of a width
//! line that goes).

use std::borrow::Cow;

use hashbrown::HashMap;
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
    /// A key the two may read differently (TSVZ.py strips its trailing whitespace and decodes
    /// `<…>` tokens), or a line with a CR (TSVZ.py cuts a run of them before the newline,
    /// tsvz-rs one) or with a NUL that is no delimiter (read alike as far as tried): the check
    /// is a cautious one. The file is then rewritten without changing any line.
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

/// What a rewrite is given of each committed line of a loose file, as a read decodes them: the
/// line as the file has it, without its `\n` (line 1 with the byte-order mark the read drops).
pub(crate) trait Planner: Send {
    fn line(&mut self, raw: &[u8]);
}

/// A planner a read feeds while the caller keeps it, to take its plan once the read is done.
pub(crate) type Sink = std::sync::Arc<std::sync::Mutex<dyn Planner>>;

/// A line as the framer reads it, without the one `\r` before its `\n`.
fn text_of(raw: &[u8]) -> Cow<'_, str> {
    String::from_utf8_lossy(raw.strip_suffix(b"\r").unwrap_or(raw))
}

/// How many delimiters `line` holds, past the NULs at its start.
fn fields(line: &str, d: u8) -> usize {
    memchr::memchr_iter(d, body(line).as_bytes()).count()
}

/// The lines that set a loose file's column count, as a rewrite pins them: the first keyed line
/// ([`keyed`]), and the first after line 1, which a reader that takes line 1 for its header
/// counts from; and line 1.
#[derive(Default)]
struct Pins {
    n: usize,
    width_line: Option<usize>,
    after_first: Option<usize>,
    one: bool,
}

impl Pins {
    /// Count the next line, `text`: its number, and whether it is pinned.
    fn next(&mut self, text: &str, d: u8) -> (usize, bool) {
        let i = self.n;
        self.n += 1;
        if keyed(text, d) {
            if self.width_line.is_none() {
                self.width_line = Some(i);
                self.one = !body(text).as_bytes().contains(&d);
            }
            if i > 0 && self.after_first.is_none() {
                self.after_first = Some(i);
            }
        }
        (i, self.pinned(i))
    }

    /// Whether line `i`, already counted, is pinned.
    fn pinned(&self, i: usize) -> bool {
        i == 0 || Some(i) == self.width_line || Some(i) == self.after_first
    }
}

impl Run {
    /// The key's line `i` deletes it: the lines after it begin a new run.
    fn delete(&mut self, i: usize, pinned: bool) {
        self.last_delete = Some((i, self.pinned));
        self.first = None;
        self.last_raw = None;
        self.pinned |= pinned;
    }
}

/// What the first read of a scrub keeps of a key: the line of a key seen once, or its lines so far.
enum KeyLines {
    Once(usize),
    More(Box<Run>),
}

/// A key's lines so far ([`ScrubPlan`]).
#[derive(Default)]
struct Run {
    /// A pinned line of the key so far.
    pinned: bool,
    /// Its last deletion, and whether a pinned line of the key lies before it.
    last_delete: Option<(usize, bool)>,
    /// The key's lines after its last deletion: the first and the last, each with its count of
    /// delimiters; whether a pinned line, or a `#_defaults_#` line, follows the first; and the
    /// last line's bytes when it is not the first.
    first: Option<(usize, usize)>,
    last: (usize, usize),
    pinned_after_first: bool,
    defaults_between: bool,
    last_raw: Option<Box<[u8]>>,
}

/// The first read of a loose `scrub`: what it keeps of each line, and each live key's latest line,
/// so that the second read writes the kept lines as it reads them (memory follows the keys, not
/// the lines). Comments, empty lines, superseded lines and dead keys' lines go; line 1, every
/// `#_defaults_#` line, the lines that set the column count, and for each live key the line that
/// places it and the one that gives its value stay. A key's value line takes the place of its
/// first line when no `#_defaults_#` line (which changes how TSVZ.py reads a row) and no other line
/// of the key kept lies between them, and that place is not line 1 (unless the two lines are the
/// same) nor a line that sets the column count (unless the two have as many cells). Both readers
/// (TSVZ.py as its command line reads, given no header) read the result as they read the file;
/// line 1, and the first keyed line after it, stay for a TSVZ.py program that takes line 1 for its
/// header. A file with a line the two may read differently ([`Kind::Odd`]) loses only the lines no
/// read depends on.
pub(crate) struct ScrubPlan {
    d: u8,
    pins: Pins,
    /// Per line: its kind.
    kinds: Vec<Kind>,
    /// The delimiters of the pinned lines.
    pinned_fields: Vec<(usize, usize)>,
    first_raw: Vec<u8>,
    odd: bool,
    last_defaults: Option<usize>,
    keys: HashMap<Box<str>, KeyLines>,
}

impl ScrubPlan {
    pub(crate) fn new(d: u8) -> ScrubPlan {
        ScrubPlan {
            d,
            pins: Pins::default(),
            kinds: Vec::new(),
            pinned_fields: Vec::new(),
            first_raw: Vec::new(),
            odd: false,
            last_defaults: None,
            keys: HashMap::new(),
        }
    }

    /// The lines to keep, and the bytes to write in place of some of them.
    pub(crate) fn finish(&mut self) -> Kept {
        let pins = &self.pins;
        let mut keep: Vec<bool> = (0..self.kinds.len())
            .map(|i| match self.kinds[i] {
                Kind::Row | Kind::Delete if !self.odd => pins.pinned(i),
                k => pins.pinned(i) || k != Kind::Skip,
            })
            .collect();
        let mut put = HashMap::new();
        for key in std::mem::take(&mut self.keys).into_values() {
            let run = match key {
                KeyLines::Once(first) => {
                    keep[first] = true;
                    continue;
                }
                KeyLines::More(run) => run,
            };
            // A pinned line of the key before its last deletion is undone by that deletion.
            if let Some((at, true)) = run.last_delete {
                keep[at] = true;
            }
            let Some((first, first_fields)) = run.first else {
                continue; // dead to both
            };
            let (last, last_fields) = run.last;
            keep[first] = true;
            keep[last] = true;
            let Some(raw) = run.last_raw else {
                continue; // one line
            };
            let fits = if first == 0 {
                self.first_raw[..] == raw[..]
            } else {
                !pins.pinned(first) || first_fields == last_fields
            };
            if fits && !run.pinned_after_first && !run.defaults_between {
                put.insert(first, raw);
                keep[last] = false;
            }
        }
        Kept { keep, put }
    }
}

impl Planner for ScrubPlan {
    fn line(&mut self, raw: &[u8]) {
        let d = self.d;
        let text = text_of(raw);
        let (i, pinned) = self.pins.next(&text, d);
        if i == 0 {
            self.first_raw = raw.to_vec();
        }
        let (k, key) = kind(&text, d, self.pins.one);
        self.kinds.push(k);
        let f = fields(&text, d);
        if pinned {
            self.pinned_fields.push((i, f));
        }
        match k {
            Kind::Odd => {
                self.odd = true;
                self.keys = HashMap::new(); // nothing moves: what was learnt of keys goes
            }
            Kind::Defaults => self.last_defaults = Some(i),
            Kind::Row | Kind::Delete if !self.odd => {
                let Some(lines) = self.keys.get_mut(key) else {
                    let lines = if k == Kind::Row {
                        KeyLines::Once(i)
                    } else {
                        let mut run = Box::<Run>::default();
                        run.delete(i, pinned);
                        KeyLines::More(run)
                    };
                    self.keys.insert(key.into(), lines);
                    return;
                };
                if let KeyLines::Once(first) = *lines {
                    // the delimiters of its line matter only where it is pinned
                    let fields = self.pinned_fields.iter().find(|p| p.0 == first);
                    let fields = fields.map_or(0, |p| p.1);
                    *lines = KeyLines::More(Box::new(Run {
                        pinned: self.pins.pinned(first),
                        first: Some((first, fields)),
                        last: (first, fields),
                        ..Run::default()
                    }));
                }
                let KeyLines::More(run) = lines else {
                    unreachable!("made a run above")
                };
                if k == Kind::Delete {
                    run.delete(i, pinned);
                } else if let Some((first, _)) = run.first {
                    run.pinned_after_first |= pinned;
                    run.defaults_between = self.last_defaults.is_some_and(|l| l > first);
                    run.last = (i, f);
                    match &mut run.last_raw {
                        Some(last) if last.len() == raw.len() => last.copy_from_slice(raw),
                        last => *last = Some(raw.into()),
                    }
                    run.pinned |= pinned;
                } else {
                    run.first = Some((i, f));
                    run.last = (i, f);
                    run.pinned_after_first = false;
                    run.defaults_between = false;
                    run.pinned |= pinned;
                }
            }
            _ => {}
        }
    }
}

/// What a rewrite planned for each line: whether it stays, and the bytes it takes instead.
pub(crate) struct Kept {
    keep: Vec<bool>,
    put: HashMap<usize, Box<[u8]>>,
}

/// The second read of a rewrite: the lines a plan keeps, as the file has them or as the plan puts
/// them, each with its `\n`.
pub(crate) struct Writer {
    kept: Kept,
    n: usize,
    pub(crate) out: Vec<u8>,
}

impl Writer {
    pub(crate) fn new(kept: Kept) -> Writer {
        Writer {
            kept,
            n: 0,
            out: Vec::new(),
        }
    }

    /// How many lines it was given: the same file gives as many as the plan's read did.
    pub(crate) fn same_lines(&self) -> bool {
        self.n == self.kept.keep.len()
    }
}

impl Planner for Writer {
    fn line(&mut self, raw: &[u8]) {
        let i = self.n;
        self.n += 1;
        if self.kept.keep.get(i) == Some(&true) {
            self.out
                .extend_from_slice(self.kept.put.get(&i).map_or(raw, |p| &p[..]));
            self.out.push(b'\n');
        }
    }
}

/// What `clear` leaves of a loose file, gathered in one read: line 1 when it is a comment or
/// empty (TSVZ.py writes an empty line where a file has no header), and, in a file of more than one
/// column, the `#_defaults_#` lines that hold a value after the last one that holds none (which
/// resets the defaults), so that rows written later read to TSVZ.py with the defaults they had
/// before. They keep the file's column count too, which the first of them may not have: a
/// `#_defaults_#` line that holds no value, at that count, goes before it (it sets the count, and
/// changes nothing else). A one-column file keeps none: its first keyed line would be a defaults
/// line of more cells, and a key alone, a row there, would be a deletion.
pub(crate) struct ClearPlan {
    d: u8,
    pins: Pins,
    header: Option<Vec<u8>>,
    width_cells: usize,
    /// The `#_defaults_#` lines that hold a value since the last that holds none, with their cells.
    in_force: Vec<(Vec<u8>, usize)>,
}

impl ClearPlan {
    pub(crate) fn new(d: u8) -> ClearPlan {
        ClearPlan {
            d,
            pins: Pins::default(),
            header: None,
            width_cells: 0,
            in_force: Vec::new(),
        }
    }

    /// The file's new content.
    pub(crate) fn finish(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if let Some(h) = &self.header {
            out.extend_from_slice(h);
            out.push(b'\n');
        }
        if self.pins.one || self.in_force.is_empty() {
            return out;
        }
        if self.in_force[0].1 != self.width_cells {
            out.extend_from_slice(DEFAULTS.as_bytes());
            out.extend(std::iter::repeat_n(self.d, self.width_cells - 1));
            out.push(b'\n');
        }
        for (raw, _) in &self.in_force {
            out.extend_from_slice(raw);
            out.push(b'\n');
        }
        out
    }
}

impl Planner for ClearPlan {
    fn line(&mut self, raw: &[u8]) {
        let d = self.d;
        let text = text_of(raw);
        let (i, _) = self.pins.next(&text, d);
        let cells = fields(&text, d) + 1;
        if Some(i) == self.pins.width_line {
            self.width_cells = cells;
        }
        if i == 0 && (body(&text).is_empty() || is_comment(&text, d)) && !keyed(&text, d) {
            self.header = Some(raw.to_vec());
        }
        let (key, rest) = first_field(body(&text), d);
        if key.trim_end_matches(py_space) == DEFAULTS {
            if rest.is_some_and(|r| !r.split(d as char).all(blank)) {
                self.in_force.push((raw.to_vec(), cells));
            } else {
                self.in_force.clear(); // a reset: what it reset goes with it
            }
        }
    }
}

/// The first read of a `delete` in a one-column file, where a key alone is a row and no line can
/// delete: which lines are those of `keys` (each without the whitespace at its end, as a line's
/// key is for TSVZ.py; a key of only whitespace as it is spelled, since TSVZ.py skips such lines
/// and tsvz-rs reads each spelling as a row of its own), which go, as TSVZ.py deletes there.
/// When the line that set the column count goes and the next keyed line has more cells, the
/// first remaining key, alone, takes its place, so that the file keeps one column, as in TSVZ.py,
/// which copies there the first later key that holds no value or, when none does, the first later
/// key (`a`, `b<TAB>1`, `c`, after the delete of `a`: TSVZ.py's file begins `c`, tsvz-rs's `b`,
/// which keeps the keys in their order).
#[cfg(feature = "cli")]
pub(crate) struct WithoutPlan {
    d: u8,
    keys: HashSet<String>,
    pins: Pins,
    gone: Vec<usize>,
    /// The line that set the column count went: the first keyed line kept after it, with its
    /// delimiters; the first cell of the first such line that is no `#_defaults_#` line; and
    /// whether a `#_defaults_#` line that holds no value is kept after it.
    width_gone: bool,
    next: Option<usize>,
    row: Option<Vec<u8>>,
    valueless: bool,
}

#[cfg(feature = "cli")]
impl WithoutPlan {
    pub(crate) fn new(d: u8, keys: HashSet<String>) -> WithoutPlan {
        WithoutPlan {
            d,
            keys,
            pins: Pins::default(),
            gone: Vec::new(),
            width_gone: false,
            next: None,
            row: None,
            valueless: false,
        }
    }

    /// None in a file of more than one column, where a key alone deletes, and Some(None) when no
    /// line is one of the keys, so that the file stays as it is (also a compressed one, whose
    /// bytes a rewrite would change); else what to keep.
    pub(crate) fn finish(&self) -> Option<Option<Kept>> {
        if !self.pins.one {
            return None;
        }
        if self.gone.is_empty() {
            return Some(None);
        }
        let mut keep = vec![true; self.pins.n];
        for &i in &self.gone {
            keep[i] = false;
        }
        let mut put = HashMap::new();
        if let (true, Some(_), Some(w)) = (self.width_gone, self.next, self.pins.width_line) {
            // With no key left, a `#_defaults_#` line that holds no value, which TSVZ.py reads as
            // a row here, is kept a row by a bare one in front of it; one that holds a value alone
            // is no row, and the file then has the count of that line.
            let stand_in = match &self.row {
                Some(row) => Some(&row[..]),
                None if self.valueless => Some(DEFAULTS.as_bytes()),
                None => None,
            };
            if let Some(stand_in) = stand_in {
                keep[w] = true;
                put.insert(w, stand_in.into());
            }
        }
        Some(Some(Kept { keep, put }))
    }
}

#[cfg(feature = "cli")]
impl Planner for WithoutPlan {
    fn line(&mut self, raw: &[u8]) {
        let d = self.d;
        let text = text_of(raw);
        let (i, _) = self.pins.next(&text, d);
        let (k, key) = kind(&text, d, true);
        let name = key.trim_end_matches(py_space);
        let gone = matches!(k, Kind::Row | Kind::Odd)
            && self.keys.contains(if name.is_empty() { key } else { name });
        if gone {
            self.gone.push(i);
            if Some(i) == self.pins.width_line {
                self.width_gone = true;
            }
            return;
        }
        if !self.width_gone || !keyed(&text, d) {
            return;
        }
        // A keyed line kept after the line that set the column count, which went.
        if self.next.is_none() {
            // Only a line wider than one cell needs a key alone before it.
            self.next = (fields(&text, d) > 0).then_some(i);
            if self.next.is_none() {
                self.width_gone = false;
                return;
            }
        }
        if is_defaults(&text, d) {
            self.valueless |= deletes(first_field(&text, d).1, d, false);
        } else if self.row.is_none() {
            // the first cell of the line as both readers read it, without the NULs at its start
            // (in a `.nsv` file the first piece of the raw line is empty)
            let start = raw.iter().take_while(|&&b| b == 0).count();
            self.row = raw[start..].split(|&b| b == d).next().map(<[u8]>::to_vec);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &str) -> Vec<Vec<u8>> {
        text.split_terminator('\n')
            .map(|l| l.as_bytes().to_vec())
            .collect()
    }

    fn feed(plan: &mut impl Planner, text: &str) {
        for l in lines(text) {
            plan.line(&l);
        }
    }

    /// What the second read writes of `text` after the first planned `kept`.
    fn written(text: &str, kept: Kept) -> String {
        let mut w = Writer::new(kept);
        feed(&mut w, text);
        assert!(w.same_lines());
        String::from_utf8(w.out).unwrap()
    }

    fn scrubbed(text: &str, d: u8) -> String {
        let mut plan = ScrubPlan::new(d);
        feed(&mut plan, text);
        written(text, plan.finish())
    }

    fn scrub(text: &str) -> String {
        scrubbed(text, b'\t')
    }

    fn cleared(text: &str) -> String {
        let mut plan = ClearPlan::new(b'\t');
        feed(&mut plan, text);
        String::from_utf8(plan.finish()).unwrap()
    }

    /// None for a file of more than one column, Some(None) for a file that stays as it is.
    #[cfg(feature = "cli")]
    fn without(text: &str, d: u8, keys: &[&str]) -> Option<Option<String>> {
        let mut plan = WithoutPlan::new(d, keys.iter().map(|k| k.to_string()).collect());
        feed(&mut plan, text);
        plan.finish()
            .map(|kept| kept.map(|kept| written(text, kept)))
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
            let mut pins = Pins::default();
            for l in lines(text) {
                pins.next(&text_of(&l), b'\t');
            }
            assert_eq!(pins.one, one, "{text:?}");
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
            // the place of its first line only where that keeps the column count. tsvz-rs never
            // moves one across a #_defaults_# line, where TSVZ.py does when what it reads stays
            // (`#h`, `a<TAB>1`, `#_defaults_#<TAB>P`, `a<TAB>2` becomes `#h`, `a<TAB>2`,
            // `#_defaults_#<TAB>P`), so it writes more there.
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
            assert_eq!(scrub(input), output, "{input:?}");
        }
    }

    /// NUL is the delimiter of a `.nsv` file: a line that holds one is no odd line there (0.1.4's
    /// scrub of such a file never compacted it), and is one with another delimiter.
    #[test]
    fn a_scrub_compacts_a_file_whose_delimiter_is_nul() {
        let nul = |text: &str| scrubbed(text, 0);
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
            scrub("#h\n#c\na\0\t1\na\t2\na\t3\n"),
            "#h\na\0\t1\na\t2\na\t3\n"
        );
    }

    /// 400,000 keys, each written twice: a block of them, then the block again, so that every
    /// key's first and last line lie 400,000 lines apart. A scan of the lines between them, per
    /// key, took 50 s of CPU in a release build here (2026-11-16; the old test, 200,000 lines of
    /// 50,000 keys in random order, took 1.4 s with the scan and so passed in a release build);
    /// counted once, the scrub takes 1.4 s in a debug build and 0.4 s in a release one. 10 s of
    /// CPU, on the thread that scrubs, lies 7 times above the debug scrub and a fifth of the
    /// release scan, whatever else the machine runs (it failed on wall time, 10 s, in a run beside
    /// 160 busy processes). The thread has 120 s of wall time to end, so that a scan that does not
    /// (a debug build takes many minutes) fails the test and does not hold it.
    #[test]
    fn a_scrub_takes_time_in_the_length_of_the_file_whatever_the_keys_repeat() {
        let keys = 400_000u64;
        let (done, took) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let cpu = || {
                let mut t = libc::timespec {
                    tv_sec: 0,
                    tv_nsec: 0,
                };
                unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut t) };
                std::time::Duration::new(t.tv_sec as u64, t.tv_nsec as u32)
            };
            let mut text = String::from("#h\tv\n");
            for round in 0..2 {
                for k in 0..keys {
                    text.push_str(&format!("k{k}\t{round}\n"));
                }
            }
            let start = cpu();
            let out = scrub(&text);
            let _ = done.send((cpu() - start, out.len() < text.len()));
        });
        let (cpu, smaller) = took
            .recv_timeout(std::time::Duration::from_secs(120))
            .expect("a scrub of 800,000 lines of 400,000 keys did not end in 120 s");
        assert!(smaller);
        assert!(
            cpu < std::time::Duration::from_secs(10),
            "the scrub took {cpu:?} of CPU"
        );
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
            assert_eq!(cleared(input), output, "{input:?}");
        }
    }

    #[test]
    #[cfg(feature = "cli")]
    fn a_nul_delimiter_leaves_the_nuls_at_a_lines_start_out_of_its_count() {
        // The NULs at the start of a line are no part of it (`body`), the delimiter's too:
        // `\0a` is a key alone, in a file of one column.
        assert_eq!(without("\0a\nb\n", 0, &["a"]), Some(Some("b\n".into())));
    }

    /// The key that takes the place of a column-count line that goes is a line's first cell
    /// without its leading NULs, which in a `.nsv` file is not the first piece of the raw line:
    /// an empty one there left the file with a `#_defaults_#` line of three cells for its count,
    /// and every row after it a deletion.
    #[test]
    #[cfg(feature = "cli")]
    fn the_key_that_replaces_a_width_line_is_read_without_its_leading_nuls() {
        assert_eq!(
            without("0\n\n#_defaults_#\0Q\0\n\0x\n", 0, &["0"]),
            Some(Some("x\n\n#_defaults_#\0Q\0\n\0x\n".into()))
        );
        assert_eq!(
            without("0\n\n#_defaults_#\tQ\t\n\0x\n", b'\t', &["0"]),
            Some(Some("x\n\n#_defaults_#\tQ\t\n\0x\n".into()))
        );
    }

    #[test]
    #[cfg(feature = "cli")]
    fn a_delete_in_a_one_column_file_keeps_one_column() {
        let gone = |text: &str, keys: &[&str]| {
            without(text, b'\t', keys)
                .unwrap()
                .unwrap_or_else(|| text.to_string())
        };
        assert_eq!(gone("#h\na\nb\nc\n", &["b"]), "#h\na\nc\n");
        assert_eq!(gone("a\nb\nc\n", &["a", "c"]), "b\n");
        // A key of only whitespace is a row of its own spelling, and goes by it alone.
        assert_eq!(gone("a\n  \t1\n \t2\n \nb\n", &[" "]), "a\n  \t1\nb\n");
        // The line that set one column goes: the first remaining key, alone, takes its place (TSVZ.py
        // copies `c`, the first later key that holds no value, to the front).
        assert_eq!(gone("a\nb\t1\nc\n", &["a"]), "b\nb\t1\nc\n");
        assert_eq!(
            gone("a\n#_defaults_#\tP\nb\n", &["a"]),
            "b\n#_defaults_#\tP\nb\n"
        );
        assert_eq!(gone("a\r\nb\r\nc\r\n", &["b"]), "a\r\nc\r\n");
        // A byte-order mark is part of line 1's key, as TSVZ.py reads it: `a` is another key
        // than `\u{feff}a`, and the mark goes with its line, never onto another.
        assert_eq!(gone("\u{feff}a\nb\n", &["a"]), "\u{feff}a\nb\n");
        assert_eq!(without("\u{feff}a\nb\n", b'\t', &["a", "zz"]), Some(None));
        assert_eq!(without("a\tb\n", b'\t', &["a", "zz"]), None);
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
