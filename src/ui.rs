//! 界面事件与 Ui 抽象。
//!
//! 流程代码只通过 [`Ui::emit`] 发送事件，界面负责渲染与弹框；
//! 需要用户拍板的事件通过 [`UiReply`] 回传结果。下载期间的高频状态查询不走事件通道。

use crate::error::{ManagerError, Result};
use crate::mode::UninstallMode;
use crate::version::VersionInfo;

use std::path::{Path, PathBuf};

/// 流程代码发给界面的所有事件。
pub enum UiEvent<'a> {
    /// 展示管理工具版本（`None` 表示未知）
    DisplayVersion(Option<&'a str>),
    GameRunningWarning,
    Message(&'a str),
    Warn(&'a str),
    /// 在 Steam 库中找到游戏
    SteamFound {
        app_id: u32,
        name: Option<&'a str>,
        path: &'a Path,
    },

    /// 安装步骤进度（序号，说明）
    InstallStep(usize, &'a str),
    InstallVersionInfo(&'a VersionInfo),
    InstallDownloadsCompleted,
    InstallStartCleanup,
    /// 安装前清理结果（删除数，失败数）
    InstallCleanupResult(usize, usize),
    /// 安装结束（是否成功）
    InstallFinished(bool),

    UpgradeDeleted(&'a Path),
    /// 删除旧文件失败（路径，原因）
    UpgradeDeleteFailed(&'a Path, &'a str),
    UpgradeCheckingInstalledVersion,
    UpgradeDetectedResourceex,
    /// BepInEx 版本对比（当前，最新）
    UpgradeBepinexVersions(&'a str, &'a str),
    /// MetaMystia DLL 版本对比（当前，最新）
    UpgradeDllVersions(&'a str, &'a str),
    /// ResourceExample ZIP 版本对比（当前，最新）
    UpgradeResourceexVersions(&'a str, &'a str),
    UpgradeNoUpdateNeeded,
    /// 是否需要升级 BepInEx
    UpgradeBepinexNeedsUpgrade(bool),
    UpgradeBepinexAlreadyLatest,
    /// 检测到已安装 DLL（当前版本，最新版本）
    UpgradeDllDetected(&'a str, &'a str),
    UpgradeDllAlreadyLatest,
    /// 是否需要升级 ResourceExample
    UpgradeResourceexNeedsUpgrade(bool),
    UpgradeDownloadingBepinex,
    UpgradeDownloadingDll,
    UpgradeDownloadingResourceex,
    UpgradeInstallingBepinex,
    UpgradeInstallingDll,
    UpgradeInstallingResourceex,
    UpgradeInstallSuccess(&'a Path),
    UpgradeCleanupStart,
    UpgradeDone,

    UninstallNoFilesFound,
    UninstallTargetFiles(&'a [PathBuf]),
    UninstallConfirmDeletion(UninstallMode),
    UninstallFilesInUse,
    /// 等待重试文件占用（等待秒数，第几轮，总轮数）
    UninstallWaitBeforeRetry(u64, usize, usize),
    UninstallAskElevate,
    UninstallRestartingElevated,
    UninstallAskRetryFailures,
    UninstallRetryingFailedItems,

    DeletionStart,
    /// 删除进度（已完成数，总数，当前路径）
    DeletionProgress(usize, usize, &'a str),
    DeletionSuccess(&'a str),
    DeletionFailure(&'a str, &'a str),
    DeletionSkipped(&'a str),
    /// 删除汇总（成功数，失败数，跳过数）
    DeletionSummary(usize, usize, usize),

    /// 本次下载计划（文件说明列表）
    DownloadPlan(&'a [&'a str]),
    /// 单个下载开始（槽位，文件名，总大小）
    DownloadStart(usize, &'a str, Option<u64>),
    /// 下载进度（槽位，已下载字节数）
    DownloadUpdate(usize, u64),
    /// 单个下载结束（槽位，文件名，结果）
    DownloadFinish(usize, &'a str, JobOutcome),
    DownloadVersionInfoStart,
    DownloadVersionInfoFailed(&'a str),
    DownloadVersionInfoSuccess,
    DownloadVersionInfoParseFailed(&'a str, &'a str),
    DownloadShareCodeStart,
    DownloadShareCodeFailed(&'a str),
    DownloadShareCodeSuccess,
    DownloadAttemptGithubDll,
    DownloadFoundGithubAsset(&'a str),
    DownloadGithubDllNotFound,
    /// GitHub Release 说明（版本，标题，正文）
    DownloadReleaseNotes(&'a str, &'a str, &'a str),
    DownloadSwitchToFallback(&'a str),
    DownloadTryFallbackMetamystia,
    DownloadBepinexAttemptPrimary,
    DownloadBepinexPrimaryFailed(&'a str),

    /// 网络重试（操作，等待秒数，第几次，总次数，原因）
    NetworkRetrying(&'a str, u64, usize, usize, &'a str),
    /// 重试耗尽（操作，总次数，原因）
    NetworkRetryFailed(&'a str, usize, &'a str),
    /// 被服务端限流（建议等待秒数）
    NetworkRateLimited(u64),

    ManagerUpdateStarting,
    ManagerUpdateFailed(&'a str),
    ManagerPromptManualUpdate,

    SsoAskOpenBrowser,

    /// 导出前确认（将收集的条目说明）
    DiagnosticsConfirmExport(&'a [String]),
    /// 诊断包已生成（路径）
    DiagnosticsExported(&'a Path),
}

/// 单个下载任务的最终结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobOutcome {
    /// 下载完成
    Completed,
    /// 下载失败
    Failed,
}

/// [`Ui::emit`] 的返回值；不需要回传结果的事件返回 [`UiReply::Ack`]。
#[derive(Clone, Copy, Debug)]
pub enum UiReply {
    Ack,
    Bool(bool),
    DownloadId(usize),
}

impl UiReply {
    fn mismatch() -> ManagerError {
        ManagerError::Other("界面事件响应类型不匹配".to_string())
    }

    /// 把回复解释为布尔值（确认/取消）。
    pub fn bool(self) -> Result<bool> {
        if let Self::Bool(value) = self {
            Ok(value)
        } else {
            Err(Self::mismatch())
        }
    }

    /// 从需要下载槽位的回复里取任务 ID。
    pub fn download_id(self) -> Result<usize> {
        if let Self::DownloadId(value) = self {
            Ok(value)
        } else {
            Err(Self::mismatch())
        }
    }
}

/// 界面抽象：流程只通过它发送事件、查询状态。
pub trait Ui: Send + Sync {
    /// 发送一个界面事件；需要用户确认的事件会阻塞到界面回传结果。
    fn emit(&self, event: UiEvent<'_>) -> Result<UiReply>;

    /// 下载或登录等待期间是否被用户取消（高频轮询）。
    fn is_download_cancelled(&self) -> bool;

    /// 同批下载中是否已有任务失败、需要中止其余任务（高频轮询）。
    fn is_download_aborted(&self) -> bool {
        false
    }

    /// 由界面标记"同批下载需要中止"，流程侧只读取 [`Ui::is_download_aborted`]。
    fn set_download_aborted(&self, _aborted: bool) {}

    /// 下载是否被暂停（高频轮询）。
    fn is_download_paused(&self) -> bool;
}
