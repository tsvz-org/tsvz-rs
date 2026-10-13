#![no_main]
//! Bytes framed in chunks and replayed; the first byte picks the variant, strictness,
//! verification and chunk size.

use std::path::Path;
use std::sync::Arc;

use libfuzzer_sys::fuzz_target;
use tsvz::frame::Framer;
use tsvz::name::Format;
use tsvz::replay::{Binding, Cells, Replay, Sink};

struct Null;

impl Sink for Null {
    fn put(&mut self, _: &str, _: Cells, _: &Arc<Binding>) {}
    fn delete(&mut self, _: &str) {}
}

fuzz_target!(|data: &[u8]| {
    let Some((&sel, rest)) = data.split_first() else { return };
    let delimiter = [b'\t', b',', 0, b'|'][(sel & 3) as usize];
    let mut replay = Replay::new(Format { delimiter, strict: sel & 4 == 0 }, sel & 8 != 0, None);
    replay.begin_part(Path::new("fuzz.tsvz"), true);
    let mut framer = Framer::new();
    let mut sink = Null;
    for chunk in rest.chunks(1 + (sel >> 4) as usize) {
        if !framer.push(chunk, &mut |l| replay.line(l, &mut sink)) {
            break;
        }
    }
});
