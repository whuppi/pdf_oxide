//! A garbage-collecting save writes what the output names, not what the
//! source did.

use super::support::{contains, pdf_from_objects, stream_object};
use crate::editor::form_fields::FormFieldValue;
use crate::editor::{DocumentEditor, SaveOptions};

const MARK: &[u8] = b"ORPHANED APPEARANCE MARK";

/// A one-field form whose widget appearance carries `MARK`.
fn form_bytes() -> Vec<u8> {
    pdf_from_objects(&[
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [4 0 R] /DA (/Helv 0 Tf 0 g) /DR << /Font << /Helv 5 0 R >> >> >> >>\nendobj\n".to_vec(),
        b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n".to_vec(),
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /Helv 5 0 R >> >> /Contents 7 0 R /Annots [4 0 R] >>\nendobj\n".to_vec(),
        b"4 0 obj\n<< /FT /Tx /T (name) /Type /Annot /Subtype /Widget /Rect [72 700 400 720] /DA (/Helv 0 Tf 0 g) /AP << /N 6 0 R >> >>\nendobj\n".to_vec(),
        b"5 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n".to_vec(),
        stream_object(6, "/Type /XObject /Subtype /Form /BBox [0 0 328 20]", &[b"/Tx BMC\n% ".as_slice(), MARK, b"\nEMC\n"].concat()),
        stream_object(7, "", b"q Q\n"),
    ])
}

fn save(flatten: bool) -> Vec<u8> {
    let mut editor = DocumentEditor::from_bytes(form_bytes()).unwrap();
    editor.set_form_field_value("name", FormFieldValue::Text("Hello".into())).unwrap();
    if flatten {
        editor.flatten_forms().unwrap();
    }
    let options = SaveOptions { object_streams: false, ..SaveOptions::full_rewrite() };
    editor.save_to_bytes_with_options(options).unwrap()
}

/// No Dart battery sees it: Dart reads the document through its page tree, and the orphan hangs off no object a reader reaches; only the raw bytes of the file hold it.
#[test]
fn a_flattened_widget_appearance_the_output_no_longer_names_is_not_written() {
    assert!(contains(&save(false), MARK), "the unflattened save keeps the appearance");
    assert!(!contains(&save(true), MARK), "the flattened save wrote an orphaned appearance");
}
