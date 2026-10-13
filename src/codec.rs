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

/// The length of the RFC 1952 header at the start of `data`, or None when it is not all
/// there or the bytes are not a gzip deflate member.
pub fn gzip_header_len(data: &[u8]) -> Option<usize> {
    if data.len() < 10 || &data[..3] != b"\x1f\x8b\x08" {
        return None;
    }
    let flags = data[3];
    let mut pos = 10usize;
    if flags & 4 != 0 {
        if data.len() < pos + 2 {
            return None;
        }
        pos += 2 + data[pos] as usize + 256 * data[pos + 1] as usize;
    }
    for flag in [8u8, 16] {
        if flags & flag != 0 {
            let rest = data.get(pos..)?;
            pos += memchr::memchr(0, rest)? + 1;
        }
    }
    if flags & 2 != 0 {
        pos += 2;
    }
    (pos <= data.len()).then_some(pos)
}

struct GzipDecoder {
    header: Vec<u8>,
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
                header: Vec::new(),
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
            while gzip_header_len(&self.header).is_none() && used < input.len() {
                self.header.push(input[used]);
                used += 1;
                let n = self.header.len();
                if n <= 3 && self.header[..] != b"\x1f\x8b\x08"[..n] {
                    return Err(());
                }
                if n > 1 << 20 {
                    return Err(()); // an absurd header: not a member a writer made
                }
            }
            if gzip_header_len(&self.header).is_none() {
                return Ok((used, false));
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

/// Decode a damaged member's bytes on their own (§16.6 rule 2), one byte at a time so that
/// nothing decoded before a failure is lost with the call that fails.
pub fn decode_alone(codec: Codec, data: &[u8]) -> Alone {
    let mut d = Decoder::new(codec);
    let mut out = Vec::new();
    for i in 0..data.len() {
        match d.feed(&data[i..i + 1], &mut out) {
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
}
