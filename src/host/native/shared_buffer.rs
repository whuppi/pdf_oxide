//! Shared memory layout between Rust pool threads and the Dart isolate.
//!
//! This file is part of the pdf_manipulator host layer (NOT upstream).
//!
//! Both sides (Rust + Dart) agree on exact byte offsets. Rust defines
//! the layout here. Dart mirrors it in `shared_buffer.dart`. The buffer
//! is allocated by Dart via `calloc` and passed to Rust as a raw pointer.
//!
//! Two channel types:
//! - Read channel: engine requests source bytes from the host.
//! - Write channel: engine sends output chunks to the host.
//!
//! Communication: pthread mutex + condvar. The pool thread sleeps
//! (zero CPU) while waiting. The Dart isolate signals the condvar
//! after fulfilling a request.

use std::sync::atomic::{AtomicU32, Ordering};

// ── Flag bits (same values on both Rust and Dart sides) ────────────

pub const FLAG_READY: u32 = 1 << 0;
pub const FLAG_ERROR: u32 = 1 << 1;
pub const FLAG_CANCELLED: u32 = 1 << 2;
pub const FLAG_ACK: u32 = 1 << 3;

// ── Read channel byte layout ───────────────────────────────────────
//
// Offset  Field              Size   Purpose
// 0       request_offset     8      byte offset engine wants to read
// 8       request_count      8      byte count engine wants
// 16      response_length    8      bytes actually returned by host
// 24      flags              4      atomic flag bits
// 28      (padding)          4
// 32      mutex              64     pthread_mutex_t
// 96      condvar            64     pthread_cond_t
// 160     data               64KB   response byte payload

pub mod read_channel {
    pub const DATA_CAPACITY: usize = crate::host::constants::READ_BUF_CAPACITY;

    pub const OFFSET_REQUEST_OFFSET: usize = 0;
    pub const OFFSET_REQUEST_COUNT: usize = 8;
    pub const OFFSET_RESPONSE_LENGTH: usize = 16;
    pub const OFFSET_FLAGS: usize = 24;
    pub const OFFSET_MUTEX: usize = 32;
    pub const OFFSET_CONDVAR: usize = 96;
    pub const OFFSET_DATA: usize = 160;

    pub const TOTAL_SIZE: usize = OFFSET_DATA + DATA_CAPACITY;
}

// ── Write channel byte layout ──────────────────────────────────────
//
// Offset  Field              Size   Purpose
// 0       chunk_length       8      bytes in this output chunk
// 8       flags              4      atomic flag bits
// 12      (padding)          4
// 16      mutex              64     pthread_mutex_t
// 80      condvar            64     pthread_cond_t
// 144     data               256KB  output byte payload

pub mod write_channel {
    pub const DATA_CAPACITY: usize = crate::host::constants::WRITE_BUF_CAPACITY;


    pub const OFFSET_CHUNK_LENGTH: usize = 0;
    pub const OFFSET_FLAGS: usize = 8;
    pub const OFFSET_MUTEX: usize = 16;
    pub const OFFSET_CONDVAR: usize = 80;
    pub const OFFSET_DATA: usize = 144;

    pub const TOTAL_SIZE: usize = OFFSET_DATA + DATA_CAPACITY;
}

// ── Accessor helpers (safe wrappers around pointer arithmetic) ─────

/// # Safety
/// `base` must point to a buffer at least `offset + 8` bytes long.
#[inline]
pub unsafe fn read_i64(base: *const u8, offset: usize) -> i64 {
    (base.add(offset) as *const i64).read()
}

/// # Safety
/// `base` must point to a buffer at least `offset + 8` bytes long.
#[inline]
pub unsafe fn write_i64(base: *mut u8, offset: usize, value: i64) {
    (base.add(offset) as *mut i64).write(value);
}

/// # Safety
/// `base` must point to a buffer with an AtomicU32 at `offset`.
#[inline]
pub unsafe fn flags_ref(base: *const u8, offset: usize) -> &'static AtomicU32 {
    &*(base.add(offset) as *const AtomicU32)
}

/// # Safety
/// `base` must have a valid pthread_mutex_t at `offset`.
#[inline]
pub unsafe fn mutex_ptr(base: *mut u8, offset: usize) -> *mut libc::pthread_mutex_t {
    base.add(offset) as *mut libc::pthread_mutex_t
}

/// # Safety
/// `base` must have a valid pthread_cond_t at `offset`.
#[inline]
pub unsafe fn condvar_ptr(base: *mut u8, offset: usize) -> *mut libc::pthread_cond_t {
    base.add(offset) as *mut libc::pthread_cond_t
}

/// # Safety
/// `base` must have data starting at `offset`.
#[inline]
pub unsafe fn data_ptr(base: *mut u8, offset: usize) -> *mut u8 {
    base.add(offset)
}

#[inline]
pub fn load_flags(base: *const u8, flags_offset: usize) -> u32 {
    unsafe { flags_ref(base, flags_offset).load(Ordering::Acquire) }
}

#[inline]
pub fn store_flags(base: *mut u8, flags_offset: usize, value: u32) {
    unsafe { flags_ref(base, flags_offset).store(value, Ordering::Release) }
}

#[inline]
pub fn set_flag_bits(base: *mut u8, flags_offset: usize, bits: u32) {
    unsafe { flags_ref(base, flags_offset).fetch_or(bits, Ordering::Release) };
}

#[inline]
pub fn clear_flags(base: *mut u8, flags_offset: usize) {
    store_flags(base, flags_offset, 0);
}

#[inline]
pub fn has_flag(base: *const u8, flags_offset: usize, bit: u32) -> bool {
    load_flags(base, flags_offset) & bit != 0
}

/// Initialize pthread mutex + condvar in the buffer.
///
/// # Safety
/// Must be called exactly once per buffer before any use.
pub unsafe fn init_sync(base: *mut u8, mutex_offset: usize, condvar_offset: usize) {
    libc::pthread_mutex_init(mutex_ptr(base, mutex_offset), std::ptr::null());
    libc::pthread_cond_init(condvar_ptr(base, condvar_offset), std::ptr::null());
}

/// Destroy pthread mutex + condvar.
///
/// # Safety
/// Must be called exactly once per buffer after all use is done.
pub unsafe fn destroy_sync(base: *mut u8, mutex_offset: usize, condvar_offset: usize) {
    libc::pthread_mutex_destroy(mutex_ptr(base, mutex_offset));
    libc::pthread_cond_destroy(condvar_ptr(base, condvar_offset));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_channel_layout() {
        assert_eq!(read_channel::OFFSET_DATA, 160);
        assert_eq!(read_channel::TOTAL_SIZE, 160 + read_channel::DATA_CAPACITY);
    }

    #[test]
    fn write_channel_layout() {
        assert_eq!(write_channel::OFFSET_DATA, 144);
        assert_eq!(write_channel::TOTAL_SIZE, 144 + write_channel::DATA_CAPACITY);
    }

    #[test]
    fn flag_operations() {
        let mut buf = vec![0u8; 64];
        let base = buf.as_mut_ptr();

        assert!(!has_flag(base, 0, FLAG_READY));
        set_flag_bits(base, 0, FLAG_READY);
        assert!(has_flag(base, 0, FLAG_READY));
        assert!(!has_flag(base, 0, FLAG_ERROR));

        set_flag_bits(base, 0, FLAG_ERROR);
        assert!(has_flag(base, 0, FLAG_READY));
        assert!(has_flag(base, 0, FLAG_ERROR));

        clear_flags(base, 0);
        assert!(!has_flag(base, 0, FLAG_READY));
    }

    #[test]
    fn i64_round_trip() {
        let mut buf = vec![0u8; 32];
        let base = buf.as_mut_ptr();
        unsafe {
            write_i64(base, 0, 42);
            write_i64(base, 8, -1);
            write_i64(base, 16, i64::MAX);
            assert_eq!(read_i64(base, 0), 42);
            assert_eq!(read_i64(base, 8), -1);
            assert_eq!(read_i64(base, 16), i64::MAX);
        }
    }
}
