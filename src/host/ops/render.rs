// Rasterization ops (the render capability): page rendering and image
// extraction, both streamed through sinks[0].

use crate::host::binary_codec::{Request, ResponseWriter};
use crate::host::bridge_api::BoxedWriter;
use crate::host::dispatch;
use crate::host::lane_state::LaneState;
use crate::host::ops::op_unit;
use crate::host::ops::support::{page_of, pages_of, req_handle};

op_unit!(RENDER, "render", pdf_op_render_anchor, |ctx| {
    let sink = ctx.take_sink(0);
    handle_render_streamed(ctx.state, ctx.req, sink, ctx.cancel)
});

op_unit!(
    EXTRACT_IMAGES,
    "extractImages",
    pdf_op_extract_images_anchor,
    |ctx| {
        let sink = ctx.take_sink(0);
        handle_extract_images_streamed(ctx.state, ctx.req, sink, ctx.cancel)
    }
);

fn handle_render_streamed(
    state: &mut LaneState,
    req: &Request<'_>,
    sink_writer: Option<BoxedWriter>,
    cancel: &crate::cancel::CancelCheck,
) -> Vec<u8> {
    let hid = req_handle(req);
    let docs = &mut state.documents;
    let doc = match docs.get_mut(&hid) {
        Some(d) => d,
        None => return ResponseWriter::error("document not found"),
    };

    let page_indices = req.get_int_list("pageIndices").unwrap_or(&[]);
    // Empty list = all pages (Dart convention: PdfPages.all() sends []).
    let count = match doc.page_count() {
        Ok(c) => c,
        Err(e) => return ResponseWriter::error_of(&e),
    };
    let pages: Vec<usize> = if page_indices.is_empty() {
        (0..count).collect()
    } else {
        match pages_of(page_indices, count) {
            Ok(p) => p,
            Err(resp) => return resp,
        }
    };
    let max_w = req.get_i32("maxWidth").unwrap_or(0) as u32;
    let max_h = req.get_i32("maxHeight").unwrap_or(0) as u32;

    if let Some(mut writer) = sink_writer {
        match dispatch::render_pages_streamed(doc, &pages, max_w, max_h, &mut writer, cancel) {
            Ok(count) => {
                let mut w = ResponseWriter::ok();
                w.put_bool("streamed", true);
                w.put_i32("itemCount", count as i32);
                w.finish()
            }
            Err(e) => ResponseWriter::error_of(&e),
        }
    } else {
        let page = match page_of(req.get_i32("page").unwrap_or(0), count) {
            Ok(i) => i,
            Err(resp) => return resp,
        };
        match dispatch::render_page(doc, page, max_w, max_h) {
            Ok(result) => {
                let mut w = ResponseWriter::ok();
                w.put_i32("width", result.width as i32);
                w.put_i32("height", result.height as i32);
                w.put_bytes("data", &result.data);
                w.finish()
            }
            Err(e) => ResponseWriter::error_of(&e),
        }
    }
}

fn handle_extract_images_streamed(
    state: &mut LaneState,
    req: &Request<'_>,
    sink_writer: Option<BoxedWriter>,
    cancel: &crate::cancel::CancelCheck,
) -> Vec<u8> {
    let hid = req_handle(req);
    let docs = &mut state.documents;
    let doc = match docs.get_mut(&hid) {
        Some(d) => d,
        None => return ResponseWriter::error("document not found"),
    };

    let count = match doc.page_count() {
        Ok(c) => c,
        Err(e) => return ResponseWriter::error_of(&e),
    };
    let page = match page_of(req.get_i32("page").unwrap_or(0), count) {
        Ok(i) => i,
        Err(resp) => return resp,
    };

    if let Some(mut writer) = sink_writer {
        match dispatch::extract_images_streamed(doc, page, &mut writer, cancel) {
            Ok(count) => {
                let mut w = ResponseWriter::ok();
                w.put_bool("streamed", true);
                w.put_i32("itemCount", count as i32);
                w.finish()
            }
            Err(e) => ResponseWriter::error_of(&e),
        }
    } else {
        match dispatch::extract_images(doc, page) {
            Ok(result) => {
                let mut w = ResponseWriter::ok();
                w.put_map_list("images", result.len(), |i, item| {
                    let img = &result[i];
                    item.put_i32("width", img.width as i32);
                    item.put_i32("height", img.height as i32);
                    item.put_str("format", &img.format);
                    item.put_str("colorSpace", &img.color_space);
                    item.put_i32("bitsPerComponent", img.bits_per_component as i32);
                    item.put_bytes("data", &img.data);
                });
                w.finish()
            }
            Err(e) => ResponseWriter::error_of(&e),
        }
    }
}
