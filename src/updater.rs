use crate::downloader::Downloader;
use crate::error::{ManagerError, Result};
use crate::metrics::report_event;
use crate::model::VersionInfo;
use crate::temp_dir::create_temp_dir_with_guard;
use crate::ui::Ui;

use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::Command;
use winapi::um::winbase::CREATE_NO_WINDOW;

pub fn perform_self_update(
    game_root: &Path,
    ui: &dyn Ui,
    downloader: &Downloader,
    version_info: &VersionInfo,
    auto_launch: bool,
) -> Result<String> {
    report_event("SelfUpdate.Start", Some(&version_info.manager));

    // 1. 准备临时目录并下载
    let (temp_dir, _guard) = create_temp_dir_with_guard(game_root)?;
    let filename = version_info.manager_filename();
    let temp_path = temp_dir.join(&filename);

    if let Err(e) = downloader.download_manager(version_info, &temp_path) {
        ui.manager_update_failed(&format!("下载失败：{}", e))?;
        report_event("SelfUpdate.Failed.Download", Some(&format!("{}", e)));
        return Err(e);
    }

    // 2. 复制到运行目录
    let exe_path = std::env::current_exe()?;
    let run_dir = exe_path
        .parent()
        .ok_or_else(|| ManagerError::Other("无法确定运行目录".to_string()))?;
    let target_path = run_dir.join(&filename);

    match std::fs::copy(&temp_path, &target_path) {
        Ok(_) => {}
        Err(e) => {
            ui.manager_prompt_manual_update()?;
            report_event("SelfUpdate.Failed.Copy", Some(&format!("{}", e)));
            return Err(ManagerError::from(std::io::Error::new(
                e.kind(),
                format!("复制到运行目录 {} 失败：{}", target_path.display(), e),
            )));
        }
    }

    // 3. 生成升级脚本
    let script_name = format!(
        "{}-updater_{}.ps1",
        env!("CARGO_PKG_NAME"),
        std::process::id()
    );
    let script_path = std::env::temp_dir().join(&script_name);

    let script = generate_powershell_script(
        &exe_path.to_string_lossy(),
        &target_path.to_string_lossy(),
        std::process::id(),
        auto_launch,
    );

    std::fs::write(&script_path, script.as_bytes()).map_err(|e| {
        report_event("SelfUpdate.Failed.ScriptWrite", Some(&format!("{}", e)));
        ManagerError::from(std::io::Error::new(
            e.kind(),
            format!("写入升级脚本 {} 失败：{}", script_path.display(), e),
        ))
    })?;

    // 4. 启动脚本
    let shells = ["pwsh.exe", "powershell.exe"];

    for shell in &shells {
        let res = Command::new(shell)
            .arg("-NoProfile")
            .arg("-ExecutionPolicy")
            .arg("Bypass")
            .arg("-File")
            .arg(&script_path)
            .creation_flags(CREATE_NO_WINDOW)
            .spawn();

        if res.is_ok() {
            report_event("SelfUpdate.Scheduled", Some(&version_info.manager));
            ui.manager_update_starting()?;
            return Ok(filename);
        }
    }

    ui.manager_update_failed("无法执行升级脚本")?;
    Err(ManagerError::Other("无法启动 PowerShell".to_string()))
}

fn generate_powershell_script(target: &str, new_exe: &str, pid: u32, auto_launch: bool) -> String {
    let launch_script = if auto_launch {
        r"
# 启动新 exe
try {
    Start-Process -FilePath $New -WorkingDirectory $targetDir
} catch {
    if ($bak -ne $null -and (Test-Path $bak)) {
        try { Move-Item -Path $bak -Destination $Old -Force -ErrorAction SilentlyContinue } catch {}
    }
    exit 1
}
"
    } else {
        ""
    };

    format!(
        r#"param(
    [string]$Old = '{target}',
    [string]$New = '{new_exe}',
    [int]$OldPid = {pid}
)

$oldName = Split-Path $Old -Leaf
$targetDir = Split-Path $Old -Parent
$bak = $null

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
{launch_script}
# 清理
Start-Sleep -Seconds 1
if ($bak -ne $null -and (Test-Path $bak)) {{
    try {{ Remove-Item -Path $bak -Force -ErrorAction SilentlyContinue }} catch {{}}
}}

exit 0
"#
    )
}
