//! 界面事件与 Ui 抽象。
//!
//! 流程代码只通过 [`Ui::emit`] 发送事件，界面负责渲染与弹框；需要用户拍板的
//! 事件通过 [`UiReply`] 回传结果。下载期间的高频状态查询不走事件通道。

use crate::config::UninstallMode;
use crate::error::{ManagerError, Result};
use crate::model::VersionInfo;

use std::path::{Path, PathBuf};

pub enum UiEvent<'a> {
    DisplayVersion(Option<&'a str>),
    GameRunningWarning,
    Message(&'a str),
    Warn(&'a str),
    SteamFound {
        app_id: u32,
        name: Option<&'a str>,
        path: &'a Path,
    },

    InstallStep(usize, &'a str),
    InstallVersionInfo(&'a VersionInfo),
    InstallDownloadsCompleted,
    InstallStartCleanup,
    InstallCleanupResult(usize, usize),
    InstallFinished(bool),

    UpgradeDeleted(&'a Path),
    UpgradeDeleteFailed(&'a Path, &'a str),
    UpgradeCheckingInstalledVersion,
    UpgradeDetectedResourceex,
    UpgradeBepinexVersions(&'a str, &'a str),
    UpgradeDllVersions(&'a str, &'a str),
    UpgradeResourceexVersions(&'a str, &'a str),
    UpgradeNoUpdateNeeded,
    UpgradeBepinexNeedsUpgrade(bool),
    UpgradeBepinexAlreadyLatest,
    UpgradeDllDetected(&'a str, &'a str),
    UpgradeDllAlreadyLatest,
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
    UninstallWaitBeforeRetry(u64, usize, usize),
    UninstallAskElevate,
    UninstallRestartingElevated,
    UninstallAskRetryFailures,
    UninstallRetryingFailedItems,

    DeletionStart,
    DeletionProgress(usize, usize, &'a str),
    DeletionSuccess(&'a str),
    DeletionFailure(&'a str, &'a str),
    DeletionSkipped(&'a str),
    DeletionSummary(usize, usize, usize),

    DownloadPlan(&'a [&'a str]),
    DownloadStart(usize, &'a str, Option<u64>),
    DownloadUpdate(usize, u64),
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
    DownloadReleaseNotes(&'a str, &'a str, &'a str),
    DownloadSwitchToFallback(&'a str),
    DownloadTryFallbackMetamystia,
    DownloadBepinexAttemptPrimary,
    DownloadBepinexPrimaryFailed(&'a str),

    NetworkRetrying(&'a str, u64, usize, usize, &'a str),
    NetworkRetryFailed(&'a str, usize, &'a str),
    NetworkRateLimited(u64),

    ManagerUpdateStarting,
    ManagerUpdateFailed(&'a str),
    ManagerPromptManualUpdate,

    SsoAskOpenBrowser,

    DiagnosticsConfirmExport(&'a [String]),
    DiagnosticsExported(&'a Path),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobOutcome {
    Completed,
    Failed,
}

/// [`Ui::emit`] 的返回值；不需要回传结果的事件返回 [`UiReply::Ack`]
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

    pub fn bool(self) -> Result<bool> {
        if let Self::Bool(value) = self {
            Ok(value)
        } else {
            Err(Self::mismatch())
        }
    }

    pub fn download_id(self) -> Result<usize> {
        if let Self::DownloadId(value) = self {
            Ok(value)
        } else {
            Err(Self::mismatch())
        }
    }
}

pub trait Ui: Send + Sync {
    /// 发送一个界面事件；需要用户确认的事件会阻塞到界面回传结果
    fn emit(&self, event: UiEvent<'_>) -> Result<UiReply>;

    /// 下载或登录等待期间是否被用户取消（高频轮询）
    fn download_cancelled(&self) -> bool;
    /// 同批下载中是否已有任务失败、需要中止其余任务（高频轮询）
    fn download_aborted(&self) -> bool {
        false
    }
    fn set_download_aborted(&self, _aborted: bool) {}
    /// 下载是否被暂停（高频轮询）
    fn download_paused(&self) -> bool;
}
