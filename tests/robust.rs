//! Arbitrary bytes and arguments never panic (`fuzz/` goes further on nightly).

use std::fs;

use tsvz::name::Codec;
use tsvz::{maint, Options, Store};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    /// Random bytes, mostly the ones the format gives meaning to.
    fn bytes(&mut self, max: usize) -> Vec<u8> {
        const SPECIAL: &[u8] = b"\t\n\r\0<>#_,|\xef\xbb\xbf\x1f\x8b\x08 sepLFCRNULlt";
        let n = self.below(max + 1);
        (0..n).map(|_| if self.below(3) == 0 { self.next() as u8 } else { SPECIAL[self.below(SPECIAL.len())] }).collect()
    }
}

#[test]
fn random_plain_parts_never_panic() {
    let dir = tempfile::tempdir().unwrap();
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    for i in 0..300 {
        for ext in ["tsvz", "csvz", "nsvz", "psvz", "tsv"] {
            let path = dir.path().join(format!("s{i}.{ext}"));
            let mut data = rng.bytes(400);
            if rng.below(4) == 0 {
                data = [b"#_on_corruption_#\tstop\n#_checksum_crc32_#\n".to_vec(), data].concat();
            }
            fs::write(&path, &data).unwrap();
            if let Ok(s) = Store::open(&path, Options { verify: true, ..Options::default() }) {
                let _ = s.iter().count();
                let _ = s.missing().map(|r| r.to_vec());
            }
            let _ = maint::scrub(&path, &Options::default());
            let _ = Store::open(&path, Options::default());
        }
    }
}

#[test]
fn random_compressed_parts_never_panic() {
    let mut rng = Rng(42);
    for codec in [Codec::Gzip, Codec::Bzip2, Codec::Xz, Codec::Zstd].into_iter().filter(|c| c.supported()) {
        let good = tsvz::codec::encode_member(codec, b"a\tx\nb\ty\n").unwrap();
        for _ in 0..300 {
            let mut data = rng.bytes(300);
            match rng.below(3) {
                0 => data = [good.clone(), data].concat(),
                1 => data = [good[..rng.below(good.len())].to_vec(), data].concat(),
                _ => {}
            }
            if !data.is_empty() && rng.below(2) == 0 {
                let at = rng.below(data.len());
                data[at] ^= 1 << rng.below(8);
            }
            let _ = tsvz::codec::items(codec, &data);
        }
    }
}

#[test]
fn random_arguments_never_panic() {
    let mut rng = Rng(7);
    let pool = ["-", "--", "-q", "-qv", "-d", "-dx", "-d=", "--format", "--form=table", "--x", "-5", "-.", "-ü", "get", "set", "read", "s.tsvz", "", "k", "=", "--=", "---"];
    for _ in 0..5000 {
        let args: Vec<String> = (0..rng.below(6)).map(|_| pool[rng.below(pool.len())].to_string()).collect();
        let _ = tsvz::cli::args::parse(&args);
    }
}
