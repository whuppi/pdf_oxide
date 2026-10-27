//! Bridge API — the door: parse the request and dispatch through the registry.
//!
//! This file is part of the pdf_manipulator host layer (NOT upstream).
//!
//! ## Lane model
//!
//! Engine state lives per LANE (`LaneState`) — one isolated execution
//! unit per native thread / per Web Worker. This file never spawns,
//! routes, or kills lanes; it is the decision-free request door that
//! every lane body calls:
//!
//!   Native lane body: `host/native/lane.rs` (mailbox thread)
//!   Web lane body:    `web_assets/lane_worker.js` (Worker + wasm_entry)
//!
//! ## Request routing
//!
//! `handle_request` parses binary request bytes, looks the op up in the
//! registry (`host/ops/registry.rs`), runs that op unit's handler against
//! the lane's own state, and returns the binary response bytes. Each op
//! unit under `host/ops/` owns its body. Same source code for both
//! targets. 100% platform-agnostic. No locks — a lane's state has
//! exactly one owner thread, so handlers take `&mut LaneState`.

use crate::host::binary_codec::{Request, ResponseWriter};
use crate::host::lane_state::LaneState;

// ═══════════════════════════════════════════════════════════════════
// BoxedReader / BoxedWriter — concrete wrappers for trait objects
// ═══════════════════════════════════════════════════════════════════

/// Concrete wrapper around a boxed Read+Seek trait object.
pub(crate) struct BoxedReader(pub(crate) Box<dyn crate::document::ReadSeek>);

impl std::io::Read for BoxedReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> { self.0.read(buf) }
}

impl std::io::Seek for BoxedReader {
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> { self.0.seek(pos) }
}

/// Concrete wrapper around a boxed Write+Send trait object.
pub(crate) struct BoxedWriter {
    inner: Box<dyn WriteSend>,
    written: u64,
}

pub(crate) trait WriteSend: std::io::Write + Send {}
impl<T: std::io::Write + Send> WriteSend for T {}

impl BoxedWriter {
    pub(crate) fn new(w: impl std::io::Write + Send + 'static) -> Self {
        Self { inner: Box::new(w), written: 0 }
    }
}

impl std::io::Write for BoxedWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.written += n as u64;
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> { self.inner.flush() }
}

impl crate::host::positioned_write::PositionedWrite for BoxedWriter {
    fn position(&mut self) -> u64 {
        self.written
    }
}

// ═══════════════════════════════════════════════════════════════════
// Core request handler — platform-agnostic
// ═══════════════════════════════════════════════════════════════════

/// Handle a binary request within an instance's context.
///
/// O(1)-memory I/O: when reader/writer are Some, PDF bytes flow
/// through them on demand — never fully buffered.
pub(crate) fn handle_request(
    state: &mut LaneState,
    bytes: &[u8],
    source_bytes: Option<&[u8]>,
    mut sources: Vec<BoxedReader>,
    mut sinks: Vec<BoxedWriter>,
    cancel: &crate::cancel::CancelCheck,
) -> Vec<u8> {
    let req = match Request::parse(bytes) {
        Ok(r) => r,
        Err(e) => return ResponseWriter::error(&format!("parse error: {e}")),
    };

    let mut ctx = crate::host::ops::OpCtx {
        state,
        req: &req,
        source_bytes,
        sources: &mut sources,
        sinks: &mut sinks,
        cancel,
    };
    match crate::host::ops::registry::find(req.op()) {
        Some(entry) => (entry.handle)(&mut ctx),
        None => ResponseWriter::error(&format!("unknown op: {}", req.op())),
    }
}

// ═══════════════════════════════════════════════════════════════════
// WASM lane body — one Worker = one lane = one LaneState
// ═══════════════════════════════════════════════════════════════════
//
// The web twin of `host/native/lane.rs`. A Web Worker IS a lane: the
// browser provides the isolated thread; this module provides the
// lane's owned state and the call into the shared request door.
// Decision-free by design — routing, pinning, queuing, and cancel
// bookkeeping all live in the shared Dart Router.
//
// Hard kill on web is `worker.terminate()` — the browser frees the
// whole WASM heap with the worker, so `lane_destroy` exists only for
// the graceful path (lane reuse without worker restart).

#[cfg(target_arch = "wasm32")]
mod wasm_entry {
    use super::*;
    use crate::host::wasm::js_reader::JsCallbackReader;
    use crate::host::wasm::js_writer::JsCallbackWriter;
    use wasm_bindgen::prelude::*;

    /// Create this worker's lane state. Returns a WASM pointer.
    /// Called exactly once per worker, from lane_worker.js bootstrap.
    #[wasm_bindgen]
    pub fn lane_init() -> u32 {
        let lane = Box::new(LaneState::new());
        Box::into_raw(lane) as u32
    }

    /// Allocate `size` bytes (align 1) from the module's global allocator.
    ///
    /// Stable twin of the glue's internal malloc, for lane_worker.js's JSPI
    /// driver: wasm-bindgen's `__wbindgen_export_N` aliases are renumbered
    /// whenever the module's export set changes, so the worker must never
    /// call those by name. Same allocator and layout as the glue's malloc,
    /// so buffers are interchangeable with glue-allocated ones.
    #[wasm_bindgen]
    pub fn lane_alloc(size: u32) -> u32 {
        if size == 0 {
            return 1; // non-null dangling, mirrors the glue's zero-size malloc
        }
        unsafe {
            let layout =
                std::alloc::Layout::from_size_align_unchecked(size as usize, 1);
            std::alloc::alloc(layout) as u32
        }
    }

    /// Free a buffer from `lane_alloc` or a response buffer handed out by
    /// `lane_execute` (both come from the same global allocator, align 1).
    #[wasm_bindgen]
    pub fn lane_dealloc(ptr: u32, size: u32) {
        if size == 0 {
            return;
        }
        unsafe {
            let layout =
                std::alloc::Layout::from_size_align_unchecked(size as usize, 1);
            std::alloc::dealloc(ptr as *mut u8, layout);
        }
    }

    /// Execute one job against this lane's state.
    ///
    /// source_lengths: packed f64 array — each 8 bytes is one source
    /// length (f64 because JS numbers cross the boundary as doubles).
    /// Sources get indexed readers (0, 1, 2, ...).
    /// sink_count: how many output sinks exist.
    /// Sinks get indexed writers (0, 1, 2, ...).
    ///
    /// The worker is single-threaded, so the &mut reconstruction from
    /// the raw pointer is sound: no other code can hold a reference
    /// while this runs.
    #[wasm_bindgen]
    pub fn lane_execute(
        lane_ptr: u32,
        request_bytes: &[u8],
        source_lengths: &[u8],
        sink_count: u32,
    ) -> Vec<u8> {
        let state = unsafe { &mut *(lane_ptr as *mut LaneState) };

        let mut sources = Vec::new();
        let num_sources = source_lengths.len() / 8;
        for i in 0..num_sources {
            let offset = i * 8;
            let len = f64::from_le_bytes([
                source_lengths[offset], source_lengths[offset+1],
                source_lengths[offset+2], source_lengths[offset+3],
                source_lengths[offset+4], source_lengths[offset+5],
                source_lengths[offset+6], source_lengths[offset+7],
            ]);
            // An empty source stays in the list, as on native: dropping it
            // would shift every later source's index.
            sources.push(BoxedReader(Box::new(JsCallbackReader::new(i as u32, len as u64))));
        }

        let mut sinks = Vec::new();
        for i in 0..sink_count {
            sinks.push(BoxedWriter::new(JsCallbackWriter::new(i)));
        }

        handle_request(state, request_bytes, None, sources, sinks, &crate::cancel::CancelCheck::from_host())
    }

    /// Drop this lane's state, freeing every handle it owns.
    /// Graceful-path only — `worker.terminate()` makes this moot.
    #[wasm_bindgen]
    pub fn lane_destroy(lane_ptr: u32) {
        if lane_ptr == 0 { return; }
        unsafe {
            let _ = Box::from_raw(lane_ptr as *mut LaneState);
        }
    }
}
