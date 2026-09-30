use crate::downloader::Downloader;
#[cfg(windows)]
use crate::error::ManagerError;
use crate::error::Result;
use crate::metrics::report_event;
use crate::model::VersionInfo;
#[cfg(windows)]
use crate::platform::suppress_console_window;
use crate::temp_dir::create_temp_dir_with_guard;
use crate::ui::{Ui, UiEvent};

use std::path::Path;

#[cfg(windows)]
use std::{
    env, fs, io,
    process::{self, Command},
};

#[cfg(windows)]
pub fn perform_self_update(
    base_dir: &Path,
    ui: &dyn Ui,
    downloader: &Downloader,
    version_info: &VersionInfo,
) -> Result<String> {
    report_event("SelfUpdate.Start", version_info.manager_version());

    let (temp_dir, _guard) = create_temp_dir_with_guard(base_dir)?;
    let filename = version_info.manager_filename()?;
    let temp_path = temp_dir.join(&filename);

    if let Err(e) = downloader.download_manager(version_info, &temp_path) {
        ui.emit(UiEvent::ManagerUpdateFailed(&format!("下载失败：{e}")))?;
        report_event("SelfUpdate.Failed.Download", Some(&format!("{e}")));
        return Err(e);
    }

    let exe_path = env::current_exe()?;
    let run_dir = exe_path
        .parent()
        .ok_or_else(|| ManagerError::Other("无法确定运行目录".to_string()))?;
    let target_path = run_dir.join(&filename);

    match fs::copy(&temp_path, &target_path) {
        Ok(_) => {}
        Err(e) => {
            ui.emit(UiEvent::ManagerPromptManualUpdate)?;
            report_event("SelfUpdate.Failed.Copy", Some(&format!("{e}")));
            return Err(ManagerError::from(io::Error::new(
                e.kind(),
                format!("复制到运行目录 {} 失败：{}", target_path.display(), e),
            )));
        }
    }

    let script_name = format!("{}-updater_{}.ps1", env!("CARGO_PKG_NAME"), process::id());
    let script_path = env::temp_dir().join(&script_name);

    let script = generate_powershell_script(
        &exe_path.to_string_lossy(),
        &target_path.to_string_lossy(),
        process::id(),
    );

    fs::write(&script_path, script.as_bytes()).map_err(|e| {
        report_event("SelfUpdate.Failed.ScriptWrite", Some(&format!("{e}")));
        ManagerError::from(io::Error::new(
            e.kind(),
            format!("写入升级脚本 {} 失败：{}", script_path.display(), e),
        ))
    })?;

    let shells = ["pwsh.exe", "powershell.exe"];

    for shell in &shells {
        let mut command = Command::new(shell);
        command
            .arg("-NoProfile")
            .arg("-ExecutionPolicy")
            .arg("Bypass")
            .arg("-File")
            .arg(&script_path);

        suppress_console_window(&mut command);

        if command.spawn().is_ok() {
            report_event("SelfUpdate.Scheduled", version_info.manager_version());
            ui.emit(UiEvent::ManagerUpdateStarting)?;
            return Ok(filename);
        }
    }

    ui.emit(UiEvent::ManagerUpdateFailed("无法执行升级脚本"))?;
    Err(ManagerError::Other("无法启动 PowerShell".to_string()))
}

#[cfg(not(windows))]
pub fn perform_self_update(
    base_dir: &Path,
    ui: &dyn Ui,
    downloader: &Downloader,
    version_info: &VersionInfo,
) -> Result<String> {
    report_event("SelfUpdate.Start", version_info.manager_version());

    let (temp_dir, _guard) = create_temp_dir_with_guard(base_dir)?;
    let filename = version_info.manager_filename()?;
    let temp_path = temp_dir.join(&filename);

    if let Err(e) = downloader.download_manager(version_info, &temp_path) {
        ui.emit(UiEvent::ManagerUpdateFailed(&format!("下载失败：{e}")))?;
        report_event("SelfUpdate.Failed.Download", Some(&format!("{e}")));
        return Err(e);
    }

    report_event("SelfUpdate.Simulated", version_info.manager_version());
    ui.emit(UiEvent::ManagerUpdateStarting)?;
    eprintln!("[dev] 已获取 {filename}，跳过替换正在运行的可执行文件（仅 Windows 支持）");

    Ok(filename)
}

#[cfg(windows)]
fn generate_powershell_script(target: &str, new_exe: &str, pid: u32) -> String {
    let target = target.replace('\'', "''");
    let new_exe = new_exe.replace('\'', "''");

    format!(
        r#"param(
    [string]$Old = '{target}',
    [string]$New = '{new_exe}',
    [int]$OldPid = {pid}
)

$oldName = Split-Path $Old -Leaf
$targetDir = Split-Path $Old -Parent
$bak = $null

function Remove-Self {{
    param([string]$Script)

    try {{ Remove-Item -LiteralPath $Script -Force -ErrorAction SilentlyContinue }} catch {{}}
}}

function WaitForExit($procId, $timeout_secs) {{
    $start = Get-Date
    while ((Get-Date) -lt $start.AddSeconds($timeout_secs)) {{
        try {{
            $p = Get-Process -Id $procId -ErrorAction SilentlyContinue
            if ($null -eq $p) {{ return $true }}
        }} catch {{ return $true }}
        Start-Sleep -Seconds 1
    }}
    return $false
}}

# 等待旧进程退出
$ok = WaitForExit $OldPid 10
if (-not $ok) {{
    Write-Output "Timeout waiting for process $OldPid to exit"
    Remove-Self $PSCommandPath
    exit 1
}}

# 备份旧 exe
if (Test-Path $Old) {{
    try {{
        $t = Get-Date -Format "yyyyMMddHHmmss"
        $bak = Join-Path $targetDir ($oldName + ".old." + $t)
        Move-Item -Path $Old -Destination $bak -Force -ErrorAction Stop
    }} catch {{
        $bak = $null
    }}
}}

# 启动新 exe
try {{
    Start-Process -FilePath $New -WorkingDirectory $targetDir
}} catch {{
    if ($bak -ne $null -and (Test-Path $bak)) {{
        try {{ Move-Item -Path $bak -Destination $Old -Force -ErrorAction SilentlyContinue }} catch {{}}
    }}
    Remove-Self $PSCommandPath
    exit 1
}}

# 清理
Start-Sleep -Seconds 1
if ($bak -ne $null -and (Test-Path $bak)) {{
    try {{ Remove-Item -Path $bak -Force -ErrorAction SilentlyContinue }} catch {{}}
}}

# 清理自身：脚本内容已解析完，删除后继续执行不受影响
Remove-Self $PSCommandPath

exit 0
"#
    )
}
