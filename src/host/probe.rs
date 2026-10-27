//! Out-of-crate measurement door — runs one wire request the way a lane
//! does, for memory probes that live outside this crate.

use crate::host::bridge_api::{self, BoxedReader, BoxedWriter};
use crate::host::lane_state::LaneState;
use std::fs::File;

/// Runs one wire request against `state`, wrapping each `File` in
/// the bridge's boxed reader/writer exactly as the transports do.
pub fn run(
    state: &mut LaneState,
    request: &[u8],
    sources: Vec<File>,
    sinks: Vec<File>,
) -> Vec<u8> {
    let sources = sources.into_iter().map(|f| BoxedReader(Box::new(f))).collect();
    let sinks = sinks.into_iter().map(BoxedWriter::new).collect();
    bridge_api::handle_request(state, request, None, sources, sinks, &crate::cancel::CancelCheck::none())
}
