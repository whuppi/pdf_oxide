// Editor-handle ops: lifecycle, metadata reads, mutation, merge, save.
// Core — always built.

use crate::host::binary_codec::{ErrorKind, Request, ResponseWriter};
use crate::host::bridge_api::{BoxedReader, BoxedWriter};
use crate::host::dispatch;
use crate::host::lane_state::LaneState;
use crate::host::ops::op_unit;
use crate::host::ops::support::{
    authenticate_or_refuse, handle_with_editor, ok_flag, ok_response, open_source, page_of,
    page_set_of, pages_of, put_page_infos, read_all_from_reader, req_handle,
};

use crate::editor::DocumentEditor;

op_unit!(OPEN, "editorOpen", pdf_op_editor_open_anchor, |ctx| {
    let source = ctx.take_source(0);
    handle_editor_open(ctx.state, ctx.req, ctx.source_bytes, source)
});

op_unit!(DISPOSE, "editorDispose", pdf_op_editor_dispose_anchor, |ctx| {
    ctx.state.editors.remove(&req_handle(ctx.req));
    ok_flag("disposed")
});

op_unit!(
    GET_METADATA,
    "editorGetMetadata",
    pdf_op_editor_get_metadata_anchor,
    |ctx| {
        handle_with_editor(ctx.state, ctx.req, |editor, _| {
            let m = dispatch::edit_get_metadata(editor);
            let mut w = ResponseWriter::ok();
            w.put_i32("pageCount", m.page_count as i32);
            w.put_str("version", &format!("{}.{}", m.version_major, m.version_minor));
            w.put_str("title", &m.title);
            w.put_str("author", &m.author);
            w.put_str("subject", &m.subject);
            w.put_str("keywords", &m.keywords);
            w.put_str("creator", &m.creator);
            w.put_str("producer", &m.producer);
            w.put_str("creationDate", &m.creation_date);
            w.put_str("modDate", &m.mod_date);
            Ok(w.finish())
        })
    }
);

op_unit!(
    IS_MODIFIED,
    "editorIsModified",
    pdf_op_editor_is_modified_anchor,
    |ctx| {
        handle_with_editor(ctx.state, ctx.req, |editor, _| {
            let modified = dispatch::edit_is_modified(editor);
            let mut w = ResponseWriter::ok();
            w.put_bool("modified", modified);
            Ok(w.finish())
        })
    }
);

op_unit!(
    PAGE_INFO,
    "editorPageInfo",
    pdf_op_editor_page_info_anchor,
    |ctx| {
        let cancel = ctx.cancel;
        handle_with_editor(ctx.state, ctx.req, |editor, req| {
            let pages = match page_set_of(req, editor.current_page_count()) {
                Ok(pages) => pages,
                Err(resp) => return Ok(resp),
            };
            let infos = dispatch::edit_page_info(editor, &pages, cancel)?;
            let mut w = ResponseWriter::ok();
            put_page_infos(&mut w, &pages, &infos);
            Ok(w.finish())
        })
    }
);

op_unit!(
    PAGE_IMAGES,
    "editorPageImages",
    pdf_op_editor_page_images_anchor,
    |ctx| {
        handle_with_editor(ctx.state, ctx.req, |editor, req| {
            let page = match page_of(req.get_i32("page").unwrap_or(0), editor.current_page_count()) {
                Ok(i) => i,
                Err(resp) => return Ok(resp),
            };
            let images = dispatch::edit_page_images(editor, page)?;
            let mut w = ResponseWriter::ok();
            w.put_map_list("images", images.len(), |i, item| {
                let img = &images[i];
                item.put_str("name", &img.name);
                item.put_f64("x", img.bounds[0] as f64);
                item.put_f64("y", img.bounds[1] as f64);
                item.put_f64("width", img.bounds[2] as f64);
                item.put_f64("height", img.bounds[3] as f64);
                item.put_f64("a", img.matrix[0] as f64);
                item.put_f64("b", img.matrix[1] as f64);
                item.put_f64("c", img.matrix[2] as f64);
                item.put_f64("d", img.matrix[3] as f64);
                item.put_f64("e", img.matrix[4] as f64);
                item.put_f64("f", img.matrix[5] as f64);
            });
            Ok(w.finish())
        })
    }
);

op_unit!(
    REDACTION_COUNT,
    "editorRedactionCount",
    pdf_op_editor_redaction_count_anchor,
    |ctx| {
        handle_with_editor(ctx.state, ctx.req, |editor, req| {
            let page = match page_of(req.get_i32("page").unwrap_or(0), editor.current_page_count()) {
                Ok(i) => i,
                Err(resp) => return Ok(resp),
            };
            let count = dispatch::edit_redaction_count(editor, page)?;
            let mut w = ResponseWriter::ok();
            w.put_i32("count", count as i32);
            Ok(w.finish())
        })
    }
);

op_unit!(MUTATE, "editorMutate", pdf_op_editor_mutate_anchor, |ctx| {
    let data = ctx.take_data_reader();
    handle_editor_mutate(ctx.state, ctx.req, data, ctx.cancel)
});

op_unit!(
    MERGE_FROM,
    "editorMergeFrom",
    pdf_op_editor_merge_from_anchor,
    |ctx| {
        let data = ctx.take_data_reader();
        handle_editor_merge_from(ctx.state, ctx.req, data)
    }
);

op_unit!(SAVE, "editorSave", pdf_op_editor_save_anchor, |ctx| {
    let sink = ctx.take_sink(0);
    handle_editor_save(ctx.state, ctx.req, sink, ctx.cancel)
});

op_unit!(
    EXTRACT_PAGES,
    "editorExtractPages",
    pdf_op_editor_extract_pages_anchor,
    |ctx| {
        let sink = ctx.take_sink(0);
        handle_editor_extract_pages(ctx.state, ctx.req, sink, ctx.cancel)
    }
);

fn handle_editor_open(
    state: &mut LaneState,
    req: &Request<'_>,
    source_bytes: Option<&[u8]>,
    source_reader: Option<BoxedReader>,
) -> Vec<u8> {
    let password = req.get_str("password");
    let doc = match open_source(source_bytes, source_reader, "editorOpen") {
        Ok(d) => d,
        Err(resp) => return resp,
    };
    if let Some(resp) = authenticate_or_refuse(&doc, password) {
        return resp;
    }
    let editor = match DocumentEditor::from_document(doc) {
        Ok(e) => e,
        Err(e) => return ResponseWriter::error_of(&e),
    };
    let hid = state.next_handle_id();
    state.editors.insert(hid, editor);
    let mut w = ResponseWriter::ok();
    w.put_i32("handleId", hid as i32);
    w.finish()
}

fn handle_editor_merge_from(
    state: &mut LaneState,
    req: &Request<'_>,
    merge_reader: Option<BoxedReader>,
) -> Vec<u8> {
    // An absent `pages` key means every page; an empty list would mean
    // "merge nothing", which no caller can express and none wants.
    let pages: Option<Vec<usize>> = req
        .get_int_list("pages")
        .map(|p| p.iter().map(|&i| i as usize).collect());
    let Some(editor) = state.editors.get_mut(&req_handle(req)) else {
        return ResponseWriter::error("editor not found");
    };
    let result = if let Some(reader) = merge_reader {
        let source = match DocumentEditor::open_merge_source(reader.0) {
            Ok(doc) => doc,
            Err(e) => return ResponseWriter::error_of(&e),
        };
        if let Some(resp) = authenticate_or_refuse(&source, req.get_str("password")) {
            return resp;
        }
        editor.merge_document(source, pages.as_deref())
    } else if let Some(other) = req.get_bytes("otherBytes") {
        editor.merge_from_bytes(other)
    } else {
        return ResponseWriter::error("editorMergeFrom: no source");
    };
    match result {
        Ok(_) => ok_flag("merged"),
        Err(e) => ResponseWriter::error_of(&e),
    }
}

fn handle_editor_mutate(
    state: &mut LaneState,
    req: &Request<'_>,
    data_reader: Option<BoxedReader>,
    cancel: &crate::cancel::CancelCheck,
) -> Vec<u8> {
    let edit_op = match req.get_str("editOp") {
        Some(op) => op,
        None => return ResponseWriter::error("missing editOp"),
    };
    let hid = req_handle(req);
    let editors = &mut state.editors;
    let editor = match editors.get_mut(&hid) {
        Some(e) => e,
        None => return ResponseWriter::error("editor not found"),
    };

    match do_editor_mutate(editor, edit_op, req, data_reader, cancel) {
        Ok(bytes) => bytes,
        Err(e) => ResponseWriter::error_of(&e),
    }
}

/// The rotation the request's `rotation` field names, in degrees.
fn rotation_of(req: &Request<'_>) -> crate::error::Result<i32> {
    let name = req.get_str("rotation").unwrap_or("");
    dispatch::rotation_degrees_from_name(name)
        .ok_or_else(|| crate::error::Error::InvalidOperation(format!("unknown rotation: {name}")))
}

fn do_editor_mutate(
    editor: &mut DocumentEditor,
    edit_op: &str,
    req: &Request<'_>,
    mut data_reader: Option<BoxedReader>,
    cancel: &crate::cancel::CancelCheck,
) -> crate::error::Result<Vec<u8>> {
    use crate::error::Error;

    // The pages the editor has now, after deletes and merges.
    let count = editor.current_page_count();
    // The checked `page` field (0 when absent), or the page-range response.
    macro_rules! page {
        () => {
            match page_of(req.get_i32("page").unwrap_or(0), count) {
                Ok(i) => i,
                Err(resp) => return Ok(resp),
            }
        };
    }

    match edit_op {
        "keepPages" => {
            let pages = match pages_of(req.get_int_list("pages").unwrap_or(&[]), count) {
                Ok(p) => p,
                Err(resp) => return Ok(resp),
            };
            dispatch::edit_select_pages(editor, &pages)?;
            ok_response()
        }
        "deletePages" => {
            let pages = match page_set_of(req, count) {
                Ok(p) => p,
                Err(resp) => return Ok(resp),
            };
            dispatch::edit_delete_pages(editor, &pages)?;
            ok_response()
        }
        "rotatePages" => {
            let pages = match page_set_of(req, count) {
                Ok(p) => p,
                Err(resp) => return Ok(resp),
            };
            dispatch::edit_rotate_pages(editor, &pages, rotation_of(req)?)?;
            ok_response()
        }
        "setRotation" => {
            let pages = match page_set_of(req, count) {
                Ok(p) => p,
                Err(resp) => return Ok(resp),
            };
            dispatch::edit_set_rotation(editor, &pages, rotation_of(req)?)?;
            ok_response()
        }
        "setMetadata" => {
            // An empty string is an absent field: it is left out of the
            // saved info dictionary.
            let field = |key: &str| {
                req.get_str(key).filter(|s| !s.is_empty()).map(str::to_string)
            };
            dispatch::edit_set_metadata(
                editor,
                crate::editor::DocumentInfo {
                    title: field("title"),
                    author: field("author"),
                    subject: field("subject"),
                    keywords: field("keywords"),
                    creator: field("creator"),
                    producer: field("producer"),
                    creation_date: field("creationDate"),
                    mod_date: field("modDate"),
                },
            )?;
            ok_response()
        }
        "movePage" => {
            let from = match page_of(req.get_i32("from").unwrap_or(0), count) {
                Ok(i) => i,
                Err(resp) => return Ok(resp),
            };
            let to = match page_of(req.get_i32("to").unwrap_or(0), count) {
                Ok(i) => i,
                Err(resp) => return Ok(resp),
            };
            dispatch::edit_move_page(editor, from, to)?;
            ok_response()
        }
        "flattenForms" => {
            let page = match req.get_i32("page").map(|p| page_of(p, count)).transpose() {
                Ok(p) => p,
                Err(resp) => return Ok(resp),
            };
            dispatch::edit_flatten_forms(editor, page)?;
            ok_response()
        }
        "flattenAnnotations" => {
            let page = match req.get_i32("page").map(|p| page_of(p, count)).transpose() {
                Ok(p) => p,
                Err(resp) => return Ok(resp),
            };
            dispatch::edit_flatten_annotations(editor, page)?;
            ok_response()
        }
        "clearEraseRegions" => {
            let page = page!();
            dispatch::edit_clear_erase_regions(editor, page);
            ok_response()
        }
        "applyRedactions" => {
            let report = dispatch::edit_apply_redactions(editor)?;
            let mut w = ResponseWriter::ok();
            w.put_i32("regions", report.regions as i32);
            w.put_i32("glyphsRemoved", report.glyphs_removed as i32);
            w.put_i32("imagesModified", report.images_modified as i32);
            w.put_i32("imagesRemoved", report.images_removed as i32);
            w.put_i32("pathsPruned", report.paths_pruned as i32);
            w.put_i32("xobjectsSpecialized", report.xobjects_specialized as i32);
            Ok(w.finish())
        }
        "sanitize" => {
            dispatch::edit_sanitize(
                editor,
                req.get_bool("metadata").unwrap_or(true),
                req.get_bool("javascript").unwrap_or(true),
                req.get_bool("embeddedFiles").unwrap_or(false),
            )?;
            ok_response()
        }
        "setMediaBox" => {
            let pages = match page_set_of(req, count) {
                Ok(p) => p,
                Err(resp) => return Ok(resp),
            };
            let x = req.get_f64("x").unwrap_or(0.0) as f32;
            let y = req.get_f64("y").unwrap_or(0.0) as f32;
            let width = req.get_f64("width").unwrap_or(0.0) as f32;
            let height = req.get_f64("height").unwrap_or(0.0) as f32;
            dispatch::edit_set_media_box(editor, &pages, [x, y, x + width, y + height])?;
            ok_response()
        }
        "setCropBox" => {
            let pages = match page_set_of(req, count) {
                Ok(p) => p,
                Err(resp) => return Ok(resp),
            };
            // No `width` key means no box: the pages lose their own crop box.
            let crop = req.get_f64("width").map(|width| {
                let x = req.get_f64("x").unwrap_or(0.0) as f32;
                let y = req.get_f64("y").unwrap_or(0.0) as f32;
                let height = req.get_f64("height").unwrap_or(0.0) as f32;
                [x, y, x + width as f32, y + height]
            });
            dispatch::edit_set_crop_box(editor, &pages, crop)?;
            ok_response()
        }
        "reduceImages" => {
            let policy = dispatch::ImagePolicyArgs {
                color_ppi: req.get_f64("colorPpi"),
                gray_ppi: req.get_f64("grayPpi"),
                mono_ppi: req.get_f64("monoPpi"),
                downsample_threshold: req.get_f64("downsampleThreshold").unwrap_or(1.5),
                jpeg_quality: req.get_i32("jpegQuality").unwrap_or(75).clamp(1, 100) as u8,
                allow_lossy: req.get_bool("allowLossy").unwrap_or(true),
                convert_cmyk_to_rgb: req.get_bool("convertCmykToRgb").unwrap_or(false),
                min_pixels: req.get_i32("minPixels").unwrap_or(32).max(0) as u32,
                min_savings: req.get_f64("minSavings").unwrap_or(0.1),
                chroma: req.get_str("chroma").unwrap_or("auto").to_string(),
                recompress_jpeg: req.get_bool("recompressJpeg").unwrap_or(false),
            };
            let rows = dispatch::edit_reduce_images(editor, policy, cancel)?;
            let mut w = ResponseWriter::ok();
            w.put_map_list("images", rows.len(), |i, item| {
                let r = &rows[i];
                item.put_i32("objectId", r.object_id as i32);
                item.put_str("encoding", r.encoding);
                item.put_str("color", r.color);
                item.put_bool("indexed", r.indexed);
                item.put_i32("bits", r.bits as i32);
                item.put_i32("width", r.width as i32);
                item.put_i32("height", r.height as i32);
                item.put_bool("softMask", r.soft_mask);
                item.put_i32("uses", r.uses as i32);
                item.put_f64("ppiMin", r.ppi_min.unwrap_or(-1.0));
                item.put_str("action", r.action);
                item.put_str("keepReason", r.keep_reason);
                item.put_i64("bytesBefore", r.bytes_before as i64);
                item.put_i64("bytesAfter", r.bytes_after as i64);
                item.put_i32("widthAfter", r.width_after as i32);
                item.put_i32("heightAfter", r.height_after as i32);
            });
            Ok(w.finish())
        }
        "embedFile" => {
            let name = req.get_str("name").unwrap_or("file");
            let data = if let Some(ref mut reader) = data_reader {
                read_all_from_reader(reader)
                    .map_err(|e| Error::InvalidPdf(e.to_string()))?
            } else {
                req.get_bytes("data").unwrap_or(&[]).to_vec()
            };
            dispatch::edit_embed_file(
                editor,
                name,
                data,
                req.get_str("description").unwrap_or(""),
                req.get_str("mimeType").unwrap_or(""),
                req.get_str("relationship").unwrap_or(""),
            )?;
            ok_response()
        }
        "eraseRegions" => {
            let page = page!();
            let coords = req.get_f64_list("regions").unwrap_or(&[]);
            // The wire carries origin + size per region; the engine's
            // region set takes two corners (`RedactionRegion::from_rect`).
            let rects: Vec<[f32; 4]> = coords.chunks(4)
                .filter(|c| c.len() == 4)
                .map(|c| {
                    let (x, y) = (c[0] as f32, c[1] as f32);
                    [x, y, x + c[2] as f32, y + c[3] as f32]
                })
                .collect();
            dispatch::edit_erase_regions(editor, page, &rects)?;
            ok_response()
        }
        "watermark" => {
            let pos_fields: Vec<f32> = req.get_f64_list("posFields")
                .unwrap_or(&[]).iter().map(|&f| f as f32).collect();
            let pages = if req.get_bool("pagesAll").unwrap_or(false) {
                None
            } else {
                match page_set_of(req, count) {
                    Ok(p) => Some(p),
                    Err(resp) => return Ok(resp),
                }
            };
            dispatch::edit_watermark(
                editor,
                pages.as_deref(),
                req.get_str("text").unwrap_or("WATERMARK"),
                req.get_f64("fontSize").unwrap_or(48.0) as f32,
                req.get_f64("rotation").unwrap_or(45.0) as f32,
                req.get_f64("opacity").unwrap_or(0.3) as f32,
                req.get_f64("r").unwrap_or(0.5) as f32,
                req.get_f64("g").unwrap_or(0.5) as f32,
                req.get_f64("b").unwrap_or(0.5) as f32,
                req.get_str("layer").unwrap_or("foreground"),
                req.get_i32("posType").unwrap_or(0),
                &pos_fields,
            )?;
            ok_response()
        }
        "addStamp" => {
            dispatch::edit_add_stamp(
                editor,
                page!(),
                req.get_str("stampType").unwrap_or("Draft"),
                req.get_f64("x").unwrap_or(0.0) as f32,
                req.get_f64("y").unwrap_or(0.0) as f32,
                req.get_f64("width").unwrap_or(100.0) as f32,
                req.get_f64("height").unwrap_or(50.0) as f32,
                req.get_f64("opacity").unwrap_or(1.0) as f32,
            )?;
            ok_response()
        }
        "addImageStamp" => {
            let image_bytes = if let Some(ref mut reader) = data_reader {
                read_all_from_reader(reader)
                    .map_err(|e| Error::InvalidPdf(e.to_string()))?
            } else {
                req.get_bytes("imageData").unwrap_or(&[]).to_vec()
            };
            dispatch::edit_add_image_stamp(
                editor,
                page!(),
                image_bytes,
                req.get_f64("x").unwrap_or(0.0) as f32,
                req.get_f64("y").unwrap_or(0.0) as f32,
                req.get_f64("width").unwrap_or(100.0) as f32,
                req.get_f64("height").unwrap_or(50.0) as f32,
                req.get_f64("opacity").unwrap_or(1.0) as f32,
            )?;
            ok_response()
        }
        "unembedStandardFonts" => {
            let count = dispatch::edit_unembed_standard_fonts(editor)?;
            let mut w = ResponseWriter::ok();
            w.put_i32("count", count as i32);
            Ok(w.finish())
        }
        "setFormFieldValue" => {
            dispatch::edit_set_form_field_value(
                editor,
                req.get_str("fieldName").unwrap_or(""),
                req.get_str("value").unwrap_or(""),
            )?;
            ok_response()
        }
        "setCheckboxFieldValue" => {
            dispatch::edit_set_checkbox_field_value(
                editor,
                req.get_str("fieldName").unwrap_or(""),
                req.get_bool("checked").unwrap_or(false),
            )?;
            ok_response()
        }
        "cropMargins" => {
            dispatch::edit_crop_margins(
                editor,
                req.get_f64("left").unwrap_or(0.0) as f32,
                req.get_f64("right").unwrap_or(0.0) as f32,
                req.get_f64("top").unwrap_or(0.0) as f32,
                req.get_f64("bottom").unwrap_or(0.0) as f32,
            )?;
            ok_response()
        }
        "convertToPdfA" => {
            let name = req.get_str("level").unwrap_or("a2");
            let level = dispatch::pdfa_level_from_name(name)
                .ok_or_else(|| Error::InvalidOperation(format!("unknown PDF/A level: {name}")))?;
            dispatch::edit_convert_to_pdf_a(editor, level, cancel)?;
            ok_response()
        }
        "setImageBounds" => {
            dispatch::edit_set_image_bounds(
                editor,
                page!(),
                req.get_str("imageName").unwrap_or(""),
                req.get_f64("x").unwrap_or(0.0) as f32,
                req.get_f64("y").unwrap_or(0.0) as f32,
                req.get_f64("width").unwrap_or(100.0) as f32,
                req.get_f64("height").unwrap_or(100.0) as f32,
            )?;
            ok_response()
        }
        "addRedaction" => {
            // `RedactionRegion::from_rect` takes two corners; the wire
            // carries origin + size, like every other rect on the surface.
            let x = req.get_f64("x").unwrap_or(0.0) as f32;
            let y = req.get_f64("y").unwrap_or(0.0) as f32;
            let width = req.get_f64("width").unwrap_or(100.0) as f32;
            let height = req.get_f64("height").unwrap_or(50.0) as f32;
            dispatch::edit_add_redaction(
                editor,
                page!(),
                [x, y, x + width, y + height],
            )?;
            ok_response()
        }
        "scrubMetadata" => {
            dispatch::edit_scrub_metadata(editor)?;
            ok_response()
        }
        "removeFormField" => {
            dispatch::edit_remove_form_field(editor, req.get_str("name").unwrap_or(""))?;
            ok_response()
        }
        "setFormFieldReadOnly" => {
            dispatch::edit_set_form_field_readonly(
                editor,
                req.get_str("name").unwrap_or(""),
                req.get_bool("readOnly").unwrap_or(false),
            )?;
            ok_response()
        }
        "setFormFieldRequired" => {
            dispatch::edit_set_form_field_required(
                editor,
                req.get_str("name").unwrap_or(""),
                req.get_bool("required").unwrap_or(false),
            )?;
            ok_response()
        }
        "setFormFieldTooltip" => {
            dispatch::edit_set_form_field_tooltip(
                editor,
                req.get_str("name").unwrap_or(""),
                req.get_str("tooltip").unwrap_or(""),
            )?;
            ok_response()
        }
        "setFormFieldBounds" => {
            dispatch::edit_set_form_field_bounds(
                editor,
                req.get_str("name").unwrap_or(""),
                req.get_f64("x").unwrap_or(0.0) as f32,
                req.get_f64("y").unwrap_or(0.0) as f32,
                req.get_f64("width").unwrap_or(0.0) as f32,
                req.get_f64("height").unwrap_or(0.0) as f32,
            )?;
            ok_response()
        }
        "setFormFieldMaxLength" => {
            dispatch::edit_set_form_field_max_length(
                editor,
                req.get_str("name").unwrap_or(""),
                req.get_i32("maxLength").unwrap_or(0) as u32,
            )?;
            ok_response()
        }
        "setFormFieldAlignment" => {
            dispatch::edit_set_form_field_alignment(
                editor,
                req.get_str("name").unwrap_or(""),
                req.get_str("alignment").unwrap_or("left"),
            )?;
            ok_response()
        }
        "setFormFieldBackgroundColor" => {
            dispatch::edit_set_form_field_background_color(
                editor,
                req.get_str("name").unwrap_or(""),
                [
                    req.get_f64("r").unwrap_or(0.0) as f32,
                    req.get_f64("g").unwrap_or(0.0) as f32,
                    req.get_f64("b").unwrap_or(0.0) as f32,
                ],
            )?;
            ok_response()
        }
        "setFormFieldBorderColor" => {
            dispatch::edit_set_form_field_border_color(
                editor,
                req.get_str("name").unwrap_or(""),
                [
                    req.get_f64("r").unwrap_or(0.0) as f32,
                    req.get_f64("g").unwrap_or(0.0) as f32,
                    req.get_f64("b").unwrap_or(0.0) as f32,
                ],
            )?;
            ok_response()
        }
        "setFormFieldBorderWidth" => {
            dispatch::edit_set_form_field_border_width(
                editor,
                req.get_str("name").unwrap_or(""),
                req.get_f64("width").unwrap_or(0.0) as f32,
            )?;
            ok_response()
        }
        "setFormFieldAppearance" => {
            dispatch::edit_set_form_field_appearance(
                editor,
                req.get_str("name").unwrap_or(""),
                req.get_str("font").unwrap_or("Helv"),
                req.get_f64("fontSize").unwrap_or(12.0),
                req.get_f64("r").unwrap_or(0.0),
                req.get_f64("g").unwrap_or(0.0),
                req.get_f64("b").unwrap_or(0.0),
            )?;
            ok_response()
        }
        "setFormFieldFlags" => {
            dispatch::edit_set_form_field_flags(
                editor,
                req.get_str("name").unwrap_or(""),
                req.get_i32("flags").unwrap_or(0) as u32,
            )?;
            ok_response()
        }
        _ => Err(Error::InvalidPdf(format!("unknown editOp: {edit_op}"))),
    }
}

fn handle_editor_save(
    state: &mut LaneState,
    req: &Request<'_>,
    sink_writer: Option<BoxedWriter>,
    cancel: &crate::cancel::CancelCheck,
) -> Vec<u8> {
    let hid = req_handle(req);
    let editors = &mut state.editors;
    let editor = match editors.get_mut(&hid) {
        Some(e) => e,
        None => return ResponseWriter::error("editor not found"),
    };

    let deflate = req.get_bool("deflate").unwrap_or(true);
    let gc = req.get_bool("garbageCollect").unwrap_or(true);
    let object_streams = req.get_bool("objectStreams").unwrap_or(true);
    // A negative level is no level: the save refuses it as out of range.
    let deflate_level = req
        .get_i32("deflateLevel")
        .map_or(9, |level| u32::try_from(level).unwrap_or(u32::MAX));
    let merge_duplicates = req.get_bool("mergeDuplicates").unwrap_or(false);
    let prune_resources = req.get_bool("pruneResources").unwrap_or(false);
    let mode = req.get_i32("saveMode").unwrap_or(0);
    let encrypt_mode = req.get_i32("encryptMode").unwrap_or(0);
    let encryption = match encrypt_mode {
        // 0 = keep (the engine writes the source's own encryption)
        // 1 = remove (strip encryption)
        0 | 1 => None,
        // 2 = apply new encryption config
        2 => {
            let algo = match req.get_str("encryptAlgo").unwrap_or("aes256") {
                "rc4_40" => crate::editor::EncryptionAlgorithm::Rc4_40,
                "rc4_128" => crate::editor::EncryptionAlgorithm::Rc4_128,
                "aes128" => crate::editor::EncryptionAlgorithm::Aes128,
                "aes256" => crate::editor::EncryptionAlgorithm::Aes256,
                other => {
                    return ResponseWriter::error_kind(
                        ErrorKind::InvalidArgument,
                        &format!("unknown encryption algorithm: {other}"),
                    )
                }
            };
            let perm_bits = req.get_i32("encryptPermissions").unwrap_or(-1);
            Some(crate::editor::EncryptionConfig {
                user_password: req.get_str("encryptUserPw").unwrap_or("").to_string(),
                owner_password: req.get_str("encryptOwnerPw").unwrap_or("").to_string(),
                algorithm: algo,
                permissions: permissions_from_bits(perm_bits),
            })
        }
        _ => None,
    };
    let options = crate::editor::SaveOptions {
        incremental: mode == 1,
        compress: deflate,
        garbage_collect: gc,
        encryption,
        keep_source_encryption: encrypt_mode == 0,
        object_streams,
        deflate_level,
        merge_duplicates,
        prune_resources,
        ..Default::default()
    };

    match (options.incremental, sink_writer) {
        (true, Some(mut writer)) => {
            use crate::host::incremental::{write, IncrementalError};
            match write(editor, &mut writer, &options, cancel) {
                Ok(()) => ok_flag("streamed"),
                Err(IncrementalError::Refused(why)) => {
                    ResponseWriter::error_kind(ErrorKind::IncrementalRefused, &why)
                }
                Err(IncrementalError::Failed(e)) => ResponseWriter::error_of(&e),
            }
        },
        (false, Some(mut writer)) => {
            match dispatch::edit_save_with_options(editor, &mut writer, &options, cancel) {
                Ok(()) => ok_flag("streamed"),
                Err(e) => ResponseWriter::error_of(&e),
            }
        },
        (_, None) => match editor.save_to_bytes_with_options(options) {
            Ok(bytes) => {
                let mut w = ResponseWriter::ok();
                w.put_bytes("data", &bytes);
                w.finish()
            },
            Err(e) => ResponseWriter::error_of(&e),
        },
    }
}

/// Editor extract pages — select → save(sink) → restore page_order. The
/// save's prune plan drops everything only the other pages reach (#261).
/// Editor state is unchanged after the call. O(1) streaming via sink.
fn handle_editor_extract_pages(
    state: &mut LaneState,
    req: &Request<'_>,
    sink_writer: Option<BoxedWriter>,
    cancel: &crate::cancel::CancelCheck,
) -> Vec<u8> {
    let hid = req_handle(req);
    let editors = &mut state.editors;
    let editor = match editors.get_mut(&hid) {
        Some(e) => e,
        None => return ResponseWriter::error("editor not found"),
    };

    let pages = req.get_int_list("pages").unwrap_or(&[]);
    if pages.is_empty() {
        return ResponseWriter::error_kind(
            ErrorKind::InvalidArgument,
            "pages list must not be empty",
        );
    }

    let pages = match pages_of(pages, editor.current_page_count()) {
        Ok(p) => p,
        Err(resp) => return resp,
    };
    if let Err(e) = dispatch::refuse_repeated_pages(pages.iter().copied()) {
        return ResponseWriter::error_of(&e);
    }

    // Save with the page list cut down, then put it back — the editor is
    // unchanged after this call, merged pages included.
    let result = editor
        .with_pages_selected(&pages, |editor| {
            if let Some(mut writer) = sink_writer {
                dispatch::edit_save_with_options(
                    editor, &mut writer, &crate::editor::SaveOptions::full_rewrite(), cancel,
                )
            } else {
                Err(crate::error::Error::InvalidPdf(
                    "editorExtractPages requires a sink".into(),
                ))
            }
        })
        .and_then(|saved| saved);

    match result {
        Ok(()) => {
            ok_flag("streamed")
        }
        Err(e) => ResponseWriter::error_of(&e),
    }
}

// Decode PDF permission bits (same layout as Dart's PdfPermissions.toBits).
fn permissions_from_bits(bits: i32) -> crate::editor::Permissions {
    crate::editor::Permissions {
        print: bits & (1 << 2) != 0,
        print_high_quality: bits & (1 << 11) != 0,
        modify: bits & (1 << 3) != 0,
        copy: bits & (1 << 4) != 0,
        annotate: bits & (1 << 5) != 0,
        fill_forms: bits & (1 << 8) != 0,
        accessibility: bits & (1 << 9) != 0,
        assemble: bits & (1 << 10) != 0,
    }
}
