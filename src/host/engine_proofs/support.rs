//! Builders the engine proofs share.

use crate::editor::DocumentEditor;

/// Objects are numbered from 1 in the order given; offsets are computed.
pub(super) fn pdf_from_objects(objs: &[Vec<u8>]) -> Vec<u8> {
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for o in objs {
        offsets.push(out.len());
        out.extend_from_slice(o);
    }
    let xref_at = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for off in offsets {
        out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_at}\n%%EOF\n", objs.len() + 1)
            .as_bytes(),
    );
    out
}

pub(super) fn stream_object(id: u32, dict: &str, data: &[u8]) -> Vec<u8> {
    [
        format!("{id} 0 obj\n<< {dict} /Length {} >>\nstream\n", data.len()).into_bytes(),
        data.to_vec(),
        b"\nendstream\nendobj\n".to_vec(),
    ]
    .concat()
}

pub(super) fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// An editor opened from a file written for it, the way a caller opens one
/// from disk.
pub(super) fn open_editor_from_bytes(bytes: &[u8], tag: &str) -> DocumentEditor {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp = std::env::temp_dir().join(format!("pdf_oxide_{tag}_{}_{id}.pdf", std::process::id()));
    std::fs::write(&tmp, bytes).unwrap();
    let editor = DocumentEditor::open(&tmp).unwrap();
    let _ = std::fs::remove_file(&tmp);
    editor
}
