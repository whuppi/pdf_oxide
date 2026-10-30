//! LZWDecode's `/EarlyChange` switch.

use crate::decoders::lzw::ConfiguredLzwDecoder;
use crate::decoders::{LzwDecoder, StreamDecoder};
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

/// No Dart battery sees it: it needs an LZW stream encoded with a chosen code-width switch, which no Dart fixture or call produces.
#[test]
fn early_change_one_round_trips_across_every_code_width_and_clear_codes() {
    let original = long_sample();
    assert_eq!(LzwDecoder.decode(&early(&original)).unwrap(), original);
}

/// No Dart battery sees it: it needs an LZW stream encoded with a chosen code-width switch, which no Dart fixture or call produces.
#[test]
fn early_change_zero_round_trips_across_every_code_width_and_clear_codes() {
    let original = long_sample();
    let late = LzwEncoder::new(BitOrder::Msb, 8).encode(&original).unwrap();
    let decoder = ConfiguredLzwDecoder { early_change: false };
    assert_eq!(decoder.decode(&late).unwrap(), original);
}

/// No Dart battery sees it: it needs an LZW stream encoded with a chosen code-width switch, which no Dart fixture or call produces.
#[test]
fn a_stream_read_with_the_wrong_switch_is_not_decoded_as_the_right_one() {
    let original = long_sample();
    let late = LzwEncoder::new(BitOrder::Msb, 8).encode(&original).unwrap();
    // Either an error or other bytes, never the original.
    let wrong = LzwDecoder.decode(&late);
    assert!(wrong.map_or(true, |bytes| bytes != original));
}

/// No Dart battery sees it: it needs an LZW stream encoded with a chosen code-width switch, which no Dart fixture or call produces.
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
        Object::stream(dict, bytes::Bytes::copy_from_slice(data))
    };

    let zero = stream(Some(("EarlyChange", 0)), &late);
    assert_eq!(zero.decode_stream_data().unwrap(), original);
    let one = stream(Some(("EarlyChange", 1)), &early(&original));
    assert_eq!(one.decode_stream_data().unwrap(), original);
    let absent = stream(None, &early(&original));
    assert_eq!(absent.decode_stream_data().unwrap(), original);
}

/// No Dart battery sees it: it needs an LZW stream encoded with a chosen code-width switch, which no Dart fixture or call produces.
#[test]
fn a_corrupt_stream_is_refused() {
    // 9-bit codes: 'A' (65), then 300, a code far past the next free one.
    let bits: u32 = (65 << 9) | 300;
    let bytes = [(bits >> 10) as u8, (bits >> 2) as u8, ((bits & 3) << 6) as u8];
    assert!(LzwDecoder.decode(&bytes).is_err());
}
