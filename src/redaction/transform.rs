//! Write-time redaction of one page's content.
//!
//! The page's operators stream through [`RedactPass`] one at a time: each is
//! decided by the text engine and its survivors are written at once, so the
//! rewritten content never exists whole in memory. The same pass runs into a
//! byte counter when a redaction is queued and into the file when it is
//! saved; both produce the same [`RedactionReport`].

use std::io::{Read, Write};

use super::options::{RedactionOptions, RedactionReport};
use super::overlay::region_overlay_ops;
use super::region::RegionSet;
use super::serialize::serialize_operator;
use super::text_engine::{FontMetrics, TextPass};
use crate::content::operators::Operator;
use crate::error::{Error, Result};
use crate::host::content::{InlineImages, Operators};

/// Redacts one page's operators into a writer.
pub struct RedactPass<'a> {
    regions: &'a RegionSet,
    opts: &'a RedactionOptions,
    text: TextPass<'a>,
    /// The rewritten form of the operator being decided, reused.
    ops: Vec<Operator>,
    /// The bytes of one rewritten operator, reused.
    bytes: Vec<u8>,
}

impl<'a> RedactPass<'a> {
    /// A pass that redacts `regions` with `opts`' padding and overlay, glyph
    /// widths from `fonts`.
    pub fn new(
        regions: &'a RegionSet,
        opts: &'a RedactionOptions,
        fonts: &'a dyn FontMetrics,
    ) -> Self {
        Self {
            regions,
            opts,
            text: TextPass::new(regions, opts.edge_padding, fonts),
            ops: Vec::new(),
            bytes: Vec::new(),
        }
    }

    /// Writes the page's rewritten content to `out`: its operators inside
    /// `q … Q`, so the overlays cannot inherit a CTM the content left
    /// behind, then one overlay per region.
    ///
    /// # Errors
    /// - [`Error::Unsupported`] when the content is damaged, or shows text in
    ///   a font whose glyphs cannot be pruned. Whatever reached `out` before
    ///   the error is not a valid redaction; the caller discards it.
    /// - [`Error::Io`] when `out` fails.
    pub fn run<R: Read, W: Write>(
        mut self,
        content: Operators<R>,
        out: &mut W,
    ) -> Result<RedactionReport> {
        let mut content = content.with_inline_images(InlineImages::Collect);
        out.write_all(b"q\n")?;
        for item in content.by_ref() {
            let op = item?;
            self.ops.clear();
            self.text.apply(&op, &mut self.ops);
            for written in &self.ops {
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
        out.write_all(b"Q\n")?;
        for region in &self.regions.regions {
            out.write_all(&region_overlay_ops(region, self.opts))?;
        }
        let totals = self.text.result();
        Ok(RedactionReport {
            regions: self.regions.len(),
            glyphs_removed: totals.glyphs_removed,
            annotations_removed: 0,
            fonts_scrubbed: 0,
            bytes_removed: totals.bytes_removed,
            ..RedactionReport::default()
        })
    }
}

fn unsupported_font() -> Error {
    Error::Unsupported(
        "destructive text redaction of composite/Type0/unknown font content is not yet \
         supported; refusing rather than risk leaving recoverable text"
            .to_string(),
    )
}
