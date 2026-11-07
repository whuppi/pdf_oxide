//! A JPEG painted smaller than itself is decoded from its stream, not from a copy of its file.

use super::filter_readers::thread_growth;
use super::support::{pdf_from_objects, stream_object};
use crate::document::PdfDocument;
use crate::extractors::images::extract_image_from_xobject_at;
use crate::extractors::{ColorSpace, ImageData, PdfImage};
use crate::object::ObjectRef;

fn noise_jpeg(color: jpeg_encoder::ColorType, width: u16, height: u16, quality: u8) -> Vec<u8> {
    let bytes = match color {
        jpeg_encoder::ColorType::Luma => 1,
        _ => 3,
    };
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let pixels: Vec<u8> = (0..width as usize * height as usize * bytes)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 24) as u8
        })
        .collect();
    let mut out = Vec::new();
    jpeg_encoder::Encoder::new(&mut out, quality).encode(&pixels, width, height, color).unwrap();
    out
}

fn document(space: &str, width: u16, height: u16, jpeg: &[u8]) -> PdfDocument {
    PdfDocument::from_bytes(pdf_from_objects(&[
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_vec(),
        b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n".to_vec(),
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] >>\nendobj\n".to_vec(),
        stream_object(
            4,
            &format!(
                "/Type /XObject /Subtype /Image /Width {width} /Height {height} /ColorSpace {space} \
                 /BitsPerComponent 8 /Filter /DCTDecode"
            ),
            jpeg,
        ),
    ]))
    .unwrap()
}

/// What the whole bytes of the file give at `target`: the decode this change
/// moved onto the stream.
fn from_bytes(space: ColorSpace, width: u16, height: u16, jpeg: &[u8], target: (u32, u32)) -> image::DynamicImage {
    PdfImage::new(u32::from(width), u32::from(height), space, 8, ImageData::Jpeg(jpeg.to_vec()))
        .to_dynamic_image_covering(Some(target))
        .unwrap()
}

fn from_stream(doc: &PdfDocument, target: Option<(u32, u32)>) -> (image::DynamicImage, bool) {
    let object = doc.load_object(ObjectRef::new(4, 0)).unwrap();
    let image = extract_image_from_xobject_at(Some(doc), &object, None, None, target).unwrap();
    let held = matches!(image.data(), ImageData::Jpeg(_));
    (image.to_dynamic_image_covering(target).unwrap(), held)
}

/// A gray and an RGB JPEG painted at an eighth, a quarter, and one size that
/// asks for no reduction decode to the pixels the file's bytes give, and
/// the extraction keeps the file only where the decode needs it whole. No
/// Dart battery can observe this: the picture is the same either way.
#[test]
fn a_jpeg_decodes_at_its_target_to_the_pixels_of_its_bytes() {
    for (name, color, space, doc_space) in [
        ("gray", jpeg_encoder::ColorType::Luma, ColorSpace::DeviceGray, "/DeviceGray"),
        ("rgb", jpeg_encoder::ColorType::Rgb, ColorSpace::DeviceRGB, "/DeviceRGB"),
    ] {
        let jpeg = noise_jpeg(color, 192, 128, 90);
        let doc = document(doc_space, 192, 128, &jpeg);
        for target in [(24, 16), (48, 32), (96, 64), (192, 128), (400, 300)] {
            let (got, held) = from_stream(&doc, Some(target));
            let want = from_bytes(space, 192, 128, &jpeg, target);
            assert!(got.as_bytes() == want.as_bytes(), "{name} at {target:?}: the pixels");
            assert_eq!((got.width(), got.height()), (want.width(), want.height()), "{name} at {target:?}");
            assert_eq!(held, target.0 >= 192, "{name} at {target:?}: whether the file is kept");
        }
        let (whole, held) = from_stream(&doc, None);
        assert!(held, "{name}: without a target the file is the image");
        assert!(whole.as_bytes() == from_bytes(space, 192, 128, &jpeg, (192, 128)).as_bytes());
    }
}

/// A JPEG of 14 MiB painted at an eighth of its size is decoded with the
/// thread growing by the raster and the decoder's rows, not by the file. No
/// Dart battery can observe this: the picture is the same either way, and
/// what differs is a copy of the file held for the length of the call.
#[test]
fn a_jpeg_painted_smaller_is_not_held_as_a_file() {
    let jpeg = noise_jpeg(jpeg_encoder::ColorType::Rgb, 2048, 1536, 100);
    assert!(jpeg.len() > 12 * 1024 * 1024, "the fixture is {} bytes", jpeg.len());
    let doc = document("/DeviceRGB", 2048, 1536, &jpeg);
    let ((_, held), growth) = thread_growth(|| from_stream(&doc, Some((256, 192))));
    assert!(!held);
    assert!(growth < 4 * 1024 * 1024, "decoding a {} byte JPEG at an eighth grew the thread by {growth} bytes", jpeg.len());
}
