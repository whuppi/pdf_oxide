//! The editor's page index over a nested page tree.

use super::support::open_editor_from_bytes;
use crate::editor::EditableDocument;

/// Three pages under a nested tree: root `[inner(4, 5), 6]`.
fn nested_page_tree_pdf_bytes() -> Vec<u8> {
    let objs: Vec<&[u8]> = vec![
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n",
        b"2 0 obj\n<< /Type /Pages /Kids [3 0 R 6 0 R] /Count 3 >>\nendobj\n",
        b"3 0 obj\n<< /Type /Pages /Parent 2 0 R /Kids [4 0 R 5 0 R] /Count 2 >>\nendobj\n",
        b"4 0 obj\n<< /Type /Page /Parent 3 0 R /MediaBox [0 0 100 100] >>\nendobj\n",
        b"5 0 obj\n<< /Type /Page /Parent 3 0 R /MediaBox [0 0 100 100] >>\nendobj\n",
        b"6 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>\nendobj\n",
    ];
    let mut body = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for o in &objs {
        offsets.push(body.len());
        body.extend_from_slice(o);
    }
    let xref_offset = body.len();
    let mut tail = format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1);
    for off in &offsets {
        tail.push_str(&format!("{off:010} 00000 n \n"));
    }
    tail.push_str(&format!(
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n",
        objs.len() + 1
    ));
    [body, tail.into_bytes()].concat()
}

/// No Dart battery sees it: it reads the editor's private output-to-source page index.
#[test]
fn output_index_follows_removes_and_moves() {
    let mut editor = open_editor_from_bytes(&nested_page_tree_pdf_bytes(), "output_index");
    assert_eq!(editor.output_to_source_index(1), 1);

    editor.remove_page(1).unwrap();
    assert_eq!(editor.output_to_source_index(0), 0);
    assert_eq!(editor.output_to_source_index(1), 2);

    editor.move_page(0, 1).unwrap();
    assert_eq!(editor.output_to_source_index(0), 2);
    assert_eq!(editor.output_to_source_index(1), 0);

    // Past the end falls back to the output index itself.
    assert_eq!(editor.output_to_source_index(5), 5);
}

// ── a merge records ids and streams the merged document at save ──
