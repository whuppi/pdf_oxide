//! Out-of-crate measurement door — runs one wire request the way a lane
//! does, for memory probes that live outside this crate.

use crate::host::bridge_api::{self, BoxedReader, BoxedWriter};
use crate::host::lane_state::LaneState;
use std::fs::File;

/// Runs one wire request against `state`, wrapping each `File` in
/// the bridge's boxed reader/writer exactly as the transports do.
pub fn run(
    state: &mut LaneState,
    request: &[u8],
    sources: Vec<File>,
    sinks: Vec<File>,
) -> Vec<u8> {
    let sources = sources.into_iter().map(|f| BoxedReader(Box::new(f))).collect();
    let sinks = sinks.into_iter().map(BoxedWriter::new).collect();
    bridge_api::handle_request(state, request, None, sources, sinks, &crate::cancel::CancelCheck::none())
}

// ── Entry points tool/fuelprobe runs under `CancelCheck::probe()`. Each is the
// function the host op of the same name calls, made reachable from outside
// the crate. ──

use crate::cancel::CancelCheck;
use crate::document::PdfDocument;
use crate::editor::{DocumentEditor, SaveOptions};
use crate::error::Result;

/// `render`: `pages` at `dpi`, as PNG chunks, to `writer`.
#[cfg(feature = "rendering")]
pub fn render_streamed(
    doc: &PdfDocument,
    pages: &[usize],
    dpi: u32,
    writer: &mut impl std::io::Write,
    cancel: &CancelCheck,
) -> Result<usize> {
    use crate::host::render_stream::{render_pages_streamed, RenderFormat, RenderSize};
    let _job = crate::cancel::enter(cancel);
    render_pages_streamed(doc, pages, RenderSize::Dpi(dpi), RenderFormat::Png, writer, cancel)
}

/// `extractImages`: the images drawn on `pages`, as chunked items, to `writer`.
#[cfg(feature = "rendering")]
pub fn extract_images_streamed(
    doc: &PdfDocument,
    pages: &[usize],
    writer: &mut impl std::io::Write,
    cancel: &CancelCheck,
) -> Result<usize> {
    let _job = crate::cancel::enter(cancel);
    crate::host::extract_stream::extract_images_streamed(doc, pages, writer, cancel)
}

/// `editorSave` in incremental mode.
pub fn save_incremental(
    editor: &mut DocumentEditor,
    writer: &mut impl crate::host::positioned_write::PositionedWrite,
    options: &SaveOptions,
    cancel: &CancelCheck,
) -> Result<()> {
    let _job = crate::cancel::enter(cancel);
    crate::host::incremental::write(editor, writer, options, cancel).map_err(|e| match e {
        crate::host::incremental::IncrementalError::Refused(why) => {
            crate::error::Error::InvalidOperation(why)
        },
        crate::host::incremental::IncrementalError::Failed(e) => e,
    })
}

/// `editorMergeFrom`: `pages` (all when `None`) of the PDF in `bytes`.
pub fn merge_pages(
    editor: &mut DocumentEditor,
    bytes: Vec<u8>,
    pages: Option<&[usize]>,
    cancel: &CancelCheck,
) -> Result<()> {
    let _job = crate::cancel::enter(cancel);
    let source = DocumentEditor::open_merge_source(Box::new(std::io::Cursor::new(bytes)))?;
    editor.merge_document(source, pages, cancel).map(|_| ())
}

/// `editorExtractPages` (split): a full-rewrite save of `pages` alone.
pub fn extract_pages(
    editor: &mut DocumentEditor,
    pages: &[usize],
    writer: &mut impl crate::host::positioned_write::PositionedWrite,
    cancel: &CancelCheck,
) -> Result<()> {
    let _job = crate::cancel::enter(cancel);
    editor
        .with_pages_selected(pages, |editor| {
            crate::host::dispatch::edit_save_with_options(
                editor,
                writer,
                &SaveOptions::full_rewrite(),
                cancel,
            )
        })
        .and_then(|saved| saved)
}

/// `open` / `editorOpen`'s source: the PDF in `bytes`, read through the
/// external-reader path the lanes use, not the in-memory one.
pub fn open_source(bytes: Vec<u8>, cancel: &CancelCheck) -> Result<PdfDocument> {
    let _job = crate::cancel::enter(cancel);
    PdfDocument::from_external_reader_cancellable(Box::new(std::io::Cursor::new(bytes)), cancel)
}
