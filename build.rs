fn main() {
    // thunk-rs (VC-LTL5 + YY-Thunks for Win7 compat) and winres (Windows resource embedding)
    // are only usable on Windows hosts:
    //   - thunk-rs downloads the VC-LTL5/YY-Thunks archives and unpacks them with 7z; the
    //     delay-load import symbols it adds also conflict with lld-link, the linker used by
    //     cargo-xwin, causing duplicate symbol errors.
    //   - winres requires rc.exe (MSVC) or windres (MinGW), unavailable on macOS/Linux.
    // When cross-compiling via cargo-xwin, both are skipped, which means:
    //   - No VC-LTL5 (smaller CRT) or YY-Thunks (Win7 API polyfills)
    //   - The resulting exe requires Windows 10+ (uses WaitOnAddress, ProcessPrng, etc.)
    //   - No embedded Windows version/resource information
    // Native Windows builds retain full Win7+ support via thunk-rs.
    #[cfg(windows)]
    {
        let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
        let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
        if target_os == "windows" && target_env == "msvc" {
            thunk::thunk();

            let mut res = winres::WindowsResource::new();

            let name = std::env::var("CARGO_PKG_NAME").unwrap_or_default();
            let desc = std::env::var("CARGO_PKG_DESCRIPTION").unwrap_or_default();
            let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_default();
            let authors = std::env::var("CARGO_PKG_AUTHORS").unwrap_or_default();
            let license = std::env::var("CARGO_PKG_LICENSE").unwrap_or_default();

            res.set("FileVersion", &version);
            res.set("ProductName", &name);
            res.set("ProductVersion", &version);

            if !desc.is_empty() {
                res.set("FileDescription", &desc);
            }
            if !authors.is_empty() {
                res.set("CompanyName", &authors);
            }
            if !license.is_empty() {
                res.set("LegalCopyright", &license);
            }

            if let Err(e) = res.compile() {
                eprintln!("[build.rs] failed to compile Windows resources: {e}");
            }
        }
    }
}
