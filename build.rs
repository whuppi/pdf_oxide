fn main() {
    // Glibc 2.34 compatibility is handled via global_asm! in src/lib.rs (#416).
    // The previous --defsym=__memcmpeq=memcmp linker flag worked with GNU ld but
    // broke lld (now the default on ubuntu-24.04 CI runners) because lld cannot
    // create --defsym aliases to PLT-resolved shared-library symbols.

    // Android 16 KB page-size alignment (Google Play API 35+, enforced
    // 2025-11-01). NDK r28 defaults its own ndk-build/CMake toolchains to 16 KB,
    // but Cargo/rustc do NOT inherit that, so the cdylib stays 4 KB-aligned
    // unless told otherwise. Emit the flags here via cargo:rustc-link-arg —
    // honored even when --target is passed (unlike .cargo/config.toml rustflags,
    // which are dropped in cross-compiles, and unlike RUSTFLAGS, which the
    // consumer build hook strips from the environment).
    let target = std::env::var("TARGET").unwrap_or_default();
    if target.contains("android") {
        println!("cargo:rustc-link-arg=-Wl,-z,max-page-size=16384");
        println!("cargo:rustc-link-arg=-Wl,-z,common-page-size=16384");
    }
}
