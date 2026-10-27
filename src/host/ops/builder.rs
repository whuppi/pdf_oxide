// Builder-handle ops: create pages from scratch. Core — always built.

use crate::host::binary_codec::{ErrorKind, FieldValue, Request, ResponseWriter};
use crate::host::bridge_api::{BoxedReader, BoxedWriter};
use crate::host::dispatch;
use crate::host::lane_state::LaneState;
use crate::host::ops::op_unit;
use crate::host::ops::support::{ok_flag, read_all_from_reader, req_handle};

use crate::writer::{DocumentBuilder, PageSize};

op_unit!(CREATE, "builderCreate", pdf_op_builder_create_anchor, |ctx| {
    handle_builder_create(ctx.state)
});

op_unit!(DISPOSE, "builderDispose", pdf_op_builder_dispose_anchor, |ctx| {
    let hid = req_handle(ctx.req);
    ctx.state.builders.remove(&hid);
    ok_flag("disposed")
});

op_unit!(
    SET_METADATA,
    "builderSetMetadata",
    pdf_op_builder_set_metadata_anchor,
    |ctx| handle_builder_set_metadata(ctx.state, ctx.req)
);

op_unit!(
    ADD_PAGE,
    "builderAddPage",
    pdf_op_builder_add_page_anchor,
    |ctx| handle_builder_add_page(ctx.state, ctx.req)
);

op_unit!(PAGE_OP, "builderPageOp", pdf_op_builder_page_op_anchor, |ctx| {
    let source = ctx.take_source(0);
    handle_builder_page_op(ctx.state, ctx.req, source)
});

op_unit!(
    PAGE_DONE,
    "builderPageDone",
    pdf_op_builder_page_done_anchor,
    |ctx| {
        let Some(builder) = ctx.state.builders.get_mut(&req_handle(ctx.req)) else {
            return ResponseWriter::error("builder not found");
        };
        builder.end_page();
        ok_flag("done")
    }
);

op_unit!(SAVE, "builderSave", pdf_op_builder_save_anchor, |ctx| {
    let sink = ctx.take_sink(0);
    handle_builder_save(ctx.state, ctx.req, sink)
});

fn handle_builder_create(state: &mut LaneState) -> Vec<u8> {
    let builder = dispatch::builder_new();
    let hid = state.next_handle_id();
    state.builders.insert(hid, builder);
    let mut w = ResponseWriter::ok();
    w.put_i32("handleId", hid as i32);
    w.finish()
}

fn handle_builder_set_metadata(state: &mut LaneState, req: &Request<'_>) -> Vec<u8> {
    let hid = req_handle(req);
    let Some(mut b) = state.builders.remove(&hid) else {
        return ResponseWriter::error("builder not found");
    };
    if let Some(v) = req.get_str("title") {
        b = dispatch::builder_set_title(b, v);
    }
    if let Some(v) = req.get_str("author") {
        b = dispatch::builder_set_author(b, v);
    }
    if let Some(v) = req.get_str("subject") {
        b = dispatch::builder_set_subject(b, v);
    }
    if let Some(v) = req.get_str("keywords") {
        b = dispatch::builder_set_keywords(b, v);
    }
    state.builders.insert(hid, b);
    ok_flag("set")
}

fn handle_builder_add_page(state: &mut LaneState, req: &Request<'_>) -> Vec<u8> {
    let size = match req.get_str("pageType") {
        Some("a4") => PageSize::A4,
        Some("letter") => PageSize::Letter,
        _ => PageSize::Custom(
            req.get_f64("width").unwrap_or(595.0) as f32,
            req.get_f64("height").unwrap_or(842.0) as f32,
        ),
    };
    let Some(builder) = state.builders.get_mut(&req_handle(req)) else {
        return ResponseWriter::error("builder not found");
    };
    builder.begin_page(size);
    ok_flag("added")
}

fn handle_builder_page_op(
    state: &mut LaneState,
    req: &Request<'_>,
    mut data_reader: Option<BoxedReader>,
) -> Vec<u8> {
    let hid = req_handle(req);
    let page_op = match req.get_str("pageOp") {
        Some(op) => op,
        None => return ResponseWriter::error("missing pageOp"),
    };

    let op = match page_op {
        "font" => dispatch::PageOp::Font(
            req.get_str("name").unwrap_or("Helvetica").to_string(),
            req.get_f64("size").unwrap_or(12.0) as f32,
        ),
        "at" => dispatch::PageOp::At(
            req.get_f64("x").unwrap_or(0.0) as f32,
            req.get_f64("y").unwrap_or(0.0) as f32,
        ),
        "text" => dispatch::PageOp::Text(req.get_str("text").unwrap_or("").to_string()),
        "heading" => dispatch::PageOp::Heading(
            req.get_i32("level").unwrap_or(1) as u8,
            req.get_str("text").unwrap_or("").to_string(),
        ),
        "paragraph" => dispatch::PageOp::Paragraph(req.get_str("text").unwrap_or("").to_string()),
        "space" => dispatch::PageOp::Space(req.get_f64("points").unwrap_or(12.0) as f32),
        "horizontalRule" => dispatch::PageOp::HorizontalRule,
        "watermark" => dispatch::PageOp::Watermark(req.get_str("text").unwrap_or("").to_string()),
        "image" => {
            let image_data = if let Some(ref mut reader) = data_reader {
                read_all_from_reader(reader).unwrap_or_default()
            } else {
                req.get_bytes("data").unwrap_or(&[]).to_vec()
            };
            dispatch::PageOp::Image {
                data: image_data,
                x: req.get_f64("x").unwrap_or(0.0) as f32,
                y: req.get_f64("y").unwrap_or(0.0) as f32,
                w: req.get_f64("width").unwrap_or(0.0) as f32,
                h: req.get_f64("height").unwrap_or(0.0) as f32,
                alt: req.get_str("altText").unwrap_or("").to_string(),
            }
        }
        "textField" => dispatch::PageOp::TextField {
            name: req.get_str("name").unwrap_or("").to_string(),
            x: req.get_f64("x").unwrap_or(0.0) as f32,
            y: req.get_f64("y").unwrap_or(0.0) as f32,
            w: req.get_f64("width").unwrap_or(0.0) as f32,
            h: req.get_f64("height").unwrap_or(0.0) as f32,
            default_value: req.get_str("defaultValue").map(|s| s.to_string()),
        },
        "checkbox" => dispatch::PageOp::Checkbox {
            name: req.get_str("name").unwrap_or("").to_string(),
            x: req.get_f64("x").unwrap_or(0.0) as f32,
            y: req.get_f64("y").unwrap_or(0.0) as f32,
            w: req.get_f64("width").unwrap_or(0.0) as f32,
            h: req.get_f64("height").unwrap_or(0.0) as f32,
            checked: req.get_bool("checked").unwrap_or(false),
        },
        "comboBox" => dispatch::PageOp::ComboBox {
            name: req.get_str("name").unwrap_or("").to_string(),
            x: req.get_f64("x").unwrap_or(0.0) as f32,
            y: req.get_f64("y").unwrap_or(0.0) as f32,
            w: req.get_f64("width").unwrap_or(0.0) as f32,
            h: req.get_f64("height").unwrap_or(0.0) as f32,
            options: req.get_string_list("options").unwrap_or_default().into_iter().map(|s| s.to_string()).collect(),
            selected: req.get_str("selected").map(|s| s.to_string()),
        },
        "pushButton" => dispatch::PageOp::PushButton {
            name: req.get_str("name").unwrap_or("").to_string(),
            x: req.get_f64("x").unwrap_or(0.0) as f32,
            y: req.get_f64("y").unwrap_or(0.0) as f32,
            w: req.get_f64("width").unwrap_or(0.0) as f32,
            h: req.get_f64("height").unwrap_or(0.0) as f32,
            caption: req.get_str("caption").unwrap_or("").to_string(),
        },
        "signatureField" => dispatch::PageOp::SignatureField {
            name: req.get_str("name").unwrap_or("").to_string(),
            x: req.get_f64("x").unwrap_or(0.0) as f32,
            y: req.get_f64("y").unwrap_or(0.0) as f32,
            w: req.get_f64("width").unwrap_or(0.0) as f32,
            h: req.get_f64("height").unwrap_or(0.0) as f32,
        },
        "radioGroup" => {
            let options = req.get_map_list("options").unwrap_or(&[]);
            dispatch::PageOp::RadioGroup {
                name: req.get_str("name").unwrap_or("").to_string(),
                values: options.iter().map(|o| option_str(o, "value")).collect(),
                xs: options.iter().map(|o| option_f32(o, "x")).collect(),
                ys: options.iter().map(|o| option_f32(o, "y")).collect(),
                ws: options.iter().map(|o| option_f32(o, "width")).collect(),
                hs: options.iter().map(|o| option_f32(o, "height")).collect(),
                selected: req.get_str("selected").map(|s| s.to_string()),
            }
        }
        "fieldKeystroke" => dispatch::PageOp::FieldKeystroke(req.get_str("script").unwrap_or("").to_string()),
        "fieldFormat" => dispatch::PageOp::FieldFormat(req.get_str("script").unwrap_or("").to_string()),
        "fieldValidate" => dispatch::PageOp::FieldValidate(req.get_str("script").unwrap_or("").to_string()),
        "fieldCalculate" => dispatch::PageOp::FieldCalculate(req.get_str("script").unwrap_or("").to_string()),
        "linkUrl" => dispatch::PageOp::LinkUrl(req.get_str("url").unwrap_or("").to_string()),
        "linkPage" => dispatch::PageOp::LinkPage(req.get_i32("targetPage").unwrap_or(0) as usize),
        "footnote" => dispatch::PageOp::Footnote {
            ref_mark: req.get_str("refMark").unwrap_or("").to_string(),
            note_text: req.get_str("noteText").unwrap_or("").to_string(),
        },
        "columns" => dispatch::PageOp::Columns {
            column_count: req.get_i32("columnCount").unwrap_or(2) as u32,
            gap_pt: req.get_f64("gapPt").unwrap_or(12.0) as f32,
            text: req.get_str("text").unwrap_or("").to_string(),
        },
        "newline" => dispatch::PageOp::Newline,
        "newPageSameSize" => dispatch::PageOp::NewPageSameSize,
        "inline" => dispatch::PageOp::Inline(req.get_str("text").unwrap_or("").to_string()),
        "inlineBold" => dispatch::PageOp::InlineBold(req.get_str("text").unwrap_or("").to_string()),
        "inlineItalic" => dispatch::PageOp::InlineItalic(req.get_str("text").unwrap_or("").to_string()),
        "inlineColor" => dispatch::PageOp::InlineColor {
            r: req.get_f64("r").unwrap_or(0.0) as f32,
            g: req.get_f64("g").unwrap_or(0.0) as f32,
            b: req.get_f64("b").unwrap_or(0.0) as f32,
            text: req.get_str("text").unwrap_or("").to_string(),
        },
        "textInRect" => dispatch::PageOp::TextInRect {
            x: req.get_f64("x").unwrap_or(0.0) as f32,
            y: req.get_f64("y").unwrap_or(0.0) as f32,
            w: req.get_f64("width").unwrap_or(0.0) as f32,
            h: req.get_f64("height").unwrap_or(0.0) as f32,
            text: req.get_str("text").unwrap_or("").to_string(),
            align: align_from_int(req.get_i32("align").unwrap_or(0)),
        },
        "rect" => dispatch::PageOp::Rect(
            req.get_f64("x").unwrap_or(0.0) as f32,
            req.get_f64("y").unwrap_or(0.0) as f32,
            req.get_f64("width").unwrap_or(0.0) as f32,
            req.get_f64("height").unwrap_or(0.0) as f32,
        ),
        "filledRect" => dispatch::PageOp::FilledRect(
            req.get_f64("x").unwrap_or(0.0) as f32,
            req.get_f64("y").unwrap_or(0.0) as f32,
            req.get_f64("width").unwrap_or(0.0) as f32,
            req.get_f64("height").unwrap_or(0.0) as f32,
            req.get_f64("r").unwrap_or(0.0) as f32,
            req.get_f64("g").unwrap_or(0.0) as f32,
            req.get_f64("b").unwrap_or(0.0) as f32,
        ),
        "line" => dispatch::PageOp::Line(
            req.get_f64("x1").unwrap_or(0.0) as f32,
            req.get_f64("y1").unwrap_or(0.0) as f32,
            req.get_f64("x2").unwrap_or(0.0) as f32,
            req.get_f64("y2").unwrap_or(0.0) as f32,
        ),
        "strokeRect" => {
            let dash: Vec<f32> = req
                .get_f64_list("dash")
                .unwrap_or(&[])
                .iter()
                .map(|d| *d as f32)
                .collect();
            if dash.is_empty() {
                dispatch::PageOp::StrokeRect {
                    x: req.get_f64("x").unwrap_or(0.0) as f32,
                    y: req.get_f64("y").unwrap_or(0.0) as f32,
                    w: req.get_f64("width").unwrap_or(0.0) as f32,
                    h: req.get_f64("height").unwrap_or(0.0) as f32,
                    width: req.get_f64("lineWidth").unwrap_or(1.0) as f32,
                    r: req.get_f64("r").unwrap_or(0.0) as f32,
                    g: req.get_f64("g").unwrap_or(0.0) as f32,
                    b: req.get_f64("b").unwrap_or(0.0) as f32,
                }
            } else {
                dispatch::PageOp::StrokeRectDashed {
                    x: req.get_f64("x").unwrap_or(0.0) as f32,
                    y: req.get_f64("y").unwrap_or(0.0) as f32,
                    w: req.get_f64("width").unwrap_or(0.0) as f32,
                    h: req.get_f64("height").unwrap_or(0.0) as f32,
                    width: req.get_f64("lineWidth").unwrap_or(1.0) as f32,
                    r: req.get_f64("r").unwrap_or(0.0) as f32,
                    g: req.get_f64("g").unwrap_or(0.0) as f32,
                    b: req.get_f64("b").unwrap_or(0.0) as f32,
                    dash,
                    phase: req.get_f64("dashPhase").unwrap_or(0.0) as f32,
                }
            }
        }
        "strokeLine" => {
            let dash: Vec<f32> = req
                .get_f64_list("dash")
                .unwrap_or(&[])
                .iter()
                .map(|d| *d as f32)
                .collect();
            if dash.is_empty() {
                dispatch::PageOp::StrokeLine {
                    x1: req.get_f64("x1").unwrap_or(0.0) as f32,
                    y1: req.get_f64("y1").unwrap_or(0.0) as f32,
                    x2: req.get_f64("x2").unwrap_or(0.0) as f32,
                    y2: req.get_f64("y2").unwrap_or(0.0) as f32,
                    width: req.get_f64("lineWidth").unwrap_or(1.0) as f32,
                    r: req.get_f64("r").unwrap_or(0.0) as f32,
                    g: req.get_f64("g").unwrap_or(0.0) as f32,
                    b: req.get_f64("b").unwrap_or(0.0) as f32,
                }
            } else {
                dispatch::PageOp::StrokeLineDashed {
                    x1: req.get_f64("x1").unwrap_or(0.0) as f32,
                    y1: req.get_f64("y1").unwrap_or(0.0) as f32,
                    x2: req.get_f64("x2").unwrap_or(0.0) as f32,
                    y2: req.get_f64("y2").unwrap_or(0.0) as f32,
                    width: req.get_f64("lineWidth").unwrap_or(1.0) as f32,
                    r: req.get_f64("r").unwrap_or(0.0) as f32,
                    g: req.get_f64("g").unwrap_or(0.0) as f32,
                    b: req.get_f64("b").unwrap_or(0.0) as f32,
                    dash,
                    phase: req.get_f64("dashPhase").unwrap_or(0.0) as f32,
                }
            }
        }
        "highlight" => dispatch::PageOp::Highlight(
            req.get_f64("r").unwrap_or(1.0) as f32,
            req.get_f64("g").unwrap_or(1.0) as f32,
            req.get_f64("b").unwrap_or(0.0) as f32,
        ),
        "underline" => dispatch::PageOp::Underline(
            req.get_f64("r").unwrap_or(0.0) as f32,
            req.get_f64("g").unwrap_or(0.0) as f32,
            req.get_f64("b").unwrap_or(0.0) as f32,
        ),
        "strikeout" => dispatch::PageOp::Strikeout(
            req.get_f64("r").unwrap_or(0.0) as f32,
            req.get_f64("g").unwrap_or(0.0) as f32,
            req.get_f64("b").unwrap_or(0.0) as f32,
        ),
        "squiggly" => dispatch::PageOp::Squiggly(
            req.get_f64("r").unwrap_or(0.0) as f32,
            req.get_f64("g").unwrap_or(0.0) as f32,
            req.get_f64("b").unwrap_or(0.0) as f32,
        ),
        "stickyNote" => {
            let text = req.get_str("text").unwrap_or("").to_string();
            match (req.get_f64("x"), req.get_f64("y")) {
                (Some(x), Some(y)) => {
                    dispatch::PageOp::StickyNoteAt(x as f32, y as f32, text)
                }
                (None, None) => dispatch::PageOp::StickyNote(text),
                _ => {
                    return ResponseWriter::error_kind(
                        ErrorKind::InvalidArgument,
                        "stickyNote takes x and y together or neither",
                    )
                }
            }
        }
        "stamp" => dispatch::PageOp::Stamp(parse_stamp_name(req.get_str("stampType").unwrap_or(""))),
        "freeText" => dispatch::PageOp::FreeText {
            x: req.get_f64("x").unwrap_or(0.0) as f32,
            y: req.get_f64("y").unwrap_or(0.0) as f32,
            w: req.get_f64("width").unwrap_or(0.0) as f32,
            h: req.get_f64("height").unwrap_or(0.0) as f32,
            text: req.get_str("text").unwrap_or("").to_string(),
        },
        "watermarkConfidential" => dispatch::PageOp::WatermarkConfidential,
        "watermarkDraft" => dispatch::PageOp::WatermarkDraft,
        "linkNamed" => dispatch::PageOp::LinkNamed(req.get_str("destination").unwrap_or("").to_string()),
        "linkJavaScript" => dispatch::PageOp::LinkJavaScript(req.get_str("script").unwrap_or("").to_string()),
        "onOpen" => dispatch::PageOp::OnOpen(req.get_str("script").unwrap_or("").to_string()),
        "onClose" => dispatch::PageOp::OnClose(req.get_str("script").unwrap_or("").to_string()),
        "imageArtifact" => {
            let image_data = if let Some(ref mut reader) = data_reader {
                read_all_from_reader(reader).unwrap_or_default()
            } else {
                req.get_bytes("data").unwrap_or(&[]).to_vec()
            };
            dispatch::PageOp::ImageArtifact {
                data: image_data,
                x: req.get_f64("x").unwrap_or(0.0) as f32,
                y: req.get_f64("y").unwrap_or(0.0) as f32,
                w: req.get_f64("width").unwrap_or(0.0) as f32,
                h: req.get_f64("height").unwrap_or(0.0) as f32,
            }
        }
        "table" => {
            let columns = table_columns_from_request(req);
            let cells = req.get_string_list("cells").unwrap_or_default();
            let column_count = req.get_i32("columnCount").unwrap_or(0);
            if column_count <= 0 || cells.len() % column_count as usize != 0 {
                return ResponseWriter::error_kind(
                    ErrorKind::InvalidArgument,
                    "table cells length must be a nonzero multiple of columnCount",
                );
            }
            let rows: Vec<Vec<String>> = cells
                .chunks(column_count as usize)
                .map(|row| row.iter().map(|s| s.to_string()).collect())
                .collect();
            dispatch::PageOp::BufferedTable {
                columns,
                rows,
                has_header: req.get_bool("hasHeader").unwrap_or(true),
            }
        }
        "streamingTable" => {
            let columns = table_columns_from_request(req);
            if columns.iter().any(|(_, width, _)| width.is_none()) {
                return ResponseWriter::error_kind(
                    ErrorKind::InvalidArgument,
                    "streamingTable requires an explicit width for every column",
                );
            }
            let cells = req.get_string_list("cells").unwrap_or_default();
            let row_spans = req.get_int_list("rowSpans").unwrap_or_default().to_vec();
            let column_count = req.get_i32("columnCount").unwrap_or(0);
            let rows = match pack_streaming_rows(&cells, &row_spans, column_count) {
                Ok(rows) => rows,
                Err(msg) => {
                    return ResponseWriter::error_kind(ErrorKind::InvalidArgument, msg);
                }
            };
            let config_columns: Vec<(String, f32, crate::writer::CellAlign)> = columns
                .into_iter()
                .map(|(header, width, align)| (header, width.unwrap_or(0.0), align))
                .collect();
            dispatch::PageOp::StreamingTableBlock {
                config_columns,
                repeat_header: req.get_bool("repeatHeader").unwrap_or(true),
                rows,
                mode: req.get_str("mode").unwrap_or("fixed").to_string(),
                sample_rows: req.get_i32("sampleRows").unwrap_or(50) as usize,
                min_col_width_pt: req.get_f64("minColWidth").unwrap_or(20.0) as f32,
                max_col_width_pt: req.get_f64("maxColWidth").unwrap_or(400.0) as f32,
                max_rowspan: req.get_i32("maxRowSpan").unwrap_or(1) as usize,
            }
        }
        _ => {
            return ResponseWriter::error(&format!("unknown pageOp: {page_op}"));
        }
    };

    let Some(builder) = state.builders.get_mut(&hid) else {
        return ResponseWriter::error("builder not found");
    };
    if let Some(msg) = refuse_unattached(builder, &op) {
        return ResponseWriter::error_kind(ErrorKind::InvalidArgument, msg);
    }

    match dispatch::apply_page_op(builder, op) {
        Ok(()) => ok_flag("applied"),
        Err(e) => ResponseWriter::error_of(&e),
    }
}

fn handle_builder_save(state: &mut LaneState, req: &Request<'_>, sink_writer: Option<BoxedWriter>) -> Vec<u8> {
    let hid = req_handle(req);
    let builders_map = &mut state.builders;
    let builder = match builders_map.remove(&hid) {
        Some(b) => b,
        None => return ResponseWriter::error("builder not found"),
    };

    let compress = req.get_bool("compress").unwrap_or(true);
    let mut builder = builder.compress_streams(compress);
    builder.end_page();

    if let Some(mut writer) = sink_writer {
        match dispatch::builder_save_to_writer(builder, &mut writer) {
            Ok(()) => {
                ok_flag("streamed")
            }
            Err(e) => ResponseWriter::error_of(&e),
        }
    } else {
        match dispatch::builder_save(builder) {
            Ok(bytes) => {
                let mut w = ResponseWriter::ok();
                w.put_bytes("data", &bytes);
                w.finish()
            }
            Err(e) => ResponseWriter::error_of(&e),
        }
    }
}

/// Pack a streaming table's flat cells into rows of `column_count` cells,
/// each paired with the number of rows it spans downward. A spanned cell
/// in a later row is still listed, as an empty placeholder.
fn pack_streaming_rows(
    cells: &[&str],
    row_spans: &[i32],
    column_count: i32,
) -> Result<Vec<Vec<(String, usize)>>, &'static str> {
    if column_count <= 0 || cells.len() % column_count as usize != 0 {
        return Err("streamingTable cells length must be a nonzero multiple of columnCount");
    }
    if row_spans.len() != cells.len() {
        return Err("streamingTable cells and rowSpans must be parallel lists");
    }
    if row_spans.iter().any(|span| *span < 1) {
        return Err("streamingTable rowSpans must be at least 1");
    }
    Ok(cells
        .chunks(column_count as usize)
        .zip(row_spans.chunks(column_count as usize))
        .map(|(row, spans)| {
            row.iter()
                .zip(spans)
                .map(|(text, span)| (text.to_string(), *span as usize))
                .collect()
        })
        .collect())
}

/// A string field of one `radioGroup` option map (empty when absent).
fn option_str(option: &[(&str, FieldValue<'_>)], key: &str) -> String {
    option
        .iter()
        .find_map(|(k, v)| match v {
            FieldValue::Str(s) if *k == key => Some(s.to_string()),
            _ => None,
        })
        .unwrap_or_default()
}

/// A number field of one `radioGroup` option map (0 when absent); whole
/// numbers may arrive as integers, as in `Request::get_f64`.
fn option_f32(option: &[(&str, FieldValue<'_>)], key: &str) -> f32 {
    option
        .iter()
        .find_map(|(k, v)| match v {
            FieldValue::F64(n) if *k == key => Some(*n as f32),
            FieldValue::I32(n) if *k == key => Some(*n as f32),
            FieldValue::I64(n) if *k == key => Some(*n as f32),
            _ => None,
        })
        .unwrap_or(0.0)
}

/// Map the wire alignment index (0 left / 1 center / 2 right) — the same
/// encoding as the WASM binding's `Align` — onto the writer's `TextAlign`.
fn align_from_int(v: i32) -> crate::writer::TextAlign {
    match v {
        1 => crate::writer::TextAlign::Center,
        2 => crate::writer::TextAlign::Right,
        _ => crate::writer::TextAlign::Left,
    }
}

fn align_from_index(v: i32) -> crate::writer::CellAlign {
    match v {
        1 => crate::writer::CellAlign::Center,
        2 => crate::writer::CellAlign::Right,
        _ => crate::writer::CellAlign::Left,
    }
}

/// Read the table-column arrays (`headers`, `widths` with -1 for auto,
/// `aligns`) shared by the buffered and streaming table ops.
fn table_columns_from_request(
    req: &Request<'_>,
) -> Vec<(String, Option<f32>, crate::writer::CellAlign)> {
    let headers = req.get_string_list("headers").unwrap_or_default();
    let widths = req.get_f64_list("widths").unwrap_or(&[]);
    let aligns = req.get_int_list("aligns").unwrap_or(&[]);
    headers
        .into_iter()
        .enumerate()
        .map(|(i, h)| {
            let width = widths.get(i).copied().filter(|w| *w >= 0.0).map(|w| w as f32);
            let align = aligns.get(i).map(|a| align_from_index(*a));
            let align = align.unwrap_or(crate::writer::CellAlign::Left);
            (h.to_string(), width, align)
        })
        .collect()
}

/// Parse a stamp-type name — the same names the WASM binding's
/// `parse_wasm_stamp_type` accepts. Unknown names fall through to
/// `Custom(name)`.
fn parse_stamp_name(name: &str) -> crate::writer::StampType {
    use crate::writer::StampType;
    match name {
        "Approved" => StampType::Approved,
        "Experimental" => StampType::Experimental,
        "NotApproved" => StampType::NotApproved,
        "AsIs" => StampType::AsIs,
        "Expired" => StampType::Expired,
        "NotForPublicRelease" => StampType::NotForPublicRelease,
        "Confidential" => StampType::Confidential,
        "Final" => StampType::Final,
        "Sold" => StampType::Sold,
        "Departmental" => StampType::Departmental,
        "ForComment" => StampType::ForComment,
        "TopSecret" => StampType::TopSecret,
        "Draft" => StampType::Draft,
        "ForPublicRelease" => StampType::ForPublicRelease,
        other => StampType::Custom(other.to_string()),
    }
}

/// Refuse a field-script or link op with nothing on the builder's current
/// page to attach to. The engine drops such calls silently — a field script
/// needs a `form_field_meta` entry, a link a `last_text_rect` — so the host
/// refuses instead of reporting success over an op that never lands. With
/// no current page there is nothing to attach to either.
fn refuse_unattached(builder: &DocumentBuilder, op: &dispatch::PageOp) -> Option<&'static str> {
    use dispatch::PageOp;
    let unattached = match op {
        PageOp::FieldKeystroke(_) => "fieldKeystroke needs a form field on this page first",
        PageOp::FieldFormat(_) => "fieldFormat needs a form field on this page first",
        PageOp::FieldValidate(_) => "fieldValidate needs a form field on this page first",
        PageOp::FieldCalculate(_) => "fieldCalculate needs a form field on this page first",
        PageOp::LinkUrl(_) => "linkUrl needs text on this page first",
        PageOp::LinkPage(_) => "linkPage needs text on this page first",
        PageOp::LinkNamed(_) => "linkNamed needs text on this page first",
        PageOp::LinkJavaScript(_) => "linkJavaScript needs text on this page first",
        PageOp::Highlight(..) => "highlight needs text on this page first",
        PageOp::Underline(..) => "underline needs text on this page first",
        PageOp::Strikeout(..) => "strikeout needs text on this page first",
        PageOp::Squiggly(..) => "squiggly needs text on this page first",
        _ => return None,
    };
    let wants_field = matches!(
        op,
        PageOp::FieldKeystroke(_)
            | PageOp::FieldFormat(_)
            | PageOp::FieldValidate(_)
            | PageOp::FieldCalculate(_)
    );
    let attached = builder.current_page_index().is_some_and(|page| {
        if wants_field {
            builder.page_has_form_field(page)
        } else {
            builder.page_has_text(page)
        }
    });
    if !attached {
        Some(unattached)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use crate::host::dispatch::{apply_page_op, PageOp};
    use crate::writer::DocumentBuilder;

    /// `op`'s refusal on a real builder after `queued` was applied to it.
    fn refuse_unattached(queued: &[PageOp], op: &PageOp) -> Option<&'static str> {
        let mut builder = DocumentBuilder::new();
        for queued_op in queued {
            apply_page_op(&mut builder, queued_op.clone()).unwrap();
        }
        super::refuse_unattached(&builder, op)
    }

    fn text_field() -> PageOp {
        PageOp::TextField {
            name: "field".into(),
            x: 50.0,
            y: 700.0,
            w: 200.0,
            h: 20.0,
            default_value: None,
        }
    }

    #[test]
    fn script_with_no_field_is_refused() {
        assert_eq!(
            refuse_unattached(&[], &PageOp::FieldKeystroke("s".into())),
            Some("fieldKeystroke needs a form field on this page first")
        );
    }

    #[test]
    fn script_after_a_field_is_allowed() {
        let queued = vec![text_field()];
        assert_eq!(refuse_unattached(&queued, &PageOp::FieldFormat("s".into())), None);
    }

    #[test]
    fn script_after_a_field_then_new_page_same_size_is_refused() {
        let queued = vec![text_field(), PageOp::NewPageSameSize];
        assert_eq!(
            refuse_unattached(&queued, &PageOp::FieldValidate("s".into())),
            Some("fieldValidate needs a form field on this page first")
        );
    }

    #[test]
    fn link_with_no_text_is_refused() {
        assert_eq!(
            refuse_unattached(&[], &PageOp::LinkUrl("https://example.com".into())),
            Some("linkUrl needs text on this page first")
        );
    }

    #[test]
    fn link_after_text_is_allowed() {
        let queued = vec![PageOp::Text("hello".into())];
        assert_eq!(refuse_unattached(&queued, &PageOp::LinkPage(0)), None);
    }

    #[test]
    fn link_after_heading_is_allowed() {
        let queued = vec![PageOp::Heading(1, "Title".into())];
        assert_eq!(
            refuse_unattached(&queued, &PageOp::LinkUrl("https://example.com".into())),
            None
        );
    }

    #[test]
    fn link_named_with_no_text_is_refused() {
        assert_eq!(
            refuse_unattached(&[], &PageOp::LinkNamed("sec-1".into())),
            Some("linkNamed needs text on this page first")
        );
    }

    #[test]
    fn link_javascript_with_no_text_is_refused() {
        assert_eq!(
            refuse_unattached(&[], &PageOp::LinkJavaScript("app.alert(1)".into())),
            Some("linkJavaScript needs text on this page first")
        );
    }

    #[test]
    fn highlight_with_no_text_is_refused() {
        assert_eq!(
            refuse_unattached(&[], &PageOp::Highlight(1.0, 1.0, 0.0)),
            Some("highlight needs text on this page first")
        );
    }

    #[test]
    fn underline_with_no_text_is_refused() {
        assert_eq!(
            refuse_unattached(&[], &PageOp::Underline(0.0, 0.0, 0.0)),
            Some("underline needs text on this page first")
        );
    }

    #[test]
    fn strikeout_with_no_text_is_refused() {
        assert_eq!(
            refuse_unattached(&[], &PageOp::Strikeout(0.0, 0.0, 0.0)),
            Some("strikeout needs text on this page first")
        );
    }

    #[test]
    fn squiggly_with_no_text_is_refused() {
        assert_eq!(
            refuse_unattached(&[], &PageOp::Squiggly(1.0, 0.0, 0.0)),
            Some("squiggly needs text on this page first")
        );
    }

    #[test]
    fn inline_counts_as_text() {
        let queued = vec![PageOp::Inline("run".into())];
        assert_eq!(
            refuse_unattached(&queued, &PageOp::Highlight(1.0, 1.0, 0.0)),
            None
        );
    }

    #[test]
    fn inline_bold_counts_as_text() {
        let queued = vec![PageOp::InlineBold("run".into())];
        assert_eq!(
            refuse_unattached(&queued, &PageOp::LinkNamed("sec-1".into())),
            None
        );
    }

    #[test]
    fn inline_italic_counts_as_text() {
        let queued = vec![PageOp::InlineItalic("run".into())];
        assert_eq!(
            refuse_unattached(&queued, &PageOp::Underline(0.0, 0.0, 0.0)),
            None
        );
    }

    #[test]
    fn inline_color_counts_as_text() {
        let queued = vec![PageOp::InlineColor {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            text: "run".into(),
        }];
        assert_eq!(
            refuse_unattached(&queued, &PageOp::Squiggly(1.0, 0.0, 0.0)),
            None
        );
    }

    #[test]
    fn text_in_rect_counts_as_text() {
        let queued = vec![PageOp::TextInRect {
            x: 0.0,
            y: 0.0,
            w: 100.0,
            h: 20.0,
            text: "run".into(),
            align: crate::writer::TextAlign::Left,
        }];
        assert_eq!(
            refuse_unattached(&queued, &PageOp::LinkJavaScript("app.alert(1)".into())),
            None
        );
    }

    #[test]
    fn inline_then_new_page_resets_text() {
        let queued = vec![PageOp::Inline("run".into()), PageOp::NewPageSameSize];
        assert_eq!(
            refuse_unattached(&queued, &PageOp::LinkUrl("https://example.com".into())),
            Some("linkUrl needs text on this page first")
        );
    }

    #[test]
    fn script_on_a_page_with_text_but_no_field_is_refused() {
        let queued = vec![PageOp::Text("hello".into())];
        assert_eq!(
            refuse_unattached(&queued, &PageOp::FieldKeystroke("s".into())),
            Some("fieldKeystroke needs a form field on this page first")
        );
    }

    #[test]
    fn link_on_a_page_with_a_field_but_no_text_is_refused() {
        let queued = vec![text_field()];
        assert_eq!(
            refuse_unattached(&queued, &PageOp::LinkUrl("https://example.com".into())),
            Some("linkUrl needs text on this page first")
        );
    }

    #[test]
    fn unusable_image_bytes_are_an_invalid_argument() {
        let mut builder = DocumentBuilder::new();
        for op in [
            PageOp::Image { data: vec![1, 2, 3], x: 0.0, y: 0.0, w: 10.0, h: 10.0, alt: String::new() },
            PageOp::ImageArtifact { data: vec![1, 2, 3], x: 0.0, y: 0.0, w: 10.0, h: 10.0 },
        ] {
            let err = apply_page_op(&mut builder, op).unwrap_err();
            assert_eq!(
                crate::host::binary_codec::kind_of(&err),
                crate::host::binary_codec::ErrorKind::InvalidArgument
            );
        }
        apply_page_op(&mut builder, PageOp::Text("still usable".into())).unwrap();
    }

    #[test]
    fn streaming_rows_pack_by_column_count_with_the_span_on_the_cell() {
        let rows = super::pack_streaming_rows(&["Saw", "3", "", "4"], &[2, 1, 1, 1], 2).unwrap();
        assert_eq!(
            rows,
            vec![
                vec![("Saw".to_string(), 2), ("3".to_string(), 1)],
                vec![(String::new(), 1), ("4".to_string(), 1)],
            ]
        );
        assert!(super::pack_streaming_rows(&["a", "b", "c"], &[1, 1, 1], 2).is_err());
        assert!(super::pack_streaming_rows(&["a", "b"], &[1, 0], 2).is_err());
        assert!(super::pack_streaming_rows(&["a", "b"], &[1], 2).is_err());
    }

    #[test]
    fn spanned_streaming_rows_reach_the_engine() {
        let mut builder = DocumentBuilder::new();
        let op = PageOp::StreamingTableBlock {
            config_columns: vec![
                ("a".into(), 100.0, crate::writer::CellAlign::Left),
                ("b".into(), 60.0, crate::writer::CellAlign::Left),
            ],
            repeat_header: true,
            rows: super::pack_streaming_rows(&["Saw", "3", "", "4"], &[2, 1, 1, 1], 2).unwrap(),
            mode: "fixed".into(),
            sample_rows: 50,
            min_col_width_pt: 20.0,
            max_col_width_pt: 400.0,
            max_rowspan: 2,
        };
        apply_page_op(&mut builder, op).unwrap();
    }

    #[test]
    fn auto_all_streaming_table_row_is_refused() {
        let mut builder = DocumentBuilder::new();
        let op = PageOp::StreamingTableBlock {
            config_columns: vec![("h".into(), 100.0, crate::writer::CellAlign::Left)],
            repeat_header: true,
            rows: vec![vec![("c".into(), 1)]],
            mode: "auto_all".into(),
            sample_rows: 50,
            min_col_width_pt: 20.0,
            max_col_width_pt: 400.0,
            max_rowspan: 1,
        };
        assert!(apply_page_op(&mut builder, op).is_err());
    }

    #[test]
    fn script_after_ending_the_page_is_refused() {
        let mut builder = DocumentBuilder::new();
        apply_page_op(&mut builder, text_field()).unwrap();
        builder.end_page();
        assert_eq!(
            super::refuse_unattached(&builder, &PageOp::FieldFormat("s".into())),
            Some("fieldFormat needs a form field on this page first")
        );
    }

    #[test]
    fn a_page_op_after_ending_the_page_opens_a_new_page() {
        let mut builder = DocumentBuilder::new();
        apply_page_op(&mut builder, PageOp::Text("one".into())).unwrap();
        builder.end_page();
        apply_page_op(&mut builder, PageOp::Text("two".into())).unwrap();
        assert_eq!(builder.current_page_index(), Some(1));
    }

    #[test]
    fn new_page_same_size_keeps_the_size_and_clears_text_and_fields() {
        let mut builder = DocumentBuilder::new();
        builder.begin_page(crate::writer::PageSize::Custom(300.0, 400.0));
        apply_page_op(&mut builder, text_field()).unwrap();
        apply_page_op(&mut builder, PageOp::Text("hello".into())).unwrap();
        apply_page_op(&mut builder, PageOp::NewPageSameSize).unwrap();
        assert_eq!(builder.current_page_index(), Some(1));
        assert_eq!(builder.page_mut(1).page_dimensions(), (300.0, 400.0));
        assert!(!builder.page_has_text(1));
        assert!(!builder.page_has_form_field(1));
        assert_eq!(
            super::refuse_unattached(&builder, &PageOp::LinkUrl("https://example.com".into())),
            Some("linkUrl needs text on this page first")
        );
    }

    /// A document's objects with their numbers and references masked, in
    /// sorted order: the writer numbers objects in hash-map order, so two
    /// builds of one document differ in numbering and order alone.
    fn canonical(pdf: &[u8]) -> Vec<String> {
        let text = String::from_utf8_lossy(pdf);
        let mut objects: Vec<&str> = text.split("endobj").collect();
        objects.pop(); // the trailer after the last object
        let mut masked: Vec<String> = objects
            .into_iter()
            .map(|object| {
                let body = object.split_once("obj").map_or(object, |(_, rest)| rest);
                let tokens: Vec<&str> = body.split_whitespace().collect();
                let mut out = Vec::with_capacity(tokens.len());
                let mut i = 0;
                while i < tokens.len() {
                    let is_ref = i + 2 < tokens.len()
                        && tokens[i].parse::<u32>().is_ok()
                        && tokens[i + 1] == "0"
                        && tokens[i + 2].starts_with('R');
                    if is_ref {
                        out.push(format!("R{}", &tokens[i + 2][1..]));
                        i += 3;
                    } else {
                        out.push(tokens[i].to_string());
                        i += 1;
                    }
                }
                out.join(" ")
            })
            .collect();
        masked.sort();
        masked
    }

    /// One page built through the wrapper chain and through in-place calls
    /// is the same document.
    #[test]
    fn page_builder_matches_the_fluent_chain() {
        use crate::geometry::Rect;
        use crate::writer::{PageSize, TextAlign};
        let rect = Rect::new(72.0, 560.0, 200.0, 40.0);
        let long = "Words that wrap across the page and run past its bottom margin. ".repeat(40);

        let mut chained = DocumentBuilder::new().title("golden");
        chained
            .page(PageSize::Letter)
            .font("Helvetica", 12.0)
            .at(72.0, 700.0)
            .heading(1, "Title")
            .text("see")
            .link_url("https://example.com")
            .paragraph(&long)
            .text_field("name", 72.0, 500.0, 150.0, 20.0, None)
            .required()
            .field_keystroke("AFNumber_Keystroke(2)")
            .text_in_rect(rect, "boxed", TextAlign::Center)
            .highlight((1.0, 1.0, 0.0))
            .sticky_note("note")
            .new_page_same_size()
            .text("second")
            .done();

        let mut placed = DocumentBuilder::new().title("golden");
        let index = placed.begin_page(PageSize::Letter);
        placed
            .page_mut(index)
            .font("Helvetica", 12.0)
            .at(72.0, 700.0)
            .heading(1, "Title")
            .text("see")
            .link_url("https://example.com")
            .paragraph(&long)
            .text_field("name", 72.0, 500.0, 150.0, 20.0, None)
            .required()
            .field_keystroke("AFNumber_Keystroke(2)")
            .text_in_rect(rect, "boxed", TextAlign::Center)
            .highlight((1.0, 1.0, 0.0))
            .sticky_note("note")
            .new_page_same_size()
            .text("second");
        placed.end_page();

        assert_eq!(canonical(&chained.build().unwrap()), canonical(&placed.build().unwrap()));
    }
}
