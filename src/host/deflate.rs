//! Flate re-encoding for the full-rewrite save.
//!
//! This file is part of the pdf_manipulator host layer (NOT upstream).
//!
//! A save re-deflates a stream at the save's level and keeps the result only
//! when it is strictly smaller than the bytes the source held. Only the zlib
//! layer is undone and redone: predictor bytes stay as they are, so the
//! stream's `/DecodeParms` remain valid. Every function here streams in
//! [`CHUNK`] pieces, so a stream that inflates to many times its size is
//! never held decoded.

use crate::object::Object;
use flate2::{read::ZlibDecoder, write::ZlibEncoder, Compression};
use std::collections::HashMap;
use std::io::{self, Read, Write};

/// The largest level a save accepts.
pub(crate) const MAX_LEVEL: u32 = 9;

/// A stream whose source data is at most this long is re-encoded into memory
/// and compared; a longer one is first re-encoded into a byte counter.
pub(crate) const MEMORY_STREAM: u64 = 256 * 1024;

const CHUNK: usize = 64 * 1024;

/// How a stream is encoded, as far as a re-deflate is concerned.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Encoding {
    /// No `/Filter`: the data is the content itself.
    Raw,
    /// A single `FlateDecode`, bare or in a one-element array.
    Flate,
    /// Any other filter or a chain of filters: copied as it is.
    Other,
}

pub(crate) fn encoding(dict: &HashMap<String, Object>) -> Encoding {
    match dict.get("Filter") {
        None | Some(Object::Null) => Encoding::Raw,
        Some(Object::Name(n)) if n == "FlateDecode" => Encoding::Flate,
        Some(Object::Array(a)) => match a.as_slice() {
            [] => Encoding::Raw,
            [Object::Name(n)] if n == "FlateDecode" => Encoding::Flate,
            _ => Encoding::Other,
        },
        Some(_) => Encoding::Other,
    }
}

/// Deflates `input` into `out` at `level`, first inflating it when
/// `inflate`. An error from `input` (a corrupt zlib stream, a failed read)
/// and one from `out` look alike here; callers use an `out` that cannot fail
/// when they must tell them apart.
pub(crate) fn recode(
    input: impl Read,
    inflate: bool,
    level: u32,
    out: impl Write,
) -> io::Result<()> {
    let mut input: Box<dyn Read + '_> = if inflate {
        Box::new(ZlibDecoder::new(input))
    } else {
        Box::new(input)
    };
    let mut enc = ZlibEncoder::new(out, Compression::new(level));
    let mut buf = vec![0u8; CHUNK];
    loop {
        let n = input.read(&mut buf)?;
        if n == 0 {
            break;
        }
        enc.write_all(&buf[..n])?;
    }
    enc.finish()?;
    Ok(())
}

/// The deflated form of `data`, or `None` when the stream is not eligible,
/// does not inflate, or the result is not strictly smaller than `data`.
pub(crate) fn smaller(data: &[u8], encoding: Encoding, level: u32) -> Option<Vec<u8>> {
    if encoding == Encoding::Other {
        return None;
    }
    let mut out = Vec::new();
    recode(data, encoding == Encoding::Flate, level, &mut out).ok()?;
    (out.len() < data.len()).then_some(out)
}
