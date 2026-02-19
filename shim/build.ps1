#Requires -Version 5.1
<#
.SYNOPSIS
    编译 Windows 7 兼容 shim DLL：api-ms-win-core-synch-l1-2-0.dll

.DESCRIPTION
    使用 MSVC cl.exe 将 api-ms-win-core-synch-l1-2-0.c 编译为 DLL。
    编译完成后将 DLL 复制到 ../target/debug/ 和 ../target/release/（如存在）。

.EXAMPLE
    .\build.ps1
    .\build.ps1 -Arch x86
#>

param(
  [ValidateSet("x64", "x86")]
  [string]$Arch = "x64"
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$ScriptDir = $PSScriptRoot

# 需要编译的 shim 列表：@(源文件，DEF 文件，输出 DLL)
$Shims = @(
  @("api-ms-win-core-synch-l1-2-0.c", "api-ms-win-core-synch-l1-2-0.def", "api-ms-win-core-synch-l1-2-0.dll"),
  @("bcryptprimitives.c", "bcryptprimitives.def", "bcryptprimitives.dll")
)

# ── 1. 定位 cl.exe ────────────────────────────────────────────────────────────
function Find-Cl {
  param([string]$Arch)

  $hostArch = if ([Environment]::Is64BitOperatingSystem) { "x64" } else { "x86" }

  # 尝试通过 vswhere 定位 Visual Studio 安装目录
  $vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
  if (-not (Test-Path $vswhere)) {
    $vswhere = "${env:ProgramFiles}\Microsoft Visual Studio\Installer\vswhere.exe"
  }

  if (Test-Path $vswhere) {
    $vsPath = & $vswhere -latest -products * -requires Microsoft.VisualCpp.Tools.HostX64.TargetX64 -property installationPath 2>$null
    if ($vsPath) {
      $cl = Get-ChildItem "$vsPath\VC\Tools\MSVC" -Filter "cl.exe" -Recurse |
      Where-Object { $_.FullName -match "Host$hostArch\\$Arch\\" } |
      Select-Object -First 1
      if ($cl) { return $cl.FullName }
    }
  }

  # 回退：直接在常见路径搜索
  $searchRoots = @(
    "${env:ProgramFiles}\Microsoft Visual Studio",
    "${env:ProgramFiles(x86)}\Microsoft Visual Studio"
  )
  foreach ($root in $searchRoots) {
    if (-not (Test-Path $root)) { continue }
    $cl = Get-ChildItem $root -Filter "cl.exe" -Recurse -ErrorAction SilentlyContinue |
    Where-Object { $_.FullName -match "Host$hostArch\\$Arch\\" } |
    Select-Object -First 1
    if ($cl) { return $cl.FullName }
  }

  return $null
}

$clPath = Find-Cl -Arch $Arch
if (-not $clPath) {
  Write-Error "未找到 cl.exe（目标架构：$Arch）。请先安装 Visual Studio 并勾选「使用 C++ 的桌面开发」工作负载。"
  exit 1
}

Write-Host "使用编译器：$clPath" -ForegroundColor Cyan

# ── 2. 定位 vcvarsall.bat ──────────────────────────────────────────────────────
$vcvarsall = $clPath -replace "\\bin\\Host.*$", "\..\..\..\..\Auxiliary\Build\vcvarsall.bat" |
Resolve-Path -ErrorAction SilentlyContinue |
Select-Object -ExpandProperty Path

if (-not $vcvarsall -or -not (Test-Path $vcvarsall)) {
  # 回退：在 VS 安装目录中搜索
  $vsDir = $clPath -replace "\\VC\\.*$", ""
  $vcvarsall = Get-ChildItem $vsDir -Filter "vcvarsall.bat" -Recurse -ErrorAction SilentlyContinue |
  Select-Object -First 1 -ExpandProperty FullName
}

if (-not $vcvarsall) {
  Write-Error "未找到 vcvarsall.bat，无法初始化 MSVC 编译环境。"
  exit 1
}

Write-Host "初始化 MSVC 环境：$vcvarsall $Arch" -ForegroundColor Cyan

# ── 3. 逐一编译所有 shim ──────────────────────────────────────────────────────
$RepoRoot = Split-Path $ScriptDir -Parent

foreach ($shim in $Shims) {
  $SrcFile, $DefFile, $OutDll = $shim

  # Minimize DLL size:
  #   /Os /Gy        - optimize for size, function-level linking
  #   /GS-           - disable buffer-security cookie (removes CRT startup dependency)
  #   /NODEFAULTLIB  - do not link any CRT; only kernel32.lib is needed
  #   /OPT:REF,ICF   - strip unreferenced functions/data, fold identical COMDATs
  #   /INCREMENTAL:NO /DEBUG:NONE - no incremental table, no PDB overhead
  #   /MERGE         - merge read-only sections to reduce section-header overhead
  $clArgs = "/LD /Os /Gy /GS- /W2 /nologo `"$ScriptDir\$SrcFile`" /link /DEF:`"$ScriptDir\$DefFile`" /OUT:`"$ScriptDir\$OutDll`" /NODEFAULTLIB /ENTRY:DllMain kernel32.lib /OPT:REF /OPT:ICF /INCREMENTAL:NO /DEBUG:NONE /MERGE:.rdata=.text"
  $cmdLine = "call `"$vcvarsall`" $Arch >nul 2>&1 && cd /d `"$ScriptDir`" && cl.exe $clArgs"

  Write-Host "正在编译 $SrcFile -> $OutDll ..." -ForegroundColor Cyan

  cmd.exe /c $cmdLine
  if ($LASTEXITCODE -ne 0) {
    Write-Error "编译 $SrcFile 失败（退出码 $LASTEXITCODE）"
    exit $LASTEXITCODE
  }

  Write-Host "编译成功：$ScriptDir\$OutDll" -ForegroundColor Green

  # ── 4. 复制到 Cargo 输出目录 ──────────────────────────────────────────────
  foreach ($buildProfile in @("debug", "release")) {
    $dest = Join-Path $RepoRoot "target\$buildProfile\$OutDll"
    if (Test-Path (Split-Path $dest -Parent)) {
      Copy-Item "$ScriptDir\$OutDll" $dest -Force
      Write-Host "已复制到 $dest" -ForegroundColor DarkGray
    }
  }
}
