//! Merged pages: copies, page-list edits and what the save writes.

use super::support::{contains, pdf_from_objects, stream_object};
use crate::editor::{DocumentEditor, PageKey, PageSlot, SaveOptions};
use crate::object::{Object, ObjectRef};
use std::collections::HashSet;

const MERGE_CONTENT: &[u8] = b"BT\n/F1 12 Tf\n1 0 0 1 72 700 Tm\n(MERGEDTEXT) Tj\nET\n";
const MERGE_IMAGE: [u8; 48] = [
    7, 200, 13, 91, 42, 250, 3, 118, 66, 29, 180, 5, 99, 141, 77, 230, 18, 61, 154, 9, 205,
    88, 37, 172, 24, 111, 246, 53, 130, 2, 195, 70, 16, 223, 84, 149, 31, 167, 58, 240, 12,
    104, 191, 45, 126, 8, 215, 93,
];

/// One page with a known content stream, an unfiltered image, a link
/// whose /P and /Dest name the page, a text field, outlines and an
/// AcroForm with /DR.
fn merge_source_pdf_bytes() -> Vec<u8> {
    pdf_from_objects(&[
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Outlines 9 0 R \
          /AcroForm << /Fields [10 0 R] /DA (/Helv 0 Tf 0 g) /DR << /Font << /Helv 8 0 R >> >> >> >>\nendobj\n"
            .to_vec(),
        b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n".to_vec(),
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 400] /Contents 4 0 R \
          /Resources << /Font << /F1 8 0 R >> /XObject << /Im1 5 0 R >> >> /Annots [6 0 R 10 0 R] >>\nendobj\n"
            .to_vec(),
        stream_object(4, "", MERGE_CONTENT),
        stream_object(
            5,
            "/Type /XObject /Subtype /Image /Width 4 /Height 4 /ColorSpace /DeviceRGB /BitsPerComponent 8",
            &MERGE_IMAGE,
        ),
        b"6 0 obj\n<< /Type /Annot /Subtype /Link /Rect [10 10 90 40] /P 3 0 R \
          /Dest [3 0 R /Fit] /Border [0 0 0] >>\nendobj\n"
            .to_vec(),
        b"7 0 obj\n<< /Unused true >>\nendobj\n".to_vec(),
        b"8 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n".to_vec(),
        b"9 0 obj\n<< /Type /Outlines /Count 0 >>\nendobj\n".to_vec(),
        b"10 0 obj\n<< /Type /Annot /Subtype /Widget /FT /Tx /T (mergedfield) /Rect [10 100 90 120] \
          /P 3 0 R /DA (/Helv 0 Tf 0 g) >>\nendobj\n"
            .to_vec(),
    ])
}

fn merge_base_pdf_bytes() -> Vec<u8> {
    crate::api::Pdf::from_text("base page").unwrap().into_bytes()
}

fn merged_editor() -> DocumentEditor {
    let mut editor = DocumentEditor::from_bytes(merge_base_pdf_bytes()).unwrap();
    assert_eq!(editor.merge_from_bytes(&merge_source_pdf_bytes()).unwrap(), 1);
    editor
}

/// No Dart battery sees it: the claim is about raw stream bytes in an uncompressed save; a decoded read cannot tell a copy from a re-encode, and test-guards ban byte-grep content claims in Dart.
#[test]
fn merged_content_and_image_are_copied_byte_for_byte() {
    let mut editor = merged_editor();
    let bytes = editor
        .save_to_bytes_with_options(SaveOptions { compress: false, ..SaveOptions::full_rewrite() })
        .unwrap();

    assert!(contains(&bytes, MERGE_CONTENT), "content stream bytes differ");
    assert!(contains(&bytes, &MERGE_IMAGE), "image bytes differ");
}

/// No Dart battery sees it: it lists every object header of the saved file, which no Dart call can.
#[test]
fn merged_save_writes_one_object_per_id() {
    let mut editor = merged_editor();
    let bytes = editor.save_to_bytes().unwrap();

    let text = String::from_utf8_lossy(&bytes);
    let mut ids = Vec::new();
    for line in text.lines() {
        if let Some(id) = line.strip_suffix(" 0 obj").and_then(|n| n.parse::<u32>().ok()) {
            ids.push(id);
        }
    }
    let unique: HashSet<u32> = ids.iter().copied().collect();
    assert_eq!(ids.len(), unique.len(), "an object id was written twice: {ids:?}");

    let doc = crate::document::PdfDocument::from_bytes(bytes).unwrap();
    for id in &ids {
        assert!(!doc.is_free_entry(*id), "object {id} is missing from the xref");
    }
}

/// No Dart battery sees it: it reads the link's `/P` and `/Dest` references, which `PdfAnnotationInfo` does not expose.
#[test]
fn merged_link_keeps_its_destination_and_points_at_the_merged_page() {
    let mut editor = merged_editor();
    let bytes = editor.save_to_bytes().unwrap();
    let doc = crate::document::PdfDocument::from_bytes(bytes).unwrap();

    let page_ref = doc.get_page_ref(1).unwrap();
    let page = doc.load_object(page_ref).unwrap();
    let annots = page.as_dict().unwrap().get("Annots").unwrap().as_array().unwrap().clone();
    let link = annots
        .iter()
        .filter_map(|a| a.as_reference())
        .filter_map(|r| doc.load_object(r).ok())
        .find(|o| o.as_dict().and_then(|d| d.get("Subtype")).and_then(|s| s.as_name()) == Some("Link"))
        .expect("the merged page lost its link");
    let link = link.as_dict().unwrap();
    assert_eq!(link.get("P").and_then(|p| p.as_reference()), Some(page_ref));
    let dest = link.get("Dest").and_then(|d| d.as_array()).expect("link lost its /Dest");
    assert_eq!(dest[0].as_reference(), Some(page_ref), "a destination names the merged page");
}

/// No Dart battery sees it: it reads the saved catalog's `/AcroForm /DR` and `/Fields`, which no Dart call returns.
#[test]
fn merge_does_not_import_outlines_and_lists_merged_fields() {
    let mut editor = merged_editor();
    let bytes = editor.save_to_bytes().unwrap();
    let doc = crate::document::PdfDocument::from_bytes(bytes).unwrap();

    let catalog = doc.catalog().unwrap();
    let catalog = catalog.as_dict().unwrap();
    assert!(!catalog.contains_key("Outlines"), "the first document's outline is kept, not the merged one");

    let acroform = catalog.get("AcroForm").and_then(|a| a.as_dict()).expect("no AcroForm");
    let fields = acroform.get("Fields").and_then(|f| f.as_array()).expect("no /Fields");
    assert_eq!(fields.len(), 1);
    let field = doc.load_object(fields[0].as_reference().unwrap()).unwrap();
    assert_eq!(
        field.as_dict().and_then(|d| d.get("T")).and_then(|t| t.as_string()),
        Some(&b"mergedfield"[..])
    );
    assert!(acroform.contains_key("DR"), "the merged /DR is carried");
}

/// One page per marker, all sharing one font. The page text is the marker
/// in parentheses, so a raw save holds `(marker)` once per written page.
fn marker_pdf_bytes(markers: &[&str]) -> Vec<u8> {
    let font_id = 3 + 2 * markers.len();
    let kids: Vec<String> = (0..markers.len()).map(|i| format!("{} 0 R", 3 + 2 * i)).collect();
    let mut objects = vec![
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_vec(),
        format!(
            "2 0 obj\n<< /Type /Pages /Kids [{}] /Count {} >>\nendobj\n",
            kids.join(" "),
            markers.len()
        )
        .into_bytes(),
    ];
    for (i, marker) in markers.iter().enumerate() {
        objects.push(
            format!(
                "{} 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 400] /Contents {} 0 R \
                 /Resources << /Font << /F1 {font_id} 0 R >> >> >>\nendobj\n",
                3 + 2 * i,
                4 + 2 * i
            )
            .into_bytes(),
        );
        let content = format!("BT\n/F1 18 Tf\n1 0 0 1 20 200 Tm\n({marker}) Tj\nET\n");
        objects.push(stream_object(4 + 2 * i as u32, "", content.as_bytes()));
    }
    objects.push(
        format!("{font_id} 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n")
            .into_bytes(),
    );
    pdf_from_objects(&objects)
}

/// Two source pages (`S0`, `S1`) with three merged pages (`M0`, `M1`,
/// `M2`) after them.
fn editor_with_three_merged_pages() -> DocumentEditor {
    let mut editor = DocumentEditor::from_bytes(marker_pdf_bytes(&["S0", "S1"])).unwrap();
    assert_eq!(editor.merge_from_bytes(&marker_pdf_bytes(&["M0", "M1", "M2"])).unwrap(), 3);
    editor
}

/// The text of every page of a saved file, in page order.
fn page_texts(bytes: &[u8]) -> Vec<String> {
    let doc = crate::document::PdfDocument::from_bytes(bytes.to_vec()).unwrap();
    (0..doc.page_count().unwrap())
        .map(|i| doc.extract_text(i).unwrap().trim().to_string())
        .collect()
}

fn raw_save() -> SaveOptions {
    SaveOptions { compress: false, ..SaveOptions::full_rewrite() }
}

/// No Dart battery sees it: it measures the size of a private type.
#[test]
fn a_page_slot_holds_both_kinds_of_page_in_four_bytes() {
    assert_eq!(std::mem::size_of::<PageSlot>(), 4);
    for index in [0, 1, 9_999, (1 << 31) - 1] {
        assert_eq!(PageSlot::source(index).key(), PageKey::Source(index));
        assert_eq!(PageSlot::merged(index).key(), PageKey::Merged(index));
    }
    assert_ne!(PageSlot::source(3), PageSlot::merged(3));
}

/// No Dart battery sees it: `duplicate_page` has no Dart counterpart, and the test lists the saved `/Kids`.
#[test]
fn a_duplicated_page_is_a_new_page_object_and_every_page_is_reachable() {
    use crate::editor::EditableDocument;
    let mut editor = editor_with_three_merged_pages();
    assert_eq!(editor.duplicate_page(0).unwrap(), 5);
    assert_eq!(editor.duplicate_page(3).unwrap(), 6);
    assert_eq!(editor.current_page_count(), 7);

    let bytes = editor.save_to_bytes().unwrap();
    let objects = saved_objects(&bytes);
    let pages = objects
        .iter()
        .find(|o| o.as_dict().and_then(|d| d.get("Type")).and_then(|t| t.as_name()) == Some("Pages"))
        .unwrap();
    let kids = pages.as_dict().and_then(|d| d.get("Kids")).and_then(|k| k.as_array()).unwrap();
    assert_eq!(kids.len(), 7);
    let ids: HashSet<u32> = kids.iter().filter_map(|k| k.as_reference()).map(|r| r.id).collect();
    assert_eq!(ids.len(), 7, "a page object is listed in /Kids once: {kids:?}");

    let doc = crate::document::PdfDocument::from_bytes(bytes).unwrap();
    assert_eq!(doc.page_count().unwrap(), 7);
    let texts: Vec<String> =
        (0..7).map(|i| doc.extract_text(i).unwrap().trim().to_string()).collect();
    assert_eq!(texts, ["S0", "S1", "M0", "M1", "M2", "S0", "M1"]);
}

/// No Dart battery sees it: `extract_pages_to_bytes` on an editor that holds merged pages has no Dart counterpart; Dart's `extractPages` opens its own source.
#[test]
fn extracting_pages_of_a_merged_document_leaves_the_editor_unchanged() {
    let mut editor = editor_with_three_merged_pages();
    let extracted = editor.extract_pages_to_bytes(&[1, 3]).unwrap();
    assert_eq!(page_texts(&extracted), ["S1", "M1"]);
    let reversed = editor.extract_pages_to_bytes(&[4, 0]).unwrap();
    assert_eq!(page_texts(&reversed), ["M2", "S0"]);

    assert_eq!(editor.current_page_count(), 5);
    assert_eq!(page_texts(&editor.save_to_bytes().unwrap()), ["S0", "S1", "M0", "M1", "M2"]);
    assert!(editor.extract_pages_to_bytes(&[5]).is_err());
}

/// No Dart battery sees it: it counts font objects and searches the raw save for dropped page text, a byte claim test-guards ban in Dart.
#[test]
fn dropped_merged_pages_are_not_written_and_a_shared_object_is_written_once() {
    let mut editor = editor_with_three_merged_pages();
    let bytes = editor.with_pages_selected(&[3], |e| e.save_to_bytes_with_options(raw_save())).unwrap().unwrap();

    assert_eq!(page_texts(&bytes), ["M1"]);
    for dropped in ["(S0)", "(S1)", "(M0)", "(M2)"] {
        assert!(!contains(&bytes, dropped.as_bytes()), "{dropped} was written");
    }
    assert!(contains(&bytes, b"(M1)"));
    let fonts = String::from_utf8_lossy(&bytes).matches("/BaseFont /Helvetica").count();
    assert_eq!(fonts, 1, "the font the pages share is written once");
}

/// No Dart battery sees it: it counts font objects and searches the raw save for dropped page text, a byte claim test-guards ban in Dart.
#[test]
fn an_object_only_a_dropped_merged_page_met_first_is_written_for_a_kept_page() {
    // The merged font is first met by M0, which is dropped; M1 and M2 still need it.
    use crate::editor::EditableDocument;
    let mut editor = editor_with_three_merged_pages();
    editor.remove_page(2).unwrap();
    let bytes = editor.save_to_bytes_with_options(raw_save()).unwrap();
    assert_eq!(page_texts(&bytes), ["S0", "S1", "M1", "M2"]);
    assert!(!contains(&bytes, b"(M0)"));
    let fonts = String::from_utf8_lossy(&bytes).matches("/BaseFont /Helvetica").count();
    assert_eq!(fonts, 2, "one font for the source pages, one for the merged pages");
}

/// Two pages: the first links to the second.
fn linked_pair_source_pdf_bytes() -> Vec<u8> {
    pdf_from_objects(&[
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_vec(),
        b"2 0 obj\n<< /Type /Pages /Kids [3 0 R 5 0 R] /Count 2 /MediaBox [0 0 200 200] >>\nendobj\n"
            .to_vec(),
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /Annots [4 0 R] >>\nendobj\n".to_vec(),
        b"4 0 obj\n<< /Type /Annot /Subtype /Link /Rect [10 10 50 50] /P 3 0 R \
          /Dest [5 0 R /Fit] >>\nendobj\n"
            .to_vec(),
        b"5 0 obj\n<< /Type /Page /Parent 2 0 R >>\nendobj\n".to_vec(),
    ])
}

/// Every object the saved file holds, once.
fn saved_objects(bytes: &[u8]) -> Vec<Object> {
    let doc = crate::document::PdfDocument::from_bytes(bytes.to_vec()).unwrap();
    doc.all_object_ids()
        .into_iter()
        .filter(|&id| id != 0 && !doc.is_free_entry(id))
        .map(|id| doc.load_object(ObjectRef::new(id, 0)).unwrap())
        .collect()
}

fn count_of_type(objects: &[Object], type_name: &str) -> usize {
    objects
        .iter()
        .filter(|o| {
            o.as_dict().and_then(|d| d.get("Type")).and_then(|t| t.as_name()) == Some(type_name)
        })
        .count()
}

/// No Dart battery sees it: it counts the `/Pages` and `/Page` objects of the saved file and reads the link's `/Dest`, which no Dart call can.
#[test]
fn a_link_to_a_page_that_was_not_merged_imports_no_page_tree() {
    let mut editor = DocumentEditor::from_bytes(merge_base_pdf_bytes()).unwrap();
    let source = Box::new(std::io::Cursor::new(linked_pair_source_pdf_bytes()));
    assert_eq!(editor.merge_pages_from_reader(source, &[0]).unwrap(), 1);
    let bytes = editor.save_to_bytes().unwrap();

    let objects = saved_objects(&bytes);
    assert_eq!(count_of_type(&objects, "Pages"), 1, "only the output's own page tree");
    assert_eq!(count_of_type(&objects, "Page"), 2, "the unmerged source page is not imported");

    let text = String::from_utf8_lossy(&bytes);
    let ids: Vec<u32> = text
        .lines()
        .filter_map(|l| l.strip_suffix(" 0 obj").and_then(|n| n.parse().ok()))
        .collect();
    let unique: HashSet<u32> = ids.iter().copied().collect();
    assert_eq!(ids.len(), unique.len(), "an object id was written twice: {ids:?}");

    let link = objects
        .iter()
        .find(|o| o.as_dict().and_then(|d| d.get("Subtype")).and_then(|s| s.as_name()) == Some("Link"))
        .expect("the merged page lost its link");
    let dest = link.as_dict().unwrap().get("Dest").and_then(|d| d.as_array()).expect("no /Dest");
    assert!(matches!(dest[0], Object::Null), "a destination page that was left out is null");
}

// ── a full rewrite writes everything a kept page reaches ──
