//! Write-time redaction of one page's content.
//!
//! The page's operators stream through [`RedactPass`] one at a time: each is
//! decided by the text engine and its survivors are written at once, so the
//! rewritten content never exists whole in memory. The same pass runs into a
//! byte counter when a redaction is queued and into the file when it is
//! saved; both produce the same [`RedactionReport`].

use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};

mod paths;

use paths::{is_path_op, paint_of, redact_path, PathRedaction};
use super::image_prune::{classify_image_placement, invert_affine};
use super::options::{RedactionOptions, RedactionReport};
use super::overlay::{region_overlay_ops, resolved_fill};
use super::region::RegionSet;
use super::serialize::serialize_operator;
use super::text_engine::{FontMetrics, TextPass};
use crate::content::operators::Operator;
use crate::content::graphics_state::Matrix;
use crate::error::{Error, Result};
use crate::host::content::{InlineImages, Operators};
use crate::host::images::redact_copy::{covered_span, ImagePlacement};
use crate::object::{Object, ObjectRef};

/// The page's image XObjects, by their resource name: the lookup a `Do`
/// decides from. A page whose resource dictionaries the engine cannot read
/// resolves to nothing: every `Do` passes through and no image is redacted
/// (T6c owns forms).
pub struct ImageResources {
    /// Resource name → the XObject's reference, for XObjects the engine
    /// reads as images (`/Subtype /Image` or `/ImageMask true`).
    pub images: HashMap<String, ObjectRef>,
    /// Resource name → the line style of an ExtGState the page names. A name
    /// whose dictionary cannot be read maps to `None`, and a `gs` of it refuses.
    pub line_styles: HashMap<String, Option<LineStyle>>,
}

impl ImageResources {
    /// An empty registry: every `Do` passes through unchanged, and a `gs`
    /// refuses, since no ExtGState can be read.
    pub fn none() -> Self {
        Self {
            images: HashMap::new(),
            line_styles: HashMap::new(),
        }
    }
}

/// The line-style entries of an ExtGState the stroke bound reads: `/LW`,
/// `/LJ`, `/ML` and `/LC`. An entry the dictionary does not set is `None`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LineStyle {
    /// Line width (`/LW`).
    pub lw: Option<f32>,
    /// Line join style (`/LJ`): 0 miter, 1 round, 2 bevel.
    pub lj: Option<u8>,
    /// Miter limit (`/ML`).
    pub ml: Option<f32>,
    /// Line cap style (`/LC`): 0 butt, 1 round, 2 projecting square.
    pub lc: Option<u8>,
}

/// Rounds the u/v keys of a covered rect: two `Do`s of one source over the
/// same rect share one copy.
fn rect_key(u0: f32, v0: f32, u1: f32, v1: f32) -> u128 {
    (u0.to_bits() as u128) << 96
        | (v0.to_bits() as u128) << 64
        | (u1.to_bits() as u128) << 32
        | v1.to_bits() as u128
}

/// What the pass hands the caller instead of a decided operator.
enum Decision {
    /// The operator goes; nothing is written.
    Dropped,
    /// The operator is written as the rewritten one it carries.
    UsedAs(Operator),
}

/// What one run of the pass produced: its report and the queue the save
/// writes from — one placement per overwritten copy, the sources those
/// touched, and the XObject names the page's surviving `Do`s still name.
pub struct PassOutcome {
    /// The counts the call reports.
    pub report: RedactionReport,
    /// One per overwritten copy, in decision order.
    pub placements: Vec<ImagePlacement>,
    /// The object ids of the images the pass overwrote or dropped.
    pub image_originals: Vec<u32>,
    /// The XObject names the page's surviving `Do`s still use.
    pub kept_xobject_names: HashSet<String>,
}

/// Redacts one page's operators into a writer.
pub struct RedactPass<'a> {
    regions: &'a RegionSet,
    opts: &'a RedactionOptions,
    text: TextPass<'a>,
    images: &'a ImageResources,
    /// The overwritten placements the pass queues, in decision order.
    placements: Vec<ImagePlacement>,
    /// `source → name → (rect, name)`: a copy already made of the same
    /// source over the same rect, which a second placement shares.
    copies: HashMap<u32, HashMap<u128, String>>,
    /// The sources whose placements were overwritten or their images dropped.
    image_originals: Vec<u32>,
    imgs_removed: usize,
    imgs_modified: usize,
    inline_modified: usize,
    /// The resource names the page's surviving `Do`s still use, kept in the
    /// redacted page's own /Resources.
    kept_xobject_names: HashSet<String>,
    /// The rewritten form of the operator being decided, reused.
    ops: Vec<Operator>,
    /// The bytes of one rewritten operator, reused.
    bytes: Vec<u8>,
    /// The construction and clip operators of the path being read, held
    /// until its paint operator decides them.
    path: Vec<Operator>,
    paths_pruned: usize,
}

/// Records a source id once in `originals`, the set the page's overwritten
/// and dropped images keep.
fn note_original(originals: &mut Vec<u32>, id: u32) {
    if !originals.contains(&id) {
        originals.push(id);
    }
}

impl<'a> RedactPass<'a> {
    /// A pass that redacts `regions` with `opts`' padding and overlay, glyph
    /// widths from `fonts`, and the page's image XObjects by `images`.
    pub fn new(
        regions: &'a RegionSet,
        opts: &'a RedactionOptions,
        fonts: &'a dyn FontMetrics,
        images: &'a ImageResources,
    ) -> Self {
        Self {
            regions,
            opts,
            text: TextPass::new(regions, opts.edge_padding, fonts),
            images,
            placements: Vec::new(),
            copies: HashMap::new(),
            image_originals: Vec::new(),
            imgs_removed: 0,
            imgs_modified: 0,
            inline_modified: 0,
            kept_xobject_names: HashSet::new(),
            ops: Vec::new(),
            bytes: Vec::new(),
            path: Vec::new(),
            paths_pruned: 0,
        }
    }

    /// Writes the page's rewritten content to `out`: its operators inside
    /// `q … Q`, so the overlays cannot inherit a CTM the content left
    /// behind, then one overlay per region.
    ///
    /// # Errors
    /// - [`Error::Unsupported`] when the content is damaged, or shows text in
    ///   a font whose glyphs cannot be pruned, or holds an image whose
    ///   copy the engine cannot write. Whatever reached `out` before
    ///   the error is not a valid redaction; the caller discards it.
    /// - [`Error::Io`] when `out` fails.
    pub fn run<R: Read, W: Write>(
        mut self,
        content: Operators<R>,
        out: &mut W,
    ) -> Result<PassOutcome> {
        let mut content = content.with_inline_images(InlineImages::Collect);
        out.write_all(b"q\n")?;
        for item in content.by_ref() {
            let op = item?;
            if is_path_op(&op) {
                self.path.push(op);
                continue;
            }
            if paint_of(&op).is_some() {
                let pending = std::mem::take(&mut self.path);
                let decided = redact_path(
                    &pending,
                    &op,
                    self.text.gstate(),
                    &self.text.ctm(),
                    self.regions,
                    self.opts.edge_padding,
                )?;
                match decided {
                    PathRedaction::Keep => {
                        for held in &pending {
                            self.emit(out, held)?;
                        }
                        self.emit(out, &op)?;
                    },
                    PathRedaction::Rewritten(written) => {
                        self.paths_pruned += 1;
                        for written in &written {
                            self.emit(out, written)?;
                        }
                    },
                }
                continue;
            }
            if !self.path.is_empty() {
                return Err(Error::Unsupported(
                    "a path is not ended by a paint operator; refusing to redact a path that \
                     cannot be read"
                        .to_string(),
                ));
            }
            if let Operator::SetExtGState { dict_name } = &op {
                match self.images.line_styles.get(dict_name.as_str()) {
                    Some(Some(style)) => self.text.set_line_style(style),
                    _ => {
                        return Err(Error::Unsupported(format!(
                            "the ExtGState /{dict_name} cannot be read; refusing to redact a \
                             page whose line style is unknown"
                        )))
                    },
                }
            }
            // An image `Do` and an inline image are decided here: the
            // placement, the overlay fill it carries, and the mechanics of
            // the copy. A `Do` of whatever else — a form (T6c), a glyph's
            // resource the pass cannot resolve — passes through and keeps
            // its name on the page.
            let replacement = match &op {
                Operator::Do { name } => {
                    match self.images.images.get(name.as_str()).copied() {
                        Some(source) => self.decide_do(source, name.as_str())?,
                        None => None,
                    }
                },
                Operator::InlineImage { dict, data } => self.decide_inline(dict, data)?,
                _ => None,
            };
            match replacement {
                Some(Decision::Dropped) => continue,
                Some(Decision::UsedAs(written)) => {
                    self.bytes.clear();
                    serialize_operator(&mut self.bytes, &written);
                    out.write_all(&self.bytes)?;
                    continue;
                },
                None => {},
            }
            self.ops.clear();
            self.text.apply(&op, &mut self.ops);
            for written in &self.ops {
                if let Operator::Do { name } = written {
                    self.kept_xobject_names.insert(name.clone());
                }
                self.bytes.clear();
                serialize_operator(&mut self.bytes, written);
                out.write_all(&self.bytes)?;
            }
            if self.text.result().unsupported_font {
                return Err(unsupported_font());
            }
        }
        if !content.damage().is_clean() {
            return Err(Error::Unsupported(
                "the content stream is damaged; refusing to redact a stream whose \
                 operators cannot be read in full"
                    .to_string(),
            ));
        }
        if !self.path.is_empty() {
            return Err(Error::Unsupported(
                "a path is not ended by a paint operator; refusing to redact a path that \
                 cannot be read"
                    .to_string(),
            ));
        }
        out.write_all(b"Q\n")?;
        for region in &self.regions.regions {
            out.write_all(&region_overlay_ops(region, self.opts))?;
        }
        let totals = self.text.result();
        Ok(PassOutcome {
            report: RedactionReport {
                regions: self.regions.len(),
                glyphs_removed: totals.glyphs_removed,
                annotations_removed: 0,
                fonts_scrubbed: 0,
                bytes_removed: totals.bytes_removed,
                images_modified: self.imgs_modified + self.inline_modified,
                images_removed: self.imgs_removed,
                paths_pruned: self.paths_pruned,
                ..RedactionReport::default()
            },
            placements: self.placements,
            image_originals: self.image_originals,
            kept_xobject_names: self.kept_xobject_names,
        })
    }

    /// Writes one operator to `out` through the pass's byte buffer.
    fn emit<W: Write>(&mut self, out: &mut W, op: &Operator) -> Result<()> {
        self.bytes.clear();
        serialize_operator(&mut self.bytes, op);
        out.write_all(&self.bytes)?;
        Ok(())
    }

    /// Decides one image `Do` against the regions through the composed CTM:
    /// the placement queued, and `None` when the `Do` passes through (the
    /// kept one is written byte for byte; the caller re-serialises it).
    fn decide_do(&mut self, source: ObjectRef, name: &str) -> Result<Option<Decision>> {
        use super::image_prune::ImageRedaction;
        let ctm = self.text.ctm();
        match classify_image_placement(&ctm, self.regions, self.opts.edge_padding) {
            ImageRedaction::Keep => Ok(None),
            ImageRedaction::DeleteFull => {
                self.imgs_removed += 1;
                note_original(&mut self.image_originals, source.id);
                Ok(Some(Decision::Dropped))
            },
            ImageRedaction::Overwrite { u0, v0, u1, v1 } => {
                let key = rect_key(u0, v0, u1, v1);
                // The same image over the same covered rect shares one copy;
                // a different rect is a copy of its own.
                let shared = self
                    .copies
                    .get(&source.id)
                    .and_then(|m| m.iter().find(|(k, _)| **k == key))
                    .map(|(_, n)| n.clone());
                let copy_name = match shared {
                    Some(n) => n,
                    None => {
                        let copy_name = self.fresh_copy_name(name);
                        let fills = self.region_fills(&ctm)?;
                        self.placements.push(ImagePlacement {
                            name: copy_name.clone(),
                            source,
                            u0,
                            v0,
                            u1,
                            v1,
                            fills,
                        });
                        note_original(&mut self.image_originals, source.id);
                        self.imgs_modified += 1;
                        self.copies
                            .entry(source.id)
                            .or_default()
                            .insert(key, copy_name.clone());
                        copy_name
                    },
                };
                Ok(Some(Decision::UsedAs(Operator::Do {
                    name: copy_name,
                })))
            },
        }
    }

    /// A resource name for a copy of the image `name`, not a name the page's
    /// resources already hold: the first `name_rN` past the placements queued.
    fn fresh_copy_name(&self, name: &str) -> String {
        let stem = name.trim_start_matches('/');
        let mut index = self.placements.len();
        loop {
            let candidate = format!("{stem}_r{index}");
            if !self.images.images.contains_key(&candidate)
                && !self.placements.iter().any(|p| p.name == candidate)
            {
                return candidate;
            }
            index += 1;
        }
    }

    /// Every region that meets the placement through `ctm`, as its covered
    /// rect in image u/v space and the overlay fill it carries; first hit
    /// wins for a pixel.
    fn region_fills(&self, ctm: &Matrix) -> Result<Vec<([f32; 4], [f32; 3])>> {
        let Some(inv) = invert_affine(ctm) else {
            // The mapping is unreliable: the whole unit square is under one
            // fill, the same over-redaction the classifier's own singular
            // case takes (never under-redact).
            return Ok(vec![(
                [0.0, 0.0, 1.0, 1.0],
                resolved_fill(&self.regions.regions[0], self.opts).unwrap_or([0.0, 0.0, 0.0]),
            )]);
        };
        let unit = crate::geometry::Rect::from_points(0.0, 0.0, 1.0, 1.0);
        let footprint = super::classify::transform_bbox(&unit, ctm);
        let clamp01 = |v: f32| v.clamp(0.0, 1.0);
        let mut out = Vec::new();
        for (index, region) in self.regions.regions.iter().enumerate() {
            let padded = region.padded_rect(self.opts.edge_padding);
            if !padded.intersects(&footprint) {
                continue;
            }
            let mut u0 = f32::INFINITY;
            let mut v0 = f32::INFINITY;
            let mut u1 = f32::NEG_INFINITY;
            let mut v1 = f32::NEG_INFINITY;
            for (px, py) in [
                (padded.left(), padded.top()),
                (padded.right(), padded.top()),
                (padded.right(), padded.bottom()),
                (padded.left(), padded.bottom()),
            ] {
                let p = inv.transform_point(px, py);
                u0 = u0.min(p.x);
                v0 = v0.min(p.y);
                u1 = u1.max(p.x);
                v1 = v1.max(p.y);
            }
            out.push((
                [clamp01(u0), clamp01(v0), clamp01(u1), clamp01(v1)],
                self.region_fill(index),
            ));
        }
        Ok(out)
    }

    /// The overlay colour a region redacts under: the mark's `/IC` colour,
    /// else the configured default when overlay drawing is on, else black —
    /// the plan's "else sample zero" for the pixels a mark of no colour
    /// covers.
    fn region_fill(&self, region: usize) -> [f32; 3] {
        resolved_fill(&self.regions.regions[region], self.opts).unwrap_or([0.0, 0.0, 0.0])
    }

    /// Decides one inline image: decode it whole (they are small), fill the
    /// covered pixels, re-emit the bytes the copy stores. The re-emit is
    /// raw when the source was, Flate when it was filtered.
    fn decide_inline(
        &mut self,
        dict: &std::collections::HashMap<String, Object>,
        data: &[u8],
    ) -> Result<Option<Decision>> {
        use super::image_prune::ImageRedaction;
        let ctm = self.text.ctm();
        match classify_image_placement(&ctm, self.regions, self.opts.edge_padding) {
            ImageRedaction::Keep => Ok(None),
            ImageRedaction::DeleteFull => {
                self.imgs_removed += 1;
                Ok(Some(Decision::Dropped))
            },
            ImageRedaction::Overwrite { u0, v0, u1, v1 } => {
                let fills = self.region_fills(&ctm)?;
                let filled = fill_inline(dict, data, u0, v0, u1, v1, &fills)?;
                self.inline_modified += 1;
                Ok(Some(Decision::UsedAs(Operator::InlineImage {
                    dict: Box::new(filled.0),
                    data: filled.1,
                })))
            },
        }
    }
}

fn unsupported_font() -> Error {
    Error::Unsupported(
        "destructive text redaction of composite/Type0/unknown font content is not yet \
         supported; refusing rather than risk leaving recoverable text"
            .to_string(),
    )
}

/// An inline image's covered pixels filled: `(dict, data)` in the re-emitted
/// form. The pixels decode through the extractor the renderer paints them
/// with, the fill is the overlay colour the region under a pixel carries,
/// and the copy is the raster's own bytes: DeviceGray or DeviceRGB at 8
/// bits, `/Decode` dropped (the samples are the physical colour now), a raw
/// byte stream when the source was raw and a Flate one when it was filtered.
fn fill_inline(
    dict: &HashMap<String, Object>,
    data: &[u8],
    u0: f32,
    v0: f32,
    u1: f32,
    v1: f32,
    fills: &[([f32; 4], [f32; 3])],
) -> Result<(HashMap<String, Object>, Vec<u8>)> {
    let expanded = crate::extractors::images::expand_inline_image_dict(dict.clone());
    let filtered = !crate::object::extract_filter_names(expanded.get("Filter").unwrap_or(&Object::Null))
        .is_empty();
    let synthetic = Object::stream(expanded, bytes::Bytes::from(data.to_vec()));
    let image = crate::extractors::images::extract_image_from_xobject_at(
        None,
        &synthetic,
        None,
        None,
        None,
    )?;
    let dynamic = image.to_dynamic_image()?;
    let (w, h) = (dynamic.width(), dynamic.height());
    let (space, pixels) = match &dynamic {
        image::DynamicImage::ImageLuma8(plain) => ("DeviceGray", plain.to_vec()),
        other => ("DeviceRGB", other.to_rgb8().to_vec()),
    };
    let channels: usize = match space {
        "DeviceGray" => 1,
        _ => 3,
    };
    let (x0, x1, y0, y1) = covered_span(u0, v0, u1, v1, w, h);
    let mut out = pixels;
    let luma = |rgb: [f32; 3]| (0.299 * rgb[0] + 0.587 * rgb[1] + 0.114 * rgb[2]).clamp(0.0, 1.0);
    for y in y0..y1 {
        for x in x0..x1 {
            let u = (x as f64 + 0.5) / w as f64;
            let v = 1.0 - (y as f64 + 0.5) / h as f64;
            // ── pdf_manipulator patch: the last region in drawing order is drawn on top ──
            let Some((_, rgb)) = fills.iter().rev().find(|(rect, _)| {
                let [ru0, rv0, ru1, rv1] = rect;
                f64::from(*ru0) <= u
                    && u < f64::from(*ru1)
                    && f64::from(*rv0) <= v
                    && v < f64::from(*rv1)
            }) else {
                continue;
            };
            // ── end pdf_manipulator patch ──
            let at = (y * w as usize + x) * channels;
            if channels == 1 {
                out[at] = (luma(*rgb) * 255.0).round() as u8;
            } else {
                out[at] = (rgb[0].clamp(0.0, 1.0) * 255.0).round() as u8;
                out[at + 1] = (rgb[1].clamp(0.0, 1.0) * 255.0).round() as u8;
                out[at + 2] = (rgb[2].clamp(0.0, 1.0) * 255.0).round() as u8;
            }
        }
    }
    // The copy's dictionary names only what its raster needs: the size, the
    // colour space, 8 bits and its filter. The source's /Decode does not
    // carry over, since the samples are the physical colour now.
    let mut out_dict = HashMap::new();
    out_dict.insert("W".to_string(), Object::Integer(u64::from(w) as i64));
    out_dict.insert("H".to_string(), Object::Integer(u64::from(h) as i64));
    out_dict.insert("BPC".to_string(), Object::Integer(8));
    out_dict.insert("CS".to_string(), Object::Name(space.to_string()));
    if filtered {
        out_dict.insert("F".to_string(), Object::Name("FlateDecode".to_string()));
        let encoded = crate::host::images::encode::encode_flate_predicted(
            &out,
            w,
            h,
            u8::try_from(channels).map_err(|_| Error::Image("inline image channels".to_string()))?,
        )?;
        Ok((out_dict, encoded.bytes))
    } else {
        Ok((out_dict, out))
    }
}


