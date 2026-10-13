//! Committed lines of a part (§4.2-§4.7, §7.1).

const BOM: &[u8] = b"\xef\xbb\xbf";

/// One committed line of a part.
#[derive(Clone, Copy, Debug)]
pub struct Line<'a> {
    /// The line's bytes including its terminating `\n` (and a `\r` before it): what digests cover (§15.4).
    pub raw: &'a [u8],
    /// 1-based line number in the part; a byte-order mark does not change it (§4.7).
    pub number: u64,
    /// Where the line starts in the part's byte stream (decompressed for a compressed part).
    pub offset: u64,
}

impl<'a> Line<'a> {
    /// The record without its `\n` and without one `\r` before it (§4.2, §7.1).
    pub fn text(&self) -> &'a [u8] {
        let body = &self.raw[..self.raw.len() - 1];
        body.strip_suffix(b"\r").unwrap_or(body)
    }
}

/// Turns a part's bytes into committed lines: one BOM removed at the start (§4.7), lines
/// ended by `\n`, bytes after the last `\n` held as the uncommitted tail (§4.3).
pub struct Framer {
    pending: Vec<u8>,
    bom_decided: bool,
    number: u64,
    consumed: u64,
    committed: u64,
}

impl Default for Framer {
    fn default() -> Self {
        Framer::new()
    }
}

impl Framer {
    /// A framer at the start of a part.
    pub fn new() -> Framer {
        Framer {
            pending: Vec::new(),
            bom_decided: false,
            number: 0,
            consumed: 0,
            committed: 0,
        }
    }

    /// A framer continuing a part at `offset`, just after committed line number `number`.
    pub fn resume(offset: u64, number: u64) -> Framer {
        Framer {
            pending: Vec::new(),
            bom_decided: offset > 0,
            number,
            consumed: offset,
            committed: offset,
        }
    }

    /// Feed the next bytes of the part. `on_line` gets each committed line; returning false
    /// stops the framing, and `push` then returns false.
    pub fn push(&mut self, data: &[u8], on_line: &mut dyn FnMut(&Line<'_>) -> bool) -> bool {
        if self.bom_decided {
            return self.push_decided(data, on_line);
        }
        // The mark is decided once three bytes or a newline are in, as the runner's oracle does.
        self.pending.extend_from_slice(data);
        self.consumed += data.len() as u64;
        if self.pending.len() < 3 && memchr::memchr(b'\n', &self.pending).is_none() {
            return true;
        }
        self.bom_decided = true;
        let mut held = std::mem::take(&mut self.pending);
        self.consumed -= held.len() as u64;
        if held.starts_with(BOM) {
            held.drain(..3);
            self.consumed += 3;
            self.committed += 3;
        }
        self.push_decided(&held, on_line)
    }

    fn push_decided(&mut self, data: &[u8], on_line: &mut dyn FnMut(&Line<'_>) -> bool) -> bool {
        self.consumed += data.len() as u64;
        let mut rest = data;
        if !self.pending.is_empty() {
            let Some(i) = memchr::memchr(b'\n', rest) else {
                self.pending.extend_from_slice(rest);
                return true;
            };
            let mut raw = std::mem::take(&mut self.pending);
            raw.extend_from_slice(&rest[..=i]);
            rest = &rest[i + 1..];
            if !self.emit(&raw, on_line) {
                return false;
            }
        }
        while let Some(i) = memchr::memchr(b'\n', rest) {
            let (line, tail) = rest.split_at(i + 1);
            if !self.emit(line, on_line) {
                return false;
            }
            rest = tail;
        }
        self.pending.extend_from_slice(rest);
        true
    }

    fn emit(&mut self, raw: &[u8], on_line: &mut dyn FnMut(&Line<'_>) -> bool) -> bool {
        self.number += 1;
        let line = Line {
            raw,
            number: self.number,
            offset: self.committed,
        };
        self.committed += raw.len() as u64;
        on_line(&line)
    }

    /// A damaged member ended here (§16.6): drop the unfinished line; a later BOM is data.
    pub fn brk(&mut self) {
        self.pending.clear();
        self.bom_decided = true;
        self.committed = self.consumed;
    }

    /// The uncommitted bytes after the last `\n` so far (§4.3).
    pub fn tail(&self) -> &[u8] {
        &self.pending
    }

    /// The offset just after the last committed line.
    pub fn committed(&self) -> u64 {
        self.committed
    }

    /// The number of the last committed line.
    pub fn number(&self) -> u64 {
        self.number
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// (raw, text, number, offset) of each line.
    type Framed = Vec<(Vec<u8>, Vec<u8>, u64, u64)>;

    fn frame(chunks: &[&[u8]]) -> (Framed, Framer) {
        let mut f = Framer::new();
        let mut lines = Vec::new();
        for c in chunks {
            f.push(c, &mut |l| {
                lines.push((l.raw.to_vec(), l.text().to_vec(), l.number, l.offset));
                true
            });
        }
        (lines, f)
    }

    #[test]
    fn lines_crlf_and_the_tail() {
        let (lines, f) = frame(&[b"a\tx\nb\ty\r\nc\r\r\npartial"]);
        let texts: Vec<&[u8]> = lines.iter().map(|l| l.1.as_slice()).collect();
        assert_eq!(texts, vec![&b"a\tx"[..], b"b\ty", b"c\r"]);
        assert_eq!(lines[1].0, b"b\ty\r\n");
        assert_eq!(
            lines.iter().map(|l| (l.2, l.3)).collect::<Vec<_>>(),
            vec![(1, 0), (2, 4), (3, 9)]
        );
        assert_eq!(f.tail(), b"partial");
        assert_eq!(f.committed(), 13);
        assert_eq!(f.number(), 3);
    }

    #[test]
    fn a_line_split_across_pushes() {
        let (lines, _) = frame(&[b"ab", b"c\td", b"", b"ef\ng", b"h\n"]);
        assert_eq!(
            lines.iter().map(|l| l.0.clone()).collect::<Vec<_>>(),
            vec![b"abc\tdef\n".to_vec(), b"gh\n".to_vec()]
        );
        assert_eq!(lines[1].3, 8);
    }

    #[test]
    fn one_bom_is_removed_at_the_start_only() {
        let (lines, _) = frame(&[b"\xef\xbb\xbfa\tx\n\xef\xbb\xbfb\ty\n"]);
        assert_eq!(lines[0].1, b"a\tx");
        assert_eq!(lines[0].3, 3);
        assert_eq!(lines[1].1, b"\xef\xbb\xbfb\ty");
        let (split, _) = frame(&[b"\xef", b"\xbb", b"\xbfk\tv\n"]);
        assert_eq!(split[0].1, b"k\tv");
        let (only, f) = frame(&[b"\xef\xbb\xbf"]);
        assert!(only.is_empty() && f.tail().is_empty());
    }

    #[test]
    fn a_break_drops_the_unfinished_line_and_decides_the_bom() {
        let mut f = Framer::new();
        let mut got = Vec::new();
        f.push(b"a\tx\nunfin", &mut |l| {
            got.push(l.text().to_vec());
            true
        });
        f.brk();
        f.push(b"\xef\xbb\xbfb\ty\n", &mut |l| {
            got.push(l.text().to_vec());
            true
        });
        assert_eq!(got, vec![b"a\tx".to_vec(), b"\xef\xbb\xbfb\ty".to_vec()]);
        let mut g = Framer::new();
        g.push(b"\xef", &mut |_| true);
        g.brk();
        let mut after = Vec::new();
        g.push(b"\xbb\xbfz\n", &mut |l| {
            after.push(l.text().to_vec());
            true
        });
        assert_eq!(after, vec![b"\xbb\xbfz".to_vec()]);
    }

    #[test]
    fn stopping_early() {
        let mut f = Framer::new();
        let mut n = 0;
        assert!(!f.push(b"a\nb\nc\n", &mut |_| {
            n += 1;
            n < 2
        }));
        assert_eq!(n, 2);
    }

    #[test]
    fn resume_continues_numbers_and_offsets() {
        let mut f = Framer::resume(100, 7);
        let mut got = Vec::new();
        f.push(b"\xef\xbb\xbfk\tv\n", &mut |l| {
            got.push((l.text().to_vec(), l.number, l.offset));
            true
        });
        assert_eq!(got, vec![(b"\xef\xbb\xbfk\tv".to_vec(), 8, 100)]);
        assert_eq!(f.committed(), 107);
    }

    #[test]
    fn a_huge_line_in_small_pieces_is_linear() {
        let piece = vec![b'x'; 16 * 1024];
        let mut f = Framer::new();
        let started = std::time::Instant::now();
        let mut lens = Vec::new();
        for _ in 0..4096 {
            f.push(&piece, &mut |_| true); // 64 MiB without a newline
        }
        f.push(b"\n", &mut |l| {
            lens.push(l.raw.len());
            true
        });
        assert_eq!(lens, vec![64 * 1024 * 1024 + 1]);
        assert!(
            started.elapsed().as_secs() < 10,
            "framing a 64 MiB line took {:?}",
            started.elapsed()
        );
    }
}
