//! Image rows as readers: what a page render draws an image from.
//!
//! This file is fork-only code (NOT upstream).
//!
//! The renderer paints a page in bands, and an image reaches into every band
//! it overlaps. An [`ImageCursor`] holds one image's open decode across the
//! bands of a page, so that each stored byte is read about once and the
//! image never exists whole:
//!
//! - A raw-sample image (no image codec in its filters) is read through the
//!   stream's filter readers, a strip of rows at a time. A strip is turned
//!   into pixels by the same extractor a whole image goes through, with the
//!   strip standing in as a small image of its own, so every colour space,
//!   palette, `/Decode` and bit depth means exactly what it means there.
//! - A codec image (DCT, JPX, CCITT, JBIG2) is decoded by its codec, whose
//!   decoder returns a raster: the JPEG at the largest inverse-DCT reduction
//!   that still covers the footprint, the JPEG 2000 codestream at the
//!   resolution level that covers it, the fax and JBIG2 bitmaps at one bit a
//!   pixel. The cursor keeps that raster and reads rows from it.
//! - `/Mask` and `/SMask` images are cursors of their own, read in step with
//!   the rows they mask.
//!
//! Rows are asked for in order. [`ImageCursor::scaled_rows`] yields the rows
//! of the image resampled to a smaller size, through [`RowResampler`];
//! [`ImageCursor::region`] yields a rectangle of the full-size rows. A
//! request that lies behind the cursor starts the decode again from the first
//! row (a flipped image meets every band from the bottom).

use super::resample::RowResampler;
use crate::decoders::{CcittParams, ccitt::FaxRowEngine};
use crate::decoders::ccitt::BitFeed;
use crate::error::{Error, Result};
use crate::extractors::images::{
    decode_jbig2_bits, row_geometry, ImageData, PdfImage,
};
use crate::object::{Object, ObjectRef};
use image::DynamicImage;
use std::collections::{HashMap, VecDeque};
use std::io::Read;

/// Rows a strip of a raw-sample image holds, to about this many bytes of
/// pixels.
const STRIP_BYTES: usize = 256 * 1024;

/// Bytes of finished rows an image cursor keeps behind its position, for the
/// margin rows of the next band's region.
const TAIL_BYTES: usize = 256 * 1024;

/// What a row holds per pixel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Output {
    /// Straight RGBA8.
    Rgba,
    /// One luma byte, for a mask.
    Luma,
}

impl Output {
    fn bytes(self) -> usize {
        match self {
            Output::Rgba => 4,
            Output::Luma => 1,
        }
    }
}

/// Where an image's rows are held while it is read.
enum Kind {
    /// A raw-sample stream read through its filters, a strip at a time.
    Strips(Strips),
    /// The raster a codec decoded, kept as the image crate holds it.
    Raster(DynamicImage),
    /// A bilevel bitmap, packed MSB first, rows padded to a byte. `one` is the
    /// gray value (0 or 255) a set bit stands for; a clear bit is the other.
    Bits { packed: Vec<u8>, row_bytes: usize, one: u8 },
    /// A fax image's rows, decoded a row at a time from the stream: two
    /// transition lines and one row are all the decode holds.
    Fax(Fax),
}

struct Strips {
    /// The stream's dictionary without its filters, which the strips'
    /// readers apply.
    dict: HashMap<String, Object>,
    /// The stream's dictionary as stored, to open its readers from.
    filtered: HashMap<String, Object>,
    body: crate::object::StreamBody,
    origin: Option<crate::decoders::stream::Origin>,
    color_spaces: HashMap<String, Object>,
    row_bytes: usize,
    strip_rows: u32,
    reader: Box<dyn Read>,
    /// Pixels of the rows `first..first + count`.
    pixels: Vec<u8>,
    first: u32,
    count: u32,
    /// A colour-key `/Mask`: per component sample ranges that are clear.
    colour_key: Option<Vec<(u32, u32)>>,
}

impl Strips {
    fn open_reader(
        dict: &HashMap<String, Object>,
        body: &crate::object::StreamBody,
        origin: Option<crate::decoders::stream::Origin>,
    ) -> Result<Box<dyn Read>> {
        let body = body.clone();
        crate::decoders::stream::reader_in(dict, origin, move || body.owned_reader())
    }
}

/// A fax image's rows, decoded a row at a time from the stream's reader.
struct Fax {
    /// The stream's dictionary as stored, to open its reader again from.
    dict: HashMap<String, Object>,
    body: crate::object::StreamBody,
    /// The decode params, `/Decode`'s inversion already folded into
    /// `black_is_1`: a decoded row's `1` bit is the sample the whole decode
    /// reports (1 = ink).
    params: CcittParams,
    /// The row count the reader will ask for: the smaller of `/Rows` and
    /// `/Height`, which the engine serves white past the stream's end.
    height: u32,
    row_bytes: usize,
    engine: FaxRowEngine,
    feed: BitFeed,
    /// The row last decoded, reused across reads. White once the stream has
    /// failed and every remaining row is the blank stand-in.
    row: Vec<u8>,
    failed: bool,
}

impl Fax {
    /// Opens the fax rows of the stream `dict` names: the reader its filters
    /// leave and the CCITT params `params` holds (the caller pulled them from
    /// the dictionary and its colour-space map exactly as the whole decode
    /// does). `height` is the image's `/Height`.
    fn open(
        dict: &HashMap<String, Object>,
        body: &crate::object::StreamBody,
        params: CcittParams,
        height: u32,
    ) -> Result<Kind> {
        // The cursor's engine reads a u16 width; wider than that stays on the
        // whole-decode path, which truncates it the way it always has.
        let width = params.columns;
        if width > u16::MAX as u32 {
            return Err(Error::Image(format!(
                "CCITT image /Width {width} exceeds the row cursor's limit {}",
                u16::MAX
            )));
        }
        let reader = Strips::open_reader(dict, body, None)?;
        Ok(Kind::Fax(Self {
            dict: dict.clone(),
            body: body.clone(),
            height,
            row_bytes: (width as usize).div_ceil(8),
            engine: FaxRowEngine::new(&params)?,
            feed: BitFeed::new(reader),
            params,
            row: Vec::new(),
            failed: false,
        }))
    }

    /// Decodes the next row and expands it into `out`: `width` pixels of
    /// `output`, a set bit inked, exactly as the `Bits` kind reads one.
    /// A stream the engine gives up on leaves the rows white, the blank
    /// stand-in the whole decode makes of such a stream. The reader stops at
    /// `min(/Rows, /Height)`; the rows past that are white as well.
    fn read(&mut self, out: &mut [u8], width: u32, output: Output) -> Result<()> {
        if self.row.len() != self.row_bytes {
            self.row.clear();
            self.row.resize(self.row_bytes, 0);
        }
        let mut failed_now = false;
        if !self.failed {
            match self.engine.pull_row(&mut self.feed, Some(self.height), &mut self.row) {
                Ok(_) => {},
                Err(e) => {
                    log::warn!(
                        "CCITT row cursor gave up ({}x{}, K={}): {e}",
                        self.params.columns,
                        self.height,
                        self.params.k
                    );
                    self.failed = true;
                    failed_now = true;
                },
            }
        }
        if self.failed || failed_now {
            self.row.fill(0);
        }
        // The params' polarity read (and `/Decode` fold, if any) applied: a
        // set bit is ink, as the whole decode's rows carry.
        if self.params.black_is_1 {
            for b in &mut self.row {
                *b = !*b;
            }
        }
        bilevel_row(&self.row, width, 0, output, out);
        Ok(())
    }

    /// Decodes `rows` rows past the cursor without producing them.
    fn skip(&mut self, rows: u32) -> Result<()> {
        if self.row.len() != self.row_bytes {
            self.row.clear();
            self.row.resize(self.row_bytes, 0);
        }
        for _ in 0..rows {
            if self.failed {
                continue; // the rows past the failure are white already
            }
            match self.engine.pull_row(&mut self.feed, Some(self.height), &mut self.row) {
                Ok(_) => {},
                Err(e) => {
                    log::warn!(
                        "CCITT row cursor gave up skipping ({}x{}, K={}): {e}",
                        self.params.columns,
                        self.height,
                        self.params.k
                    );
                    self.failed = true;
                },
            }
        }
        Ok(())
    }

    /// Bytes this source holds in memory: the row and its window, plus the
    /// engine's two transition lines, one column each.
    fn retained(&self) -> usize {
        self.row.capacity()
            + self.feed.buf.capacity()
            + 2 * self.params.columns as usize * 2
    }

    /// Opens the stream's reader and the row decode again from the first row.
    fn restart(&mut self) -> Result<()> {
        let reader = Strips::open_reader(&self.dict, &self.body, None)?;
        self.feed = BitFeed::new(reader);
        self.engine = FaxRowEngine::new(&self.params)?;
        self.row.clear();
        self.failed = false;
        Ok(())
    }
}

/// One image's rows, read from the first to the last.
pub(crate) struct ImageRows {
    width: u32,
    height: u32,
    output: Output,
    /// The next row to read.
    next: u32,
    kind: Kind,
}

/// The `DynamicImage` in the form rows are cut from: the variants whose rows
/// convert without a whole-image pass are kept, anything else is converted
/// once the way the whole-image path converts it.
fn normalised(img: DynamicImage, output: Output) -> DynamicImage {
    match output {
        Output::Luma => match img {
            DynamicImage::ImageLuma8(_) => img,
            other => DynamicImage::ImageLuma8(other.to_luma8()),
        },
        Output::Rgba => match img {
            DynamicImage::ImageLuma8(_)
            | DynamicImage::ImageLumaA8(_)
            | DynamicImage::ImageRgb8(_)
            | DynamicImage::ImageRgba8(_) => img,
            other => DynamicImage::ImageRgba8(other.to_rgba8()),
        },
    }
}

/// Row `y` of `img` as `output` pixels, exactly as the image crate's
/// `to_rgba8` / `to_luma8` give it.
fn dynamic_row(img: &DynamicImage, y: u32, output: Output, out: &mut [u8]) {
    let w = img.width() as usize;
    let y = y as usize;
    match (img, output) {
        (DynamicImage::ImageLuma8(b), Output::Luma) => {
            out.copy_from_slice(&b.as_raw()[y * w..(y + 1) * w]);
        },
        (DynamicImage::ImageLuma8(b), Output::Rgba) => {
            for (g, px) in b.as_raw()[y * w..(y + 1) * w].iter().zip(out.chunks_exact_mut(4)) {
                px.copy_from_slice(&[*g, *g, *g, 255]);
            }
        },
        (DynamicImage::ImageLumaA8(b), Output::Rgba) => {
            for (ga, px) in b.as_raw()[y * w * 2..(y + 1) * w * 2]
                .chunks_exact(2)
                .zip(out.chunks_exact_mut(4))
            {
                px.copy_from_slice(&[ga[0], ga[0], ga[0], ga[1]]);
            }
        },
        (DynamicImage::ImageRgb8(b), Output::Rgba) => {
            for (rgb, px) in b.as_raw()[y * w * 3..(y + 1) * w * 3]
                .chunks_exact(3)
                .zip(out.chunks_exact_mut(4))
            {
                px.copy_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
            }
        },
        (DynamicImage::ImageRgba8(b), Output::Rgba) => {
            out.copy_from_slice(&b.as_raw()[y * w * 4..(y + 1) * w * 4]);
        },
        // `normalised` leaves no other pair.
        _ => out.fill(0),
    }
}

/// One packed bilevel row as `output` pixels: `width` samples, MSB first, `one`
/// the gray value a set bit stands for, `255 - one` a clear bit. What the
/// `Bits` kind reads a row of its whole bitmap as, and what `Fax::read` reads
/// a decoded row as.
fn bilevel_row(row: &[u8], width: u32, one: u8, output: Output, out: &mut [u8]) {
    for (x, px) in out.chunks_exact_mut(output.bytes()).enumerate().take(width as usize) {
        let bit = (row[x / 8] >> (7 - (x % 8))) & 1;
        let gray = if bit == 1 { one } else { 255 - one };
        if output == Output::Rgba {
            px.copy_from_slice(&[gray, gray, gray, 255]);
        } else {
            px[0] = gray;
        }
    }
}

/// Zeroes the alpha of every pixel of `rgba` whose raw component samples all
/// fall in the colour-key `ranges` (ISO 32000-1 §8.9.6.4). `image` holds the
/// samples of the same rows.
///
/// Colour-key masking is defined against the raw pre-Decode samples. Those are
/// only recoverable from an 8-bit `ImageData::Raw` buffer whose per-pixel byte
/// count matches `ranges.len()`. For anything else (JPEG, non-8-bit depths, or
/// a palette-expanded Indexed image whose original indices are lost) the
/// ranges cannot be mapped onto the decoded pixels, so masking is skipped
/// rather than applied incorrectly.
fn apply_colour_key(image: &PdfImage, ranges: &[(u32, u32)], rgba: &mut [u8]) {
    let ncomp = ranges.len();
    let ImageData::Raw { pixels, format } = image.data() else {
        log::debug!("color-key /Mask: non-raw (e.g. JPEG) image, skipping");
        return;
    };
    if image.bits_per_component() != 8 || format.bytes_per_pixel() != ncomp {
        log::debug!(
            "color-key /Mask: unsupported layout (bpc={}, bpp={}, ncomp={}), skipping",
            image.bits_per_component(),
            format.bytes_per_pixel(),
            ncomp
        );
        return;
    }
    if pixels.len() < rgba.len() / 4 * ncomp {
        log::debug!("color-key /Mask: sample buffer too small, skipping");
        return;
    }
    for (samples, px) in pixels.chunks_exact(ncomp).zip(rgba.chunks_exact_mut(4)) {
        if crate::rendering::page_renderer::color_key_pixel_masked(samples, ranges) {
            px[3] = 0;
        }
    }
}

/// Straight RGBA8 to premultiplied, in place. `tiny_skia::Pixmap` stores
/// premultiplied alpha and `Pixmap::from_vec` converts nothing, so every
/// buffer handed to it must already satisfy `r,g,b <= a`; resampling is also
/// only right on premultiplied pixels, since straight alpha bleeds colour
/// across an alpha edge from pixels that contribute none.
pub(crate) fn premultiply_in_place(data: &mut [u8]) {
    let (pixels, _remainder) = data.as_chunks_mut::<4>();
    for px in pixels {
        let a = u32::from(px[3]);
        if a == 255 {
            continue;
        }
        if a == 0 {
            px[0] = 0;
            px[1] = 0;
            px[2] = 0;
            continue;
        }
        // Rounded, so a round trip through premultiply/unpremultiply does not
        // drift downward on every pass.
        px[0] = ((u32::from(px[0]) * a + 127) / 255) as u8;
        px[1] = ((u32::from(px[1]) * a + 127) / 255) as u8;
        px[2] = ((u32::from(px[2]) * a + 127) / 255) as u8;
    }
}

fn is_filter(filters: &[String], names: &[&str]) -> bool {
    filters
        .iter()
        .any(|f| names.iter().any(|n| f.eq_ignore_ascii_case(n)))
}

impl ImageRows {
    /// Opens the image stream `xobject` for reading as `output` rows.
    /// `target` is the size in device pixels it will be painted at: a codec
    /// that decodes in reduced form stops at the reduction that covers it.
    pub(crate) fn open(
        doc: &crate::document::PdfDocument,
        xobject: &Object,
        color_spaces: &HashMap<String, Object>,
        target: Option<(u32, u32)>,
        output: Output,
        colour_key: Option<&[Object]>,
    ) -> Result<Self> {
        use crate::extractors::images::extract_image_from_xobject_at;

        let Object::Stream { dict, body } = xobject else {
            return Err(Error::Image("XObject is not a stream".to_string()));
        };
        let filters = dict
            .get("Filter")
            .map(crate::object::extract_filter_names)
            .unwrap_or_default();

        if is_filter(&filters, &["JBIG2Decode"]) {
            let geometry = row_geometry(Some(doc), dict, Some(color_spaces))?;
            let packed =
                decode_jbig2_bits(xobject, dict, Some(doc), geometry.width, geometry.height, true)?;
            return Ok(Self {
                width: geometry.width,
                height: geometry.height,
                output,
                next: 0,
                kind: Kind::Bits {
                    packed,
                    row_bytes: (geometry.width as usize).div_ceil(8),
                    one: 255,
                },
            });
        }

        let is_codec = is_filter(&filters, &["JPXDecode", "CCITTFaxDecode", "DCTDecode"]);
        if !is_codec {
            let geometry = row_geometry(Some(doc), dict, Some(color_spaces))?;
            let filtered = dict.clone();
            let mut dict = dict.clone();
            // The strips are read through the filters, so the extractor sees
            // plain samples.
            dict.remove("Filter");
            dict.remove("DecodeParms");
            let origin = body.origin();
            let reader = Strips::open_reader(&filtered, body, origin.clone())?;
            let strip_rows = u32::try_from((STRIP_BYTES / (geometry.width as usize * 4).max(1)).max(1))
                .unwrap_or(1)
                .min(geometry.height);
            return Ok(Self {
                width: geometry.width,
                height: geometry.height,
                output,
                next: 0,
                kind: Kind::Strips(Strips {
                    dict,
                    filtered,
                    body: body.clone(),
                    origin,
                    color_spaces: color_spaces.clone(),
                    row_bytes: geometry.row_bytes,
                    strip_rows,
                    reader,
                    pixels: Vec::new(),
                    first: 0,
                    count: 0,
                    colour_key: colour_key.and_then(|ranges| {
                        let parsed = crate::rendering::page_renderer::parse_color_key_mask(
                            ranges,
                            geometry.components,
                        );
                        if parsed.is_none() {
                            log::debug!(
                                "Ignoring malformed color-key /Mask array (ncomp={})",
                                geometry.components
                            );
                        }
                        parsed
                    }),
                }),
            });
        }

        // A fax image is decoded a row at a time; the packed whole bitmap the
        // decode used to produce never exists.
        if is_filter(&filters, &["CCITTFaxDecode"]) {
            let geometry = row_geometry(Some(doc), dict, Some(color_spaces))?;
            if let Some(params) = crate::extractors::images::fax_params(
                Some(doc),
                dict,
                Some(color_spaces),
                geometry.width,
                geometry.height,
            ) {
                // `/Columns` is the row the cursor packs and paints;
                // `/Rows` and `/Height` bound the rows it serves, at the
                // smaller.
                let width = params.columns;
                let height = params.rows.map_or(geometry.height, |rows| rows.min(geometry.height));
                let kind = Fax::open(dict, body, params, height)?;
                return Ok(Self { width, height, output, next: 0, kind });
            }
        }

        let pdf_image = extract_image_from_xobject_at(Some(doc), xobject, None, Some(color_spaces), target)?;
        let mut raster = pdf_image.to_dynamic_image_covering(target)?;
        // A colour-key /Mask is tested against the raw samples, which a JPEG
        // 2000 decode still holds beside the picture. Where a sample is in
        // range the pixel is made clear in the raster itself, so that the
        // raw samples need not outlive the open.
        if is_filter(&filters, &["JPXDecode"]) && pdf_image.samples_are_raw() {
            let ranges = colour_key.and_then(|key| {
                crate::rendering::page_renderer::parse_color_key_mask(
                    key,
                    pdf_image.color_space().components(),
                )
            });
            if let Some(ranges) = ranges {
                let mut rgba = raster.to_rgba8();
                apply_colour_key(&pdf_image, &ranges, &mut rgba);
                raster = DynamicImage::ImageRgba8(rgba);
            }
        }
        let raster = normalised(raster, output);
        Ok(Self {
            width: raster.width(),
            height: raster.height(),
            output,
            next: 0,
            kind: Kind::Raster(raster),
        })
    }

    /// A stencil mask (`/ImageMask true`) that has no colour space, as luma
    /// rows: 255 where the base image shows through, 0 where it is masked out
    /// (ISO 32000-1 §8.9.6.2: under the default `/Decode`, sample 0 marks the
    /// page, which for an explicit mask means the base image shows).
    pub(crate) fn open_stencil(
        doc: &crate::document::PdfDocument,
        xobject: &Object,
    ) -> Result<Self> {
        let Object::Stream { dict, body } = xobject else {
            return Err(Error::Image("mask is not a stream".to_string()));
        };
        let is_image_mask = matches!(dict.get("ImageMask"), Some(Object::Boolean(true)));
        if !is_image_mask {
            return Err(Error::Image("mask is not an image mask".to_string()));
        }
        // `as u32` turns a negative /Width into a near-u32::MAX stride.
        let dimension = |key: &str| {
            dict.get(key)
                .and_then(|o| o.as_integer())
                .and_then(|v| u32::try_from(v).ok())
                .filter(|v| *v > 0)
                .ok_or_else(|| Error::Image(format!("mask /{key} must be positive")))
        };
        let (mw, mh) = (dimension("Width")?, dimension("Height")?);
        let row_bytes = (mw as usize).div_ceil(8);
        let expected = row_bytes * mh as usize;
        let filters = dict
            .get("Filter")
            .map(crate::object::extract_filter_names)
            .unwrap_or_default();
        let decode_reverses = dict
            .get("Decode")
            .and_then(|o| o.as_array())
            .and_then(|a| a.first())
            .map(|o| o.as_real().or_else(|| o.as_integer().map(|i| i as f64)).unwrap_or(0.0) >= 0.5)
            .unwrap_or(false);

        // The packed 1-bit samples of the stencil. A mask the codec could not
        // decode has no sample to test: it reads as the base image visible,
        // the same result an absent /Mask gives.
        let packed = if is_filter(&filters, &["JBIG2Decode"]) {
            crate::extractors::images::decode_jbig2_stencil(xobject, dict, Some(doc), mw, mh)
                .unwrap_or_default()
        } else if is_filter(&filters, &["CCITTFaxDecode"]) {
            // The stencil's codestream is decoded a row at a time off the
            // stream's reader, never held whole. The decoded rows are
            // complemented so `0` is the inked sample the `/Decode` mapping
            // below reads, and rows the cursor could not serve stay white —
            // the blank stand-in the whole decode made of a stream it could
            // not read.
            let decode_parms = dict.get("DecodeParms").and_then(|o| match o {
                Object::Reference(_) => doc.resolve_object(o).ok(),
                other => Some(other.clone()),
            });
            let mut params =
                crate::object::extract_ccitt_params_with_width(decode_parms.as_ref(), Some(mw))
                    .unwrap_or(crate::decoders::CcittParams {
                        // The filter's default: Group 3, pure 1-D.
                        k: 0,
                        columns: mw,
                        ..Default::default()
                    });
            params.columns = mw;
            params.rows = Some(mh);
            let rows = crate::decoders::ccitt::FaxRowEngine::new(&params);
            let mut packed = vec![0u8; expected];
            match rows {
                Ok(mut engine) => {
                    if params.columns <= u16::MAX as u32 {
                        let reader = Strips::open_reader(dict, body, None)?;
                        let mut feed = BitFeed::new(reader);
                        for row in packed.chunks_exact_mut(row_bytes) {
                            if engine.pull_row(&mut feed, Some(mh), row).is_err() {
                                log::warn!("CCITT stencil rows stopped early: the cursor could not read a row");
                                break; // the rest stay white padding
                            }
                            if params.black_is_1 {
                                for b in row.iter_mut() {
                                    *b = !*b;
                                }
                            }
                        }
                    } else {
                        log::warn!(
                            "CCITT stencil /Width {mw} exceeds the row cursor's limit {}",
                            u16::MAX
                        );
                    }
                },
                Err(e) => {
                    log::warn!("CCITT stencil rows unreadable: {e}");
                },
            }
            // The decoder emits 1 for an inked pixel, the complement of the
            // sample value the rule below is written against.
            for b in &mut packed {
                *b = !*b;
            }
            packed
        } else {
            let raw = xobject.decode_stream_data()?;
            if raw.len() < expected / 2 {
                // Data is still compressed: Group 4 CCITT.
                let k = dict
                    .get("DecodeParms")
                    .and_then(|o| o.as_dict())
                    .and_then(|d| d.get("K"))
                    .and_then(|o| o.as_integer())
                    .unwrap_or(0);
                if k == -1 {
                    #[allow(deprecated)]
                    match crate::extractors::ccitt_bilevel::decompress_ccitt_group4(&raw, mw, mh) {
                        Ok(mut decompressed) => {
                            // The CCITT decoder emits 1 for an inked pixel,
                            // the complement of the sample value the rule
                            // below is written against.
                            for b in &mut decompressed {
                                *b = !*b;
                            }
                            decompressed
                        },
                        Err(_) => raw,
                    }
                } else {
                    raw
                }
            } else {
                raw
            }
        };
        // A set bit lets the base image show only when /Decode reverses;
        // otherwise a clear bit does. Past the end of the stream there is no
        // sample to test, so the base image stays visible: the padding is the
        // bit that does.
        let pad = if decode_reverses { 0xFF } else { 0x00 };
        let mut packed = packed;
        if packed.len() < expected {
            packed.resize(expected, pad);
        }
        Ok(Self {
            width: mw,
            height: mh,
            output: Output::Luma,
            next: 0,
            kind: Kind::Bits {
                packed,
                row_bytes,
                one: if decode_reverses { 255 } else { 0 },
            },
        })
    }

    pub(crate) fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Bytes this source holds in memory.
    fn retained_bytes(&self) -> usize {
        match &self.kind {
            Kind::Strips(s) => {
                s.pixels.capacity() + s.body.inline().map_or(0, |bytes| bytes.len())
            },
            Kind::Raster(img) => img.as_bytes().len(),
            Kind::Bits { packed, .. } => packed.len(),
            Kind::Fax(fax) => fax.retained(),
        }
    }

    /// Starts again from the first row.
    fn restart(&mut self) -> Result<()> {
        self.next = 0;
        if let Kind::Strips(s) = &mut self.kind {
            s.reader = Strips::open_reader(&s.filtered, &s.body, s.origin.clone())?;
            s.pixels.clear();
            s.first = 0;
            s.count = 0;
        }
        if let Kind::Fax(fax) = &mut self.kind {
            fax.restart()?;
        }
        Ok(())
    }

    /// Passes over `rows` rows without producing them.
    fn skip(&mut self, rows: u32) -> Result<()> {
        let target = (self.next + rows).min(self.height);
        if let Kind::Strips(s) = &mut self.kind {
            let read_to = s.first + s.count;
            if target > read_to {
                let discard = u64::from(target - read_to) * s.row_bytes as u64;
                std::io::copy(&mut s.reader.by_ref().take(discard), &mut std::io::sink())
                    .map_err(crate::encryption::stream::from_io)?;
                s.pixels.clear();
                s.first = target;
                s.count = 0;
            }
        }
        if let Kind::Fax(fax) = &mut self.kind {
            fax.skip(target - self.next)?;
        }
        self.next = target;
        Ok(())
    }

    /// Reads the next row into `out`, `width * output.bytes()` bytes.
    pub(crate) fn read(&mut self, doc: &crate::document::PdfDocument, out: &mut [u8]) -> Result<()> {
        let y = self.next;
        if y >= self.height {
            return Err(Error::Image("image has no more rows".to_string()));
        }
        let (width, output) = (self.width, self.output);
        match &mut self.kind {
            Kind::Raster(img) => dynamic_row(img, y, output, out),
            Kind::Bits { packed, row_bytes, one } => {
                let row = &packed[y as usize * *row_bytes..(y as usize + 1) * *row_bytes];
                bilevel_row(row, width, *one, output, out);
            },
            Kind::Fax(fax) => {
                // `Fax::read` decodes and expands one row, `black_is_1`
                // folded into the params it opens with.
                fax.read(out, width, output)?;
            },
            Kind::Strips(s) => {
                if y >= s.first + s.count {
                    Self::fill(s, doc, y, width, self.height, output)?;
                }
                let stride = width as usize * output.bytes();
                let at = (y - s.first) as usize * stride;
                out.copy_from_slice(&s.pixels[at..at + stride]);
            },
        }
        self.next += 1;
        Ok(())
    }

    /// Decodes the strip that starts at row `first`.
    fn fill(
        s: &mut Strips,
        doc: &crate::document::PdfDocument,
        first: u32,
        width: u32,
        height: u32,
        output: Output,
    ) -> Result<()> {
        use crate::extractors::images::extract_image_from_xobject_at;

        let rows = s.strip_rows.min(height - first);
        let mut data = vec![0u8; rows as usize * s.row_bytes];
        // A stream shorter than its declared size is padded with zero
        // samples, the way the whole-image path pads it.
        let mut filled = 0;
        while filled < data.len() {
            match s.reader.read(&mut data[filled..]) {
                Ok(0) => break,
                Ok(n) => filled += n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {},
                Err(e) => return Err(crate::encryption::stream::from_io(e)),
            }
        }
        let mut dict = s.dict.clone();
        dict.insert("Height".to_string(), Object::Integer(i64::from(rows)));
        let strip = Object::stream(dict, data);
        let pdf_image =
            extract_image_from_xobject_at(Some(doc), &strip, None, Some(&s.color_spaces), None)?;
        let dynamic = pdf_image.to_dynamic_image()?;
        if dynamic.width() != width || dynamic.height() != rows {
            return Err(Error::Image(format!(
                "image strip is {}x{}, expected {}x{}",
                dynamic.width(),
                dynamic.height(),
                width,
                rows
            )));
        }
        let dynamic = normalised(dynamic, output);
        let stride = width as usize * output.bytes();
        s.pixels.clear();
        s.pixels.resize(rows as usize * stride, 0);
        for r in 0..rows {
            dynamic_row(&dynamic, r, output, &mut s.pixels[r as usize * stride..(r as usize + 1) * stride]);
        }
        if let (Some(ranges), Output::Rgba) = (&s.colour_key, output) {
            if pdf_image.samples_are_raw() {
                apply_colour_key(&pdf_image, ranges, &mut s.pixels);
            } else {
                // The extractor mapped a non-default /Decode into the stored
                // samples, so they are no longer in the space the /Mask
                // ranges are expressed in. Masking them here would hide the
                // wrong pixels.
                log::warn!(
                    "Skipping colour-key /Mask: the stored samples are no longer in the \
                     raw sample space the mask ranges are expressed in"
                );
            }
        }
        s.first = first;
        s.count = rows;
        Ok(())
    }
}

/// A mask read in step with the image it masks: a gray image, sampled
/// nearest-neighbour onto the base image's grid (§8.9.6.3 maps both to the
/// same unit square, so they need not share a resolution).
struct MaskRows {
    rows: ImageRows,
    /// The mask row held and its samples.
    held: Option<(u32, Vec<u8>)>,
    /// Set when the mask could not be read: it masks nothing from then on.
    /// A mask that cannot be read has no sample to test, so the base image
    /// shows through, the result an absent mask gives; resolving it the other
    /// way would erase the picture the mask only meant to refine.
    failed: bool,
}

impl MaskRows {
    fn new(rows: ImageRows) -> Self {
        Self { rows, held: None, failed: false }
    }

    fn restart(&mut self) {
        self.held = None;
        if self.rows.restart().is_err() {
            self.failed = true;
        }
    }

    /// Multiplies the alpha of base row `y` (of `base_h` rows, `row` the RGBA8
    /// of `base_w` pixels) by the mask sample under each pixel.
    fn apply(
        &mut self,
        doc: &crate::document::PdfDocument,
        y: u32,
        base_w: u32,
        base_h: u32,
        row: &mut [u8],
    ) -> Result<()> {
        let (mw, mh) = self.rows.size();
        let my = ((u64::from(y) * u64::from(mh) / u64::from(base_h)) as u32).min(mh - 1);
        if self.held.as_ref().map(|(index, _)| *index) != Some(my) {
            if my < self.rows.next {
                self.rows.restart()?;
            }
            self.rows.skip(my - self.rows.next)?;
            let mut samples = self.held.take().map(|(_, v)| v).unwrap_or_default();
            samples.clear();
            samples.resize(mw as usize, 0);
            self.rows.read(doc, &mut samples)?;
            self.held = Some((my, samples));
        }
        let Some((_, samples)) = &self.held else {
            return Ok(());
        };
        for x in 0..base_w {
            let mx = ((u64::from(x) * u64::from(mw) / u64::from(base_w)) as u32).min(mw - 1);
            let px = &mut row[x as usize * 4..x as usize * 4 + 4];
            px[3] = ((u32::from(px[3]) * u32::from(samples[mx as usize])) / 255) as u8;
        }
        Ok(())
    }
}

/// An image open across the bands of a page.
pub(crate) struct ImageCursor {
    base: ImageRows,
    masks: Vec<MaskRows>,
    resampler: Option<RowResampler>,
    /// The next resampled row to produce.
    next_scaled: u32,
    /// The last full-size rows read, premultiplied: rows
    /// `tail_first..base.next`.
    tail: VecDeque<Vec<u8>>,
    tail_first: u32,
}

impl ImageCursor {
    /// Opens the image `xobject`; `mask` is its `/Mask` entry (a reference to
    /// a mask image, or a colour-key array) and `smask` its `/SMask` entry.
    pub(crate) fn open(
        doc: &crate::document::PdfDocument,
        xobject: &Object,
        color_spaces: &HashMap<String, Object>,
        smask: Option<&Object>,
        mask: Option<&Object>,
        target: Option<(u32, u32)>,
    ) -> Result<Self> {
        let colour_key = match mask {
            Some(Object::Array(ranges)) => Some(ranges.as_slice()),
            _ => None,
        };
        let base = ImageRows::open(doc, xobject, color_spaces, target, Output::Rgba, colour_key)?;
        let mut masks = Vec::new();

        // /Mask: a stencil or gray image, "1 = opaque" after the image's own
        // /Decode (§8.9.6.3).
        if let Some(reference) = mask.and_then(|m| m.as_reference()) {
            if let Ok(stream) = doc.load_object(reference) {
                match ImageRows::open(doc, &stream, color_spaces, target, Output::Luma, None) {
                    Ok(rows) => masks.push(MaskRows::new(rows)),
                    Err(_) => {
                        if let Ok(rows) = ImageRows::open_stencil(doc, &stream) {
                            masks.push(MaskRows::new(rows));
                        }
                    },
                }
            }
        }
        if let Some(smask) = smask {
            if let Ok(resolved) = doc.resolve_object(smask) {
                if let Ok(rows) =
                    ImageRows::open(doc, &resolved, color_spaces, target, Output::Luma, None)
                {
                    masks.push(MaskRows::new(rows));
                }
            }
        }
        Ok(Self {
            base,
            masks,
            resampler: None,
            next_scaled: 0,
            tail: VecDeque::new(),
            tail_first: 0,
        })
    }

    /// The image's size in samples, which a codec may have reduced.
    pub(crate) fn size(&self) -> (u32, u32) {
        self.base.size()
    }

    /// Bytes this cursor holds in memory: the decoded raster or strip, the
    /// masks' the same, the rows kept behind the position and the resampler's
    /// rows, with 16 KiB for what the cursor is itself (its dictionaries, the
    /// colour spaces it was opened with and the state of its filter readers).
    fn retained_bytes(&self) -> usize {
        let masks: usize = self
            .masks
            .iter()
            .map(|m| m.rows.retained_bytes() + m.held.as_ref().map_or(0, |(_, v)| v.capacity()))
            .sum();
        let tail: usize = self.tail.iter().map(|row| row.capacity()).sum();
        let resampler = self.resampler.as_ref().map_or(0, |r| r.retained_bytes());
        16 * 1024 + self.base.retained_bytes() + masks + tail + resampler
    }

    /// Resamples to `dst_w x dst_h`; fixed for the cursor's life.
    pub(crate) fn scale_to(&mut self, dst_w: u32, dst_h: u32) {
        if self.resampler.is_none() {
            let (w, h) = self.base.size();
            self.resampler = Some(RowResampler::new(w, h, dst_w, dst_h));
        }
    }

    /// Reads the next full-size row as straight RGBA8, its masks applied to
    /// the alpha.
    pub(crate) fn read_straight(
        &mut self,
        doc: &crate::document::PdfDocument,
        out: &mut [u8],
    ) -> Result<()> {
        let y = self.base.next;
        let (w, h) = self.base.size();
        self.base.read(doc, out)?;
        for mask in self.masks.iter_mut().filter(|m| !m.failed) {
            if let Err(e) = mask.apply(doc, y, w, h, out) {
                log::debug!("image mask could not be read, so it masks nothing: {e}");
                mask.failed = true;
            }
        }
        Ok(())
    }

    /// Whether the image has a mask that is read with it.
    pub(crate) fn has_masks(&self) -> bool {
        !self.masks.is_empty()
    }

    /// Reads the next full-size row, masks applied and premultiplied.
    fn pull(&mut self, doc: &crate::document::PdfDocument, out: &mut [u8]) -> Result<()> {
        self.read_straight(doc, out)?;
        premultiply_in_place(out);
        Ok(())
    }

    fn restart(&mut self) -> Result<()> {
        self.base.restart()?;
        for mask in &mut self.masks {
            mask.restart();
        }
        if let Some(resampler) = &mut self.resampler {
            resampler.reset();
        }
        self.next_scaled = 0;
        self.tail.clear();
        self.tail_first = 0;
        Ok(())
    }

    /// Resampled rows `first..end` as premultiplied RGBA8, each
    /// `dst_w * 4` bytes. A request behind the last row produced starts the
    /// decode over.
    pub(crate) fn scaled_rows(
        &mut self,
        doc: &crate::document::PdfDocument,
        first: u32,
        end: u32,
    ) -> Result<Vec<u8>> {
        let Some(resampler) = self.resampler.as_ref() else {
            return Err(Error::Image("image cursor has no scale".to_string()));
        };
        let (dst_w, dst_h) = resampler.dst_size();
        let end = end.min(dst_h);
        if first >= end {
            return Ok(Vec::new());
        }
        if first < self.next_scaled {
            self.restart()?;
        }
        let (w, _) = self.base.size();
        let mut row = vec![0u8; w as usize * 4];
        let mut out = vec![0u8; (end - first) as usize * dst_w as usize * 4];
        for d in first..end {
            let (start, stop) = self.resampler.as_ref().map_or((0, 0), |r| r.source_rows_of(d));
            while self.resampler.as_ref().is_some_and(|r| r.next_source_row() < start) {
                self.base.skip(1)?;
                if let Some(r) = self.resampler.as_mut() {
                    r.skip();
                }
            }
            while self.resampler.as_ref().is_some_and(|r| r.next_source_row() < stop) {
                self.pull(doc, &mut row)?;
                if let Some(r) = self.resampler.as_mut() {
                    r.feed(&row);
                }
            }
            let at = (d - first) as usize * dst_w as usize * 4;
            if let Some(r) = self.resampler.as_mut() {
                r.take(d, &mut out[at..at + dst_w as usize * 4]);
            }
            self.next_scaled = d + 1;
        }
        Ok(out)
    }

    /// The rectangle columns `x0..x1`, rows `y0..y1` of the full-size image as
    /// premultiplied RGBA8. Rows are asked for from the top down; the last
    /// few are kept so that the next request may begin a little above where
    /// this one ended, and one that begins above those starts the decode over.
    pub(crate) fn region(
        &mut self,
        doc: &crate::document::PdfDocument,
        x0: u32,
        x1: u32,
        y0: u32,
        y1: u32,
    ) -> Result<Vec<u8>> {
        let (w, h) = self.base.size();
        let (x1, y1) = (x1.min(w), y1.min(h));
        if x0 >= x1 || y0 >= y1 {
            return Ok(Vec::new());
        }
        if y0 < self.tail_first {
            self.restart()?;
        }
        if y0 > self.base.next {
            let skip = y0 - self.base.next;
            self.base.skip(skip)?;
            self.tail.clear();
            self.tail_first = self.base.next;
        }
        let width = (x1 - x0) as usize;
        let mut out = vec![0u8; width * (y1 - y0) as usize * 4];
        for y in y0..y1 {
            if y >= self.base.next {
                let mut row = vec![0u8; w as usize * 4];
                self.pull(doc, &mut row)?;
                self.tail.push_back(row);
            }
            let row = &self.tail[(y - self.tail_first) as usize];
            let at = (y - y0) as usize * width * 4;
            out[at..at + width * 4].copy_from_slice(&row[x0 as usize * 4..x1 as usize * 4]);
        }
        let keep = (TAIL_BYTES / (w as usize * 4)).max(1);
        while self.tail.len() > keep {
            self.tail.pop_front();
            self.tail_first += 1;
        }
        Ok(out)
    }
}

/// Premultiplied RGBA8 of the columns `x0..x1`, rows `y0..y1` of a packed
/// 1-bit stencil, `colour` where the stencil paints and clear elsewhere. A
/// sample paints where it is 0, or where it is 1 when `invert`; a byte past
/// the end of `packed` reads as 0.
pub(crate) fn stencil_region(
    packed: &[u8],
    row_bytes: usize,
    invert: bool,
    colour: [u8; 4],
    x0: u32,
    x1: u32,
    y0: u32,
    y1: u32,
) -> Vec<u8> {
    let width = (x1 - x0) as usize;
    let mut out = vec![0u8; width * (y1 - y0) as usize * 4];
    for y in y0..y1 {
        let row = y as usize * row_bytes;
        for x in x0..x1 {
            let byte = packed.get(row + x as usize / 8).copied().unwrap_or(0);
            let bit = (byte >> (7 - (x % 8))) & 1 == 1;
            if bit == invert {
                let at = ((y - y0) as usize * width + (x - x0) as usize) * 4;
                out[at..at + 4].copy_from_slice(&colour);
            }
        }
    }
    out
}

/// What a page's open images are keyed by: the stream, or for an inline image
/// the hash of its data, and the transform the image is painted under.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct CursorKey {
    source: Source,
    transform: [u32; 6],
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Source {
    Reference(ObjectRef),
    Stream(u64),
}

impl CursorKey {
    pub(crate) fn new(obj_ref: Option<ObjectRef>, xobject: &Object, transform: [f32; 6]) -> Self {
        use std::hash::{Hash, Hasher};
        let source = match (obj_ref, xobject) {
            (Some(r), _) => Source::Reference(r),
            (None, Object::Stream { body, .. }) => {
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                match body.inline() {
                    Some(bytes) => bytes.hash(&mut hasher),
                    None => format!("{body:?}").hash(&mut hasher),
                }
                Source::Stream(hasher.finish())
            },
            (None, _) => Source::Stream(0),
        };
        Self { source, transform: transform.map(f32::to_bits) }
    }
}

/// Bytes of open images a page keeps beside the one it is drawing. An image
/// kept past this is read again from its first row when it is next drawn.
const TABLE_BYTES: usize = 4 * 1024 * 1024;

/// A page's open images. One that is not drawn in the next band is of no use
/// to a page that paints thousands of inline images, so the table holds a
/// byte budget of them and puts the least recently drawn out.
#[derive(Default)]
pub(crate) struct CursorTable {
    open: HashMap<CursorKey, (u64, usize, ImageCursor)>,
    bytes: usize,
    clock: u64,
}

impl CursorTable {
    pub(crate) fn clear(&mut self) {
        self.open.clear();
        self.bytes = 0;
    }

    /// Bytes the open images hold.
    #[cfg(test)]
    pub(crate) fn bytes(&self) -> usize {
        self.bytes
    }

    /// The open image for `key`, which leaves the table while it is drawn.
    pub(crate) fn take(&mut self, key: &CursorKey) -> Option<ImageCursor> {
        let (_, bytes, cursor) = self.open.remove(key)?;
        self.bytes -= bytes;
        Some(cursor)
    }

    /// Keeps `cursor` as the most recently drawn, and puts out older ones
    /// while the table holds more than its budget. The cursor just drawn is
    /// kept whatever it holds: a raster that is one image's alone is the
    /// decode, and dropping it would repeat the decode in the next band.
    pub(crate) fn put(&mut self, key: CursorKey, cursor: ImageCursor) {
        self.clock += 1;
        let bytes = cursor.retained_bytes();
        self.bytes += bytes;
        self.open.insert(key.clone(), (self.clock, bytes, cursor));
        while self.bytes > TABLE_BYTES {
            let oldest = self
                .open
                .iter()
                .filter(|(k, _)| **k != key)
                .min_by_key(|(_, (stamp, _, _))| *stamp)
                .map(|(k, _)| k.clone());
            let Some(oldest) = oldest else {
                break;
            };
            if let Some((_, bytes, _)) = self.open.remove(&oldest) {
                self.bytes -= bytes;
            }
        }
    }
}
