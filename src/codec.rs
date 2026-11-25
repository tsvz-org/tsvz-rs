//! Compressed parts (§16): decoding members, the damaged-member rules of §16.6, and (Task 6) encoding.

use std::borrow::Cow;
use std::io;

use crate::name::Codec;

/// A damaged member's bytes decoded on their own (§16.6 rule 2).
#[derive(Debug, PartialEq, Eq)]
pub enum Alone {
    /// They merely end before the member does (or decode whole): the output is used.
    Short(Vec<u8>),
    /// Decoding failed: the output decoded before the failure, for the corruption policy.
    Corrupt(Vec<u8>),
}

/// A compressed part as the framer takes it (§16.6). `for_each_item` lends each member's output
/// from a buffer it reuses; `items` collects owned copies.
#[derive(Debug, PartialEq, Eq)]
pub enum Item<'a> {
    /// The output of a member that passed its check: all of it, or a page of it from a part
    /// reader that keeps less than the member's output ([`read_items`]).
    Data(Cow<'a, [u8]>),
    /// A member cut short: its output is used, with a warning.
    CutShort { offset: usize, out: Cow<'a, [u8]> },
    /// A corrupt member: the policy in force decides what happens to `out` (§4.10).
    Corrupt { offset: usize, out: Cow<'a, [u8]> },
    /// A damaged member ended here, with another member after it or at the part's end: the
    /// unfinished line before this point is dropped.
    Break,
}

impl Item<'_> {
    /// The item with its bytes owned.
    pub fn into_owned(self) -> Item<'static> {
        match self {
            Item::Data(d) => Item::Data(Cow::Owned(d.into_owned())),
            Item::CutShort { offset, out } => Item::CutShort {
                offset,
                out: Cow::Owned(out.into_owned()),
            },
            Item::Corrupt { offset, out } => Item::Corrupt {
                offset,
                out: Cow::Owned(out.into_owned()),
            },
            Item::Break => Item::Break,
        }
    }
}

/// Where an RFC 1952 header parse is: the optional fields come in the order FEXTRA, FNAME,
/// FCOMMENT, FHCRC.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Stage {
    #[default]
    Fixed,
    XLen(Option<u8>),
    Extra(usize),
    Name,
    Comment,
    Crc(u8),
    Done,
}

/// The optional header fields: (flag bit, the stage that reads the field).
const FIELDS: [(u8, Stage); 4] = [
    (4, Stage::XLen(None)),
    (8, Stage::Name),
    (16, Stage::Comment),
    (2, Stage::Crc(2)),
];

/// An RFC 1952 header parsed a byte at a time, with constant work per byte, so a long or
/// unterminated name or comment costs linear time however the bytes arrive. As zlib does, it
/// rejects reserved flag bits and checks the header CRC when FHCRC is set.
#[derive(Default)]
struct GzipHeader {
    seen: usize,
    flags: u8,
    stage: Stage,
    /// CRC-32 of the header bytes before the FHCRC field.
    crc: crc32fast::Hasher,
    /// The FHCRC field's first byte.
    crc_low: u8,
}

impl GzipHeader {
    /// The stage that reads the first flagged field from `FIELDS[i]` on.
    fn from(&self, i: usize) -> Stage {
        FIELDS[i..]
            .iter()
            .find(|(bit, _)| self.flags & bit != 0)
            .map_or(Stage::Done, |&(_, stage)| stage)
    }

    /// Take one byte: Ok(true) once the header is complete; Err(()) when the bytes are not a gzip
    /// deflate member.
    fn push(&mut self, b: u8) -> Result<bool, ()> {
        self.seen += 1;
        if !matches!(self.stage, Stage::Crc(_) | Stage::Done) {
            self.crc.update(&[b]);
        }
        self.stage = match self.stage {
            Stage::Fixed => {
                let i = self.seen - 1;
                if i < 3 && b != b"\x1f\x8b\x08"[i] {
                    return Err(());
                }
                if i == 3 {
                    if b & 0xe0 != 0 {
                        return Err(()); // reserved flag bits: not a member a gzip writer made
                    }
                    self.flags = b;
                }
                if self.seen == 10 {
                    self.from(0)
                } else {
                    Stage::Fixed
                }
            }
            Stage::XLen(None) => Stage::XLen(Some(b)),
            Stage::XLen(Some(low)) => match low as usize + 256 * b as usize {
                0 => self.from(1),
                n => Stage::Extra(n),
            },
            Stage::Extra(1) => self.from(1),
            Stage::Extra(n) => Stage::Extra(n - 1),
            Stage::Name if b == 0 => self.from(2),
            Stage::Comment if b == 0 => self.from(3),
            Stage::Crc(2) => {
                self.crc_low = b;
                Stage::Crc(1)
            }
            Stage::Crc(_) => {
                if u16::from_le_bytes([self.crc_low, b])
                    != (self.crc.clone().finalize() & 0xffff) as u16
                {
                    return Err(()); // the header CRC does not match
                }
                Stage::Done
            }
            stage => stage,
        };
        Ok(self.stage == Stage::Done)
    }
}

/// The length of the RFC 1952 header at the start of `data`, or None when it is not all there or
/// the bytes are not a gzip deflate member.
pub fn gzip_header_len(data: &[u8]) -> Option<usize> {
    let mut header = GzipHeader::default();
    for (i, &b) in data.iter().enumerate() {
        if header.push(b).ok()? {
            return Some(i + 1);
        }
    }
    None
}

pub(crate) struct GzipDecoder {
    header: GzipHeader,
    /// The raw inflater once the header is complete, and whether its deflate stream ended.
    inflate: Option<(flate2::Decompress, bool)>,
    trailer: Vec<u8>,
    crc: crc32fast::Hasher,
    size: u32,
    done: bool,
}

/// An incremental decoder for one member.
pub(crate) enum Decoder {
    Gzip(Box<GzipDecoder>),
    #[cfg(feature = "bzip2")]
    Bzip2(bzip2::Decompress),
    #[cfg(feature = "xz")]
    Xz(liblzma::stream::Stream),
    #[cfg(feature = "zstd")]
    Zstd(zstd::zstd_safe::DCtx<'static>),
    /// A codec this build does not include: every byte is a data error. `Store::open` refuses
    /// such parts before decoding (§16.5), so this is only a safe default.
    Unsupported,
}

impl Decoder {
    pub(crate) fn new(codec: Codec) -> Decoder {
        match codec {
            Codec::Gzip => Decoder::Gzip(Box::new(GzipDecoder {
                header: GzipHeader::default(),
                inflate: None,
                trailer: Vec::new(),
                crc: crc32fast::Hasher::new(),
                size: 0,
                done: false,
            })),
            #[cfg(feature = "bzip2")]
            Codec::Bzip2 => Decoder::Bzip2(bzip2::Decompress::new(false)),
            #[cfg(feature = "xz")]
            // At most 1 GiB of decoder memory: far above any xz or lzma preset (64 MiB of
            // dictionary at most), so only a member whose header demands more is refused, as
            // damaged, instead of reserving up to 4 GiB for a few bytes.
            Codec::Xz => match liblzma::stream::Stream::new_auto_decoder(1 << 30, 0) {
                Ok(s) => Decoder::Xz(s),
                Err(_) => Decoder::Unsupported,
            },
            #[cfg(feature = "zstd")]
            Codec::Zstd => Decoder::Zstd(zstd::zstd_safe::DCtx::create()),
            #[allow(unreachable_patterns)]
            _ => Decoder::Unsupported,
        }
    }

    /// Feed `input`, appending output to `out`. Ok((bytes consumed, member ended and checked))
    /// or Err(()) on a data error. Output decoded before an error stays in `out`.
    pub(crate) fn feed(&mut self, input: &[u8], out: &mut Vec<u8>) -> Result<(usize, bool), ()> {
        match self {
            Decoder::Gzip(g) => g.feed(input, out),
            #[cfg(feature = "bzip2")]
            Decoder::Bzip2(d) => {
                let before = d.total_in();
                loop {
                    out.reserve(64 * 1024);
                    let status = d
                        .decompress_vec(&input[(d.total_in() - before) as usize..], out)
                        .map_err(|_| ())?;
                    let used = (d.total_in() - before) as usize;
                    if status == bzip2::Status::StreamEnd {
                        return Ok((used, true));
                    }
                    if used >= input.len() && out.len() < out.capacity() {
                        return Ok((used, false));
                    }
                }
            }
            #[cfg(feature = "xz")]
            Decoder::Xz(s) => {
                let before = s.total_in();
                loop {
                    out.reserve(64 * 1024);
                    let used = (s.total_in() - before) as usize;
                    let status = s
                        .process_vec(&input[used..], out, liblzma::stream::Action::Run)
                        .map_err(|_| ())?;
                    let used = (s.total_in() - before) as usize;
                    if status == liblzma::stream::Status::StreamEnd {
                        return Ok((used, true));
                    }
                    if used >= input.len() && out.len() < out.capacity() {
                        return Ok((used, false));
                    }
                }
            }
            #[cfg(feature = "zstd")]
            Decoder::Zstd(d) => {
                let mut src = zstd::zstd_safe::InBuffer::around(input);
                loop {
                    out.reserve(64 * 1024);
                    let start = out.len();
                    let hint = {
                        let mut dst = zstd::zstd_safe::OutBuffer::around_pos(out, start);
                        d.decompress_stream(&mut dst, &mut src).map_err(|_| ())?
                    };
                    if hint == 0 {
                        return Ok((src.pos(), true));
                    }
                    if src.pos() >= input.len() && out.len() < out.capacity() {
                        return Ok((src.pos(), false));
                    }
                }
            }
            Decoder::Unsupported => Err(()),
        }
    }
}

impl GzipDecoder {
    fn feed(&mut self, input: &[u8], out: &mut Vec<u8>) -> Result<(usize, bool), ()> {
        let mut used = 0;
        if self.done {
            return Ok((0, true));
        }
        if self.inflate.is_none() {
            loop {
                let Some(&b) = input.get(used) else {
                    return Ok((used, false));
                };
                used += 1;
                if self.header.push(b)? {
                    break;
                }
            }
            self.inflate = Some((flate2::Decompress::new(false), false));
        }
        let (z, ended) = self.inflate.as_mut().expect("set above");
        while !*ended && used < input.len() {
            let before_in = z.total_in();
            let before_out = out.len();
            out.reserve(64 * 1024);
            let status = z
                .decompress_vec(&input[used..], out, flate2::FlushDecompress::None)
                .map_err(|_| ())?;
            used += (z.total_in() - before_in) as usize;
            self.crc.update(&out[before_out..]);
            self.size = self.size.wrapping_add((out.len() - before_out) as u32);
            if status == flate2::Status::StreamEnd {
                *ended = true;
            } else if z.total_in() == before_in && out.len() == before_out {
                break;
            }
        }
        if !*ended {
            return Ok((used, false));
        }
        while self.trailer.len() < 8 && used < input.len() {
            self.trailer.push(input[used]);
            used += 1;
        }
        if self.trailer.len() < 8 {
            return Ok((used, false));
        }
        let crc = u32::from_le_bytes(self.trailer[..4].try_into().expect("4 bytes"));
        let size = u32::from_le_bytes(self.trailer[4..].try_into().expect("4 bytes"));
        if crc != self.crc.clone().finalize() || size != self.size {
            return Err(());
        }
        self.done = true;
        Ok((used, true))
    }
}

#[cfg(test)]
enum Judged {
    Complete(usize),
    Short,
    Bad,
}

/// Decode the member at the start of `data` into `out`, which is cleared first.
#[cfg(test)]
fn member_into(codec: Codec, data: &[u8], out: &mut Vec<u8>) -> Judged {
    out.clear();
    match Decoder::new(codec).feed(data, out) {
        Err(()) => Judged::Bad,
        Ok((end, true)) => Judged::Complete(end),
        Ok((_, false)) => Judged::Short,
    }
}

/// Decode a damaged member's bytes on their own (§16.6 rule 2). They are fed in large steps;
/// when a step fails, they are decoded again with that step fed a byte at a time, so nothing
/// decoded before the failing byte is lost with the call that fails (the runner's oracle feeds
/// every byte alone; decoders are deterministic, so the result is the same in linear time).
/// What the failing byte itself decodes is dropped with it, as the oracle's decoders raise
/// without returning it.
pub fn decode_alone(codec: Codec, data: &[u8]) -> Alone {
    const STEP: usize = 1 << 16;
    let mut d = Decoder::new(codec);
    let mut out = Vec::new();
    let mut failed = None;
    for (i, chunk) in data.chunks(STEP).enumerate() {
        match d.feed(chunk, &mut out) {
            Err(()) => {
                failed = Some(i);
                break;
            }
            Ok((_, true)) => return Alone::Short(out),
            Ok(_) => {}
        }
    }
    let Some(step) = failed else {
        return Alone::Short(out);
    };
    let (before, failing) = data.split_at(step * STEP);
    let mut d = Decoder::new(codec);
    let mut out = Vec::new();
    for piece in before
        .chunks(STEP)
        .chain(failing[..failing.len().min(STEP)].chunks(1))
    {
        let kept = out.len();
        match d.feed(piece, &mut out) {
            Err(()) => {
                out.truncate(kept);
                return Alone::Corrupt(out);
            }
            Ok((_, true)) => return Alone::Short(out),
            Ok(_) => {}
        }
    }
    Alone::Short(out)
}

/// Feed `input` to a raw inflater and discard the output: Ok(Some(bytes consumed)) when the
/// deflate stream ended within `input`, Ok(None) when it needs more, Err on a data error.
fn inflate_to_end(
    z: &mut flate2::Decompress,
    input: &[u8],
    scratch: &mut [u8],
) -> Result<Option<usize>, ()> {
    let mut pos = 0;
    loop {
        let (in0, out0) = (z.total_in(), z.total_out());
        let status = z
            .decompress(&input[pos..], scratch, flate2::FlushDecompress::None)
            .map_err(|_| ())?;
        pos += (z.total_in() - in0) as usize;
        if status == flate2::Status::StreamEnd {
            return Ok(Some(pos));
        }
        let produced = (z.total_out() - out0) as usize;
        if (pos >= input.len() && produced < scratch.len())
            || (z.total_in() == in0 && produced == 0)
        {
            return Ok(None);
        }
    }
}

/// How many bytes of a damaged gzip member's region belong to it (§16.6 rule 2): zero bytes
/// ending the region are padding, except those its compressed data needs to reach its end
/// and those that complete an 8-byte trailer it has begun. Mirrors `_gzip_unpad` in the runner.
pub fn gzip_keep(region: &[u8]) -> usize {
    let stripped = region.iter().rposition(|&b| b != 0).map_or(0, |i| i + 1);
    let zeros = region.len() - stripped;
    if zeros == 0 {
        return region.len();
    }
    let start = match gzip_header_len(region) {
        Some(start) if start <= stripped => start,
        _ => return stripped,
    };
    let mut z = flate2::Decompress::new(false);
    let mut scratch = vec![0u8; 1 << 16];
    match inflate_to_end(&mut z, &region[start..stripped], &mut scratch) {
        Err(()) => stripped,
        Ok(Some(used)) => {
            let begun = stripped - start - used; // trailer bytes before the zeros
            if begun > 0 && begun < 8 {
                stripped + (8 - begun).min(zeros)
            } else {
                stripped
            }
        }
        Ok(None) => {
            // Fed only zeros, deflate data in a Huffman block ends within a few hundred bytes, fails or never
            // ends; in a stored block it ends only if the block is final, after the rest of its NLEN (both
            // bytes zero only when LEN is 65535) and its data: at most 2 + 65535 zeros.
            let zeros = zeros.min(2 + 65535);
            let pad = vec![0u8; 1 << 16];
            let mut fed = 0;
            while fed < zeros {
                let n = (zeros - fed).min(pad.len());
                match inflate_to_end(&mut z, &pad[..n], &mut scratch) {
                    Err(()) => return stripped,
                    Ok(Some(used)) => return stripped + fed + used,
                    Ok(None) => fed += n,
                }
            }
            stripped
        }
    }
}

/// Where a compressed part's bytes are read from: at an offset, so that a member can be read
/// again from its start (a file, or bytes in memory).
pub(crate) trait Source {
    /// Read into `buf` from `offset`: how many bytes were read, 0 at the end.
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize>;
}

impl Source for [u8] {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        let rest = self.get(offset as usize..).unwrap_or_default();
        let n = rest.len().min(buf.len());
        buf[..n].copy_from_slice(&rest[..n]);
        Ok(n)
    }
}

impl Source for std::fs::File {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        loop {
            match std::os::unix::fs::FileExt::read_at(self, buf, offset) {
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                other => return other,
            }
        }
    }
}

/// How many compressed bytes are read at a time: what `decode_alone` feeds a decoder in one call.
const PAGE: usize = 1 << 16;

/// How the member at a position ended, judged by decoding it.
enum Verdict {
    /// It passed its check: this many bytes long, and whether its output was all kept.
    Complete(u64, bool),
    /// It is damaged: cut short, or not valid (§16.6 rule 2).
    Damaged,
}

/// The items of the compressed part read from `src` (§16.6), one at a time: `each` gets every item
/// as it is found and returns false to stop. Mirrors `decompress_items` in the runner, but reads
/// the part a page at a time and holds one member's output at a time, in a buffer reused from
/// member to member, so a part costs its largest member's output, not the part. A member whose
/// output passes `keep` bytes is judged without keeping it, and once it has passed its check it is
/// decoded again and given a page at a time (several `Item::Data`): a read that stops early, at a
/// part's first lines, then costs the time of decoding the member to its check and holds about
/// `keep` whatever the member's size, plus what one input page decodes to before the next check
/// (a decoder takes the whole page): by the formats' best ratios up to 64 MiB for deflate (1032 to
/// one) and 2 GiB for zstd (a 128 KiB run-length block in 4 bytes), which only crafted input
/// reaches (docs/limitations.md). Only a damaged member's bytes, which run to the next member
/// start (rule 2), are held whole.
pub(crate) fn read_items<S: Source + ?Sized>(
    codec: Codec,
    src: &S,
    keep: usize,
    each: &mut dyn FnMut(Item<'_>) -> bool,
) -> io::Result<()> {
    let mut page = vec![0u8; PAGE];
    let mut buf = Vec::new();
    let mut pos = 0u64;
    loop {
        if codec == Codec::Gzip {
            pos = past_zeros(src, pos, &mut page)?; // padding before a member
        }
        let verdict = match judge(codec, src, pos, keep, &mut page, &mut buf)? {
            None => return Ok(()), // the part ends here
            Some(v) => v,
        };
        if let Verdict::Complete(len, kept) = verdict {
            if kept {
                if !each(Item::Data(Cow::Borrowed(&buf))) {
                    return Ok(());
                }
            } else {
                match redecode(codec, src, pos, len, &mut page, &mut buf, each)? {
                    Redone::Whole => {}
                    Redone::Stopped => return Ok(()),
                    // The bytes ended sooner than they did a moment ago (a failed batch cut back
                    // from the active part, which a reader holds no lock against): what was given
                    // is what is left of the member, and its unfinished line goes.
                    Redone::Cut => {
                        let cut = Item::CutShort {
                            offset: pos as usize,
                            out: Cow::Borrowed(&[]),
                        };
                        if !each(cut) || !each(Item::Break) {
                            return Ok(());
                        }
                    }
                }
            }
            pos += len.max(1);
            continue;
        }
        // A damaged member's bytes run to the next member start, complete or not (rule 2).
        let following = find_magic(src, pos + 1, codec.magic(), &mut page)?;
        let mut region = Vec::new();
        let end = following.unwrap_or(u64::MAX);
        let mut at = pos;
        while at < end {
            let want = PAGE.min((end - at).try_into().unwrap_or(PAGE));
            let n = src.read_at(&mut page[..want], at)?;
            if n == 0 {
                break;
            }
            region.extend_from_slice(&page[..n]);
            at += n as u64;
        }
        if codec == Codec::Gzip {
            region.truncate(gzip_keep(&region));
        }
        let offset = pos as usize;
        let item = match decode_alone(codec, &region) {
            Alone::Short(out) => Item::CutShort {
                offset,
                out: Cow::Owned(out),
            },
            Alone::Corrupt(out) => Item::Corrupt {
                offset,
                out: Cow::Owned(out),
            },
        };
        if !each(item) {
            return Ok(());
        }
        // The member's unfinished last line goes with it (a break after it, where another member
        // follows or not).
        if !each(Item::Break) {
            return Ok(());
        }
        match following {
            Some(next) => pos = next,
            None => return Ok(()),
        }
    }
}

/// The first position from `pos` that holds no zero byte.
fn past_zeros<S: Source + ?Sized>(src: &S, mut pos: u64, page: &mut [u8]) -> io::Result<u64> {
    loop {
        let n = src.read_at(page, pos)?;
        let zeros = page[..n].iter().take_while(|&&b| b == 0).count();
        pos += zeros as u64;
        if zeros < n || n == 0 {
            return Ok(pos);
        }
    }
}

/// Decode the member at `pos` to judge it, its output in `out` while it is no longer than `keep`
/// bytes; None when the part ends at `pos`.
fn judge<S: Source + ?Sized>(
    codec: Codec,
    src: &S,
    pos: u64,
    keep: usize,
    page: &mut [u8],
    out: &mut Vec<u8>,
) -> io::Result<Option<Verdict>> {
    let mut decoder = Decoder::new(codec);
    out.clear();
    let mut kept = true;
    let mut at = pos;
    loop {
        let n = src.read_at(page, at)?;
        if n == 0 {
            return Ok((at > pos).then_some(Verdict::Damaged)); // cut short, or the end
        }
        match decoder.feed(&page[..n], out) {
            Err(()) => return Ok(Some(Verdict::Damaged)),
            Ok((used, true)) => return Ok(Some(Verdict::Complete(at + used as u64 - pos, kept))),
            // A decoder that takes less than it was given and has not ended takes no more.
            Ok((used, false)) if used < n => return Ok(Some(Verdict::Damaged)),
            Ok(_) => at += n as u64,
        }
        if out.len() > keep {
            kept = false;
            out.clear();
        }
    }
}

/// How [`redecode`] ended.
enum Redone {
    /// The member decoded to its end again.
    Whole,
    /// `each` stopped.
    Stopped,
    /// The bytes no longer decode as they did: the part was rewritten meanwhile.
    Cut,
}

/// Decode again the member at `pos`, `len` bytes that passed their check, giving its output to
/// `each` a page at a time.
fn redecode<S: Source + ?Sized>(
    codec: Codec,
    src: &S,
    pos: u64,
    len: u64,
    page: &mut [u8],
    out: &mut Vec<u8>,
    each: &mut dyn FnMut(Item<'_>) -> bool,
) -> io::Result<Redone> {
    let mut decoder = Decoder::new(codec);
    let mut at = pos;
    while at < pos + len {
        let want = (pos + len - at).min(page.len() as u64) as usize;
        let n = src.read_at(&mut page[..want], at)?;
        out.clear();
        let fed = decoder.feed(&page[..n], out);
        if !out.is_empty() && !each(Item::Data(Cow::Borrowed(out))) {
            return Ok(Redone::Stopped);
        }
        match fed {
            Ok((used, false)) if n > 0 && used == n => at += n as u64,
            Ok((_, true)) => return Ok(Redone::Whole),
            _ => return Ok(Redone::Cut),
        }
    }
    Ok(if len == 0 { Redone::Whole } else { Redone::Cut })
}

/// Where `magic` next begins at or after `from`, read a page at a time.
fn find_magic<S: Source + ?Sized>(
    src: &S,
    from: u64,
    magic: &[u8],
    page: &mut [u8],
) -> io::Result<Option<u64>> {
    let overlap = magic.len() - 1;
    let mut at = from;
    loop {
        let n = src.read_at(page, at)?;
        if let Some(i) = memchr::memmem::find(&page[..n], magic) {
            return Ok(Some(at + i as u64));
        }
        if n <= overlap {
            return Ok(None);
        }
        at += (n - overlap) as u64;
    }
}

/// A compressed part's bytes as items for the framer (§16.6), one at a time: `each` gets every
/// item as it is found and returns false to stop. Every member's output is one `Item::Data`.
pub fn for_each_item(codec: Codec, data: &[u8], each: &mut dyn FnMut(Item<'_>) -> bool) {
    read_items(codec, data, usize::MAX, each).expect("bytes in memory read without error");
}

/// All the items of a compressed part, owned (`for_each_item` streams them instead).
pub fn items(codec: Codec, data: &[u8]) -> Vec<Item<'static>> {
    let mut all = Vec::new();
    for_each_item(codec, data, &mut |item| {
        all.push(item.into_owned());
        true
    });
    all
}

/// One member holding `data`, or None when this build does not include the codec.
/// gzip members have a fixed header (no name, mtime 0, OS 255); zstd frames carry the content
/// checksum (§16.8).
pub fn encode_member(codec: Codec, data: &[u8]) -> Option<Vec<u8>> {
    use std::io::Write;
    match codec {
        Codec::Gzip => {
            let mut out = b"\x1f\x8b\x08\x00\x00\x00\x00\x00\x00\xff".to_vec();
            let mut e = flate2::write::DeflateEncoder::new(
                out.split_off(10),
                flate2::Compression::default(),
            );
            e.write_all(data).ok()?;
            out.extend(e.finish().ok()?);
            out.extend(crc32fast::hash(data).to_le_bytes());
            out.extend((data.len() as u32).to_le_bytes());
            Some(out)
        }
        #[cfg(feature = "bzip2")]
        Codec::Bzip2 => {
            let mut e = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::default());
            e.write_all(data).ok()?;
            e.finish().ok()
        }
        #[cfg(feature = "xz")]
        Codec::Xz => {
            let mut e = liblzma::write::XzEncoder::new(Vec::new(), 6);
            e.write_all(data).ok()?;
            e.finish().ok()
        }
        #[cfg(feature = "zstd")]
        Codec::Zstd => {
            let mut c = zstd::bulk::Compressor::new(3).ok()?;
            c.set_parameter(zstd::zstd_safe::CParameter::ChecksumFlag(true))
                .ok()?;
            c.compress(data).ok()
        }
        #[allow(unreachable_patterns)]
        _ => None,
    }
}

/// `payload` (whole records ending in `\n`) as members of at most `member_size` uncompressed
/// bytes each, ending at record boundaries; a single longer record is a member of its own
/// (§16.7). None when this build does not include the codec.
pub fn encode_members(codec: Codec, payload: &[u8], member_size: usize) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut start = 0;
    while start < payload.len() {
        let mut end = start;
        loop {
            let next =
                memchr::memchr(b'\n', &payload[end..]).map_or(payload.len(), |i| end + i + 1);
            if end > start && next - start > member_size {
                break;
            }
            end = next;
            if end >= payload.len() {
                break;
            }
        }
        out.extend(encode_member(codec, &payload[start..end])?);
        start = end;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// How the member at the start of some bytes decoded (§16.6 rule 1).
    #[derive(Debug, PartialEq, Eq)]
    enum Member {
        /// Decoded to its end and passed its check; `end` is its length in bytes.
        Complete { out: Vec<u8>, end: usize },
        /// The bytes ended before the member did.
        Short,
        /// The bytes are not a valid member.
        Bad,
    }

    /// Decode the member at the start of `data` (§16.6 rule 1).
    fn member(codec: Codec, data: &[u8]) -> Member {
        let mut out = Vec::new();
        match member_into(codec, data, &mut out) {
            Judged::Bad => Member::Bad,
            Judged::Complete(end) => Member::Complete { out, end },
            Judged::Short => Member::Short,
        }
    }

    /// A member built by the codec crates' own encoders, so decoding is tested independently of Task 6.
    fn fixture(codec: Codec, data: &[u8]) -> Vec<u8> {
        match codec {
            Codec::Gzip => {
                let mut e =
                    flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
                e.write_all(data).unwrap();
                e.finish().unwrap()
            }
            #[cfg(feature = "bzip2")]
            Codec::Bzip2 => {
                let mut e = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::default());
                e.write_all(data).unwrap();
                e.finish().unwrap()
            }
            #[cfg(feature = "xz")]
            Codec::Xz => {
                let mut e = liblzma::write::XzEncoder::new(Vec::new(), 6);
                e.write_all(data).unwrap();
                e.finish().unwrap()
            }
            #[cfg(feature = "zstd")]
            Codec::Zstd => {
                let mut c = zstd::bulk::Compressor::new(3).unwrap();
                c.set_parameter(zstd::zstd_safe::CParameter::ChecksumFlag(true))
                    .unwrap();
                c.compress(data).unwrap()
            }
            #[allow(unreachable_patterns)]
            _ => unreachable!("tests only use codecs this build has"),
        }
    }

    fn codecs() -> Vec<Codec> {
        [Codec::Gzip, Codec::Bzip2, Codec::Xz, Codec::Zstd]
            .into_iter()
            .filter(|c| c.supported())
            .collect()
    }

    #[test]
    fn complete_short_and_bad_members() {
        for codec in codecs() {
            let data = b"a\tx\nb\ty\n".repeat(50);
            let mut two = fixture(codec, &data);
            let first = two.len();
            two.extend(fixture(codec, b"c\tz\n"));
            assert_eq!(
                member(codec, &two),
                Member::Complete {
                    out: data.clone(),
                    end: first
                },
                "{codec:?}"
            );
            assert_eq!(member(codec, &two[..first - 3]), Member::Short, "{codec:?}");
            let mut bad = fixture(codec, &data);
            let n = bad.len();
            bad[n / 2] ^= 0xff;
            assert_eq!(member(codec, &bad), Member::Bad, "{codec:?}");
            assert!(
                matches!(decode_alone(codec, &bad), Alone::Corrupt(_)),
                "{codec:?}"
            );
            assert!(
                matches!(decode_alone(codec, &two[..first - 3]), Alone::Short(_)),
                "{codec:?}"
            );
            assert_eq!(member(codec, b"garbage!garbage!"), Member::Bad, "{codec:?}");
        }
    }

    #[test]
    fn a_failed_gzip_check_is_corrupt_with_its_output() {
        let mut g = fixture(Codec::Gzip, b"hello\n");
        let n = g.len();
        g[n - 8] ^= 1; // the stored CRC-32
        assert_eq!(member(Codec::Gzip, &g), Member::Bad);
        assert_eq!(
            decode_alone(Codec::Gzip, &g),
            Alone::Corrupt(b"hello\n".to_vec())
        );
        let mut l = fixture(Codec::Gzip, b"hello\n");
        l[n - 4] ^= 1; // the stored length
        assert_eq!(member(Codec::Gzip, &l), Member::Bad);
    }

    #[cfg(feature = "zstd")]
    #[test]
    fn a_failed_zstd_checksum_is_bad() {
        let mut z = fixture(Codec::Zstd, b"hello world hello world\n");
        let n = z.len();
        z[n - 1] ^= 1;
        assert_eq!(member(Codec::Zstd, &z), Member::Bad);
    }

    #[test]
    fn gzip_header_lengths() {
        assert_eq!(gzip_header_len(b"\x1f\x8b\x08\x00\0\0\0\0\0\xff"), Some(10));
        assert_eq!(
            gzip_header_len(b"\x1f\x8b\x08\x08\0\0\0\0\0\xffname\0rest"),
            Some(15)
        );
        assert_eq!(gzip_header_len(b"\x1f\x8b\x08\x08\0\0\0\0\0\xffname"), None);
        assert_eq!(
            gzip_header_len(b"\x1f\x8b\x08\x04\0\0\0\0\0\xff\x02\0ab"),
            Some(14)
        );
        assert_eq!(gzip_header_len(b"\x1f\x8b\x09\x00\0\0\0\0\0\xff"), None);
    }

    #[test]
    fn items_of_a_healthy_multi_member_part() {
        for codec in codecs() {
            let mut part = fixture(codec, b"a\tx\n");
            part.extend(fixture(codec, b"b\ty\n"));
            assert_eq!(
                items(codec, &part),
                vec![
                    Item::Data(b"a\tx\n".to_vec().into()),
                    Item::Data(b"b\ty\n".to_vec().into())
                ],
                "{codec:?}"
            );
        }
    }

    #[test]
    fn zero_padding_between_gzip_members_is_skipped() {
        let mut part = vec![0u8; 7];
        part.extend(fixture(Codec::Gzip, b"a\tx\n"));
        part.extend([0u8; 100]);
        part.extend(fixture(Codec::Gzip, b"b\ty\n"));
        part.extend([0u8; 3]);
        assert_eq!(
            items(Codec::Gzip, &part),
            vec![
                Item::Data(b"a\tx\n".to_vec().into()),
                Item::Data(b"b\ty\n".to_vec().into())
            ]
        );
    }

    #[test]
    fn a_cut_short_member_before_another() {
        for codec in codecs() {
            let first = fixture(codec, &b"k\tvalue\n".repeat(200));
            let mut part = first[..first.len() / 2].to_vec();
            part.extend(fixture(codec, b"z\t1\n"));
            let got = items(codec, &part);
            assert_eq!(got.len(), 3, "{codec:?}: {got:?}");
            assert!(
                matches!(&got[0], Item::CutShort { offset: 0, .. }),
                "{codec:?}"
            );
            assert_eq!(got[1], Item::Break);
            assert_eq!(got[2], Item::Data(b"z\t1\n".to_vec().into()));
        }
    }

    /// Bytes in memory read through a part reader, counting how far it read.
    struct Counting<'a> {
        data: &'a [u8],
        reached: std::cell::Cell<u64>,
    }

    impl Source for Counting<'_> {
        fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
            let n = self.data.read_at(buf, offset)?;
            self.reached.set(self.reached.get().max(offset + n as u64));
            Ok(n)
        }
    }

    /// A part that shrinks once its first member has passed its check: from the `shrink_at`th read
    /// from its start on, that member has only its first `cut` bytes (a failed batch cut back from
    /// the active part while a reader holds no lock).
    struct Shrinking<'a> {
        data: &'a [u8],
        first: usize,
        cut: usize,
        shrink_at: usize,
        starts: std::cell::Cell<usize>,
    }

    impl Source for Shrinking<'_> {
        fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
            if offset == 0 {
                self.starts.set(self.starts.get() + 1);
            }
            let shrunk = self.starts.get() >= self.shrink_at && (offset as usize) < self.first;
            self.data[..if shrunk { self.cut } else { self.data.len() }].read_at(buf, offset)
        }
    }

    #[test]
    fn a_damaged_member_ends_with_a_break_also_when_it_is_the_last() {
        // Its unfinished last line goes: nothing follows to join it to, and a reader must not
        // take it for a line of its own.
        for codec in codecs() {
            let whole = fixture(codec, &b"k\tvalue\n".repeat(200));
            let got = items(codec, &whole[..whole.len() / 2]);
            assert_eq!(got.len(), 2, "{codec:?}: {got:?}");
            assert!(
                matches!(&got[0], Item::CutShort { offset: 0, .. }),
                "{codec:?}"
            );
            assert_eq!(got[1], Item::Break, "{codec:?}");
        }
    }

    #[test]
    fn a_member_that_shrinks_between_its_two_passes_is_cut_short_and_ends_with_a_break() {
        // Judged complete, then given again a page at a time: bytes that end sooner are reported,
        // and the member's unfinished line goes, as for any damaged member; the next member is
        // read as it was.
        for codec in codecs() {
            // longer than a page of compressed bytes and than a bzip2 block, so that it is judged as it is read
            let first = fixture(codec, &noise(50_000));
            assert!(first.len() > PAGE, "{codec:?}");
            let part = [first.clone(), fixture(codec, b"z\t1\n")].concat();
            let src = Shrinking {
                data: &part,
                first: first.len(),
                cut: first.len() / 2,
                // the reads from the start: gzip looks for padding, the judging, the second pass
                shrink_at: if codec == Codec::Gzip { 3 } else { 2 },
                starts: 0.into(),
            };
            let mut got = Vec::new();
            read_items(codec, &src, 100, &mut |item| {
                got.push(item.into_owned());
                true
            })
            .unwrap();
            let kinds: Vec<String> = got
                .iter()
                .map(|i| match i {
                    Item::Data(d) => format!("Data({})", d.len()),
                    Item::CutShort { offset, out } => format!("CutShort({offset}, {})", out.len()),
                    Item::Corrupt { offset, out } => format!("Corrupt({offset}, {})", out.len()),
                    Item::Break => "Break".into(),
                })
                .collect();
            assert!(got.len() >= 3, "{codec:?}: {kinds:?}");
            let n = got.len();
            let cut = Item::CutShort {
                offset: 0,
                out: Vec::new().into(),
            };
            assert_eq!(
                got[n - 3..],
                [cut, Item::Break, Item::Data(b"z\t1\n".to_vec().into())],
                "{codec:?}: {kinds:?}"
            );
        }
    }

    /// Rows of pseudo-random hex, which compress little.
    fn noise(n: usize) -> Vec<u8> {
        let mut x: u64 = 1;
        (0..n)
            .flat_map(|i| {
                x = x
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                format!("k{i}\t{x:016x}\n").into_bytes()
            })
            .collect()
    }

    /// `items` with each run of `Item::Data` joined into one.
    fn glue(items: Vec<Item<'static>>) -> Vec<Item<'static>> {
        let mut out: Vec<Item<'static>> = Vec::new();
        for item in items {
            match (item, out.last_mut()) {
                (Item::Data(d), Some(Item::Data(last))) => last.to_mut().extend_from_slice(&d),
                (item, _) => out.push(item),
            }
        }
        out
    }

    /// Every item a part reader keeping `keep` bytes of a member gives.
    fn read_all(codec: Codec, data: &[u8], keep: usize) -> Vec<Item<'static>> {
        let mut all = Vec::new();
        read_items(codec, data, keep, &mut |item| {
            all.push(item.into_owned());
            true
        })
        .unwrap();
        all
    }

    #[test]
    fn a_member_longer_than_what_is_kept_is_given_in_pieces_once_it_passed_its_check() {
        for codec in codecs() {
            // more than one bzip2 block (900 kB), which gives its output whole
            let big = noise(50_000);
            let part = [
                fixture(codec, b"a\tx\n"),
                fixture(codec, &big),
                fixture(codec, b"z\t1\n"),
            ]
            .concat();
            assert_eq!(items(codec, &part).len(), 3, "{codec:?}");
            let got = read_all(codec, &part, 1000);
            assert!(got.len() > 3, "{codec:?}: {} items", got.len());
            let data: Vec<u8> = got
                .iter()
                .flat_map(|i| match i {
                    Item::Data(d) => d.to_vec(),
                    other => panic!("{codec:?}: {other:?}"),
                })
                .collect();
            assert_eq!(
                data,
                [&b"a\tx\n"[..], &big, b"z\t1\n"].concat(),
                "{codec:?}"
            );
        }
    }

    #[test]
    fn what_is_kept_changes_no_item_of_a_damaged_part() {
        // The damage of §16.6 is judged the same whatever part of a member is held: a member cut
        // short before another, a corrupt one in the middle, a cut tail, gzip's zero padding.
        for codec in codecs() {
            let big = noise(5_000);
            let a = fixture(codec, &big);
            let mut corrupt = fixture(codec, &big);
            let n = corrupt.len();
            if codec == Codec::Gzip {
                corrupt[n - 8] ^= 1
            } else {
                corrupt[n / 2] ^= 0xff
            }
            let z = fixture(codec, b"z\t1\n");
            let mut parts = vec![
                [&a[..a.len() / 2], &z[..]].concat(),
                [&a[..], &corrupt, &z].concat(),
                [&a[..], &z, &a[..a.len() - 5]].concat(),
                Vec::new(),
            ];
            if codec == Codec::Gzip {
                parts.push([&a[..], &[0u8; 70_000], &z].concat());
            }
            for (i, part) in parts.iter().enumerate() {
                let want = glue(items(codec, part));
                for keep in [0, 100, 70_000] {
                    assert_eq!(
                        glue(read_all(codec, part, keep)),
                        want,
                        "{codec:?} part {i} keep {keep}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_read_that_stops_at_the_first_member_reads_no_further_than_it_needs() {
        for codec in codecs() {
            let first = fixture(codec, b"a\tx\n");
            let rest = fixture(codec, &noise(50_000));
            assert!(rest.len() > 4 * PAGE, "{codec:?}");
            let part = [&first[..], &rest].concat();
            let src = Counting {
                data: &part,
                reached: std::cell::Cell::new(0),
            };
            let mut seen = 0;
            read_items(codec, &src, usize::MAX, &mut |_| {
                seen += 1;
                false
            })
            .unwrap();
            assert_eq!(seen, 1, "{codec:?}");
            assert!(
                src.reached.get() <= (first.len() + PAGE) as u64,
                "{codec:?}: read {} of {} bytes",
                src.reached.get(),
                part.len()
            );
        }
    }

    #[test]
    fn a_corrupt_member_in_the_middle() {
        for codec in codecs() {
            let a = fixture(codec, b"a\tx\n");
            let mut b = fixture(codec, &b"b\ty\n".repeat(100));
            let n = b.len();
            // gzip: break the stored CRC-32, a sure check failure; the others: a byte in the middle.
            if codec == Codec::Gzip {
                b[n - 8] ^= 1
            } else {
                b[n / 2] ^= 0xff
            }
            let mut part = a.clone();
            part.extend(&b);
            part.extend(fixture(codec, b"c\tz\n"));
            let got = items(codec, &part);
            assert_eq!(got[0], Item::Data(b"a\tx\n".to_vec().into()), "{codec:?}");
            assert!(
                matches!(&got[1], Item::Corrupt { offset, .. } if *offset == a.len()),
                "{codec:?}: {got:?}"
            );
            assert_eq!(got[2], Item::Break, "{codec:?}");
            assert_eq!(got[3], Item::Data(b"c\tz\n".to_vec().into()), "{codec:?}");
        }
    }

    #[test]
    fn gzip_padding_after_an_unfinished_header_is_not_its_own() {
        // The header's own zero bytes run into the padding: its end lies past the last non-zero byte.
        let mut region = b"\x1f\x8b\x08".to_vec();
        region.extend([0u8; 40]);
        assert_eq!(gzip_keep(&region), 3);
        assert_eq!(gzip_keep(b"\x1f\x8b\x08\x00\x01"), 5); // no trailing zeros: all of it
    }

    #[test]
    fn gzip_zeros_that_complete_a_begun_trailer_are_its_own() {
        let full = fixture(Codec::Gzip, b"hello\n");
        let cut = full.len() - 5; // the deflate data and 3 trailer bytes
        assert!(
            full[cut - 1] != 0,
            "fixture precondition: the last kept byte is not zero"
        );
        let mut region = full[..cut].to_vec();
        region.extend([0u8; 10]);
        assert_eq!(gzip_keep(&region), cut + 5);
    }

    #[test]
    fn encoded_members_decode_back() {
        for codec in codecs() {
            let data = b"a\tx\nb\ty\n";
            let m = encode_member(codec, data).unwrap();
            assert_eq!(
                member(codec, &m),
                Member::Complete {
                    out: data.to_vec(),
                    end: m.len()
                },
                "{codec:?}"
            );
        }
    }

    #[test]
    fn gzip_members_have_a_fixed_header() {
        let m = encode_member(Codec::Gzip, b"x\n").unwrap();
        assert_eq!(&m[..10], b"\x1f\x8b\x08\x00\x00\x00\x00\x00\x00\xff");
    }

    #[cfg(feature = "zstd")]
    #[test]
    fn zstd_frames_carry_a_checksum() {
        let m = encode_member(Codec::Zstd, b"x\n").unwrap();
        assert_ne!(
            m[4] & 0x04,
            0,
            "frame header descriptor has the content checksum flag"
        );
    }

    #[test]
    fn members_split_at_record_boundaries() {
        let payload: Vec<u8> = (0..100)
            .flat_map(|i| format!("k{i:03}\t{}\n", "v".repeat(20)).into_bytes())
            .collect();
        for codec in codecs() {
            let all = encode_members(codec, &payload, 300).unwrap();
            let got = items(codec, &all);
            let outs: Vec<&Cow<[u8]>> = got
                .iter()
                .map(|i| match i {
                    Item::Data(d) => d,
                    other => panic!("{other:?}"),
                })
                .collect();
            assert!(outs.len() > 5, "{codec:?}");
            assert!(
                outs.iter().all(|o| o.len() <= 300 && o.ends_with(b"\n")),
                "{codec:?}"
            );
            assert_eq!(
                outs.iter()
                    .flat_map(|o| o.iter())
                    .copied()
                    .collect::<Vec<u8>>(),
                payload,
                "{codec:?}"
            );
        }
    }

    #[test]
    fn a_record_longer_than_the_size_is_its_own_member() {
        let payload = [b"short\n".to_vec(), vec![b'x'; 1000], b"\nend\n".to_vec()].concat();
        let all = encode_members(Codec::Gzip, &payload, 100).unwrap();
        let got = items(Codec::Gzip, &all);
        assert_eq!(got.len(), 3);
        assert_eq!(
            got[1],
            Item::Data([vec![b'x'; 1000], b"\n".to_vec()].concat().into())
        );
    }

    #[test]
    fn empty_payload_is_no_member() {
        assert_eq!(
            encode_members(Codec::Gzip, b"", 100).unwrap(),
            Vec::<u8>::new()
        );
    }

    #[test]
    fn a_long_unterminated_gzip_name_is_judged_in_linear_time() {
        // FNAME set, then 200 KB without its terminating zero: the header never completes.
        let mut data = b"\x1f\x8b\x08\x08\0\0\0\0\0\xff".to_vec();
        data.extend(std::iter::repeat_n(b'a', 200_000));
        let started = std::time::Instant::now();
        assert_eq!(member(Codec::Gzip, &data), Member::Short);
        assert_eq!(decode_alone(Codec::Gzip, &data), Alone::Short(Vec::new()));
        assert_eq!(gzip_header_len(&data), None);
        assert!(
            started.elapsed().as_millis() < 500,
            "took {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn a_long_cut_short_member_decodes_alone_in_large_steps() {
        for codec in codecs() {
            let data: Vec<u8> = (0..400_000u32)
                .flat_map(|i| format!("k{i}\tv{}\n", i % 97).into_bytes())
                .collect();
            let full = fixture(codec, &data);
            let cut = &full[..full.len() * 9 / 10];
            let started = std::time::Instant::now();
            let Alone::Short(out) = decode_alone(codec, cut) else {
                panic!("{codec:?}: not short")
            };
            assert!(
                data.starts_with(&out) && out.len() > data.len() / 2,
                "{codec:?}"
            );
            assert!(
                started.elapsed().as_secs() < 3,
                "{codec:?}: took {:?}",
                started.elapsed()
            );
        }
    }

    #[cfg(feature = "xz")]
    #[test]
    fn the_byte_that_fails_takes_its_output_with_it() {
        // A corrupt xz member from the differential harness (seed 192): fed a byte at a time,
        // byte 198 decodes the `\n` that ends `k1,ü<LF>#\r` and fails in the same call. The
        // runner's oracle raises there and loses that call's output, so the line stays an
        // uncommitted tail (§16.6 rule 2); keeping the `\n` committed a row TSVZ.py never reads.
        let hex = "fd377a585a000004e6d6b4460200210116000000742fe5a3e000d0ffa85d0016080c10201dfe4563de2fab4affa95f4ff69ca40597288158e95387200d99a217c32a53cc3ecf1bdb015a6e50cfd3eda358917aa0e4d55d5e14bcecdad52799275d2d743b9fae2572083238cd95836d862e677ed36f7941b64ed044e055915f9e7c41a197d6b455b9a6b2e81ea1c41107cc6d7389d51ae5a295713f67cc468b06cede5f249a1eaaeb339689a8df773d9d7920ce5d4cea786a3793bd889dc25523d5f9c89e939d0000a17c4d921b6e786b0001c401d1010000041566ebb1c467fb020000000004595a";
        let member: Vec<u8> = (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect();
        let Alone::Corrupt(out) = decode_alone(Codec::Xz, &member) else {
            panic!("not corrupt")
        };
        assert!(
            out.ends_with("k1,ü<LF>#\r".as_bytes()),
            "{:?}",
            String::from_utf8_lossy(&out[out.len().saturating_sub(24)..])
        );
    }

    #[cfg(all(feature = "xz", target_os = "linux"))]
    #[test]
    fn an_xz_member_demanding_a_huge_dictionary_is_not_allocated() {
        // A legacy .lzma header (the xz codec auto-detects it) declaring a 4 GiB dictionary: the
        // decoder must refuse it rather than reserve 4 GiB for 20 bytes (found by cargo fuzz).
        fn vm_peak_kib() -> u64 {
            let status = std::fs::read_to_string("/proc/self/status").unwrap();
            let line = status.lines().find(|l| l.starts_with("VmPeak:")).unwrap();
            line.split_whitespace().nth(1).unwrap().parse().unwrap()
        }
        let data = [
            0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
            0x80, 0x00, 0x00, 0xef,
        ];
        let before = vm_peak_kib();
        assert_eq!(member(Codec::Xz, &data), Member::Bad);
        assert!(matches!(decode_alone(Codec::Xz, &data), Alone::Corrupt(_)));
        let grown = vm_peak_kib() - before;
        assert!(
            grown < 2 * 1024 * 1024,
            "decoding 18 bytes reserved {grown} KiB"
        );
    }

    #[test]
    fn a_gzip_header_with_reserved_flags_is_corrupt() {
        // A damaged flag byte (found by the differential harness): zlib rejects reserved flags.
        let mut m = fixture(Codec::Gzip, b"hello\n");
        m[3] = 0xff;
        assert_eq!(member(Codec::Gzip, &m), Member::Bad);
        assert!(matches!(decode_alone(Codec::Gzip, &m), Alone::Corrupt(_)));
        assert_eq!(gzip_header_len(&m), None);
    }

    #[test]
    fn a_gzip_header_crc_is_checked() {
        let body = &fixture(Codec::Gzip, b"hello\n")[10..];
        let header = b"\x1f\x8b\x08\x02\0\0\0\0\0\xff";
        let crc16 = (crc32fast::hash(header) & 0xffff) as u16;
        let good = [&header[..], &crc16.to_le_bytes(), body].concat();
        assert!(matches!(
            member(Codec::Gzip, &good),
            Member::Complete { .. }
        ));
        let bad = [&header[..], &(crc16 ^ 1).to_le_bytes(), body].concat();
        assert_eq!(member(Codec::Gzip, &bad), Member::Bad);
    }
}
