//! Arbitrary bytes and arguments never panic (`fuzz/` goes further on nightly).

use std::fs;

use tsvz::name::Codec;
use tsvz::{Options, Store, maint};

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
        (0..n)
            .map(|_| {
                if self.below(3) == 0 {
                    self.next() as u8
                } else {
                    SPECIAL[self.below(SPECIAL.len())]
                }
            })
            .collect()
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
                data = [
                    b"#_on_corruption_#\tstop\n#_checksum_crc32_#\n".to_vec(),
                    data,
                ]
                .concat();
            }
            fs::write(&path, &data).unwrap();
            let mut verify = Options::default();
            verify.verify = true;
            if let Ok(s) = Store::open(&path, verify) {
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
    for codec in [Codec::Gzip, Codec::Bzip2, Codec::Xz, Codec::Zstd]
        .into_iter()
        .filter(|c| c.supported())
    {
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
    let pool = [
        "-",
        "--",
        "-q",
        "-qv",
        "-d",
        "-dx",
        "-d=",
        "--format",
        "--form=table",
        "--x",
        "-5",
        "-.",
        "-ü",
        "get",
        "set",
        "read",
        "s.tsvz",
        "",
        "k",
        "=",
        "--=",
        "---",
    ];
    for _ in 0..5000 {
        let args: Vec<String> = (0..rng.below(6))
            .map(|_| pool[rng.below(pool.len())].to_string())
            .collect();
        let _ = tsvz::cli::args::parse(&args);
    }
}

#[test]
fn random_requests_never_panic() {
    use std::io::Cursor;
    use tsvz::handler::engine::Engine;
    use tsvz::handler::protocol::{parse, read_line, read_response};
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::open(&dir.path().join("s.tsvz"), None).unwrap();
    let mut rng = Rng(0x2545_f491_4f6c_dd1d);
    const WORDS: &[&str] = &[
        "read",
        "get",
        "set",
        "append",
        "delete",
        "has",
        "len",
        "keys",
        "pop",
        "popitem",
        "setdefault",
        "verify",
        "parts",
        "clear",
        "scrub",
        "--sync",
        "--x-written",
        "-",
        "first",
        "#_defaults_#",
        "#_strip_trailing_whites_#",
        "<#>k",
        "k ",
        "k",
        "",
    ];
    for _ in 0..2000 {
        let mut text = Vec::new();
        for _ in 0..1 + rng.below(4) {
            text.extend_from_slice(WORDS[rng.below(WORDS.len())].as_bytes());
            text.push(b'\t');
        }
        text.extend(rng.bytes(12));
        text.push(b'\n');
        let mut input = Cursor::new(text.clone());
        while let Ok(Some(line)) = read_line(&mut input, 1 << 16) {
            if let Ok(request) = parse(&line) {
                let bulk: Vec<String> = (0..rng.below(3))
                    .map(|_| String::from_utf8_lossy(&rng.bytes(16)).replace('\n', ""))
                    .collect();
                let bulk = request.bulk().then_some(&bulk[..]);
                let _ = engine.handle(&request, bulk);
            }
        }
        let _ = read_response(&mut Cursor::new(text));
    }
}
