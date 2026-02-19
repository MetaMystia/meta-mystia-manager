use std::{path::Path, process::Command};

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS")
        .map(|s| s == "windows")
        .unwrap_or(false)
    {
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
            eprintln!("[build.rs] failed to compile Windows resources: {}", e);
        }

        // 每个 shim 解决一个 Rust libstd 通过 raw-dylib 依赖的 Win8+ API：
        //   - api-ms-win-core-synch-l1-2-0.dll : WaitOnAddress / WakeByAddress*
        //   - bcryptprimitives.dll             : ProcessPrng

        // 注册 shim 源文件变动时重新触发构建
        for src in &[
            "shim/api-ms-win-core-synch-l1-2-0.c",
            "shim/api-ms-win-core-synch-l1-2-0.def",
            "shim/bcryptprimitives.c",
            "shim/bcryptprimitives.def",
        ] {
            println!("cargo:rerun-if-changed={}", src);
        }

        // 调用 shim/build.ps1 编译 shim DLL（需要本机 MSVC）
        // 若 PowerShell 或 MSVC 不可用则仅打印警告，不中断构建
        let ps_script = Path::new("shim/build.ps1");
        if ps_script.exists() {
            let status = Command::new("powershell.exe")
                .args([
                    "-NoProfile",
                    "-ExecutionPolicy",
                    "Bypass",
                    "-File",
                    ps_script.to_str().unwrap(),
                ])
                .status();
            match status {
                Ok(s) if s.success() => {}
                Ok(s) => eprintln!("[build.rs] warning: shim/build.ps1 exited with {}", s),
                Err(e) => eprintln!("[build.rs] warning: failed to run shim/build.ps1: {}", e),
            }
        }

        // 将 shim DLL 复制到输出目录（与 exe 同目录）
        let out_dir = std::env::var("OUT_DIR").unwrap_or_default();
        let exe_dir = (|| {
            let out = Path::new(&out_dir);
            for ancestor in out.ancestors() {
                if ancestor.file_name().map(|n| n == "build").unwrap_or(false) {
                    return ancestor.parent().map(|p| p.to_path_buf());
                }
            }
            None
        })();
        for shim_src_str in &[
            "shim/api-ms-win-core-synch-l1-2-0.dll",
            "shim/bcryptprimitives.dll",
        ] {
            let shim_src = Path::new(shim_src_str);
            let dll_name = shim_src.file_name().unwrap().to_string_lossy();
            if shim_src.exists() {
                if let Some(ref dir) = exe_dir {
                    let dst = dir.join(dll_name.as_ref());
                    if let Err(e) = std::fs::copy(shim_src, &dst) {
                        eprintln!(
                            "[build.rs] warning: failed to copy {} to {}: {}",
                            dll_name,
                            dst.display(),
                            e
                        );
                    } else {
                        println!(
                            "cargo:info=Copied Win7 shim {} to {}",
                            dll_name,
                            dst.display()
                        );
                    }
                }
                println!("cargo:rerun-if-changed={}", shim_src_str);
            } else {
                println!(
                    "cargo:warning={} not found; Win7 compatibility shim will NOT be available",
                    shim_src_str
                );
            }
        }
    }
}
