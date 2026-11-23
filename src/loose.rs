//! Loose files (§5.2, §20.2.4), read as TSVZ.py reads them: a loose file's data semantics are
//! implementation-defined, and tsvz-rs takes TSVZ.py 5.18's (`_processLine`, with `_unsanitize` and
//! `_fitRow`), so that the two tools read a loose file, and name its keys, alike. [`parse`] gives a
//! line's cells as TSVZ.py splits and decodes them; the replay ([`crate::replay`]) applies the
//! column count, the deletions and the `#_defaults_#` lines; a writer encodes cells as TSVZ.py's
//! does ([`sanitize`]).
//!
//! A rewrite keeps each line it keeps as the file has it, `\r` included, and changes what a reader
//! reads only where TSVZ.py's reading stays: [`ScrubPlan`] (which moves a key's last line into its
//! first line's place where that is so), [`ClearPlan`] and, in a one-column file, where no line can
//! delete, [`WithoutPlan`] (which puts a key alone in the place of a column-count line that goes).

use std::borrow::Cow;

use hashbrown::HashMap;
#[cfg(feature = "cli")]
use std::collections::HashSet;

pub(crate) const DEFAULTS: &str = "#_defaults_#";

/// Whether `c` is whitespace to Python's `str.rstrip()`, which TSVZ.py applies to every cell.
pub(crate) fn py_space(c: char) -> bool {
    c.is_whitespace() || ('\x1c'..='\x1f').contains(&c)
}

/// A cell as TSVZ.py decodes it (`_unsanitize`): without the whitespace at its end, `<sep>` the
/// delimiter, `<LF>` a newline, and `</sep/>` and `</LF/>` the tokens `<sep>` and `<LF>`, in one
/// pass from the left.
pub(crate) fn unsanitize(cell: &str, d: u8) -> Cow<'_, str> {
    let cell = cell.trim_end_matches(py_space);
    if !cell.contains('<') {
        return Cow::Borrowed(cell);
    }
    let mut out = String::with_capacity(cell.len());
    let mut rest = cell;
    while let Some(i) = rest.find('<') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let (token, with): (&str, &str) = if rest.starts_with("</sep/>") {
            ("</sep/>", "<sep>")
        } else if rest.starts_with("</LF/>") {
            ("</LF/>", "<LF>")
        } else if rest.starts_with("<sep>") {
            ("<sep>", "")
        } else if rest.starts_with("<LF>") {
            ("<LF>", "\n")
        } else {
            ("<", "<")
        };
        if token == "<sep>" {
            out.push(d as char);
        } else {
            out.push_str(with);
        }
        rest = &rest[token.len()..];
    }
    out.push_str(rest);
    Cow::Owned(out)
}

/// A cell as TSVZ.py writes it to a loose file and prints it (`_sanitize`): the delimiter `<sep>`, a
/// newline `<LF>`, and the tokens `<sep>` and `<LF>` `</sep/>` and `</LF/>`, so that
/// [`unsanitize`] gives it back but for the whitespace at its end. `</sep/>` and `</LF/>`
/// themselves stay as they are, and read back as `<sep>` and `<LF>` ([`unwritable`]).
pub(crate) fn sanitize(cell: &str, d: u8) -> Cow<'_, str> {
    let special = |b: u8| b == d || b == b'\n' || b == b'<';
    if !cell.bytes().any(special) {
        return Cow::Borrowed(cell);
    }
    let mut out = String::with_capacity(cell.len() + 8);
    let mut rest = cell;
    while let Some(i) = rest.bytes().position(special) {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let (len, with) = if rest.as_bytes()[0] == d {
            (1, "<sep>")
        } else if rest.starts_with('\n') {
            (1, "<LF>")
        } else if rest.starts_with("</sep/>") {
            (7, "</sep/>")
        } else if rest.starts_with("</LF/>") {
            (6, "</LF/>")
        } else if rest.starts_with("<sep>") {
            (5, "</sep/>")
        } else if rest.starts_with("<LF>") {
            (4, "</LF/>")
        } else {
            (1, "<")
        };
        out.push_str(with);
        rest = &rest[len..];
    }
    out.push_str(rest);
    Cow::Owned(out)
}

/// The token in `cell` that [`sanitize`] cannot write so that it reads back: `</sep/>` or
/// `</LF/>`, which TSVZ.py writes as they are (with a warning) and reads back as `<sep>` and
/// `<LF>`.
pub(crate) fn unwritable(cell: &str) -> Option<&'static str> {
    ["</sep/>", "</LF/>"].into_iter().find(|t| cell.contains(t))
}

/// A line's cells as TSVZ.py reads it (`_processLine`): without the NULs at its start (and at its
/// end, for an unterminated last line, which has no `\n` after them) and the CRs and newlines at
/// its end, split on the delimiter, each cell decoded ([`unsanitize`]). None for a line read as
/// nothing: empty, a comment (it begins with `#`, but not with `#_defaults_#`), or one whose key is
/// empty once decoded (only whitespace). `line` is the line without its `\n`.
pub(crate) fn parse(line: &str, d: u8, terminated: bool) -> Option<Vec<Cow<'_, str>>> {
    let mut line = line.trim_start_matches('\0');
    if !terminated {
        line = line.trim_end_matches('\0');
    }
    let line = line.trim_end_matches(['\r', '\n']);
    if line.is_empty() || (line.starts_with('#') && !line.starts_with(DEFAULTS)) {
        return None;
    }
    let cells: Vec<Cow<str>> = line.split(d as char).map(|c| unsanitize(c, d)).collect();
    (!cells[0].is_empty()).then_some(cells)
}

/// Whether a line's cells after its key are none or all empty: a deletion, a reset of the
/// defaults, or, in a one-column file, a row.
pub(crate) fn valueless(cells: &[Cow<'_, str>]) -> bool {
    cells[1..].iter().all(|c| c.is_empty())
}

/// What a line is to a reader, in a file whose column count is one or not (`one`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// Read as nothing: empty, a comment, or an empty key.
    Skip,
    /// A `#_defaults_#` line: it sets the defaults of the rows after it, or resets them.
    Defaults,
    /// A row.
    Row,
    /// A deletion: a key with no value, in a file of more than one column.
    Delete,
}

/// A line's kind and key, from its cells ([`parse`]).
fn kind(cells: Option<Vec<Cow<'_, str>>>, one: bool) -> (Kind, Cow<'_, str>) {
    let Some(mut cells) = cells else {
        return (Kind::Skip, Cow::Borrowed(""));
    };
    let empty = valueless(&cells);
    let key = cells.swap_remove(0);
    let kind = match (key == DEFAULTS, empty, one) {
        (_, true, true) => Kind::Row, // a key alone is a row in a one-column file, `#_defaults_#` too
        (true, ..) => Kind::Defaults,
        (false, true, false) => Kind::Delete,
        (false, false, _) => Kind::Row,
    };
    (kind, key)
}

/// What a rewrite is given of each line of a loose file, as a read decodes them: the line as the
/// file has it, with its `\n` (an unterminated last line without one), line 1 with the byte-order
/// mark the read drops.
pub(crate) trait Planner: Send {
    fn line(&mut self, raw: &[u8]);
}

/// A planner a read feeds while the caller keeps it, to take its plan once the read is done.
pub(crate) type Sink = std::sync::Arc<std::sync::Mutex<dyn Planner>>;

/// A line as a reader reads it ([`parse`]), from its bytes as a planner gets them: its text
/// without the `\n`, and whether it had one.
fn text_of(raw: &[u8]) -> (Cow<'_, str>, bool) {
    let terminated = raw.last() == Some(&b'\n');
    let raw = if terminated {
        &raw[..raw.len() - 1]
    } else {
        raw
    };
    (String::from_utf8_lossy(raw), terminated)
}

/// `raw` ending with its `\n`, which an unterminated last line put before other lines needs.
#[cfg(feature = "cli")]
fn ended(raw: &[u8]) -> Box<[u8]> {
    if raw.last() == Some(&b'\n') {
        raw.into()
    } else {
        [raw, b"\n"].concat().into()
    }
}

/// The lines that set a loose file's column count, as a rewrite pins them: the first keyed line
/// (read as something, [`parse`]), and the first after line 1, which a reader that takes line 1
/// for its header counts from; and line 1.
#[derive(Default)]
struct Pins {
    n: usize,
    width_line: Option<usize>,
    after_first: Option<usize>,
    one: bool,
}

impl Pins {
    /// Count the next line, of `cells` cells when it is read as something: its number, and
    /// whether it is pinned.
    fn next(&mut self, cells: Option<usize>) -> (usize, bool) {
        let i = self.n;
        self.n += 1;
        if let Some(cells) = cells {
            if self.width_line.is_none() {
                self.width_line = Some(i);
                self.one = cells == 1;
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

impl Run {
    /// The key's line `i` deletes it: the lines after it begin a new run.
    fn delete(&mut self, i: usize, pinned: bool) {
        self.last_delete = Some((i, self.pinned));
        self.first = None;
        self.last_raw = None;
        self.pinned |= pinned;
    }
}

/// The first read of a loose `scrub`: what it keeps of each line, and each live key's latest line,
/// so that the second read writes the kept lines as it reads them (memory follows the keys, not
/// the lines). Comments, empty lines, superseded lines and dead keys' lines go; line 1, every
/// `#_defaults_#` line, the lines that set the column count, and for each live key the line that
/// places it and the one that gives its value stay. A key's value line takes the place of its
/// first line when no `#_defaults_#` line (which changes how a reader fills a row) and no other
/// line of the key kept lies between them, and that place is not line 1 (unless the two lines are
/// the same) nor a line that sets the column count (unless the two have as many cells). A reader
/// (tsvz-rs, and TSVZ.py as its command line reads, given no header) reads the result as the file;
/// line 1, and the first keyed line after it, stay for a TSVZ.py program that takes line 1 for its
/// header.
pub(crate) struct ScrubPlan {
    d: u8,
    pins: Pins,
    /// Per line: its kind.
    kinds: Vec<Kind>,
    /// The delimiters of the pinned lines.
    pinned_fields: Vec<(usize, usize)>,
    first_raw: Vec<u8>,
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
            last_defaults: None,
            keys: HashMap::new(),
        }
    }

    /// The lines to keep, and the bytes to write in place of some of them.
    pub(crate) fn finish(&mut self) -> Kept {
        let pins = &self.pins;
        let mut keep: Vec<bool> = (0..self.kinds.len())
            .map(|i| match self.kinds[i] {
                Kind::Row | Kind::Delete => pins.pinned(i),
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
            if raw.last() != Some(&b'\n') {
                continue; // an unterminated last line stays last: the end of a line is read otherwise
            }
            let fits = if first == 0 {
                self.first_raw[..] == *raw.strip_suffix(b"\n").unwrap_or(&raw)
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
        let (text, terminated) = text_of(raw);
        let cells = parse(&text, d, terminated);
        let f = cells.as_ref().map_or(0, |c| c.len() - 1);
        let (i, pinned) = self.pins.next(cells.as_ref().map(Vec::len));
        if i == 0 {
            self.first_raw = raw.strip_suffix(b"\n").unwrap_or(raw).to_vec();
        }
        if pinned {
            self.pinned_fields.push((i, f));
        }
        let (k, key) = kind(cells, self.pins.one);
        self.kinds.push(k);
        match k {
            Kind::Defaults => self.last_defaults = Some(i),
            Kind::Row | Kind::Delete => {
                let Some(lines) = self.keys.get_mut(&*key) else {
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
                    // the cells of its line matter only where it is pinned
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
            Kind::Skip => {}
        }
    }
}

/// What a rewrite planned for each line: whether it stays, and the bytes it takes instead.
pub(crate) struct Kept {
    keep: Vec<bool>,
    put: HashMap<usize, Box<[u8]>>,
}

/// The second read of a rewrite: the lines a plan keeps, as the file has them or as the plan puts
/// them.
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
            let line = self.kept.put.get(&i).map_or(raw, |p| &p[..]);
            // An unterminated last line stays one; a line after it is no longer last.
            if self.out.last().is_some_and(|&b| b != b'\n') {
                self.out.push(b'\n');
            }
            self.out.extend_from_slice(line);
        }
    }
}

/// What `clear` leaves of a loose file, gathered in one read: line 1 when it is a comment or
/// empty (TSVZ.py writes an empty line where a file has no header), and, in a file of more than one
/// column, the `#_defaults_#` lines that hold a value after the last one that holds none (which
/// resets the defaults), so that rows written later read with the defaults they had before. They keep the file's column count too, which the first of them may not have: a
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
        let mut put = |line: &[u8]| {
            // an unterminated line 1 stays one, unless a line follows it
            if out.last().is_some_and(|&b| b != b'\n') {
                out.push(b'\n');
            }
            out.extend_from_slice(line);
        };
        if let Some(h) = &self.header {
            put(h);
        }
        if self.pins.one || self.in_force.is_empty() {
            return out;
        }
        if self.in_force[0].1 != self.width_cells {
            let mut width = DEFAULTS.as_bytes().to_vec();
            width.extend(std::iter::repeat_n(self.d, self.width_cells - 1));
            width.push(b'\n');
            put(&width);
        }
        for (raw, _) in &self.in_force {
            put(raw);
        }
        out
    }
}

impl Planner for ClearPlan {
    fn line(&mut self, raw: &[u8]) {
        let d = self.d;
        let (text, terminated) = text_of(raw);
        let parsed = parse(&text, d, terminated);
        let cells = parsed.as_ref().map(Vec::len);
        let (i, _) = self.pins.next(cells);
        if Some(i) == self.pins.width_line {
            self.width_cells = cells.unwrap_or(1);
        }
        let Some(parsed) = parsed else {
            // line 1, read as nothing: a header comment, or an empty line
            let bare = text.trim_start_matches('\0').trim_end_matches(['\r', '\n']);
            if i == 0 && (bare.is_empty() || bare.starts_with('#')) {
                self.header = Some(raw.to_vec());
            }
            return;
        };
        if parsed[0] == DEFAULTS {
            if valueless(&parsed) {
                self.in_force.clear(); // a reset: what it reset goes with it
            } else {
                self.in_force.push((raw.to_vec(), parsed.len()));
            }
        }
    }
}

/// The first read of a `delete` in a one-column file, where a key alone is a row and no line can
/// delete: which lines are rows of `keys` (as a reader names a line's key, [`parse`]), which go,
/// as TSVZ.py deletes there.
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
    /// The line that set the column count went: the first keyed line kept after it, when it has
    /// more than one cell; the first cell of the first such line that is no `#_defaults_#` line;
    /// and whether a `#_defaults_#` line that holds no value is kept after it.
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
            // With no key left, a `#_defaults_#` line that holds no value, which a reader reads as a
            // row here, is kept a row by a bare one in front of it; one that holds a value alone is
            // no row, and the file then has the count of that line.
            let stand_in = match &self.row {
                Some(row) => Some(&row[..]),
                None if self.valueless => Some(DEFAULTS.as_bytes()),
                None => None,
            };
            if let Some(stand_in) = stand_in {
                keep[w] = true;
                put.insert(w, ended(stand_in));
            }
        }
        Some(Some(Kept { keep, put }))
    }
}

#[cfg(feature = "cli")]
impl Planner for WithoutPlan {
    fn line(&mut self, raw: &[u8]) {
        let d = self.d;
        let (text, terminated) = text_of(raw);
        let parsed = parse(&text, d, terminated);
        let cells = parsed.as_ref().map(Vec::len);
        let (i, _) = self.pins.next(cells);
        let defaults = parsed.as_ref().is_some_and(|p| p[0] == DEFAULTS);
        let empty = parsed.as_deref().is_some_and(valueless);
        let (k, key) = kind(parsed, true);
        if k == Kind::Row && self.keys.contains(&*key) {
            self.gone.push(i);
            if Some(i) == self.pins.width_line {
                self.width_gone = true;
            }
            return;
        }
        let Some(cells) = cells else { return };
        if !self.width_gone {
            return;
        }
        // A keyed line kept after the line that set the column count, which went.
        if self.next.is_none() {
            // Only a line wider than one cell needs a key alone before it.
            self.next = (cells > 1).then_some(i);
            if self.next.is_none() {
                self.width_gone = false;
                return;
            }
        }
        if defaults {
            self.valueless |= empty;
        } else if self.row.is_none() {
            // the line's key as a reader reads it, written so that it reads back the same
            self.row = Some(sanitize(&key, d).as_bytes().to_vec());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The lines of `text` as a read gives them to a planner, each with its `\n`.
    fn lines(text: &str) -> Vec<Vec<u8>> {
        text.split_inclusive('\n')
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
            // A key of only whitespace is read as nothing; the NULs at the end of an unterminated
            // last line are not part of it.
            ("  \t1\na\n", true),
            ("a\0", true),
        ] {
            let mut pins = Pins::default();
            for l in lines(text) {
                let (text, terminated) = text_of(&l);
                pins.next(parse(&text, b'\t', terminated).map(|c| c.len()));
            }
            assert_eq!(pins.one, one, "{text:?}");
        }
    }

    /// A line's cells as TSVZ.py 5.18's `_processLine` reads them (asked of TSVZ.py, 2026-11-23).
    #[test]
    fn lines_are_parsed_as_tsvz_py_reads_them() {
        let p = |line: &str, terminated: bool| {
            parse(line, b'\t', terminated)
                .map(|c| c.iter().map(|c| c.to_string()).collect::<Vec<_>>())
        };
        let v = |cells: &[&str]| Some(cells.iter().map(|c| c.to_string()).collect::<Vec<_>>());
        assert_eq!(p("a", true), v(&["a"]));
        assert_eq!(p("  ", true), None);
        assert_eq!(p("  \t1", true), None);
        assert_eq!(p("#c", true), None);
        assert_eq!(p("", true), None);
        assert_eq!(p("#_defaults_#x\t1", true), v(&["#_defaults_#x", "1"]));
        assert_eq!(p("#_defaults_# \tP", true), v(&["#_defaults_#", "P"]));
        assert_eq!(p("\0\0a\tb ", true), v(&["a", "b"]));
        assert_eq!(p("a\t1\r\r", true), v(&["a", "1"]));
        assert_eq!(p("a\t<sep>x</sep/><LF> ", true), v(&["a", "\tx<sep>\n"]));
        assert_eq!(p("a\t1\0", false), v(&["a", "1"]));
        assert_eq!(p("a\t1\0", true), v(&["a", "1\0"]));
        assert_eq!(p("\u{feff}a\t1", true), v(&["\u{feff}a", "1"]));
        assert_eq!(p("a \t1 \u{a0}\t\u{1c}", true), v(&["a", "1", ""]));
        // What TSVZ.py writes reads back, but for the whitespace at a cell's end.
        for cell in ["a\tb", "x<sep>y", "l\nm<LF>", "<", "<se>", "plain"] {
            assert_eq!(unsanitize(&sanitize(cell, b'\t'), b'\t'), cell, "{cell:?}");
        }
        assert_eq!(sanitize("x</sep/>y<sep>", b'\t'), "x</sep/>y</sep/>");
        assert_eq!(unwritable("x</LF/>"), Some("</LF/>"));
        assert_eq!(unwritable("x<LF>"), None);
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
            // A key is read without the whitespace at its end, so `a ` is `a`; a byte-order mark
            // is part of line 1's key.
            ("#h\n#c\na \t1\na\t2\na\t3\n", "#h\na\t3\n"),
            ("\u{feff}a\t1\n#c\na\t2\n", "\u{feff}a\t1\na\t2\n"),
            // An unterminated last line is a line, which stays where it is: a reader reads the NULs
            // at its end as nothing, which another line keeps.
            ("#h\na\t1\nb\t2\na\t3", "#h\na\t1\nb\t2\na\t3"),
            ("#h\na\t1\nb\t2\na\tx\0", "#h\na\t1\nb\t2\na\tx\0"),
            ("#h\na\t1\nb\t2", "#h\na\t1\nb\t2"),
            ("#h\nb\t2\na\t1\nb\t3\na\t4", "#h\nb\t3\na\t1\na\t4"),
        ] {
            assert_eq!(scrub(input), output, "{input:?}");
        }
    }

    /// NUL is the delimiter of a `.nsv` file, and the NULs at a line's start are no part of it
    /// (0.1.4's scrub of such a file never compacted it).
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
        // With another delimiter a NUL after a key's first byte is part of it: `a<NUL>` is a key
        // of its own.
        assert_eq!(scrub("#h\n#c\na\0\t1\na\t2\na\t3\n"), "#h\na\0\t1\na\t3\n");
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
        // A key of only whitespace is read as nothing, and names no line.
        assert_eq!(
            gone("a\n  \t1\n \t2\n \nb\n", &[" "]),
            "a\n  \t1\n \t2\n \nb\n"
        );
        // An unterminated last line is a row, and goes; one kept stays unterminated.
        assert_eq!(gone("a\nb\nc", &["c"]), "a\nb\n");
        assert_eq!(gone("a\nb\nc", &["a"]), "b\nc");
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
