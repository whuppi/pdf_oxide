//! A stream's extent read without loading its data.

use crate::document::{PdfDocument, StreamHead};
use crate::object::ObjectRef;

/// A sound PDF whose object 4 is `object_4` (a whole indirect object),
/// with the integer 11 as object 5.
fn pdf_with(object_4: &[u8]) -> Vec<u8> {
    let objects: Vec<Vec<u8>> = vec![
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_vec(),
        b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n".to_vec(),
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] >>\nendobj\n".to_vec(),
        object_4.to_vec(),
        b"5 0 obj\n11\nendobj\n".to_vec(),
    ];
    let mut pdf = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for o in &objects {
        offsets.push(pdf.len());
        pdf.extend_from_slice(o);
    }
    let xref_at = pdf.len();
    pdf.extend_from_slice(b"xref\n0 6\n0000000000 65535 f \n");
    for off in offsets {
        pdf.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!("trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref_at}\n%%EOF\n").as_bytes(),
    );
    pdf
}

fn head_of(object_4: &[u8]) -> (Vec<u8>, Option<StreamHead>) {
    let pdf = pdf_with(object_4);
    let doc = PdfDocument::from_bytes(pdf.clone()).unwrap();
    let head = doc.stream_head(ObjectRef { id: 4, gen: 0 }).unwrap();
    (pdf, head)
}

/// No Dart battery sees it: `stream_head` is a crate-internal read that no Dart call exposes.
#[test]
fn direct_length_gives_the_data_extent() {
    let (pdf, head) = head_of(b"4 0 obj\n<< /Length 5 /Subtype /Image >>\nstream\nhello\nendstream\nendobj\n");
    let head = head.expect("a plain stream");
    assert_eq!(head.data_len, 5);
    assert_eq!(&pdf[head.data_start as usize..][..5], b"hello");
    assert_eq!(head.dict.get("Subtype").and_then(|o| o.as_name()), Some("Image"));
}

/// No Dart battery sees it: `stream_head` is a crate-internal read that no Dart call exposes.
#[test]
fn indirect_length_is_resolved() {
    let (pdf, head) = head_of(b"4 0 obj\n<< /Length 5 0 R >>\nstream\nabcdefghijk\nendstream\nendobj\n");
    let head = head.expect("a stream with an indirect length");
    assert_eq!(head.data_len, 11);
    assert_eq!(&pdf[head.data_start as usize..][..11], b"abcdefghijk");
}

/// No Dart battery sees it: `stream_head` is a crate-internal read that no Dart call exposes.
#[test]
fn crlf_after_stream_is_skipped() {
    let (pdf, head) = head_of(b"4 0 obj\n<< /Length 3 >>\nstream\r\nxyz\r\nendstream\nendobj\n");
    let head = head.expect("a stream with CRLF");
    assert_eq!(&pdf[head.data_start as usize..][..3], b"xyz");
}

/// No Dart battery sees it: `stream_head` is a crate-internal read that no Dart call exposes.
#[test]
fn wrong_length_gives_none() {
    let (_, head) = head_of(b"4 0 obj\n<< /Length 2 >>\nstream\nhello\nendstream\nendobj\n");
    assert!(head.is_none());
}

/// No Dart battery sees it: `stream_head` is a crate-internal read that no Dart call exposes.
#[test]
fn non_stream_object_gives_none() {
    let (_, head) = head_of(b"4 0 obj\n<< /Type /Font >>\nendobj\n");
    assert!(head.is_none());
}

/// No Dart battery sees it: it asserts the object cache stays empty, and Dart cannot read the cache.
#[test]
fn a_stream_head_leaves_the_object_cache_alone() {
    let pdf = pdf_with(b"4 0 obj\n<< /Length 5 >>\nstream\nhello\nendstream\nendobj\n");
    let doc = PdfDocument::from_bytes(pdf).unwrap();
    let r = ObjectRef { id: 4, gen: 0 };
    assert!(doc.stream_head(r).unwrap().is_some());
    assert!(!doc.object_is_cached(r));
}
