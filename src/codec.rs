//! Compressed parts (§16): decoding members, the damaged-member rules of §16.6, and (Task 6) encoding.

use crate::name::Codec;

/// How the member at the start of some bytes decoded (§16.6 rule 1).
#[derive(Debug, PartialEq, Eq)]
pub enum Member {
    /// Decoded to its end and passed its check; `end` is its length in bytes.
    Complete { out: Vec<u8>, end: usize },
    /// The bytes ended before the member did.
    Short,
    /// The bytes are not a valid member.
    Bad,
}

/// A damaged member's bytes decoded on their own (§16.6 rule 2).
#[derive(Debug, PartialEq, Eq)]
pub enum Alone {
    /// They merely end before the member does (or decode whole): the output is used.
    Short(Vec<u8>),
    /// Decoding failed: the output decoded before the failure, for the corruption policy.
    Corrupt(Vec<u8>),
}

/// A compressed part as the framer takes it (§16.6).
#[derive(Debug, PartialEq, Eq)]
pub enum Item {
    /// A complete member's output.
    Data(Vec<u8>),
    /// A member cut short: its output is used, with a warning.
    CutShort { offset: usize, out: Vec<u8> },
    /// A corrupt member: the policy in force decides what happens to `out` (§4.10).
    Corrupt { offset: usize, out: Vec<u8> },
    /// A damaged member ended here: the unfinished line before this point is dropped.
    Break,
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
const FIELDS: [(u8, Stage); 4] = [(4, Stage::XLen(None)), (8, Stage::Name), (16, Stage::Comment), (2, Stage::Crc(2))];

/// An RFC 1952 header parsed a byte at a time, with constant work per byte, so a long or
/// unterminated name or comment costs linear time however the bytes arrive.
#[derive(Debug, Default)]
struct GzipHeader {
    seen: usize,
    flags: u8,
    stage: Stage,
}

impl GzipHeader {
    /// The stage that reads the first flagged field from `FIELDS[i]` on.
    fn from(&self, i: usize) -> Stage {
        FIELDS[i..].iter().find(|(bit, _)| self.flags & bit != 0).map_or(Stage::Done, |&(_, stage)| stage)
    }

    /// Take one byte: Ok(true) once the header is complete; Err(()) when the bytes are not a gzip
    /// deflate member.
    fn push(&mut self, b: u8) -> Result<bool, ()> {
        self.seen += 1;
        self.stage = match self.stage {
            Stage::Fixed => {
                let i = self.seen - 1;
                if i < 3 && b != b"\x1f\x8b\x08"[i] {
                    return Err(());
                }
                if i == 3 {
                    self.flags = b;
                }
                if self.seen == 10 { self.from(0) } else { Stage::Fixed }
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
            Stage::Crc(1) => Stage::Done,
            Stage::Crc(n) => Stage::Crc(n - 1),
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

struct GzipDecoder {
    header: GzipHeader,
    /// The raw inflater once the header is complete, and whether its deflate stream ended.
    inflate: Option<(flate2::Decompress, bool)>,
    trailer: Vec<u8>,
    crc: crc32fast::Hasher,
    size: u32,
    done: bool,
}

/// An incremental decoder for one member.
enum Decoder {
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
    fn new(codec: Codec) -> Decoder {
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
            Codec::Xz => match liblzma::stream::Stream::new_auto_decoder(u64::MAX, 0) {
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
    fn feed(&mut self, input: &[u8], out: &mut Vec<u8>) -> Result<(usize, bool), ()> {
        match self {
            Decoder::Gzip(g) => g.feed(input, out),
            #[cfg(feature = "bzip2")]
            Decoder::Bzip2(d) => {
                let before = d.total_in();
                loop {
                    out.reserve(64 * 1024);
                    let status = d.decompress_vec(&input[(d.total_in() - before) as usize..], out).map_err(|_| ())?;
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
                    let status = s.process_vec(&input[used..], out, liblzma::stream::Action::Run).map_err(|_| ())?;
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
                let Some(&b) = input.get(used) else { return Ok((used, false)) };
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
            let status = z.decompress_vec(&input[used..], out, flate2::FlushDecompress::None).map_err(|_| ())?;
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

/// Decode the member at the start of `data` (§16.6 rule 1).
pub fn member(codec: Codec, data: &[u8]) -> Member {
    let mut d = Decoder::new(codec);
    let mut out = Vec::new();
    match d.feed(data, &mut out) {
        Err(()) => Member::Bad,
        Ok((end, true)) => Member::Complete { out, end },
        Ok((_, false)) => Member::Short,
    }
}

/// Decode a damaged member's bytes on their own (§16.6 rule 2). They are fed in large steps;
/// when a step fails, they are decoded again with that step fed a byte at a time, so nothing
/// decoded before the failing byte is lost with the call that fails (the runner's oracle feeds
/// every byte alone; decoders are deterministic, so the result is the same in linear time).
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
    let Some(step) = failed else { return Alone::Short(out) };
    let (before, failing) = data.split_at(step * STEP);
    let mut d = Decoder::new(codec);
    let mut out = Vec::new();
    for piece in before.chunks(STEP).chain(failing[..failing.len().min(STEP)].chunks(1)) {
        match d.feed(piece, &mut out) {
            Err(()) => return Alone::Corrupt(out),
            Ok((_, true)) => return Alone::Short(out),
            Ok(_) => {}
        }
    }
    Alone::Short(out)
}

/// Feed `input` to a raw inflater and discard the output: Ok(Some(bytes consumed)) when the
/// deflate stream ended within `input`, Ok(None) when it needs more, Err on a data error.
fn inflate_to_end(z: &mut flate2::Decompress, input: &[u8], scratch: &mut [u8]) -> Result<Option<usize>, ()> {
    let mut pos = 0;
    loop {
        let (in0, out0) = (z.total_in(), z.total_out());
        let status = z.decompress(&input[pos..], scratch, flate2::FlushDecompress::None).map_err(|_| ())?;
        pos += (z.total_in() - in0) as usize;
        if status == flate2::Status::StreamEnd {
            return Ok(Some(pos));
        }
        let produced = (z.total_out() - out0) as usize;
        if (pos >= input.len() && produced < scratch.len()) || (z.total_in() == in0 && produced == 0) {
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
            if begun > 0 && begun < 8 { stripped + (8 - begun).min(zeros) } else { stripped }
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

/// A compressed part's bytes as items for the framer (§16.6). Mirrors `decompress_items`.
pub fn items(codec: Codec, data: &[u8]) -> Vec<Item> {
    let magic = codec.magic();
    let mut items = Vec::new();
    let mut pos = 0;
    while pos < data.len() {
        if codec == Codec::Gzip && data[pos] == 0 {
            pos += data[pos..].iter().take_while(|&&b| b == 0).count(); // padding before a member
            continue;
        }
        if let Member::Complete { out, end } = member(codec, &data[pos..]) {
            items.push(Item::Data(out));
            pos += end.max(1);
            continue;
        }
        // A damaged member's bytes run to the next member start, complete or not (rule 2).
        let following = memchr::memmem::find(&data[pos + 1..], magic).map(|i| pos + 1 + i);
        let mut region = &data[pos..following.unwrap_or(data.len())];
        if codec == Codec::Gzip {
            region = &region[..gzip_keep(region)];
        }
        items.push(match decode_alone(codec, region) {
            Alone::Short(out) => Item::CutShort { offset: pos, out },
            Alone::Corrupt(out) => Item::Corrupt { offset: pos, out },
        });
        match following {
            Some(next) => {
                items.push(Item::Break);
                pos = next;
            }
            None => break,
        }
    }
    items
}

/// One member holding `data`, or None when this build does not include the codec.
/// gzip members have a fixed header (no name, mtime 0, OS 255); zstd frames carry the content
/// checksum (§16.8).
pub fn encode_member(codec: Codec, data: &[u8]) -> Option<Vec<u8>> {
    use std::io::Write;
    match codec {
        Codec::Gzip => {
            let mut out = b"\x1f\x8b\x08\x00\x00\x00\x00\x00\x00\xff".to_vec();
            let mut e = flate2::write::DeflateEncoder::new(out.split_off(10), flate2::Compression::default());
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
            c.set_parameter(zstd::zstd_safe::CParameter::ChecksumFlag(true)).ok()?;
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
            let next = memchr::memchr(b'\n', &payload[end..]).map_or(payload.len(), |i| end + i + 1);
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

    /// A member built by the codec crates' own encoders, so decoding is tested independently of Task 6.
    fn fixture(codec: Codec, data: &[u8]) -> Vec<u8> {
        match codec {
            Codec::Gzip => {
                let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
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
                c.set_parameter(zstd::zstd_safe::CParameter::ChecksumFlag(true)).unwrap();
                c.compress(data).unwrap()
            }
            #[allow(unreachable_patterns)]
            _ => unreachable!("tests only use codecs this build has"),
        }
    }

    fn codecs() -> Vec<Codec> {
        [Codec::Gzip, Codec::Bzip2, Codec::Xz, Codec::Zstd].into_iter().filter(|c| c.supported()).collect()
    }

    #[test]
    fn complete_short_and_bad_members() {
        for codec in codecs() {
            let data = b"a\tx\nb\ty\n".repeat(50);
            let mut two = fixture(codec, &data);
            let first = two.len();
            two.extend(fixture(codec, b"c\tz\n"));
            assert_eq!(member(codec, &two), Member::Complete { out: data.clone(), end: first }, "{codec:?}");
            assert_eq!(member(codec, &two[..first - 3]), Member::Short, "{codec:?}");
            let mut bad = fixture(codec, &data);
            let n = bad.len();
            bad[n / 2] ^= 0xff;
            assert_eq!(member(codec, &bad), Member::Bad, "{codec:?}");
            assert!(matches!(decode_alone(codec, &bad), Alone::Corrupt(_)), "{codec:?}");
            assert!(matches!(decode_alone(codec, &two[..first - 3]), Alone::Short(_)), "{codec:?}");
            assert_eq!(member(codec, b"garbage!garbage!"), Member::Bad, "{codec:?}");
        }
    }

    #[test]
    fn a_failed_gzip_check_is_corrupt_with_its_output() {
        let mut g = fixture(Codec::Gzip, b"hello\n");
        let n = g.len();
        g[n - 8] ^= 1; // the stored CRC-32
        assert_eq!(member(Codec::Gzip, &g), Member::Bad);
        assert_eq!(decode_alone(Codec::Gzip, &g), Alone::Corrupt(b"hello\n".to_vec()));
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
        assert_eq!(gzip_header_len(b"\x1f\x8b\x08\x08\0\0\0\0\0\xffname\0rest"), Some(15));
        assert_eq!(gzip_header_len(b"\x1f\x8b\x08\x08\0\0\0\0\0\xffname"), None);
        assert_eq!(gzip_header_len(b"\x1f\x8b\x08\x04\0\0\0\0\0\xff\x02\0ab"), Some(14));
        assert_eq!(gzip_header_len(b"\x1f\x8b\x09\x00\0\0\0\0\0\xff"), None);
    }

    #[test]
    fn items_of_a_healthy_multi_member_part() {
        for codec in codecs() {
            let mut part = fixture(codec, b"a\tx\n");
            part.extend(fixture(codec, b"b\ty\n"));
            assert_eq!(items(codec, &part), vec![Item::Data(b"a\tx\n".to_vec()), Item::Data(b"b\ty\n".to_vec())], "{codec:?}");
        }
    }

    #[test]
    fn zero_padding_between_gzip_members_is_skipped() {
        let mut part = vec![0u8; 7];
        part.extend(fixture(Codec::Gzip, b"a\tx\n"));
        part.extend([0u8; 100]);
        part.extend(fixture(Codec::Gzip, b"b\ty\n"));
        part.extend([0u8; 3]);
        assert_eq!(items(Codec::Gzip, &part), vec![Item::Data(b"a\tx\n".to_vec()), Item::Data(b"b\ty\n".to_vec())]);
    }

    #[test]
    fn a_cut_short_member_before_another() {
        for codec in codecs() {
            let first = fixture(codec, &b"k\tvalue\n".repeat(200));
            let mut part = first[..first.len() / 2].to_vec();
            part.extend(fixture(codec, b"z\t1\n"));
            let got = items(codec, &part);
            assert_eq!(got.len(), 3, "{codec:?}: {got:?}");
            assert!(matches!(&got[0], Item::CutShort { offset: 0, .. }), "{codec:?}");
            assert_eq!(got[1], Item::Break);
            assert_eq!(got[2], Item::Data(b"z\t1\n".to_vec()));
        }
    }

    #[test]
    fn a_corrupt_member_in_the_middle() {
        for codec in codecs() {
            let a = fixture(codec, b"a\tx\n");
            let mut b = fixture(codec, &b"b\ty\n".repeat(100));
            let n = b.len();
            // gzip: break the stored CRC-32, a sure check failure; the others: a byte in the middle.
            if codec == Codec::Gzip { b[n - 8] ^= 1 } else { b[n / 2] ^= 0xff }
            let mut part = a.clone();
            part.extend(&b);
            part.extend(fixture(codec, b"c\tz\n"));
            let got = items(codec, &part);
            assert_eq!(got[0], Item::Data(b"a\tx\n".to_vec()), "{codec:?}");
            assert!(matches!(&got[1], Item::Corrupt { offset, .. } if *offset == a.len()), "{codec:?}: {got:?}");
            assert_eq!(got[2], Item::Break, "{codec:?}");
            assert_eq!(got[3], Item::Data(b"c\tz\n".to_vec()), "{codec:?}");
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
        assert!(full[cut - 1] != 0, "fixture precondition: the last kept byte is not zero");
        let mut region = full[..cut].to_vec();
        region.extend([0u8; 10]);
        assert_eq!(gzip_keep(&region), cut + 5);
    }

    #[test]
    fn encoded_members_decode_back() {
        for codec in codecs() {
            let data = b"a\tx\nb\ty\n";
            let m = encode_member(codec, data).unwrap();
            assert_eq!(member(codec, &m), Member::Complete { out: data.to_vec(), end: m.len() }, "{codec:?}");
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
        assert_ne!(m[4] & 0x04, 0, "frame header descriptor has the content checksum flag");
    }

    #[test]
    fn members_split_at_record_boundaries() {
        let payload: Vec<u8> = (0..100).flat_map(|i| format!("k{i:03}\t{}\n", "v".repeat(20)).into_bytes()).collect();
        for codec in codecs() {
            let all = encode_members(codec, &payload, 300).unwrap();
            let got = items(codec, &all);
            let outs: Vec<&Vec<u8>> = got.iter().map(|i| match i { Item::Data(d) => d, other => panic!("{other:?}") }).collect();
            assert!(outs.len() > 5, "{codec:?}");
            assert!(outs.iter().all(|o| o.len() <= 300 && o.ends_with(b"\n")), "{codec:?}");
            assert_eq!(outs.into_iter().flatten().copied().collect::<Vec<u8>>(), payload, "{codec:?}");
        }
    }

    #[test]
    fn a_record_longer_than_the_size_is_its_own_member() {
        let payload = [b"short\n".to_vec(), vec![b'x'; 1000], b"\nend\n".to_vec()].concat();
        let all = encode_members(Codec::Gzip, &payload, 100).unwrap();
        let got = items(Codec::Gzip, &all);
        assert_eq!(got.len(), 3);
        assert_eq!(got[1], Item::Data([vec![b'x'; 1000], b"\n".to_vec()].concat()));
    }

    #[test]
    fn empty_payload_is_no_member() {
        assert_eq!(encode_members(Codec::Gzip, b"", 100).unwrap(), Vec::<u8>::new());
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
        assert!(started.elapsed().as_millis() < 500, "took {:?}", started.elapsed());
    }

    #[test]
    fn a_long_cut_short_member_decodes_alone_in_large_steps() {
        for codec in codecs() {
            let data: Vec<u8> = (0..400_000u32).flat_map(|i| format!("k{i}\tv{}\n", i % 97).into_bytes()).collect();
            let full = fixture(codec, &data);
            let cut = &full[..full.len() * 9 / 10];
            let started = std::time::Instant::now();
            let Alone::Short(out) = decode_alone(codec, cut) else { panic!("{codec:?}: not short") };
            assert!(data.starts_with(&out) && out.len() > data.len() / 2, "{codec:?}");
            assert!(started.elapsed().as_secs() < 3, "{codec:?}: took {:?}", started.elapsed());
        }
    }
}
