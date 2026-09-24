//! 非 Windows 宿主（macOS / Linux）的开发模拟实现
//!
//! 仅用于本地开发调试：Windows 特有的能力（提权、进程枚举、CNG 加密、控制台事件钩子）在这里
//! 用等价实现或桩替代，副作用限制在项目 `target/` 下的沙箱目录里。
//! 所有开关都通过环境变量控制，默认值面向“能直接跑起来调试”。

use crate::config::GAME_EXECUTABLE;
use crate::error::{ManagerError, Result};
use crate::model::{DownloadPaths, VersionInfo};

use std::{
    env, fs, io,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::Command,
};

const ENV_MODE: &str = "MMM_DEV_MODE";
const ENV_ROOT: &str = "MMM_DEV_ROOT";
const ENV_SIM_DOWNLOAD: &str = "MMM_DEV_SIM_DOWNLOAD";
const ENV_SIM_LOGIN: &str = "MMM_DEV_SIM_LOGIN";
const ENV_SIM_SELF_UPDATE: &str = "MMM_DEV_SIM_SELF_UPDATE";
const ENV_SIM_FS: &str = "MMM_DEV_SIM_FS";
const ENV_GAME_RUNNING: &str = "MMM_DEV_GAME_RUNNING";

const SHA256_LENGTH: usize = 32;

/// 开发模式下的占位产物类型
pub enum FakeArtifact {
    /// MetaMystia DLL
    Dll,
    /// ResourceExample / BepInEx ZIP
    Zip,
    /// 管理工具可执行文件
    Exe,
}

fn flag(name: &str, default: bool) -> bool {
    env::var(name).map_or(default, |value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

/// 是否处于开发模拟模式
pub fn dev_mode() -> bool {
    flag(ENV_MODE, true)
}

/// 是否跳过网络下载，改用占位产物
pub fn sim_download() -> bool {
    dev_mode() && flag(ENV_SIM_DOWNLOAD, false)
}

/// 是否跳过账号登录
pub fn sim_login() -> bool {
    dev_mode() && flag(ENV_SIM_LOGIN, true)
}

/// 是否强制“游戏正在运行”
pub fn force_game_running() -> bool {
    dev_mode() && flag(ENV_GAME_RUNNING, false)
}

/// 沙箱游戏目录
pub fn sandbox_root() -> PathBuf {
    env::var_os(ENV_ROOT).map_or_else(
        || {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("target")
                .join("dev-game")
        },
        PathBuf::from,
    )
}

/// 确保沙箱游戏目录存在，并铺好能通过目录检查的最小结构
pub fn ensure_sandbox_root() -> io::Result<PathBuf> {
    let root = sandbox_root();

    fs::create_dir_all(root.join("BepInEx").join("plugins"))?;
    fs::create_dir_all(root.join("ResourceEx"))?;

    // 目录检查要求游戏可执行文件存在，这里只放一个空占位文件
    let game_exe = root.join(GAME_EXECUTABLE);
    if !game_exe.is_file() {
        fs::write(&game_exe, b"")?;
    }

    Ok(root)
}

/// 初始化开发模拟模式
pub fn init() {
    if !dev_mode() {
        eprintln!("[dev] 开发模拟模式已关闭（{ENV_MODE}=0），仅缺少的平台能力会被替代");
        return;
    }

    crate::metrics::disable();

    eprintln!("[dev] 开发模拟模式：Windows 特有行为已由开发实现替代");
    eprintln!("[dev] 沙箱游戏目录：{}", sandbox_root().display());

    let switches = [
        (
            ENV_SIM_DOWNLOAD,
            sim_download(),
            "跳过网络下载，使用占位产物",
        ),
        (
            ENV_SIM_LOGIN,
            sim_login(),
            "跳过账号登录（真实联网下载需设为 0）",
        ),
        (ENV_SIM_SELF_UPDATE, !self_update_enabled(), "跳过自更新"),
        (ENV_SIM_FS, fs_dry_run(), "游戏目录只打印不落盘"),
        (ENV_GAME_RUNNING, force_game_running(), "强制游戏运行中"),
    ];

    for (name, enabled, desc) in switches {
        eprintln!("[dev]   {name}={} → {desc}", u8::from(enabled));
    }

    eprintln!("[dev] 提示：开发模式不注册 Ctrl+C 钩子，中断时清理回调不会执行");
}

/// 开发模式下视为已具备权限（不会触发提权重启流程）
pub const fn is_elevated() -> bool {
    true
}

/// 开发模式不支持提权重启
pub fn elevate_and_restart() -> Result<()> {
    Err(ManagerError::Other(
        "开发模拟模式不支持提权重启，请检查沙箱目录权限".to_string(),
    ))
}

/// 是否启用自更新（开发模式默认跳过）
pub fn self_update_enabled() -> bool {
    !(dev_mode() && flag(ENV_SIM_SELF_UPDATE, true))
}

/// 游戏目录是否只做干跑（不产生任何变更）
pub fn fs_dry_run() -> bool {
    dev_mode() && flag(ENV_SIM_FS, false)
}

/// 检查游戏进程是否正在运行（开发模式下由开关决定）
#[allow(clippy::unnecessary_wraps, reason = "与 Windows 实现保持相同签名")]
pub fn is_game_running() -> Result<bool> {
    Ok(force_game_running())
}

/// 用系统默认程序打开链接
pub fn open_url(url: &str) -> Result<()> {
    for opener in ["open", "xdg-open"] {
        match Command::new(opener).arg(url).spawn() {
            Ok(_) => return Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(ManagerError::SsoLoginFailed(format!(
                    "无法打开默认浏览器（{opener}）：{e}"
                )));
            }
        }
    }

    Err(ManagerError::SsoLoginFailed(
        "无法打开默认浏览器（未找到 open 或 xdg-open）".to_string(),
    ))
}

/// 读取系统随机数填充缓冲区
pub fn random_bytes(buffer: &mut [u8]) -> Result<()> {
    fs::File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(buffer))
        .map_err(|e| ManagerError::SsoLoginFailed(format!("读取系统随机数失败：{e}")))
}

/// SHA-256 摘要
#[allow(clippy::unnecessary_wraps, reason = "与 Windows 实现保持相同签名")]
pub fn sha256(data: &[u8]) -> Result<[u8; SHA256_LENGTH]> {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(data);

    Ok(hasher.finalize().into())
}

/// `MMM_DEV_SIM_DOWNLOAD=1` 时使用的伪版本信息，用于离线跑通流程
pub fn fake_version_info() -> VersionInfo {
    VersionInfo {
        // 与 v3 服务端一致：`bepInEx` 只放构建号，文件名单独下发
        bep_in_ex: "1".to_string(),
        bep_in_ex_file_name: Some("BepInEx-Unity.IL2CPP-win-x64-6.0.0-be.1+dev.zip".to_string()),
        manager: env!("CARGO_PKG_VERSION").to_string(),
        // 离线模式下不会请求运行期配置，仅保证字段可用
        config_url: String::new(),
        dlls: vec!["1.0.0".to_string()],
        paths: DownloadPaths {
            bep_in_ex: None,
            dll: None,
            zip: None,
        },
        zips: vec!["1.0.0".to_string()],
    }
}

/// `MMM_DEV_SIM_DOWNLOAD=1` 时生成最小合法占位产物
pub fn write_fake_artifact(dest: &Path, kind: &FakeArtifact) -> Result<()> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(ManagerError::from)?;
    }

    match kind {
        FakeArtifact::Zip => write_fake_zip(dest),
        FakeArtifact::Dll | FakeArtifact::Exe => {
            let name = dest
                .file_name()
                .map_or_else(String::new, |name| name.to_string_lossy().to_string());

            fs::write(dest, format!("[dev] 占位产物：{name}\n")).map_err(ManagerError::from)
        }
    }
}

fn write_fake_zip(dest: &Path) -> Result<()> {
    use zip::write::SimpleFileOptions;

    let file = fs::File::create(dest).map_err(ManagerError::from)?;
    let mut writer = zip::ZipWriter::new(file);

    for entry in [
        "BepInEx/core/dev-placeholder.txt",
        "BepInEx/plugins/dev-placeholder.txt",
    ] {
        let options = SimpleFileOptions::default();
        writer
            .start_file(entry, options)
            .map_err(|e| ManagerError::Other(format!("生成占位压缩包失败：{e}")))?;
        writer
            .write_all(b"[dev] placeholder\n")
            .map_err(ManagerError::from)?;
    }

    writer
        .finish()
        .map_err(|e| ManagerError::Other(format!("生成占位压缩包失败：{e}")))?;

    Ok(())
}
