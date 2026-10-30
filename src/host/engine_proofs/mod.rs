//! Proofs of the engine behaviour no Dart battery can observe.
//!
//! Every test here opens a document or an editor, and its doc comment says
//! why a battery in `test/ops/` cannot see what it proves. A proof a battery
//! can make belongs there.

mod determinism;
mod document_pages;
mod editor_deflate;
mod editor_duplicates;
mod editor_merge;
mod editor_nested;
mod editor_objstm;
mod editor_orphans;
mod editor_packed;
mod editor_trailer;
mod editor_pages;
mod editor_save;
mod kept_encryption;
mod lzw;
#[cfg(feature = "rendering")]
mod source_trips;
mod stream_head;
mod support;
