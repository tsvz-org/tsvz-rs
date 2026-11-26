//! Loose files (§5.2, §20.2.4), read as TSVZ.py reads them: a loose file's data semantics are
//! implementation-defined, and tsvz-rs takes TSVZ.py 5.18's (`_processLine`, with `_unsanitize` and
//! `_fitRow`), so that the two tools read a loose file alike. A key a command names is named as the
//! reader reads the cell ([`name`]); TSVZ.py's commands do not share a rule, and
//! docs/silent-choices.md says where they differ. [`parse`] gives a
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
/// themselves stay as they are, and read back as `<sep>` and `<LF>` ([`name`]).
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

/// The key a reader names `key` by, in a loose file of delimiter `d`: the one cell that
/// [`parse`] reads from the key as [`sanitize`] writes it (the encoded cell trimmed of the
/// whitespace at its end, then decoded). None when that is not `key` less some whitespace at its
/// end, so that no row can have `key`: its line begins with `#` (a comment, but for a `#_defaults_#`
/// line) or a NUL; it is only whitespace that its encoding leaves as it is (a reader trims that
/// away; the delimiter or a line break encodes as `<sep>` or `<LF>`, and so is a key); it holds `</sep/>` or
/// `</LF/>` (read back as `<sep>` and `<LF>`); or its encoding holds the delimiter (`-d e`: `<sep>`
/// has an `e`). The rule for every key a loose command names, and, field by field, for what a loose
/// write may write ([`reads_back`]).
pub(crate) fn name(key: &str, d: u8) -> Option<String> {
    let encoded = sanitize(key, d);
    let cells = parse(&encoded, d, true)?;
    let [cell] = &cells[..] else {
        return None;
    };
    read_as(key, cell).then(|| cell.to_string())
}

/// The key a loose command names for `requested` ([`name`]), or, for a key no row can have, the key
/// itself. A `delete` drops such a key instead ([`name`] itself), as nothing is written for it.
pub(crate) fn lookup<'k>(requested: &'k str, d: u8) -> Cow<'k, str> {
    name(requested, d).map_or(Cow::Borrowed(requested), Cow::Owned)
}

/// Whether a field written as `field` reads back as `cell`: `field` less whitespace at its end.
fn read_as(field: &str, cell: &str) -> bool {
    field
        .strip_prefix(cell)
        .is_some_and(|rest| rest.chars().all(py_space))
}

/// Why a reader cannot read `fields` back from the line a loose write makes of them ([`reads_back`]
/// fails): the first field that does not read back, and what in it a reader reads otherwise.
pub(crate) fn cause(fields: &[&str], d: u8) -> String {
    let dq = (d as char).escape_default();
    for (i, f) in fields.iter().enumerate() {
        let what = if i == 0 { "the key" } else { "the value" };
        // The line begins with the key as written: encoded, less the whitespace at its end.
        let written = sanitize(f, d);
        let written = written.trim_end_matches(py_space);
        if i == 0 && written.starts_with('#') && !written.starts_with(DEFAULTS) {
            return format!(
                "{what} {f:?} begins with `#`, so a reader takes the line for a comment"
            );
        }
        if i == 0 && written.starts_with('\0') {
            return format!("{what} {f:?} begins with a NUL, which a reader drops");
        }
        if i == 0 && written.is_empty() {
            return format!("{what} {f:?} is empty or only whitespace, so a reader skips the line");
        }
        if sanitize(f, d).contains(d as char) {
            return format!(
                "{what} {f:?} written as {:?} holds the delimiter `{dq}`, so a reader splits it there",
                sanitize(f, d)
            );
        }
        if f.contains("</sep/>") || f.contains("</LF/>") {
            return format!(
                "{what} {f:?} holds `</sep/>` or `</LF/>`, which a reader reads as `<sep>` and `<LF>`"
            );
        }
    }
    "a reader reads the line as other cells".to_string()
}

/// Whether the line `line`, written from `fields`, reads back as them ([`parse`]): as many cells
/// as fields, each its field less some whitespace at its end ([`name`]'s rule, for a line).
/// Else what it reads back as: None for a line read as nothing.
pub(crate) fn reads_back<'l>(
    line: &'l str,
    d: u8,
    fields: &[&str],
) -> Result<(), Option<Vec<Cow<'l, str>>>> {
    match parse(line, d, true) {
        Some(cells)
            if cells.len() == fields.len()
                && cells.iter().zip(fields).all(|(c, f)| read_as(f, c)) =>
        {
            Ok(())
        }
        other => Err(other),
    }
}

/// A line's cells as TSVZ.py reads it (`_processLine`): without the NULs at its start (and at its
/// end, for an unterminated last line, which has no `\n` after them) and the CRs and newlines at
/// its end, split on the delimiter, each cell decoded ([`unsanitize`]). None for a line read as
/// nothing: empty, a comment (it begins with `#`, but not with `#_defaults_#`), or one whose key is
/// empty once decoded (only whitespace). `line` is the line without its `\n`.
pub(crate) fn parse(line: &str, d: u8, terminated: bool) -> Option<Vec<Cow<'_, str>>> {
    split(line, d, terminated).map(Split::cells)
}

/// A line as [`parse`] reads it, but for its cells after the key, which are decoded only when
/// asked for ([`Split::cells`]): a read that does not want a row's cells counts them and tells
/// whether they hold a value without decoding them.
pub(crate) struct Split<'l> {
    /// The key, decoded.
    pub(crate) key: Cow<'l, str>,
    /// The line, without what a reader drops at its ends.
    line: &'l str,
    d: u8,
}

#[cfg(test)]
thread_local! {
    /// How many lines' cells this thread decoded ([`Split::cells`]), for a test.
    pub(crate) static DECODED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The line `line` split as [`parse`] reads it; None for a line read as nothing.
pub(crate) fn split(line: &str, d: u8, terminated: bool) -> Option<Split<'_>> {
    let mut line = line.trim_start_matches('\0');
    if !terminated {
        line = line.trim_end_matches('\0');
    }
    let line = line.trim_end_matches(['\r', '\n']);
    if line.is_empty() || (line.starts_with('#') && !line.starts_with(DEFAULTS)) {
        return None;
    }
    let first = line.split(d as char).next().unwrap_or(line);
    let key = unsanitize(first, d);
    (!key.is_empty()).then_some(Split { key, line, d })
}

impl<'l> Split<'l> {
    /// How many cells the line has, its key's included.
    pub(crate) fn count(&self) -> usize {
        1 + memchr::memchr_iter(self.d, self.line.as_bytes()).count()
    }

    /// Whether the cells after the key are none or all empty once decoded ([`valueless`]): a
    /// field decodes to nothing exactly when it is only whitespace.
    pub(crate) fn valueless(&self) -> bool {
        self.line
            .split(self.d as char)
            .skip(1)
            .all(|f| f.trim_end_matches(py_space).is_empty())
    }

    /// The line's cells, its key first, each decoded ([`parse`]).
    pub(crate) fn cells(self) -> Vec<Cow<'l, str>> {
        #[cfg(test)]
        DECODED.with(|n| n.set(n.get() + 1));
        let d = self.d;
        self.line
            .split(d as char)
            .map(|c| unsanitize(c, d))
            .collect()
    }
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

/// A line's first cell as the file has it, the bytes [`parse`] would decode into the key: after the
/// NULs at the line's start (and at its end, for an unterminated last line) and before the
/// delimiter or the CRs and newlines at its end.
#[cfg(feature = "cli")]
fn first_cell(raw: &[u8], d: u8) -> &[u8] {
    let terminated = raw.last() == Some(&b'\n');
    let mut line = raw;
    while let [b'\0', rest @ ..] = line {
        line = rest;
    }
    if !terminated {
        while let [rest @ .., b'\0'] = line {
            line = rest;
        }
    }
    while let [rest @ .., b'\r' | b'\n'] = line {
        line = rest;
    }
    line.split(|&b| b == d).next().unwrap_or(line)
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

/// The line that sets a loose file's column count, as `clear` and a one-column `delete` count it:
/// the first keyed line (read as something, [`parse`]).
#[derive(Default)]
struct Pins {
    n: usize,
    width_line: Option<usize>,
    one: bool,
}

impl Pins {
    /// Count the next line, of `cells` cells when it is read as something: its number.
    fn next(&mut self, cells: Option<usize>) -> usize {
        let i = self.n;
        self.n += 1;
        if let (Some(cells), None) = (cells, self.width_line) {
            self.width_line = Some(i);
            self.one = cells == 1;
        }
        i
    }
}

/// How many readers a scrub follows ([`READERS`]).
const N: usize = 5;

/// A reader of a loose file, as far as a rewrite follows it: TSVZ.py 5.18's `_processLine` for one
/// line, less the cells. It tells which lines the reader applies, as a row of their key or as its
/// deletion, by its own column count and `#_defaults_#` state, which decide that.
#[derive(Clone, Copy)]
struct Reader {
    /// It drops a line that holds a value and is narrower than its column count, while no
    /// `#_defaults_#` cell that holds a value is in force (`readTSV(strict=True)`).
    strict: bool,
    /// It takes line 1 for its header, and reads from line 2.
    header: bool,
    /// Its header is the one line 1 holds, so its column count is line 1's cells
    /// (`_headerCellsMatch`); any other reader's is that of the first keyed line it reads.
    matched: bool,
    /// Its column count, once set.
    count: Option<usize>,
    /// `#_defaults_#` cells that hold a value are in force (`any(defaults[1:])`).
    active: bool,
}

/// What a reader needs of a keyed line ([`Split`]).
#[derive(Clone, Copy)]
struct Shape {
    /// Its cells, its key's included.
    cells: usize,
    /// Its cells after the key are none or all empty ([`Split::valueless`]).
    valueless: bool,
    /// Its key is `#_defaults_#`.
    defaults: bool,
}

impl Shape {
    fn of(line: &Split<'_>) -> Shape {
        Shape {
            cells: line.count(),
            valueless: line.valueless(),
            defaults: line.key == DEFAULTS,
        }
    }
}

/// What a reader does with a keyed line: nothing (it drops it, or it is a `#_defaults_#` line), or
/// it applies it to its key, as a row or as the key's deletion.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Applied {
    No,
    Row,
    Delete,
}

impl Reader {
    const fn new(strict: bool, header: bool, matched: bool) -> Reader {
        Reader {
            strict,
            header,
            matched,
            count: None,
            active: false,
        }
    }

    /// The reader before line 1 of a file, `raw`: a header that line 1 holds has as many cells.
    fn start(&mut self, raw: &[u8], d: u8) {
        if self.matched {
            self.count = Some(1 + memchr::memchr_iter(d, raw).count());
        }
    }

    /// What the reader does with the keyed line `i`, of shape `s`, and whether that line set its
    /// column count.
    fn line(&mut self, i: usize, s: Shape) -> (Applied, bool) {
        if self.header && i == 0 {
            return (Applied::No, false); // its header
        }
        let counted = self.count.is_none();
        let count = *self.count.get_or_insert(s.cells);
        let applied = if s.valueless {
            if count == 1 {
                Applied::Row // a key alone is a row in a one-column file, `#_defaults_#` too
            } else if s.defaults {
                self.active = false; // a reset
                Applied::No
            } else {
                Applied::Delete
            }
        } else if self.strict && s.cells < count && !self.active {
            Applied::No // a strict reader drops a narrower line
        } else if s.defaults {
            self.active = true;
            Applied::No
        } else {
            Applied::Row
        };
        (applied, counted)
    }
}

/// The readers whose readings a scrub keeps, in this order: the command line's (both tools': lenient,
/// no header; TSVZ.py's lenient library read too); TSVZ.py's strict library read (`readTSV`'s
/// default); a lenient one given the header line 1 holds (TSVZ.py's `--x-header` and `TSVZed` too);
/// a lenient one given another header; a strict one given the header line 1 holds. A strict one
/// given another header raises, so it reads nothing.
const READERS: [Reader; N] = [
    Reader::new(false, false, false),
    Reader::new(true, false, false),
    Reader::new(false, true, true),
    Reader::new(false, true, false),
    Reader::new(true, true, true),
];

/// A key to one reader: its last deletion of the key, and the first and the last line it read as a
/// row of the key since (the key's place among the keys, and its value); `usize::MAX` for none.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Life {
    deleted: usize,
    first: usize,
    last: usize,
}

impl Life {
    /// A key the reader applied no line of.
    const NONE: Life = Life {
        deleted: usize::MAX,
        first: usize::MAX,
        last: usize::MAX,
    };

    /// The first and the last line the reader read as a row of the key since its last deletion.
    fn rows(&self) -> Option<(usize, usize)> {
        (self.first != usize::MAX).then_some((self.first, self.last))
    }

    /// The reader applied line `i` to the key; whether it held a row of the key already.
    fn apply(&mut self, applied: Applied, i: usize) -> bool {
        match applied {
            Applied::Row => {
                let again = self.first != usize::MAX;
                self.first = self.first.min(i);
                self.last = i;
                again
            }
            Applied::Delete => {
                *self = Life {
                    deleted: i,
                    ..Life::NONE
                };
                false
            }
            Applied::No => false,
        }
    }
}

/// A key to the readers: one life each, readers that did the same with every line of the key
/// sharing one, so that while all did (nearly every key) there is one.
enum Lives {
    Same(Life),
    /// Reader `r`'s life is `lives[of[r]]`; reader 0's is the first.
    Apart {
        of: [u8; N],
        lives: Box<[Life]>,
    },
}

impl Lives {
    /// The lives, each once.
    fn all(&self) -> &[Life] {
        match self {
            Lives::Same(life) => std::slice::from_ref(life),
            Lives::Apart { lives, .. } => lives,
        }
    }

    /// Reader `r` applied line `i` as `applied[r]` says; whether one held a row of the key already.
    fn apply(&mut self, applied: &[Applied; N], i: usize) -> bool {
        let of = match self {
            Lives::Same(life) if applied.iter().all(|a| *a == applied[0]) => {
                return life.apply(applied[0], i);
            }
            Lives::Same(_) => [0; N],
            Lives::Apart { of, .. } => *of,
        };
        // Readers that shared a life go on sharing one where they do the same with this line.
        let mut shared = [(0, Applied::No); N];
        let mut lives = [Life::NONE; N];
        let mut new_of = [0; N];
        let mut n = 0;
        for ((&was, &a), slot) in of.iter().zip(applied).zip(&mut new_of) {
            *slot = match shared[..n].iter().position(|s| *s == (was, a)) {
                Some(c) => c as u8,
                None => {
                    shared[n] = (was, a);
                    lives[n] = self.all()[usize::from(was)];
                    n += 1;
                    n as u8 - 1
                }
            };
        }
        let mut again = false;
        for (life, (_, a)) in lives.iter_mut().zip(shared).take(n) {
            again |= life.apply(a, i);
        }
        match self {
            // As many lives as before: the same readers share each.
            Lives::Apart { lives: was, .. } if was.len() == n => was.copy_from_slice(&lives[..n]),
            _ => {
                *self = Lives::Apart {
                    of: new_of,
                    lives: lives[..n].into(),
                }
            }
        }
        again
    }
}

/// What the first read keeps of a key: the line of a key that every reader read as a row and that
/// has no other line (most keys of a file of keys written once), or its [`Lines`].
enum Key {
    Row(usize),
    More(Box<Lines>),
}

/// A key's lives, and the last line a reader read as a row of the key again, as the file has it.
struct Lines {
    lives: Lives,
    last_raw: Option<Box<[u8]>>,
}

impl Key {
    fn lines(&mut self) -> &mut Lines {
        if let Key::Row(first) = *self {
            *self = Key::More(Box::new(Lines {
                lives: Lives::Same(Life {
                    first,
                    last: first,
                    ..Life::NONE
                }),
                last_raw: None,
            }));
        }
        match self {
            Key::More(lines) => lines,
            Key::Row(_) => unreachable!("made above"),
        }
    }
}

/// A line kept whatever its key's lives: line 1 when it is keyed, and a line that set a reader's
/// column count; with its key and its cells.
struct Pin {
    line: usize,
    key: Box<str>,
    cells: usize,
}

/// The first read of a loose `scrub`: each reader of [`READERS`] reads the lines by its own rule
/// ([`Reader`]), and a line stays where a reader needs it, in its place and as the file has it:
/// line 1; every `#_defaults_#` line; each line that sets a reader's column count (so that each
/// line kept is read in the state it was read in); for each reader and key, the first and the last
/// line it reads as a row of the key since its last deletion (the key's place among its keys, and
/// its value); and a reader's last deletion of a key where a line of the key kept lies before it,
/// which the deletion undoes. Comments, empty lines, keys of only whitespace and every other line
/// go. One change of place: a key's last line takes the place of its first where every reader that
/// reads a row of the key reads its first and its last there (to one that reads none the key stays
/// gone), no `#_defaults_#` line lies between them, the first is not line 1, nor a line that sets
/// a column count unless the two have as many cells, the last ends in a newline, and the key is not
/// `#_defaults_#` (every such line stays where it is). Each reader then reads each key from lines
/// read in the state they were read in, so as before. A header reader that reads at one column a
/// file that the command line reads at more is not followed (it reads as a row every key ever
/// written, so nothing could go): only the line that sets its column count stays. The second read
/// writes the lines kept as it reads them, so memory follows the keys, not the lines: a key that
/// every reader read once, as a row, is its line's number.
pub(crate) struct ScrubPlan {
    d: u8,
    readers: [Reader; N],
    /// Per line: whether it stays whatever its key's lives (line 1, a pin, a `#_defaults_#` line).
    keep: Vec<bool>,
    pins: Vec<Pin>,
    /// The `#_defaults_#` lines, in order.
    defaults: Vec<usize>,
    keys: HashMap<Box<str>, Key>,
}

impl ScrubPlan {
    pub(crate) fn new(d: u8) -> ScrubPlan {
        ScrubPlan {
            d,
            readers: READERS,
            keep: Vec::new(),
            pins: Vec::new(),
            defaults: Vec::new(),
            keys: HashMap::new(),
        }
    }

    /// The lines to keep, and the bytes to write in place of some of them.
    pub(crate) fn finish(&mut self) -> Kept {
        let mut keep = std::mem::take(&mut self.keep);
        let mut put = HashMap::new();
        for (name, key) in std::mem::take(&mut self.keys) {
            let Lines { lives, last_raw } = match key {
                Key::Row(i) => {
                    keep[i] = true;
                    continue;
                }
                Key::More(lines) => *lines,
            };
            let lives = lives.all();
            let mut first = usize::MAX;
            for (a, l) in lives.iter().filter_map(Life::rows) {
                keep[a] = true;
                keep[l] = true;
                first = first.min(a);
            }
            let pinned_before = |at: usize| self.pins.iter().any(|p| p.line < at && p.key == name);
            for life in lives.iter().filter(|x| x.deleted != usize::MAX) {
                if first < life.deleted || pinned_before(life.deleted) {
                    keep[life.deleted] = true;
                }
            }
            let (Some(raw), Some((a, l))) = (last_raw, lives[0].rows()) else {
                continue;
            };
            let after = self.defaults.partition_point(|&x| x <= a);
            let defaults_between = self.defaults.get(after).is_some_and(|&x| x < l);
            let as_wide = || {
                let (text, terminated) = text_of(&raw);
                let cells = split(&text, self.d, terminated).map_or(0, |s| s.count());
                self.pins.iter().all(|p| p.line != a || p.cells == cells)
            };
            if a != 0
                && a != l
                && &*name != DEFAULTS
                && lives
                    .iter()
                    .all(|x| x.rows().is_none_or(|rows| rows == (a, l)))
                && !defaults_between
                && raw.last() == Some(&b'\n')
                && as_wide()
            {
                put.insert(a, raw);
                keep[l] = false;
            }
        }
        Kept { keep, put }
    }
}

impl Planner for ScrubPlan {
    fn line(&mut self, raw: &[u8]) {
        let d = self.d;
        let i = self.keep.len();
        if i == 0 {
            for reader in &mut self.readers {
                reader.start(raw, d);
            }
        }
        let (text, terminated) = text_of(raw);
        let Some(line) = split(&text, d, terminated) else {
            self.keep.push(i == 0);
            return;
        };
        let s = Shape::of(&line);
        let mut applied = [Applied::No; N];
        let mut pinned = i == 0;
        for (a, reader) in applied.iter_mut().zip(&mut self.readers) {
            let (did, counted) = reader.line(i, s);
            *a = did;
            pinned |= counted;
        }
        // A header reader that reads at one column where the command line reads more is not
        // followed: to the plan it reads as the command line, but for its column-count line.
        let one = self.readers[0].count == Some(1);
        let lenient = applied[0];
        for (a, reader) in applied.iter_mut().zip(&self.readers) {
            if reader.header && reader.count == Some(1) && !one {
                *a = lenient;
            }
        }
        if pinned {
            self.pins.push(Pin {
                line: i,
                key: line.key.as_ref().into(),
                cells: s.cells,
            });
        }
        if s.defaults {
            self.defaults.push(i);
        }
        self.keep.push(pinned || s.defaults);
        let Some(key) = self.keys.get_mut(&*line.key) else {
            let key = if applied.iter().all(|a| *a == Applied::Row) {
                Key::Row(i)
            } else {
                let mut lives = Lives::Same(Life::NONE);
                lives.apply(&applied, i);
                Key::More(Box::new(Lines {
                    lives,
                    last_raw: None,
                }))
            };
            self.keys.insert(line.key.as_ref().into(), key);
            return;
        };
        let lines = key.lines();
        if lines.lives.apply(&applied, i) {
            match &mut lines.last_raw {
                Some(last) if last.len() == raw.len() => last.copy_from_slice(raw),
                last => *last = Some(raw.into()),
            }
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
/// empty, else an empty line in its place, as TSVZ.py writes where a file has no header (a program
/// that takes line 1 for a header then takes no row or defaults line for it; TSVZ.py's `clear`
/// writes it over a comment line 1 too, which tsvz-rs keeps: both read the two files alike); and,
/// in a file of more than one column, the `#_defaults_#` lines that hold a value after the last
/// one that holds none (which resets the defaults), so that rows written later read with the
/// defaults they had before. They keep the file's column count too, which the first of them may not have: a
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
        put(self.header.as_deref().unwrap_or(b"\n"));
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
        let i = self.pins.next(cells);
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
        let i = self.pins.next(cells);
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
            // the line's own first cell, as the file has it (also where it is not UTF-8)
            self.row = Some(first_cell(raw, d).to_vec());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each cause a loose write is refused for names itself (the checks of `cause` are in the
    /// order of the writer's, the first field that does not read back first).
    #[test]
    fn a_refusal_names_its_cause() {
        let cases: [(&[&str], u8, &str); 13] = [
            (&["#tag", "1"], b'\t', "begins with `#`"),
            (&["\0k", "1"], b'\t', "begins with a NUL"),
            (&[" ", "1"], b'\t', "is empty or only whitespace"),
            (&["k", "a</sep/>"], b'\t', "holds `</sep/>` or `</LF/>`"),
            (&["k", "x</LF/>"], b'\t', "holds `</sep/>` or `</LF/>`"),
            (&["k", "hello"], b'e', "holds the delimiter `e`"),
            (&["#k", "hello"], b'e', "begins with `#`"),
            // `</sep/>` holds an `e`: a reader splits it before it would decode it.
            (&["k", "</sep/>"], b'e', "holds the delimiter `e`"),
            // A key that reads back, beside a value that does not: the value is named.
            (
                &["#_defaults_#", "x</sep/>"],
                b'\t',
                "the value \"x</sep/>\" holds",
            ),
            (&["#tag", "x</sep/>"], b'#', "the value \"x</sep/>\" holds"),
            (&[" ", "x</sep/>"], b' ', "the value \"x</sep/>\" holds"),
            // A key that begins with `#_defaults_#` is no comment, so its own cause is named.
            (
                &["#_defaults_#</sep/>", "1"],
                b'\t',
                "the key \"#_defaults_#</sep/>\" holds",
            ),
            (
                &["#_defaults_#</LF/>", "1"],
                b'\t',
                "the key \"#_defaults_#</LF/>\" holds",
            ),
        ];
        for (fields, d, cause_text) in cases {
            let line = fields
                .iter()
                .map(|f| sanitize(f, d).trim_end_matches(py_space).to_string())
                .collect::<Vec<_>>()
                .join(&(d as char).to_string());
            assert!(reads_back(&line, d, fields).is_err(), "{fields:?}");
            assert!(cause(fields, d).contains(cause_text), "{fields:?}");
        }
    }

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
        assert_eq!(name("x</LF/>", b'\t'), None);
        assert_eq!(name("x<LF>", b'\t').as_deref(), Some("x<LF>"));
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
            // Line 1 stays, and so does the first row of a key to a reader that takes line 1
            // for its header (`a<TAB>2`, which also sets its column count).
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
            // One column: a key alone is a row. A reader given the header `a` reads line 3 as
            // the row `a` (0.2.3 dropped it, and that reader lost `a`), and one given another
            // header, which counts two columns from `b<TAB>1`, as a deletion.
            ("a\nb\t1\na\n", "a\nb\t1\na\n"),
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
            // A strict reader (TSVZ.py's `readTSV(strict=True)`) drops a line that holds a value
            // and is narrower than the column count, three here: `K` reads `[K, 1, 2]` to it, and
            // so that line stays (0.2.1 moved `K<TAB>9` into its place, and `K` was gone to that
            // reader). Its first and last row of a key stay beside the command line's, so a key's
            // last line moves only where both read the key from the same two lines: `K<TAB>1` is
            // the command line's first row of `K`, `K<TAB>2<TAB>2` the strict reader's, and both
            // stay (0.2.3 moved the second into the place of the first).
            (
                "#h\nx\t0\t0\ny\t5\t5\nK\t1\t2\nK\t9\n",
                "#h\nx\t0\t0\ny\t5\t5\nK\t1\t2\nK\t9\n",
            ),
            (
                "#h\nx\t0\t0\nK\t1\nK\t2\t2\nz\t0\t0\nK\t3\nK\t4\n",
                "#h\nx\t0\t0\nK\t1\nK\t2\t2\nz\t0\t0\nK\t4\n",
            ),
            (
                "#h\nx\t0\t0\nK\t1\nK\t2\t2\n",
                "#h\nx\t0\t0\nK\t1\nK\t2\t2\n",
            ),
            // It reads none of `K`'s lines: the last moves.
            ("#h\nx\t0\t0\nK\t1\nK\t3\n", "#h\nx\t0\t0\nK\t3\n"),
            // Where a `#_defaults_#` line that holds a value is in force, it takes a narrower line,
            // padded: `K<TAB>9` stays, which it reads as `[K, 9, Q]` (0.2.3 dropped it).
            (
                "#h\nx\t0\t0\nK\t1\t2\n#_defaults_#\tP\tQ\nK\t9\n#_defaults_#\nK\t8\n",
                "#h\nx\t0\t0\nK\t1\t2\n#_defaults_#\tP\tQ\nK\t9\n#_defaults_#\nK\t8\n",
            ),
            // Its first row of `L` is `L<TAB>8<TAB>1`, after `M`'s: it reads x, M, L, and the
            // command line x, L, M (0.2.3 moved `L<TAB>8<TAB>1` up, so that it read x, L, M).
            (
                "x\t0\t0\nL\nL\t2\nM\t9\t9\t1\nL\t8\t1\n",
                "x\t0\t0\nL\nL\t2\nM\t9\t9\t1\nL\t8\t1\n",
            ),
            // A reader given line 1 for its header reads keys a, Name, b: its first row of `Name`
            // is `Name<TAB>2`, which stays (0.2.3 dropped it, so that reader read a, b, Name).
            (
                "Name\tv\na\t1\nName\t2\nb\t3\nName\t4\na\t5\n",
                "Name\tv\na\t5\nName\t2\nb\t3\nName\t4\n",
            ),
        ] {
            assert_eq!(scrub(input), output, "{input:?}");
        }
    }

    /// Where each key's row comes from, in the order of the keys, as `reader` reads `text`
    /// ([`Reader`]): what TSVZ.py gives as `readTabularFile(storeOffset=True)`.
    fn read_by(mut reader: Reader, text: &str) -> String {
        let mut rows: Vec<(String, usize)> = Vec::new();
        for (i, raw) in lines(text).iter().enumerate() {
            if i == 0 {
                reader.start(raw, b'\t');
            }
            let (text, terminated) = text_of(raw);
            let Some(line) = split(&text, b'\t', terminated) else {
                continue;
            };
            match reader.line(i, Shape::of(&line)).0 {
                Applied::Row => match rows.iter_mut().find(|(k, _)| *k == *line.key) {
                    Some(row) => row.1 = i,
                    None => rows.push((line.key.to_string(), i)),
                },
                Applied::Delete => rows.retain(|(k, _)| *k != *line.key),
                Applied::No => {}
            }
        }
        let rows: Vec<String> = rows.iter().map(|(k, i)| format!("{k}@{i}")).collect();
        rows.join(" ")
    }

    /// Each reader a scrub follows ([`READERS`], in its order) applies the lines TSVZ.py 5.18's
    /// does: where each key's row comes from, in the order of the keys, as TSVZ.py read these
    /// files (`readTabularFile(storeOffset=True)`, given no header, line 1's cells, or another
    /// header; 2026-11-25). `-`: line 1 holds no cell, so no reader takes it for a header.
    #[test]
    fn a_reader_applies_the_lines_tsvz_py_applies() {
        let cases: [(&str, [&str; N]); 13] = [
            (
                "#h\nx\t0\t0\nK\t1\t2\n#_defaults_#\tP\tQ\nK\t9\n#_defaults_#\nK\t8\n",
                [
                    "x@1 K@6",
                    "x@1 K@4",
                    "x@1 K@6 #_defaults_#@5",
                    "x@1 K@6",
                    "x@1 K@6 #_defaults_#@5",
                ],
            ),
            (
                "x\t0\t0\nL\nL\t2\nM\t9\t9\t1\nL\t8\t1\n",
                [
                    "x@0 L@4 M@3",
                    "x@0 M@3 L@4",
                    "L@4 M@3",
                    "L@4 M@3",
                    "M@3 L@4",
                ],
            ),
            (
                "a\nb\t1\na\n",
                ["a@2 b@1", "a@2 b@1", "b@1 a@2", "b@1", "b@1 a@2"],
            ),
            (
                "Name\tv\na\t1\nName\t2\nb\t3\nName\t4\na\t5\n",
                [
                    "Name@4 a@5 b@3",
                    "Name@4 a@5 b@3",
                    "a@5 Name@4 b@3",
                    "a@5 Name@4 b@3",
                    "a@5 Name@4 b@3",
                ],
            ),
            (
                "a\n#_defaults_#\nb\n#_defaults_#\nc\na\n",
                [
                    "a@5 #_defaults_#@3 b@2 c@4",
                    "a@5 #_defaults_#@3 b@2 c@4",
                    "#_defaults_#@3 b@2 c@4 a@5",
                    "#_defaults_#@3 b@2 c@4 a@5",
                    "#_defaults_#@3 b@2 c@4 a@5",
                ],
            ),
            (
                "x\t0\t0\n#_defaults_#\tP\nk\t1\n#_defaults_#\tP\tQ\nk\t2\n#_defaults_#\t\tR\nj\t3\n#_defaults_#\t\t\nj\t4\n",
                [
                    "x@0 k@4 j@8",
                    "x@0 k@4 j@6",
                    "k@4 j@8",
                    "k@4 j@8",
                    "k@4 j@6",
                ],
            ),
            (
                "#h\tv\n#c\n\na\t1\nb\nb\t2\t3\na\t\n",
                ["b@5", "b@5", "b@5", "b@5", "b@5"],
            ),
            (
                "\0\0\na\t1\r\nb\t\r\n\0c\t2\nb\t \t\n",
                [
                    "a@1 c@3",
                    "a@1 c@3",
                    "a@1 b@4 c@3",
                    "a@1 c@3",
                    "a@1 b@4 c@3",
                ],
            ),
            (
                "k\nx\t1\ny\nx\n",
                ["k@0 x@3 y@2", "k@0 x@3 y@2", "x@3 y@2", "", "x@3 y@2"],
            ),
            (
                "Name\tA\tB\nk\t1\nk\t1\t2\nk\t9\nj\t\t\n",
                ["Name@0 k@3", "Name@0 k@2", "k@3", "k@3", "k@2"],
            ),
            (
                "a\t1\nb\t2\na\t3",
                ["a@2 b@1", "a@2 b@1", "b@1 a@2", "b@1 a@2", "b@1 a@2"],
            ),
            (
                "#_defaults_#\tP\tQ\na\t\t\nb\t1\n#_defaults_#\nb\t\t2\na\t1\n",
                ["b@4 a@5", "b@4", "b@4 a@5", "b@4 a@5", "b@4"],
            ),
            (
                "  \t1\nk\t1\t2\n#_defaults_#x\t3\nk\t5\n",
                [
                    "k@3 #_defaults_#x@2",
                    "k@1",
                    "k@3 #_defaults_#x@2",
                    "k@3 #_defaults_#x@2",
                    "k@3 #_defaults_#x@2",
                ],
            ),
        ];
        for (text, readings) in cases {
            for (r, (reader, want)) in READERS.iter().zip(readings).enumerate() {
                if want != "-" {
                    assert_eq!(read_by(*reader, text), want, "reader {r} of {text:?}");
                }
            }
        }
    }

    /// A second scrub changes nothing, on random files of the shapes the design's probe drew
    /// (dc024s, 2026-11-25): line 1 a comment, empty, a header, a row, a `#_defaults_#` line or a
    /// key alone; rows narrower and wider than the column count, keys alone, keys with empty cells,
    /// `#_defaults_#` lines that hold a value (narrower ones too), resets, comments and empty lines;
    /// CRs, NULs and an unterminated last line. (A rule that blocked the move where a reader had
    /// deleted the key made a second scrub compact 1.5% of such files further.)
    #[test]
    fn a_second_scrub_changes_nothing() {
        let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
        let mut next = |n: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % n as u64) as usize
        };
        for _ in 0..5_000 {
            let d = [b'\t', b'\t', b',', 0][next(4)];
            let ds = (d as char).to_string();
            let width = 1 + next(4);
            let keys = ["a", "b", "c", "d", "Name"];
            let cells = |n: usize, next: &mut dyn FnMut(usize) -> usize| {
                let values = ["0", "1", "x", "", ""];
                (0..n)
                    .map(|_| values[next(values.len())])
                    .collect::<Vec<_>>()
            };
            let mut lines: Vec<String> = Vec::new();
            let header = (1..width).map(|i| format!("{ds}v{i}")).collect::<String>();
            match next(6) {
                0 => lines.push("#h\n".into()),
                1 => lines.push("\n".into()),
                2 => lines.push(format!("Name{header}\n")),
                3 => lines.push(format!("{DEFAULTS}{ds}P\n")),
                4 => lines.push(format!("{}\n", keys[next(keys.len())])),
                _ => {}
            }
            for _ in 0..1 + next(25) {
                let key = keys[next(keys.len())];
                let line = match next(10) {
                    0 => ["#c", "", "\0", DEFAULTS][next(4)].to_string(),
                    1 => DEFAULTS.to_string() + &ds.repeat(next(width)),
                    2 => {
                        let n = 1 + next(width + 1);
                        [DEFAULTS]
                            .iter()
                            .copied()
                            .chain(cells(n, &mut next))
                            .collect::<Vec<_>>()
                            .join(&ds)
                    }
                    3 => key.to_string(),
                    4 => key.to_string() + &ds.repeat(1 + next(width)),
                    _ => {
                        let n = [1, width - 1, width, width, width + 1][next(5)].max(1);
                        [key]
                            .iter()
                            .copied()
                            .chain(cells(n - 1, &mut next))
                            .collect::<Vec<_>>()
                            .join(&ds)
                    }
                };
                let end = ["\n", "\n", "\n", "\r\n", "\0\n"][next(5)];
                lines.push(line + end);
            }
            let mut text = lines.concat();
            if next(5) == 0 {
                text = text.trim_end_matches('\n').to_string();
            }
            let once = scrubbed(&text, d);
            assert_eq!(scrubbed(&once, d), once, "{text:?}");
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
            ("a\t1\n#_defaults_#\tP\n", "\n#_defaults_#\tP\n"),
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
                "\n#_defaults_#\t\n#_defaults_#\tP\tQ\tS\n",
            ),
            (
                "a\t1\t2\t3\n#_defaults_#\tP\tQ\n",
                "\n#_defaults_#\t\t\t\n#_defaults_#\tP\tQ\n",
            ),
            (
                "#h\na\t1\n#_defaults_#\tP\tQ\n#_defaults_#\t\t\tR\n",
                "#h\n#_defaults_#\t\n#_defaults_#\tP\tQ\n#_defaults_#\t\t\tR\n",
            ),
            // A one-column file keeps none: a defaults line of more cells would make it two.
            ("a\n#_defaults_#\tP\n#_defaults_#\n", "\n"),
            ("#h\na\n#_defaults_#\tP\tQ\n", "#h\n"),
            // With a byte-order mark, line 1 is a row to TSVZ.py.
            ("\u{feff}#_defaults_#\tP\na\t1\n", "\n"),
            // A line 1 of only NULs is empty to TSVZ.py: kept as the empty line it writes.
            ("\0\na\t1\n", "\0\n"),
            // A line 1 that is a row (or a defaults line) goes, an empty line in its place, as
            // TSVZ.py's clear writes one: a program that takes line 1 for a header takes that.
            ("a\t1\nb\t2\n", "\n"),
            ("#_defaults_#\tP\na\t1\n", "\n#_defaults_#\tP\n"),
            ("", "\n"),
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

    /// The key put in the place of a column-count line that goes is the first line's own cell,
    /// byte for byte: a key that is not UTF-8 stays what it was (0.2.0 did; a lossy decode made
    /// it U+FFFD, another key to a reader that decodes it otherwise).
    #[test]
    #[cfg(feature = "cli")]
    fn the_key_that_replaces_a_width_line_is_the_first_cell_as_the_file_has_it() {
        let text: &[u8] = b"a\n\xe9b\x80 \tv\n";
        let mut plan = WithoutPlan::new(b'\t', ["a".to_string()].into());
        for l in text.split_inclusive(|&b| b == b'\n') {
            plan.line(l);
        }
        let kept = plan.finish().unwrap().unwrap();
        let mut w = Writer::new(kept);
        for l in text.split_inclusive(|&b| b == b'\n') {
            w.line(l);
        }
        assert_eq!(w.out, b"\xe9b\x80 \n\xe9b\x80 \tv\n");
        // NULs at the start of the line and at the end of an unterminated one are no part of it.
        assert_eq!(first_cell(b"\0\0k\0x\n", b'\t'), b"k\0x");
        assert_eq!(first_cell(b"k\0\0", b'\t'), b"k");
        assert_eq!(first_cell(b"k\r\n", b'\t'), b"k");
        assert_eq!(first_cell(b"\0k\0v", 0), b"k");
    }

    /// A key is named as a reader names its cell: the encoding trimmed, then decoded (a line break
    /// or a delimiter at its end stays, a blank goes); None for a key no row can have.
    #[test]
    fn a_key_is_named_as_a_reader_names_its_cell() {
        for (key, d, named) in [
            ("#tag", b'\t', None),
            ("#_defaults_#", b'\t', Some("#_defaults_#")),
            ("#_defaults_#x", b'\t', Some("#_defaults_#x")),
            ("#_defaults_# ", b'\t', Some("#_defaults_#")),
            ("#_DEFAULTS_#", b'\t', None),
            (" ", b'\t', None),
            ("\r \u{a0}", b'\t', None),
            ("", b'\t', None),
            ("\0a", b'\t', None),
            ("a#", b'\t', Some("a#")),
            (" a", b'\t', Some(" a")),
            ("a ", b'\t', Some("a")),
            ("a\n", b'\t', Some("a\n")), // `a<LF>`, read back with its newline
            ("a\n ", b'\t', Some("a\n")),
            ("a\t", b'\t', Some("a\t")), // `a<sep>`
            ("\n", b'\t', Some("\n")),
            ("a ", b' ', Some("a ")), // the delimiter is encoded, so not trimmed
            ("</sep/>", b'\t', None),
            ("x</LF/>", b'\t', None),
            ("hello", b'e', None), // its encoding holds the delimiter
            ("hllo", b'e', Some("hllo")),
            ("a<", b'e', Some("a<")), // a `<` that begins no token is written as it is
            ("\0a", 0, Some("\0a")),  // with the NUL as the delimiter the NUL is encoded away
        ] {
            assert_eq!(name(key, d).as_deref(), named, "{key:?} {d}");
        }
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
