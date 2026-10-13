#![no_main]
//! Bytes as handler protocol input: request lines, the bulk records after them, and responses.

use std::io::Cursor;
use std::path::Path;

use libfuzzer_sys::fuzz_target;
use tsvz::handler::protocol::{parse, read_line, read_response};
use tsvz::name::Format;
use tsvz::ops::parse_records;

fuzz_target!(|data: &[u8]| {
    let mut input = Cursor::new(data);
    while let Ok(Some(line)) = read_line(&mut input, 1 << 16) {
        if let Ok(request) = parse(&line) {
            let _ = request.bulk();
        }
    }
    let tsvz = Format { delimiter: b'\t', strict: true };
    let _ = parse_records(data, tsvz, data.first() == Some(&b'd'), Path::new("fuzz"), None);
    let _ = read_response(&mut Cursor::new(data));
});
