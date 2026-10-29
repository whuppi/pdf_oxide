//! The checks behind a save that keeps the source's encryption.

use crate::document::PdfDocument;
use crate::editor::{DocumentEditor, EncryptionConfig, SaveOptions};
use crate::encryption::{Algorithm, EncryptDictBuilder};
use crate::error::Error;
use crate::object::Object;
use crate::writer::ObjectSerializer;
use std::collections::HashMap;

const FILE_ID: [u8; 16] = [0x11; 16];

type Entries = HashMap<String, Object>;

/// An AES-256 source whose /Encrypt dictionary is the engine writer's own
/// (`EncryptDictBuilder`) after `edit` changed it, since wrapping the file
/// key by hand is the engine's job. Every offset is computed here.
/// `id_entry` is the trailer's /ID text, empty for none.
fn source(user_password: &[u8], id_entry: &str, edit: impl FnOnce(&mut Entries)) -> Vec<u8> {
    let (dict, _key) = EncryptDictBuilder::new(Algorithm::Aes256)
        .user_password(user_password)
        .owner_password(b"owner")
        .permissions(-3900)
        .encrypt_metadata(true)
        .build_with_key(&FILE_ID)
        .unwrap();
    let mut encrypt = dict.to_object();
    if let Object::Dictionary(entries) = &mut encrypt {
        edit(entries);
    }
    let objects: Vec<Vec<u8>> = vec![
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_vec(),
        b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n".to_vec(),
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] >>\nendobj\n".to_vec(),
        ObjectSerializer::compact().serialize_indirect(4, 0, &encrypt),
    ];
    let mut pdf = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for o in &objects {
        offsets.push(pdf.len());
        pdf.extend_from_slice(o);
    }
    let xref_at = pdf.len();
    pdf.extend_from_slice(b"xref\n0 5\n0000000000 65535 f \n");
    for off in offsets {
        pdf.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!(
            "trailer\n<< /Size 5 /Root 1 0 R /Encrypt 4 0 R {id_entry} >>\n\
             startxref\n{xref_at}\n%%EOF\n"
        )
        .as_bytes(),
    );
    pdf
}

const ID: &str = "/ID [<11111111111111111111111111111111> <22222222222222222222222222222222>]";

fn name(n: &str) -> Object {
    Object::Name(n.to_string())
}

fn refusal(pdf: Vec<u8>, reason: &str) {
    let doc = PdfDocument::from_bytes(pdf).unwrap();
    match doc.kept_encryption() {
        Err(Error::Unsupported(msg)) => {
            assert!(
                msg.starts_with("keeping this source's encryption is not supported: ")
                    && msg.contains(reason)
                    && msg.ends_with("save it with explicit encryption"),
                "unexpected message: {msg}"
            );
        },
        Err(other) => panic!("expected Unsupported, got {other:?}"),
        Ok(_) => panic!("expected Unsupported, got Ok"),
    }
}

/// No Dart battery sees it: each case edits an `/Encrypt` dictionary the engine's own writer produced, since the AES-256 key wrapping (`/UE`, `/OE`) is computed in Rust and no Dart fixture can compute it.
#[test]
fn a_plain_source_keeps_its_parameters() {
    let doc = PdfDocument::from_bytes(source(b"", ID, |_| {})).unwrap();
    let kept = doc.kept_encryption().unwrap().expect("an encrypted source");
    assert_eq!(kept.file_id, (vec![0x11; 16], vec![0x22; 16]));
    assert_eq!(kept.algorithm, Algorithm::Aes256);
    assert_eq!(kept.file_key.len(), 32);
    assert!(kept.encrypt_metadata);
    let dict = kept.encrypt_dict.as_dict().unwrap();
    for key in ["Filter", "V", "R", "O", "U", "OE", "UE", "P", "Perms", "CF", "StmF", "StrF"] {
        assert!(dict.contains_key(key), "/{key} missing from the kept dictionary");
    }
}

/// No Dart battery sees it: each case edits an `/Encrypt` dictionary the engine's own writer produced, since the AES-256 key wrapping (`/UE`, `/OE`) is computed in Rust and no Dart fixture can compute it.
#[test]
fn an_unencrypted_source_has_nothing_to_keep() {
    let mut pdf = b"%PDF-1.4\n".to_vec();
    let objects: [&[u8]; 3] = [
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n",
        b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n",
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] >>\nendobj\n",
    ];
    let mut offsets = Vec::new();
    for o in objects {
        offsets.push(pdf.len());
        pdf.extend_from_slice(o);
    }
    let xref_at = pdf.len();
    pdf.extend_from_slice(b"xref\n0 4\n0000000000 65535 f \n");
    for off in offsets {
        pdf.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref_at}\n%%EOF\n").as_bytes(),
    );
    let doc = PdfDocument::from_bytes(pdf).unwrap();
    assert!(doc.kept_encryption().unwrap().is_none());
}

/// No Dart battery sees it: each case edits an `/Encrypt` dictionary the engine's own writer produced, since the AES-256 key wrapping (`/UE`, `/OE`) is computed in Rust and no Dart fixture can compute it.
#[test]
fn refuses_a_non_standard_filter() {
    let pdf = source(b"", ID, |d| {
        d.insert("Filter".into(), name("Adobe.PubSec"));
    });
    refusal(pdf, "public-key or non-standard security handler");
}

/// No Dart battery sees it: each case edits an `/Encrypt` dictionary the engine's own writer produced, since the AES-256 key wrapping (`/UE`, `/OE`) is computed in Rust and no Dart fixture can compute it.
#[test]
fn refuses_a_sub_filter() {
    let pdf = source(b"", ID, |d| {
        d.insert("SubFilter".into(), name("adbe.pkcs7.s5"));
    });
    refusal(pdf, "/SubFilter");
}

/// No Dart battery sees it: each case edits an `/Encrypt` dictionary the engine's own writer produced, since the AES-256 key wrapping (`/UE`, `/OE`) is computed in Rust and no Dart fixture can compute it.
#[test]
fn refuses_version_three() {
    // The handler rejects V=3 while it initialises, before the keep checks.
    let pdf = source(b"", ID, |d| {
        d.insert("V".into(), Object::Integer(3));
    });
    let doc = PdfDocument::from_bytes(pdf).unwrap();
    match doc.kept_encryption() {
        Err(Error::Unsupported(msg)) => assert!(msg.contains("V=3"), "{msg}"),
        Err(other) => panic!("expected Unsupported, got {other:?}"),
        Ok(_) => panic!("expected Unsupported, got Ok"),
    }
}

/// No Dart battery sees it: each case edits an `/Encrypt` dictionary the engine's own writer produced, since the AES-256 key wrapping (`/UE`, `/OE`) is computed in Rust and no Dart fixture can compute it.
#[test]
fn refuses_strings_and_streams_on_different_filters() {
    let pdf = source(b"", ID, |d| {
        d.insert("StrF".into(), name("Identity"));
    });
    refusal(pdf, "strings and streams use different crypt filters");
}

/// No Dart battery sees it: each case edits an `/Encrypt` dictionary the engine's own writer produced, since the AES-256 key wrapping (`/UE`, `/OE`) is computed in Rust and no Dart fixture can compute it.
#[test]
fn refuses_identity_streams() {
    let pdf = source(b"", ID, |d| {
        d.insert("StmF".into(), name("Identity"));
    });
    refusal(pdf, "streams use /Identity");
}

/// No Dart battery sees it: each case edits an `/Encrypt` dictionary the engine's own writer produced, since the AES-256 key wrapping (`/UE`, `/OE`) is computed in Rust and no Dart fixture can compute it.
#[test]
fn refuses_embedded_files_on_another_filter() {
    let pdf = source(b"", ID, |d| {
        d.insert("EFF".into(), name("Identity"));
    });
    refusal(pdf, "embedded files use a different crypt filter");
}

/// No Dart battery sees it: each case edits an `/Encrypt` dictionary the engine's own writer produced, since the AES-256 key wrapping (`/UE`, `/OE`) is computed in Rust and no Dart fixture can compute it.
#[test]
fn refuses_a_crypt_method_that_is_not_the_versions() {
    let pdf = source(b"", ID, |d| {
        if let Some(Object::Dictionary(cf)) = d.get_mut("CF") {
            if let Some(Object::Dictionary(std_cf)) = cf.get_mut("StdCF") {
                std_cf.insert("CFM".into(), name("AESV2"));
            }
        }
    });
    refusal(pdf, "crypt filter method is not AESV3");
}

/// No Dart battery sees it: each case edits an `/Encrypt` dictionary the engine's own writer produced, since the AES-256 key wrapping (`/UE`, `/OE`) is computed in Rust and no Dart fixture can compute it.
#[test]
fn refuses_a_missing_id() {
    refusal(source(b"", "", |_| {}), "no two-string /ID");
}

/// No Dart battery sees it: each case edits an `/Encrypt` dictionary the engine's own writer produced, since the AES-256 key wrapping (`/UE`, `/OE`) is computed in Rust and no Dart fixture can compute it.
#[test]
fn refuses_a_one_element_id() {
    refusal(
        source(b"", "/ID [<11111111111111111111111111111111>]", |_| {}),
        "no two-string /ID",
    );
}

/// No Dart battery sees it: each case edits an `/Encrypt` dictionary the engine's own writer produced, since the AES-256 key wrapping (`/UE`, `/OE`) is computed in Rust and no Dart fixture can compute it.
#[test]
fn refuses_an_id_that_is_not_strings() {
    refusal(source(b"", "/ID [1 2]", |_| {}), "not two strings");
}

/// No Dart battery sees it: each case edits an `/Encrypt` dictionary the engine's own writer produced, since the AES-256 key wrapping (`/UE`, `/OE`) is computed in Rust and no Dart fixture can compute it.
#[test]
fn refuses_an_unauthenticated_source() {
    refusal(source(b"secret", ID, |_| {}), "the source is not authenticated");
}

/// No Dart battery sees it: each case edits an `/Encrypt` dictionary the engine's own writer produced, since the AES-256 key wrapping (`/UE`, `/OE`) is computed in Rust and no Dart fixture can compute it.
#[test]
fn keep_and_an_explicit_config_exclude_each_other() {
    let doc = PdfDocument::from_bytes(source(b"", ID, |_| {})).unwrap();
    let mut editor = DocumentEditor::from_document(doc).unwrap();
    let options = SaveOptions {
        keep_source_encryption: true,
        encryption: Some(EncryptionConfig::new("user", "owner")),
        ..Default::default()
    };
    match editor.save_to_bytes_with_options(options) {
        Err(Error::InvalidOperation(msg)) => assert!(msg.contains("exclusive"), "{msg}"),
        Err(other) => panic!("expected InvalidOperation, got {other:?}"),
        Ok(_) => panic!("expected InvalidOperation, got Ok"),
    }
}
