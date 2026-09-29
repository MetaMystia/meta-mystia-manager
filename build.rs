use std::{env, fs, path::Path};

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
        let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
        let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
        if target_os == "windows" && target_env == "msvc" {
            thunk::thunk();

            let mut res = winres::WindowsResource::new();

            let name = env::var("CARGO_PKG_NAME").unwrap_or_default();
            let desc = env::var("CARGO_PKG_DESCRIPTION").unwrap_or_default();
            let version = env::var("CARGO_PKG_VERSION").unwrap_or_default();
            let authors = env::var("CARGO_PKG_AUTHORS").unwrap_or_default();
            let license = env::var("CARGO_PKG_LICENSE").unwrap_or_default();

            if Path::new("assets/app.ico").exists() {
                res.set_icon("assets/app.ico");
            }
            if Path::new("app.manifest").exists()
                && let Ok(out_dir) = env::var("OUT_DIR")
            {
                let manifest = fs::read_to_string("app.manifest")
                    .unwrap_or_default()
                    .replace("__VERSION__", &manifest_version(&version));
                let manifest_path = Path::new(&out_dir).join("app.manifest");

                if let Some(manifest_path) = manifest_path.to_str()
                    && fs::write(manifest_path, manifest).is_ok()
                {
                    res.set_manifest_file(manifest_path);
                }
            }

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

            // thunk-rs 会给整个包强制 /SUBSYSTEM:CONSOLE，覆盖 #[windows_subsystem]；
            // 正式目标改回 WINDOWS 子系统（debug 保留控制台，方便看 panic）
            if env::var("PROFILE").as_deref() == Ok("release") {
                println!("cargo:rustc-link-arg-bin=meta-mystia-manager=/SUBSYSTEM:WINDOWS");
                println!("cargo:rustc-link-arg-bin=meta-mystia-manager=/ENTRY:mainCRTStartup");
            }
        }
    }
}

fn manifest_version(version: &str) -> String {
    if version.matches('.').count() == 2 {
        format!("{version}.0")
    } else {
        version.to_string()
    }
}
