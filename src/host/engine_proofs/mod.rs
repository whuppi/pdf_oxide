//! Proofs of the engine behaviour no Dart battery can observe.
//!
//! Every test here opens a document or an editor, and its doc comment says
//! why a battery in `test/ops/` cannot see what it proves. A proof a battery
//! can make belongs there.

#[cfg(feature = "rendering")]
mod band_proof;
mod content_inline;
mod content_stream;
mod determinism;
mod encrypted_streams;
mod document_pages;
mod editor_deflate;
mod editor_duplicates;
mod editor_merge;
mod editor_nested;
mod editor_objstm;
mod editor_orphans;
mod editor_packed;
mod editor_trailer;
mod filter_readers;
mod editor_pages;
mod editor_resource_prune;
mod editor_save;
mod kept_encryption;
mod legacy_decoders;
mod lzw;
mod page_operators;
#[cfg(feature = "rendering")]
mod source_trips;
mod stream_extent;
mod stream_head;
mod support;
mod table_rulings;
mod text_dense_baseline;
mod text_tj_chunks;
mod whole_reads;
