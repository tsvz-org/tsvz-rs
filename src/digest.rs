//! The digest algorithms of §15.2.1.

use sha2::Digest;

/// A digest algorithm named by `#_checksum_<algo>_#`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Algo {
    Crc32,
    Crc32c,
    Sha256,
    Blake3,
}

impl Algo {
    /// The algorithm an `<algo>` name (any ASCII case) names; None for names this crate does not implement.
    pub fn from_name(name: &str) -> Option<Algo> {
        match name.to_ascii_lowercase().as_str() {
            "crc32" => Some(Algo::Crc32),
            "crc32c" => Some(Algo::Crc32c),
            "sha256" => Some(Algo::Sha256),
            "blake3" => Some(Algo::Blake3),
            _ => None,
        }
    }

    /// The lowercase name `verify` prints (§20.2).
    pub fn name(self) -> &'static str {
        match self {
            Algo::Crc32 => "crc32",
            Algo::Crc32c => "crc32c",
            Algo::Sha256 => "sha256",
            Algo::Blake3 => "blake3",
        }
    }
}

/// A running digest over a segment (§15.4).
#[derive(Clone)]
pub enum Accumulator {
    Crc32(crc32fast::Hasher),
    Crc32c(u32),
    Sha256(sha2::Sha256),
    Blake3(Box<blake3::Hasher>),
}

impl Accumulator {
    pub fn new(algo: Algo) -> Accumulator {
        match algo {
            Algo::Crc32 => Accumulator::Crc32(crc32fast::Hasher::new()),
            Algo::Crc32c => Accumulator::Crc32c(0),
            Algo::Sha256 => Accumulator::Sha256(sha2::Sha256::new()),
            Algo::Blake3 => Accumulator::Blake3(Box::new(blake3::Hasher::new())),
        }
    }

    pub fn update(&mut self, data: &[u8]) {
        match self {
            Accumulator::Crc32(h) => h.update(data),
            Accumulator::Crc32c(c) => *c = crc32c::crc32c_append(*c, data),
            Accumulator::Sha256(h) => h.update(data),
            Accumulator::Blake3(h) => {
                h.update(data);
            }
        }
    }

    /// The digest so far as §15.2.1 writes it, in lowercase hexadecimal.
    pub fn hex(&self) -> String {
        match self {
            Accumulator::Crc32(h) => format!("{:08x}", h.clone().finalize()),
            Accumulator::Crc32c(c) => format!("{c:08x}"),
            Accumulator::Sha256(h) => h
                .clone()
                .finalize()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect(),
            Accumulator::Blake3(h) => h.finalize().to_hex().to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(algo: Algo, parts: &[&[u8]]) -> String {
        let mut acc = Accumulator::new(algo);
        for p in parts {
            acc.update(p);
        }
        acc.hex()
    }

    #[test]
    fn known_vectors() {
        assert_eq!(digest(Algo::Crc32, &[b"123456789"]), "cbf43926");
        assert_eq!(digest(Algo::Crc32c, &[b"123456789"]), "e3069283");
        assert_eq!(
            digest(Algo::Sha256, &[b"abc"]),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            digest(Algo::Blake3, &[b"abc"]),
            "6437b3ac38465133ffb63b75273a8db548c558465d79db03fd359c6cd5bd9d85"
        );
        assert_eq!(digest(Algo::Crc32, &[]), "00000000");
    }

    #[test]
    fn incremental_equals_whole_and_hex_does_not_consume() {
        for algo in [Algo::Crc32, Algo::Crc32c, Algo::Sha256, Algo::Blake3] {
            assert_eq!(
                digest(algo, &[b"a\tx\n", b"b\ty\n"]),
                digest(algo, &[b"a\tx\nb\ty\n"])
            );
            let mut acc = Accumulator::new(algo);
            acc.update(b"a");
            let first = acc.hex();
            assert_eq!(first, acc.hex());
            acc.update(b"b");
            assert_ne!(first, acc.hex());
        }
    }

    #[test]
    fn names() {
        assert_eq!(Algo::from_name("CRC32C"), Some(Algo::Crc32c));
        assert_eq!(Algo::from_name("sha256"), Some(Algo::Sha256));
        assert_eq!(Algo::from_name("md5"), None);
        assert_eq!(Algo::Blake3.name(), "blake3");
    }
}
