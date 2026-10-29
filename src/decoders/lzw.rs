//! LZWDecode implementation for PDF.
//!
//! Decompresses data using the Lempel-Ziv-Welch (LZW) algorithm as specified
//! in the PDF Reference (Section 7.4.4).
//!
//! PDF's LZW implementation:
//! - Uses MSB-first bit ordering
//! - Starts with 9-bit codes
//! - Increases code size when table fills up
//! - Uses EarlyChange=1 by default (change code size one code earlier than
//!   GIF); a `/EarlyChange 0` entry in DecodeParms selects the GIF switch
//! - Clear code is 256, EOD code is 257
//! - First available code is 258

use crate::decoders::StreamDecoder;
use crate::error::{Error, Result};

// ── pdf_manipulator patch: one weezl decoder, early or late switch per /EarlyChange ──
/// LZWDecode filter implementation with PDF's default, `/EarlyChange 1`.
///
/// Decompresses data using the LZW algorithm. A stream that sets
/// `/EarlyChange 0` goes through the filter pipeline, which reads the entry.
pub struct LzwDecoder;

impl StreamDecoder for LzwDecoder {
    fn decode(&self, input: &[u8]) -> Result<Vec<u8>> {
        decode_lzw(input, true)
    }

    fn name(&self) -> &str {
        "LZWDecode"
    }
}

/// LZWDecode with the `/EarlyChange` value the stream's DecodeParms name.
pub(crate) struct ConfiguredLzwDecoder {
    pub(crate) early_change: bool,
}

impl StreamDecoder for ConfiguredLzwDecoder {
    fn decode(&self, input: &[u8]) -> Result<Vec<u8>> {
        decode_lzw(input, self.early_change)
    }

    fn name(&self) -> &str {
        "LZWDecode"
    }
}

/// Decodes with weezl: MSB bit order, 8-bit minimum code size. The code
/// width grows one code early (TIFF size switch) with `/EarlyChange 1` and
/// at the GIF boundary with `/EarlyChange 0`; reading one as the other
/// decodes the wrong bytes without an error.
fn decode_lzw(input: &[u8], early_change: bool) -> Result<Vec<u8>> {
    use weezl::{decode::Decoder, BitOrder};

    let mut decoder = if early_change {
        Decoder::with_tiff_size_switch(BitOrder::Msb, 8)
    } else {
        Decoder::new(BitOrder::Msb, 8)
    };
    decoder
        .decode(input)
        .map_err(|e| Error::Decode(format!("LZWDecode error: {:?}", e)))
}
// ── end pdf_manipulator patch ──

#[cfg(test)]
mod tests {
    use super::*;
    use weezl::{encode::Encoder as LzwEncoder, BitOrder};

    fn early(data: &[u8]) -> Vec<u8> {
        LzwEncoder::with_tiff_size_switch(BitOrder::Msb, 8).encode(data).unwrap()
    }

    /// Varied bytes, enough to fill the 4096-entry table several times, so
    /// the stream crosses the 9, 10, 11 and 12 bit code widths and holds
    /// clear codes.
    fn long_sample() -> Vec<u8> {
        let mut state = 12345u32;
        (0..300_000)
            .map(|i| {
                state = state.wrapping_mul(1664525).wrapping_add(1013904223);
                if i % 5000 < 1500 { (i % 251) as u8 } else { ((state >> 24) % 11) as u8 }
            })
            .collect()
    }

    #[test]
    fn test_lzw_decode_simple() {
        let original = b"ABCABCABCABC";
        assert_eq!(LzwDecoder.decode(&early(original)).unwrap(), original);
    }

    #[test]
    fn test_lzw_decode_empty() {
        assert_eq!(LzwDecoder.decode(&early(b"")).unwrap(), b"");
    }

    #[test]
    fn test_lzw_decode_repeated_pattern() {
        let original = b"The quick brown fox jumps over the lazy dog. ".repeat(10);
        assert_eq!(LzwDecoder.decode(&early(&original)).unwrap(), original);
    }

    #[test]
    fn early_change_one_round_trips_across_every_code_width_and_clear_codes() {
        let original = long_sample();
        assert_eq!(LzwDecoder.decode(&early(&original)).unwrap(), original);
    }

    #[test]
    fn early_change_zero_round_trips_across_every_code_width_and_clear_codes() {
        let original = long_sample();
        let late = LzwEncoder::new(BitOrder::Msb, 8).encode(&original).unwrap();
        let decoder = ConfiguredLzwDecoder { early_change: false };
        assert_eq!(decoder.decode(&late).unwrap(), original);
    }

    #[test]
    fn a_stream_read_with_the_wrong_switch_is_not_decoded_as_the_right_one() {
        let original = long_sample();
        let late = LzwEncoder::new(BitOrder::Msb, 8).encode(&original).unwrap();
        // Either an error or other bytes, never the original.
        let wrong = LzwDecoder.decode(&late);
        assert!(wrong.map_or(true, |bytes| bytes != original));
    }

    #[test]
    fn a_streams_early_change_entry_selects_the_switch() {
        use crate::object::Object;
        use std::collections::HashMap;

        let original = long_sample();
        let late = LzwEncoder::new(BitOrder::Msb, 8).encode(&original).unwrap();
        let stream = |parms: Option<(&str, i64)>, data: &[u8]| {
            let mut dict = HashMap::new();
            dict.insert("Filter".to_string(), Object::Name("LZWDecode".to_string()));
            if let Some((key, value)) = parms {
                let mut p = HashMap::new();
                p.insert(key.to_string(), Object::Integer(value));
                dict.insert("DecodeParms".to_string(), Object::Dictionary(p));
            }
            Object::Stream { dict, data: bytes::Bytes::copy_from_slice(data) }
        };

        let zero = stream(Some(("EarlyChange", 0)), &late);
        assert_eq!(zero.decode_stream_data().unwrap(), original);
        let one = stream(Some(("EarlyChange", 1)), &early(&original));
        assert_eq!(one.decode_stream_data().unwrap(), original);
        let absent = stream(None, &early(&original));
        assert_eq!(absent.decode_stream_data().unwrap(), original);
    }

    #[test]
    fn a_corrupt_stream_is_refused() {
        // 9-bit codes: 'A' (65), then 300, a code far past the next free one.
        let bits: u32 = (65 << 9) | 300;
        let bytes = [(bits >> 10) as u8, (bits >> 2) as u8, ((bits & 3) << 6) as u8];
        assert!(LzwDecoder.decode(&bytes).is_err());
    }

    #[test]
    fn test_lzw_decode_invalid_data() {
        let invalid = b"This is not LZW compressed data";
        assert!(LzwDecoder.decode(invalid).is_err());
    }

    #[test]
    fn test_lzw_decoder_name() {
        assert_eq!(LzwDecoder.name(), "LZWDecode");
    }
}
