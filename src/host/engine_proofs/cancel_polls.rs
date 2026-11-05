//! Every loop over a document-scale collection polls for a cancel, and a
//! cancel at any of its polls leaves the document or the editor as it was.
//!
//! Each proof runs one operation under `CancelCheck::tripping_at(n)` for every
//! `n` from the first poll to the last one a full run makes: each of those
//! runs ends with the cancelled error, and then the same document or editor
//! does the operation again with no cancel and gets what a fresh one gets, so
//! no cancelled run left a cache or a staged object behind. The loops they
//! reach were polled for `tool/fuelprobe` and `tool/check_cancel_polls.sh`:
//! the operator stream of a page, the document-wide running-header pre-pass,
//! the pages of a classification or a validation, the bookmarks of an
//! outline, the pages of a merge, the pages and objects of a redaction, a
//! save and an incremental save, the rebuild of a damaged file's table and
//! the operators a renderer paints.
//!
//! No Dart battery can observe this: a cancel from Dart lands at whatever
//! poll the timing reaches, so a battery sees "cancelled or finished", never
//! that every one of the loop's polls is a safe place to stop, and never that
//! a poll exists in a loop a small input finishes before the cancel arrives.

use super::support::{pdf_from_objects, stream_object};
use crate::cancel::CancelCheck;
use crate::document::PdfDocument;
use crate::editor::{DocumentEditor, SaveOptions};
use crate::error::{Error, Result};
use std::fmt::Debug;

fn is_cancelled(error: &Error) -> bool {
    matches!(error, Error::Io(e) if e.to_string() == "operation cancelled")
}

/// `pages` pages of `ops` text-showing operators each, one shared font.
fn text_pdf(pages: usize, ops: usize) -> Vec<u8> {
    let mut objects = vec![
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_vec(),
        Vec::new(),
        b"3 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n".to_vec(),
    ];
    let mut kids = String::new();
    for i in 0..pages {
        let page = 4 + 2 * i as u32;
        kids.push_str(&format!("{page} 0 R "));
        objects.push(
            format!(
                "{page} 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 300] \
                 /Contents {} 0 R /Resources << /Font << /F1 3 0 R >> >> >>\nendobj\n",
                page + 1
            )
            .into_bytes(),
        );
        let mut content = format!("BT /F1 8 Tf\n1 0 0 1 20 280 Tm (page {i}) Tj\n");
        for k in 0..ops {
            content.push_str(&format!("1 0 0 1 {} {} Tm (w{k}) Tj\n", 20 + k % 200, 20 + k % 250));
        }
        content.push_str("ET\n72 100 100 20 re S\n");
        objects.push(stream_object(page + 1, "", content.as_bytes()));
    }
    objects[1] = format!("2 0 obj\n<< /Type /Pages /Kids [{kids}] /Count {pages} >>\nendobj\n").into_bytes();
    pdf_from_objects(&objects)
}

fn open(bytes: &[u8]) -> PdfDocument {
    PdfDocument::from_bytes(bytes.to_vec()).expect("the fixture parses")
}

/// Runs `op` on fresh documents under a check that cancels at poll `n`, for
/// every `n` up to the polls of a full run; the document that was cancelled
/// then runs it again to the full run's result.
fn read_at_every_poll<T: PartialEq + Debug>(
    fresh: impl Fn() -> PdfDocument,
    op: impl Fn(&mut PdfDocument, &CancelCheck) -> Result<T>,
    min_polls: usize,
) -> usize {
    let counting = CancelCheck::tripping_at(usize::MAX);
    let whole = op(&mut fresh(), &counting).expect("a full run succeeds");
    let polls = counting.polls();
    assert!(polls >= min_polls, "a full run polls at least {min_polls} times: {polls}");
    for n in 1..=polls {
        let mut doc = fresh();
        match op(&mut doc, &CancelCheck::tripping_at(n)) {
            Ok(_) => panic!("a cancel at poll {n} of {polls} did not stop the op"),
            Err(error) => assert!(is_cancelled(&error), "poll {n} of {polls} ended with {error:?}"),
        }
        let again = op(&mut doc, &CancelCheck::none()).expect("the document still works");
        assert!(again == whole, "after a cancel at poll {n} of {polls} the result differs");
    }
    polls
}

/// What an editor holds that a save writes.
fn snapshot(editor: &mut DocumentEditor) -> (Vec<u8>, Vec<u32>, usize) {
    let bytes = editor.save_to_bytes().expect("the editor saves");
    let mut staged: Vec<u32> = editor.modified_objects_mut().keys().copied().collect();
    staged.sort_unstable();
    (bytes, staged, editor.current_page_count())
}

fn editor_of(bytes: &[u8]) -> DocumentEditor {
    DocumentEditor::from_document(open(bytes)).expect("the editor opens")
}

/// Runs `op` on fresh editors under a check that cancels at poll `n`, for
/// every `n` up to the polls of a full run: each ends cancelled with the
/// editor as it was. `changes` says whether a full run changes the editor.
fn editor_at_every_poll(
    fresh: impl Fn() -> DocumentEditor,
    op: impl Fn(&mut DocumentEditor, &CancelCheck) -> Result<()>,
    changes: bool,
    min_polls: usize,
) -> usize {
    let mut editor = fresh();
    let before = snapshot(&mut editor);
    let counting = CancelCheck::tripping_at(usize::MAX);
    op(&mut editor, &counting).expect("a full run succeeds");
    let polls = counting.polls();
    assert!(polls >= min_polls, "a full run polls at least {min_polls} times, once for each page of each loop: {polls}");
    if changes {
        assert!(snapshot(&mut editor) != before, "a full run changes the editor");
    }
    for n in 1..=polls {
        let mut editor = fresh();
        let before = snapshot(&mut editor);
        match op(&mut editor, &CancelCheck::tripping_at(n)) {
            Ok(()) => panic!("a cancel at poll {n} of {polls} did not stop the op"),
            Err(error) => assert!(is_cancelled(&error), "poll {n} of {polls} ended with {error:?}"),
        }
        assert!(snapshot(&mut editor) == before, "poll {n} of {polls} left the editor changed");
    }
    polls
}

/// No Dart battery sees it: the text of a page is read through its operator
/// stream, the running-header pre-pass reads every page twice, and a cancel
/// that lands in either must neither return a partial page nor leave a
/// partial page in the span or char caches the next read serves from.
#[test]
fn extract_text_cancelled_at_any_poll_caches_nothing() {
    let bytes = text_pdf(3, 5000);
    read_at_every_poll(
        || open(&bytes),
        |doc, cancel| {
        crate::host::dispatch::extract_text(doc, None, "plainText", cancel).map(|r| r.text)
    },
        // every page's pre-pass reads and the operator streams of three pages of 5,000 operators
        61,
    );
}

/// No Dart battery sees it: a search builds the page's index from its
/// operators, and a cancel inside that must not leave a half-built index in
/// the document's search cache.
#[test]
fn search_cancelled_at_any_poll_caches_nothing() {
    let bytes = text_pdf(3, 5000);
    read_at_every_poll(
        || open(&bytes),
        |doc, cancel| {
        crate::host::dispatch::search_text(doc, "w4999", None, cancel).map(|r| r.hits.len())
    },
        // the search index of three pages and the pre-pass
        40,
    );
}

/// No Dart battery sees it: classification walks every page, and only a check
/// that cancels at an exact page can prove the walk stops before the next.
#[test]
fn classify_document_cancelled_at_any_poll_stops_the_walk() {
    let bytes = text_pdf(4, 20);
    read_at_every_poll(
        || open(&bytes),
        |doc, cancel| {
        crate::host::dispatch::classify_document(doc, cancel).map(|r| r.type_name)
    },
        // one poll before each of four pages
        4,
    );
}

/// A catalog with a flat outline of `n` bookmarks, one to each of `n` pages.
fn outline_pdf(n: usize) -> Vec<u8> {
    let first_item = 4 + 2 * n as u32 + 1;
    let mut objects = vec![
        format!("1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Outlines {} 0 R >>\nendobj\n", first_item - 1)
            .into_bytes(),
        Vec::new(),
        b"3 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n".to_vec(),
    ];
    let mut kids = String::new();
    for i in 0..n {
        let page = 4 + 2 * i as u32;
        kids.push_str(&format!("{page} 0 R "));
        objects.push(
            format!(
                "{page} 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents {} 0 R >>\nendobj\n",
                page + 1
            )
            .into_bytes(),
        );
        objects.push(stream_object(page + 1, "", b"0 0 m 1 1 l S\n"));
    }
    objects[1] = format!("2 0 obj\n<< /Type /Pages /Kids [{kids}] /Count {n} >>\nendobj\n").into_bytes();
    objects.push(
        format!(
            "{} 0 obj\n<< /Type /Outlines /First {first_item} 0 R /Last {} 0 R /Count {n} >>\nendobj\n",
            first_item - 1,
            first_item + n as u32 - 1
        )
        .into_bytes(),
    );
    for i in 0..n {
        let id = first_item + i as u32;
        let prev = if i > 0 { format!("/Prev {} 0 R", id - 1) } else { String::new() };
        let next = if i + 1 < n { format!("/Next {} 0 R", id + 1) } else { String::new() };
        objects.push(
            format!(
                "{id} 0 obj\n<< /Title (B{i}) /Parent {} 0 R {prev} {next} /Dest [{} 0 R /Fit] >>\nendobj\n",
                first_item - 1,
                4 + 2 * i
            )
            .into_bytes(),
        );
    }
    pdf_from_objects(&objects)
}

/// No Dart battery sees it: each bookmark resolves its page by a walk of the
/// page tree, so the outline is the quadratic part of a split plan, and only
/// a cancel at an exact bookmark proves the walk stops there.
#[test]
fn bookmark_ranges_cancelled_at_any_poll_stops_the_walk() {
    let bytes = outline_pdf(6);
    read_at_every_poll(
        || open(&bytes),
        |doc, cancel| {
        crate::host::dispatch::plan_split_by_bookmarks(doc, cancel)
            .map(|splits| splits.iter().map(|s| (s.start_page, s.end_page)).collect::<Vec<_>>())
    },
        // one poll before each of six bookmarks and one after the last
        7,
    );
}

/// No Dart battery sees it: a merge imports its pages one at a time into an
/// editor that keeps serving, and a cancel between two of them must put the
/// editor's object-id counter back so the next merge numbers as the first did.
#[test]
fn merge_cancelled_at_any_poll_changes_nothing() {
    let base = text_pdf(2, 3);
    let other = text_pdf(4, 3);
    editor_at_every_poll(
        || editor_of(&base),
        |editor, cancel| {
            let source = DocumentEditor::open_merge_source(Box::new(std::io::Cursor::new(other.clone())))?;
            editor.merge_document(source, None, cancel).map(|_| ())
        },
        true,
        // four pages: four polls of the page-tree walk, four of the import
        8,
    );
    // The same after a cancel: the merge that finishes numbers its objects as
    // a merge on a fresh editor does.
    let merged = |cancel_first: bool| {
        let mut editor = editor_of(&base);
        if cancel_first {
            let source = DocumentEditor::open_merge_source(Box::new(std::io::Cursor::new(other.clone()))).unwrap();
            assert!(editor.merge_document(source, None, &CancelCheck::tripping_at(3)).is_err());
        }
        let source = DocumentEditor::open_merge_source(Box::new(std::io::Cursor::new(other.clone()))).unwrap();
        editor.merge_document(source, None, &CancelCheck::none()).unwrap();
        editor.save_to_bytes().unwrap()
    };
    assert!(merged(true) == merged(false), "a cancelled merge changed the next one's numbering");
}

/// No Dart battery sees it: the redaction plans every page before it records
/// any, so a cancel at any of the plan's polls must leave every page whole,
/// and only a check that cancels at an exact poll can try each.
#[test]
fn apply_redactions_cancelled_at_any_poll_changes_nothing() {
    let bytes = text_pdf(3, 30);
    editor_at_every_poll(
        || {
            let mut editor = editor_of(&bytes);
            for page in 0..3 {
                editor.add_redaction(page, [10.0, 10.0, 200.0, 200.0], None).expect("the mark queues");
            }
            editor
        },
        |editor, cancel| crate::host::dispatch::edit_apply_redactions(editor, cancel).map(|_| ()),
        true,
        // three pages: three polls of the scan for marked pages, three of the plan
        6,
    );
}

#[cfg(feature = "pdfa")]
/// No Dart battery sees it: the PDF/A validators walk every page's fonts and
/// content, and a cancel must end the walk without a verdict.
#[test]
fn validate_pdf_a_cancelled_at_any_poll_gives_no_verdict() {
    let bytes = text_pdf(3, 5000);
    read_at_every_poll(
        || open(&bytes),
        |doc, cancel| {
        crate::host::dispatch::validate_pdf_a(doc, 2, cancel).map(|r| (r.compliant, r.errors, r.warnings))
    },
        // nine rule batches, three pages of fonts, three of colors and the operator streams of the colors
        22,
    );
}

#[cfg(feature = "pdfa")]
/// No Dart battery sees it: a conversion that embeds a font walks every page
/// for the objects that use it and rewrites each, and the whole conversion is
/// all or nothing, so each poll is a place to prove the editor is untouched.
#[test]
fn convert_to_pdf_a_with_fonts_cancelled_at_any_poll_changes_nothing() {
    let bytes = text_pdf(3, 5);
    editor_at_every_poll(
        || editor_of(&bytes),
        |editor, cancel| crate::host::dispatch::edit_convert_to_pdf_a(editor, 2, cancel),
        true,
        38,
    );
}

/// No Dart battery sees it: the full save writes every page, then every
/// object no page reached, then the cross-reference stream, and each of those
/// loops polls; a cancel in any of them must leave the editor able to save
/// the same file again.
#[test]
fn full_save_cancelled_at_any_poll_leaves_the_editor_saving_the_same_file() {
    let bytes = text_pdf(5, 3);
    let baseline = {
        let mut editor = editor_of(&bytes);
        let mut out = crate::host::positioned_write::CountingWriter::new(Vec::new());
        crate::host::dispatch::edit_save_with_options(
            &mut editor,
            &mut out,
            &SaveOptions::full_rewrite(),
            &CancelCheck::none(),
        )
        .unwrap();
        out.into_inner()
    };
    let save = |editor: &mut DocumentEditor, cancel: &CancelCheck| {
        let mut out = crate::host::positioned_write::CountingWriter::new(Vec::new());
        crate::host::dispatch::edit_save_with_options(editor, &mut out, &SaveOptions::full_rewrite(), cancel)
            .map(|()| out.into_inner())
    };
    let counting = CancelCheck::tripping_at(usize::MAX);
    save(&mut editor_of(&bytes), &counting).expect("a full save succeeds");
    let polls = counting.polls();
    assert!(polls >= 55, "a full save of five pages polls at least 55 times: {polls}");
    for n in 1..=polls {
        let mut editor = editor_of(&bytes);
        match save(&mut editor, &CancelCheck::tripping_at(n)) {
            Ok(_) => panic!("a cancel at poll {n} of {polls} did not stop the save"),
            Err(error) => assert!(is_cancelled(&error), "poll {n} of {polls} ended with {error:?}"),
        }
        assert!(
            save(&mut editor, &CancelCheck::none()).unwrap() == baseline,
            "after a cancel at poll {n} of {polls} the editor saves another file"
        );
    }
}

/// No Dart battery sees it: the incremental save serialises every staged
/// object before it writes a byte, and a cancel there must leave the source
/// untouched and the editor able to append the same update again.
#[test]
fn incremental_save_cancelled_at_any_poll_leaves_the_editor_saving_the_same_update() {
    let bytes = text_pdf(5, 3);
    let fresh = || {
        let mut editor = editor_of(&bytes);
        for page in 0..5 {
            editor.set_page_rotation(page, 90).expect("the page turns");
        }
        editor
    };
    let save = |editor: &mut DocumentEditor, cancel: &CancelCheck| {
        let mut out = crate::host::positioned_write::CountingWriter::new(Vec::new());
        crate::host::incremental::write(editor, &mut out, &SaveOptions::incremental(), cancel)
            .map_err(|e| match e {
                crate::host::incremental::IncrementalError::Refused(why) => crate::error::Error::InvalidOperation(why),
                crate::host::incremental::IncrementalError::Failed(e) => e,
            })
            .map(|()| out.into_inner())
    };
    let baseline = save(&mut fresh(), &CancelCheck::none()).expect("an incremental save succeeds");
    let counting = CancelCheck::tripping_at(usize::MAX);
    save(&mut fresh(), &counting).expect("a counted save succeeds");
    let polls = counting.polls();
    assert!(polls >= 16, "an incremental save of five staged pages polls at least 16 times: {polls}");
    for n in 1..=polls {
        let mut editor = fresh();
        match save(&mut editor, &CancelCheck::tripping_at(n)) {
            Ok(_) => panic!("a cancel at poll {n} of {polls} did not stop the save"),
            Err(error) => assert!(is_cancelled(&error), "poll {n} of {polls} ended with {error:?}"),
        }
        assert!(
            save(&mut editor, &CancelCheck::none()).unwrap() == baseline,
            "after a cancel at poll {n} of {polls} the editor appends another update"
        );
    }
}

/// A file with `objects` indirect objects and no cross-reference table.
fn damaged_pdf(objects: usize) -> Vec<u8> {
    let mut out = b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n\
        2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n\
        3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] >>\nendobj\n"
        .to_vec();
    for i in 0..objects {
        out.extend_from_slice(format!("{} 0 obj\n<< /N {i} >>\nendobj\n", 4 + i).as_bytes());
    }
    out.extend_from_slice(b"trailer\n<< /Size 9999 /Root 1 0 R >>\nstartxref\n7\n%%EOF\n");
    out
}

/// No Dart battery sees it: the table of a damaged file is rebuilt by a scan
/// that finishes in a blink on any file a battery can hold, so only a file
/// of thousands of objects under a check that cancels at the first poll shows
/// the scan polls at all.
#[test]
fn open_of_a_damaged_file_cancelled_in_the_rebuild_scan() {
    let bytes = damaged_pdf(9000);
    let reader = || Box::new(std::io::Cursor::new(bytes.clone()));
    let counting = CancelCheck::tripping_at(usize::MAX);
    PdfDocument::from_external_reader_cancellable(reader(), &counting).expect("the file opens");
    let polls = counting.polls();
    assert!(polls >= 2, "a scan of 9000 objects polls at least twice: {polls}");
    for n in 1..=polls {
        match PdfDocument::from_external_reader_cancellable(reader(), &CancelCheck::tripping_at(n)) {
            Ok(_) => panic!("a cancel at poll {n} of {polls} did not stop the open"),
            Err(error) => assert!(is_cancelled(&error), "poll {n} of {polls} ended with {error:?}"),
        }
    }
}

#[cfg(feature = "rendering")]
/// No Dart battery sees it: a band of the page paints every operator before
/// it hands the rows on, and the renderer polls between operators, which a
/// battery's page of a few operators never reaches.
#[test]
fn render_cancelled_between_the_operators_of_a_band() {
    use crate::host::render_stream::{render_pages_streamed, RenderFormat, RenderSize};
    let bytes = text_pdf(1, 200);
    let doc = open(&bytes);
    let render = |cancel: &CancelCheck| {
        render_pages_streamed(&doc, &[0], RenderSize::Dpi(72), RenderFormat::Png, &mut std::io::sink(), cancel)
    };
    let counting = CancelCheck::tripping_at(usize::MAX);
    render(&counting).expect("the page renders");
    let polls = counting.polls();
    assert!(polls > 4, "a page of 200 operators polls between them: {polls}");
    for n in 1..=polls {
        match render(&CancelCheck::tripping_at(n)) {
            Ok(_) => panic!("a cancel at poll {n} of {polls} did not stop the render"),
            Err(error) => assert!(is_cancelled(&error), "poll {n} of {polls} ended with {error:?}"),
        }
    }
    assert!(render(&CancelCheck::none()).is_ok(), "the document renders after the cancels");
}

/// No Dart battery sees it: these edits take no check, so their loops poll
/// the check the job entered, and only a check that cancels at an exact poll
/// proves each page of the list is a safe place to stop with the editor as it
/// was.
#[test]
fn page_list_edits_cancelled_at_any_poll_change_nothing() {
    let bytes = text_pdf(6, 2);
    let all = [0usize, 1, 2, 3, 4, 5];
    editor_at_every_poll(
        || editor_of(&bytes),
        |editor, cancel| {
            let _job = crate::cancel::enter(cancel);
            crate::host::dispatch::edit_select_pages(editor, &[5, 3, 1])
        },
        true,
        // one poll for each of the three pages of the list's repeat check
        3,
    );
    editor_at_every_poll(
        || editor_of(&bytes),
        |editor, cancel| {
            let _job = crate::cancel::enter(cancel);
            crate::host::dispatch::edit_add_stamp(editor, &all, "Approved", 10.0, 10.0, 60.0, 20.0, 1.0)
        },
        true,
        // the repeat check and the editable check of six pages, then the build of six stamps
        18,
    );
    editor_at_every_poll(
        || editor_of(&bytes),
        |editor, cancel| {
            let _job = crate::cancel::enter(cancel);
            crate::host::dispatch::edit_watermark(
                editor,
                Some(&all),
                "DRAFT",
                24.0,
                45.0,
                0.5,
                0.5,
                0.5,
                0.5,
                "background",
                crate::host::dispatch::WatermarkPosition::Centre,
            )
        },
        true,
        // the repeat check of six pages and the font plan of six pages
        12,
    );
}

/// One page of `n` save, transform and restore triples: the operators an image
/// walk reads, since it skips the text ones.
fn graphics_pdf(n: usize) -> Vec<u8> {
    let content = "q 1 0 0 1 5 5 cm Q\n".repeat(n);
    let objects = vec![
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_vec(),
        b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n".to_vec(),
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 4 0 R >>\nendobj\n".to_vec(),
        stream_object(4, "", content.as_bytes()),
    ];
    pdf_from_objects(&objects)
}

/// No Dart battery sees it: a long page's operators are read for its images
/// by a loop that takes no check, and a cancel in it must leave neither a
/// half-read page in the document's caches nor a different result for the
/// next read.
#[test]
fn page_images_of_a_long_page_cancelled_at_any_poll_caches_nothing() {
    let bytes = graphics_pdf(3000);
    read_at_every_poll(
        || open(&bytes),
        |doc, cancel| {
            let _job = crate::cancel::enter(cancel);
            doc.page_image_handles(0).map(|handles| handles.len())
        },
        // two strides of 4,096 operators in 9,000 (3,000 triples)
        2,
    );
    let mut editor_polls = 0;
    editor_polls += editor_at_every_poll(
        || editor_of(&bytes),
        |editor, cancel| {
            let _job = crate::cancel::enter(cancel);
            editor.get_page_images(0).map(|_| ())
        },
        false,
        2,
    );
    assert!(editor_polls >= 2);
}

/// No Dart battery sees it: `get_page_images` lexes the page's whole content
/// stream before it walks one operator, and a cancel that lands in that parse
/// must leave the editor as it was, with nothing cached for the next read.
#[test]
fn get_page_images_cancelled_in_the_parse_of_a_long_page_changes_nothing() {
    let bytes = graphics_pdf(8_000);
    let polls = editor_at_every_poll(
        || editor_of(&bytes),
        |editor, cancel| {
            let _job = crate::cancel::enter(cancel);
            editor.get_page_images(0).map(|_| ())
        },
        false,
        // 72,000 tokens (nine to a triple): 17 strides of 4,096 in the parse and 5 in the walk of 24,000 operators
        22,
    );
    assert!(polls >= 22, "the parse and the walk both poll: {polls}");
    let mut editor = editor_of(&bytes);
    let whole = editor.get_page_images(0).expect("an uncancelled read");
    let mut cancelled = editor_of(&bytes);
    {
        let _job = crate::cancel::enter(&CancelCheck::tripping_at(1));
        assert!(cancelled.get_page_images(0).is_err());
    }
    assert_eq!(
        cancelled.get_page_images(0).expect("the editor still reads").len(),
        whole.len()
    );
}
