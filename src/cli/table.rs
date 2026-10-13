//! The table format (§20.4.2), laid out as TSVZ.py lays it out.

use std::borrow::Cow;
use std::cmp::Ordering;

use super::width_table::{WIDE, ZERO};

fn in_ranges(table: &[(u32, u32)], cp: u32) -> bool {
    table
        .binary_search_by(|&(a, b)| if b < cp { Ordering::Less } else if a > cp { Ordering::Greater } else { Ordering::Equal })
        .is_ok()
}

/// Terminal columns a character takes, by TSVZ.py's rule: 1 below U+0300; 0 for combining
/// marks, categories Mn, Me and Cf, and U+1160–U+11FF; 2 for East Asian Wide and Fullwidth.
pub(crate) fn char_width(c: char) -> usize {
    let cp = c as u32;
    if cp < 0x300 {
        1
    } else if in_ranges(ZERO, cp) {
        0
    } else if in_ranges(WIDE, cp) {
        2
    } else {
        1
    }
}

pub(crate) fn width(s: &str) -> usize {
    if s.is_ascii() { s.len() } else { s.chars().map(char_width).sum() }
}

fn is_control(c: char) -> bool {
    matches!(c, '\u{0}'..='\u{1f}' | '\u{7f}'..='\u{9f}' | '\u{61c}' | '\u{200e}' | '\u{200f}' | '\u{2028}' | '\u{2029}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

/// A cell as a table shows it: control characters escaped, so no stored value can move the
/// cursor, reorder the line or send the terminal an escape sequence. Backslashes are not
/// escaped: the table is for reading, records are exact.
pub(crate) fn escape_cell(s: &str) -> Cow<'_, str> {
    if !s.chars().any(is_control) {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c if is_control(c) && (c as u32) < 0x100 => out.push_str(&format!("\\x{:02x}", c as u32)),
            c if is_control(c) => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    Cow::Owned(out)
}

/// What fits in `w` columns: the cell, or what fits in `w - 2` columns and `..`.
fn truncate(s: &str, w: usize) -> Cow<'_, str> {
    if width(s) <= w {
        return Cow::Borrowed(s);
    }
    if w < 2 {
        return Cow::Owned(".".repeat(w));
    }
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let cw = char_width(c);
        if used + cw > w - 2 {
            break;
        }
        out.push(c);
        used += cw;
    }
    out.push_str("..");
    Cow::Owned(out)
}

fn pad(out: &mut String, s: &str, w: usize) {
    out.push_str(s);
    for _ in width(s)..w {
        out.push(' ');
    }
}

fn render(header: Option<&[Cow<'_, str>]>, rows: &[Vec<Cow<'_, str>>], widths: &[usize], sep: &str, hsep: &str) -> String {
    let rule = widths.iter().map(|&w| "-".repeat(w)).collect::<Vec<_>>().join(hsep);
    let mut out = String::new();
    if let Some(h) = header {
        for (j, (cell, &w)) in h.iter().zip(widths).enumerate() {
            if j > 0 {
                out.push_str(sep);
            }
            pad(&mut out, cell, w);
        }
        out.push('\n');
        out.push_str(&rule);
        out.push('\n');
    }
    for row in rows {
        if row.iter().all(|c| c.is_empty()) {
            out.push_str(&rule);
        } else {
            for (j, (cell, &w)) in row.iter().zip(widths).enumerate() {
                if j > 0 {
                    out.push_str(sep);
                }
                pad(&mut out, &truncate(cell, w), w);
            }
        }
        out.push('\n');
    }
    out
}

fn escaped<'a>(rows: &'a [Vec<String>], cols: usize) -> Vec<Vec<Cow<'a, str>>> {
    rows.iter().map(|r| (0..cols).map(|j| r.get(j).map_or(Cow::Borrowed(""), |c| escape_cell(c))).collect()).collect()
}

/// Row output (read, get, pop, popitem, setdefault, keys): no header; every row padded to the
/// widest row; each column padded to its widest cell; ` | ` between cells; nothing shortened;
/// an all-empty row drawn as the rule. TSVZ.py's `pretty_format_table(rows, header=False, full=True)`.
pub(crate) fn rows_table(rows: &[Vec<String>]) -> String {
    if rows.is_empty() {
        return String::new();
    }
    let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
    let cells = escaped(rows, cols);
    let widths: Vec<usize> = (0..cols).map(|j| cells.iter().map(|r| width(&r[j])).max().unwrap_or(0)).collect();
    render(None, &cells, &widths, " | ", "-+-")
}

/// `verify` and `parts`: a header and a rule, fitted to `cols` terminal columns: ` | ` between
/// cells when that fits, else `|`, else the columns wider than their header are shortened,
/// most first, and cut cells end in `..`. TSVZ.py's `pretty_format_table([header] + rows)`.
pub(crate) fn fields_table(header: &[&str], rows: &[Vec<String>], cols: usize) -> String {
    let n = header.len();
    let head: Vec<Cow<'_, str>> = header.iter().map(|h| escape_cell(h)).collect();
    let cells = escaped(rows, n);
    let mut widths: Vec<usize> =
        (0..n).map(|j| cells.iter().map(|r| width(&r[j])).chain([width(&head[j])]).max().unwrap_or(0)).collect();
    let total = |widths: &[usize], sep: usize| widths.iter().sum::<usize>() + sep * n.saturating_sub(1);
    if total(&widths, 3) <= cols {
        return render(Some(&head), &cells, &widths, " | ", "-+-");
    }
    if total(&widths, 1) > cols {
        let head_widths: Vec<usize> = head.iter().map(|h| width(h)).collect();
        let mut overflow = total(&widths, 1) - cols;
        let mut order: Vec<usize> = (0..n).collect();
        order.sort_by_key(|&j| std::cmp::Reverse(widths[j].saturating_sub(head_widths[j])));
        for j in order {
            let diff = widths[j].saturating_sub(head_widths[j]);
            if overflow == 0 || diff == 0 {
                continue;
            }
            let cut = diff.min(overflow);
            widths[j] -= cut;
            overflow -= cut;
        }
    }
    render(Some(&head), &cells, &widths, "|", "+")
}

/// The width of the terminal on stdout; `$COLUMNS` when it cannot be read; else 240.
pub(crate) fn terminal_columns() -> usize {
    // SAFETY: TIOCGWINSZ only writes the `winsize` passed in.
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    if unsafe { libc::ioctl(1, libc::TIOCGWINSZ, &mut ws) } == 0 {
        return ws.ws_col as usize;
    }
    std::env::var("COLUMNS").ok().and_then(|c| c.parse().ok()).unwrap_or(240)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(cells: &[&str]) -> Vec<String> {
        cells.iter().map(|c| c.to_string()).collect()
    }

    #[test]
    fn widths_follow_tsvz_py() {
        for (c, w) in [('A', 1), ('é', 1), ('日', 2), ('\u{301}', 0), ('\u{200b}', 0), ('\u{1160}', 0), ('😀', 2), ('\u{ff21}', 2)] {
            assert_eq!(char_width(c), w, "{c:?}");
        }
        assert_eq!(width("日本a"), 5);
    }

    #[test]
    fn control_characters_are_escaped() {
        assert_eq!(escape_cell("plain"), "plain");
        assert_eq!(escape_cell("a\tb\nc\rd\u{1b}[31m\u{7f}\u{85}\u{202e}\u{2066}"), "a\\tb\\nc\\rd\\x1b[31m\\x7f\\x85\\u202e\\u2066");
        assert_eq!(escape_cell("back\\slash"), "back\\slash");
    }

    #[test]
    fn rows_pad_to_the_widest_and_are_never_shortened() {
        assert_eq!(rows_table(&[v(&["a", "x"]), v(&["bb", "yyy"]), v(&["c"])]), "a  | x  \nbb | yyy\nc  |    \n");
        assert_eq!(rows_table(&[v(&["ab", "c"]), v(&["", ""])]), "ab | c\n---+--\n");
        assert_eq!(rows_table(&[v(&["日本", "x"]), v(&["a", "y"])]), "日本 | x\na    | y\n");
        let long = "z".repeat(500);
        assert_eq!(rows_table(&[v(&["k", &long])]), format!("k | {long}\n"));
        assert_eq!(rows_table(&[]), "");
    }

    #[test]
    fn fields_fit_the_terminal() {
        let rows = [v(&["0", "/very/long/path/name"])];
        let header = ["index", "path"];
        assert_eq!(fields_table(&header, &rows, 80), "index | path                \n------+---------------------\n0     | /very/long/path/name\n");
        assert_eq!(fields_table(&header, &rows, 26), "index|path                \n-----+--------------------\n0    |/very/long/path/name\n");
        assert_eq!(fields_table(&header, &rows, 15), "index|path     \n-----+---------\n0    |/very/l..\n");
    }
}
