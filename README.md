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

## 开发模拟模式（macOS / Linux）

非 Windows 宿主上可以直接编译并运行本工具，用于功能开发与调试：Windows 特有行为（提权、进程枚举、系统浏览器、CNG 加密、控制台事件钩子）由 `src/platform/dev.rs` 的等价实现或桩替代，所有副作用限制在项目 `target/dev-game` 沙箱目录内，**真实游戏目录与本机 Steam 安装不会被触碰**。

```bash
cargo run                                   # 交互式界面，直接在沙箱里跑
MMM_DEV_SIM_DOWNLOAD=1 cargo run            # 完全离线：用占位产物跑通流程
```

首次运行会自动在沙箱里铺好 `Touhou Mystia Izakaya.exe` 占位文件、`BepInEx/plugins/`、`ResourceEx/`，因此安装 / 升级 / 卸载 / 诊断包导出都能完整走一遍。因为平台相关代码都被 `cfg` 隔离，编辑器与 rust-analyzer 按宿主目标分析时不再报 `cannot find windows in os` 之类的错误。

| 环境变量 | 默认值 | 作用 |
| --- | --- | --- |
| `MMM_DEV_MODE` | `1` | 开发模拟总开关；置 `0` 时尽量走真实行为（仅缺少的平台能力降级） |
| `MMM_DEV_ROOT` | `target/dev-game` | 沙箱游戏目录，可指向任意路径 |
| `MMM_DEV_SIM_DOWNLOAD` | `0` | `1` 时不联网：返回伪版本信息并生成最小合法占位产物（含可解压的 zip） |
| `MMM_DEV_SIM_LOGIN` | `1` | `1` 时跳过 SSO 登录并注入假账号（此时只能配合离线占位产物）；**想真实联网下载要设为 `0`**，走浏览器登录后使用真实下载凭证 |
| `MMM_DEV_SIM_SELF_UPDATE` | `1` | `1` 时跳过自更新检查与执行 |
| `MMM_DEV_SIM_FS` | `0` | `1` 时沙箱目录不发生任何变更，只打印将要执行的动作 |
| `MMM_DEV_GAME_RUNNING` | `0` | `1` 时强制“游戏正在运行”，用于调试该分支 |

例如完全离线跑通安装流程：`MMM_DEV_SIM_DOWNLOAD=1 cargo run`；想看真实下载链路：`MMM_DEV_SIM_LOGIN=0 cargo run`。

> 这些代码全部位于 `cfg(not(windows))` 分支，且 `sha2`、`windows-sys`、`steamlocate`、`winres`、`thunk-rs` 都是目标限定依赖，因此 Windows 产物不受影响。可用 `cargo tree --target x86_64-pc-windows-msvc | grep sha2`（无输出）与 `strings -a <exe> | grep MMM_DEV_`（无输出）复核。
