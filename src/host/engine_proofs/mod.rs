//! Proofs of the engine behaviour no Dart battery can observe.
//!
//! Every test here opens a document or an editor, and its doc comment says
//! why a battery in `test/ops/` cannot see what it proves. A proof a battery
//! can make belongs there.

#[cfg(feature = "rendering")]
mod band_proof;
#[cfg(feature = "rendering")]
mod cancel_at_every_poll;
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
mod editor_page_sets;
mod editor_trailer;
#[cfg(feature = "rendering")]
mod ext_gstate_lines;
mod filter_readers;
#[cfg(feature = "rendering")]
mod extract_1bit;
#[cfg(feature = "rendering")]
mod extract_proof;
#[cfg(feature = "rendering")]
mod fax_proof;
#[cfg(feature = "rendering")]
mod group_proof;
#[cfg(feature = "rendering")]
mod reduce_proof;
mod editor_pages;
mod editor_resource_prune;
mod editor_save;
mod kept_encryption;
mod legacy_decoders;
mod lzw;
mod page_operators;
#[cfg(feature = "rendering")]
mod pattern_paint;
#[cfg(feature = "rendering")]
mod predictor_depths;
#[cfg(feature = "rendering")]
mod image_mods_belt;
#[cfg(feature = "rendering")]
mod redact_after_value;
#[cfg(feature = "rendering")]
mod redact_all_or_nothing;
mod redact_pass;
#[cfg(feature = "rendering")]
mod redact_flatten;
#[cfg(feature = "rendering")]
mod redact_font_scrub;
#[cfg(feature = "rendering")]
mod redact_gaps;
#[cfg(feature = "rendering")]
mod redact_path_ink;
mod redact_paths;
#[cfg(not(feature = "rendering"))]
mod redact_refusal;
#[cfg(feature = "rendering")]
mod redact_copies;
#[cfg(feature = "rendering")]
mod redact_image_save;
#[cfg(feature = "rendering")]
mod redact_jpeg_dct;
#[cfg(feature = "rendering")]
mod redact_path_save;
#[cfg(feature = "rendering")]
mod redact_random;
#[cfg(feature = "rendering")]
mod redact_save;
#[cfg(feature = "rendering")]
mod render_fit_crop;
#[cfg(feature = "rendering")]
mod render_gaps;
#[cfg(feature = "rendering")]
mod render_region_crop;
#[cfg(feature = "rendering")]
mod render_stream;
mod scan_proof;
#[cfg(feature = "rendering")]
mod stencil_proof;
#[cfg(feature = "rendering")]
mod source_trips;
mod stream_extent;
mod stream_head;
mod support;
mod table_rulings;
mod text_dense_baseline;
#[cfg(feature = "rendering")]
mod text_render_modes;
mod text_tj_chunks;
#[cfg(feature = "rendering")]
mod watermark_positions;
mod whole_reads;
#[cfg(feature = "rendering")]
mod redact_extra_type3;
