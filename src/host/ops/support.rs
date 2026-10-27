// Helpers shared by more than one op unit: request fields, page checks,
// response shapes, source opening and the handle-scoped handler wrappers.

use crate::host::binary_codec::{ErrorKind, Request, ResponseWriter};
use crate::host::bridge_api::BoxedReader;
use crate::host::lane_state::LaneState;

use crate::document::PdfDocument;
use crate::editor::DocumentEditor;

/// Read all bytes from a reader in 64KB chunks.
/// Used when the engine needs the full data (images, embedded files)
/// but the transport must stream it — never as one blob in the message.
pub(crate) fn read_all_from_reader(reader: &mut BoxedReader) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 65536];
    loop {
        let n = reader.read(&mut chunk)?;
        if n == 0 { break; }
        buf.extend_from_slice(&chunk[..n]);
    }
    Ok(buf)
}

/// The request's handle id (0 when absent — never a valid handle).
pub(crate) fn req_handle(req: &Request<'_>) -> u32 {
    req.get_i32("handleId").unwrap_or(0) as u32
}

/// The request's page index when it names a page of a document with
/// `count` pages, or the page-range error response to send instead.
pub(crate) fn page_of(page: i32, count: usize) -> Result<usize, Vec<u8>> {
    if page < 0 || page as usize >= count {
        Err(ResponseWriter::page_range(page, count as i32))
    } else {
        Ok(page as usize)
    }
}

/// Every index of `pages` checked like [`page_of`]; the first bad entry
/// is the one answered.
pub(crate) fn pages_of(pages: &[i32], count: usize) -> Result<Vec<usize>, Vec<u8>> {
    pages.iter().map(|&p| page_of(p, count)).collect()
}

/// A success response carrying a single `true` flag.
pub(crate) fn ok_flag(key: &str) -> Vec<u8> {
    let mut w = ResponseWriter::ok();
    w.put_bool(key, true);
    w.finish()
}

pub(crate) fn ok_response() -> crate::error::Result<Vec<u8>> {
    let mut w = ResponseWriter::ok();
    w.put_bool("ok", true);
    Ok(w.finish())
}

/// Open a document from the op's source: the streaming reader when the
/// transport provided one, else inline bytes. `Err` carries the
/// encoded error response, ready to return.
pub(crate) fn open_source(
    source_bytes: Option<&[u8]>,
    source_reader: Option<BoxedReader>,
    op: &str,
) -> Result<PdfDocument, Vec<u8>> {
    if let Some(reader) = source_reader {
        PdfDocument::from_external_reader(reader.0)
            .map_err(|e| ResponseWriter::error_of(&e))
    } else if let Some(bytes) = source_bytes {
        PdfDocument::from_bytes(bytes.to_vec())
            .map_err(|e| ResponseWriter::error_of(&e))
    } else {
        Err(ResponseWriter::error(&format!("no source for {op}")))
    }
}

/// Authenticates `doc` against the request's optional password, or
/// returns the error response that refuses the operation.
///
/// Always runs — with the empty password when none was supplied —
/// because the PDF spec's empty-user-password convention is what lets
/// owner-only documents open for everyone, while a real user password
/// makes both the no-password and wrong-password attempts fail.
/// Discarding the result here is the bug class this helper replaces:
/// an unauthenticated encrypted document must refuse, never open.
pub(crate) fn authenticate_or_refuse(doc: &PdfDocument, password: Option<&str>) -> Option<Vec<u8>> {
    match doc.authenticate(password.unwrap_or("").as_bytes()) {
        Ok(true) => None,
        Ok(false) => Some(if password.is_some() {
            ResponseWriter::error_kind(ErrorKind::WrongPassword, "wrong password")
        } else {
            ResponseWriter::error_kind(
                ErrorKind::PasswordRequired,
                "password required: document is encrypted with a user password",
            )
        }),
        Err(e) => Some(ResponseWriter::error_of(&e)),
    }
}

pub(crate) fn handle_with_doc<F>(state: &mut LaneState, req: &Request<'_>, f: F) -> Vec<u8>
where F: FnOnce(&mut PdfDocument, &Request<'_>) -> crate::error::Result<Vec<u8>>
{
    let hid = req_handle(req);
    let docs = &mut state.documents;
    match docs.get_mut(&hid) {
        Some(doc) => match f(doc, req) {
            Ok(bytes) => bytes,
            Err(e) => ResponseWriter::error_of(&e),
        },
        None => ResponseWriter::error("document not found"),
    }
}

pub(crate) fn handle_with_editor<F>(state: &mut LaneState, req: &Request<'_>, f: F) -> Vec<u8>
where F: FnOnce(&mut DocumentEditor, &Request<'_>) -> crate::error::Result<Vec<u8>>
{
    let hid = req_handle(req);
    let editors = &mut state.editors;
    match editors.get_mut(&hid) {
        Some(editor) => match f(editor, req) {
            Ok(bytes) => bytes,
            Err(e) => ResponseWriter::error_of(&e),
        },
        None => ResponseWriter::error("editor not found"),
    }
}

#[cfg(test)]
mod tests {
    use super::{page_of, pages_of};
    use crate::host::binary_codec::ResponseWriter;

    #[test]
    fn page_of_accepts_in_range_and_refuses_outside() {
        assert_eq!(page_of(0, 2), Ok(0));
        assert_eq!(page_of(1, 2), Ok(1));
        assert_eq!(page_of(-1, 2), Err(ResponseWriter::page_range(-1, 2)));
        assert_eq!(page_of(2, 2), Err(ResponseWriter::page_range(2, 2)));
        assert_eq!(page_of(0, 0), Err(ResponseWriter::page_range(0, 0)));
    }

    #[test]
    fn pages_of_answers_the_first_bad_entry() {
        assert_eq!(pages_of(&[1, 0], 2), Ok(vec![1, 0]));
        assert_eq!(pages_of(&[], 2), Ok(vec![]));
        assert_eq!(pages_of(&[0, 2, -1], 2), Err(ResponseWriter::page_range(2, 2)));
        assert_eq!(pages_of(&[-1], 2), Err(ResponseWriter::page_range(-1, 2)));
    }
}
