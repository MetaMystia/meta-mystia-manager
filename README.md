# meta-mystia-manager

用 Rust 编写的一键安装/升级/卸载 [MetaMystia Mod](https://github.com/MetaMikuAI/MetaMystia) 的工具。

## 构建

该工具仅发布 Windows 平台产物，目标为 `x86_64-pc-windows-msvc`。

### 单平台

```bash
# Windows 宿主
cargo build --release --target x86_64-pc-windows-msvc

# macOS / Linux 宿主交叉编译（静态链接 CRT，目标机无需 VC++ 运行库；
# /IGNORE:4099 用于抑制 xwin 静态 CRT 库缺少 PDB 的链接告警）
RUSTFLAGS="-C target-feature=+crt-static -C link-arg=/IGNORE:4099" cargo xwin build --release --target x86_64-pc-windows-msvc
```

### 一键构建

**环境要求**

- Windows 宿主：MSVC 工具链（`x86_64-pc-windows-msvc`）
- 非 Windows 宿主（macOS / Linux）：[cargo-xwin](https://github.com/rust-cross/cargo-xwin)；macOS 上还需要 Homebrew LLVM（提供 `clang-cl`）
- 两者都需要先安装目标平台标准库：`rustup target add x86_64-pc-windows-msvc`

```powershell
./build_all.ps1
```

该脚本在 Windows 宿主上使用原生 MSVC 工具链（`cargo build`），在 macOS / Linux 宿主上使用 `cargo xwin build` 交叉编译 MSVC ABI（并加上 `target-feature=+crt-static`），产物统一复制到 `target/output/` 目录并按版本 API 的发布命名 `meta-mystia-manager-v{version}.exe` 重命名。

> **兼容性说明**：非 Windows 宿主交叉编译的 exe 不包含 thunk-rs（VC-LTL5 + YY-Thunks），因为 YY-Thunks 的 delay-load 符号与 cargo-xwin 使用的 `lld-link` 链接器冲突，且 thunk-rs 依赖 `7z` 解包其下载的二进制包。VC-LTL5 被跳过后，交叉编译改为静态链接 CRT，因此产物是自包含的单文件，不需要 VC++ 运行库，但需要 **Windows 10+**（依赖 `WaitOnAddress`、`ProcessPrng` 等系统 API）；而 Windows 宿主原生构建的产物通过 YY-Thunks 与 VC-LTL5 支持 **Windows 7+**，同样不需要额外的运行库。此外，交叉编译的产物不包含 winres 嵌入的 Windows 版本/资源信息（因 macOS / Linux 上无 `rc.exe` 资源编译器）。
