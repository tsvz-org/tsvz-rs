#![no_main]
//! A compressed part's bytes split into members; the first byte picks the codec.

use libfuzzer_sys::fuzz_target;
use tsvz::name::Codec;

fuzz_target!(|data: &[u8]| {
    let Some((&sel, rest)) = data.split_first() else { return };
    let codec = [Codec::Gzip, Codec::Bzip2, Codec::Xz, Codec::Zstd][(sel & 3) as usize];
    let _ = tsvz::codec::items(codec, rest);
});
