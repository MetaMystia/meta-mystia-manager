//! 界面多语言：`TextId` 枚举是唯一文案键，五种语言各有一份 `i18n/*.json` 词条表。

use std::{
    collections::HashMap,
    sync::{
        OnceLock,
        atomic::{AtomicU8, Ordering},
    },
};

/// 界面文案键。
///
/// 变体名即词条表里的键（PascalCase，`分组_名称`），新增文案时先在此登记。
macro_rules! text_ids {
    ($($name:ident),+ $(,)?) => {
        /// 界面文案键。
        #[derive(Clone, Copy, PartialEq, Eq)]
        #[allow(non_camel_case_types)]
        pub enum TextId {
            $(
                #[allow(missing_docs)]
                $name,
            )+
        }

        impl TextId {
            /// 全部文案键，供校验与新增词条时使用。
            #[allow(dead_code, reason = "发布构建不遍历全部键，单元测试与新增词条时使用")]
            pub const ALL: &'static [TextId] = &[$(TextId::$name),+];

            /// 词条表里的键名。
            #[must_use]
            pub const fn name(self) -> &'static str {
                match self {
                    $(TextId::$name => stringify!($name)),+
                }
            }
        }
    };
}

text_ids! {
    App_ErrorTitle,
    App_Title,
    Bridge_AdminRequiredContent,
    Bridge_AdminRequiredTitle,
    Bridge_AlreadyLatest,
    Bridge_AutoRollbackCheckFailed,
    Bridge_AutoRollbackFailed,
    Bridge_AutoRollbackGameRunning,
    Bridge_AutoRollbackRestored,
    Bridge_BepInExPrimaryFailed,
    Bridge_CheckingInstalled,
    Bridge_CleanupResult,
    Bridge_ConsoleEnabled,
    Bridge_DeleteFailed,
    Bridge_Deleted,
    Bridge_DeletionStart,
    Bridge_DeletionSummary,
    Bridge_DetectComponentsFailed,
    Bridge_DiagnosticsConfirmStart,
    Bridge_DiagnosticsConfirmTitle,
    Bridge_DiagnosticsContent,
    Bridge_FileFound,
    Bridge_FileInUseWarning,
    Bridge_FileStillInUse,
    Bridge_FilesToDelete,
    Bridge_GameDirNotFound,
    Bridge_GameRunningRetry,
    Bridge_GithubFallback,
    Bridge_LoginRequiredContent,
    Bridge_LoginRequiredTitle,
    Bridge_ManagerLatest,
    Bridge_ManagerManualUpdate,
    Bridge_ManagerUpdateFailed,
    Bridge_ManagerUpdateStarting,
    Bridge_NetworkRetryFailed,
    Bridge_NetworkRetrying,
    Bridge_NoComponentUpdates,
    Bridge_NothingToDelete,
    Bridge_OldFileDeleted,
    Bridge_OpenBrowser,
    Bridge_RateLimited,
    Bridge_ReleaseNotesLabel,
    Bridge_ResourceExDetected,
    Bridge_RestartingElevated,
    Bridge_RetryFailedContent,
    Bridge_RetryFailedTitle,
    Bridge_RetryingFailedItems,
    Bridge_RunAsAdmin,
    Bridge_ShareCodeFailed,
    Bridge_Skip,
    Bridge_Skipped,
    Bridge_SteamFound,
    Bridge_StepLabel,
    Bridge_SwitchFallback,
    Bridge_TryFallback,
    Bridge_TryGithub,
    Bridge_UninstallConfirmStart,
    Bridge_UninstallConfirmTitle,
    Bridge_UninstallFullConfirm,
    Bridge_UninstallLightConfirm,
    Bridge_UnknownName,
    Bridge_UpgradeDone,
    Bridge_VersionArrow,
    Bridge_VersionParseFailed,
    Bridge_WillInstall,
    Bridge_WillInstallComponent,
    Bridge_WillUpgrade,
    Bridge_WillUpgradeMetaMystia,
    Button_Back,
    Button_Browse,
    Button_Cancel,
    Button_Finish,
    Button_HistoryVersions,
    Button_Next,
    Button_OK,
    Button_OpenFolder,
    Button_Recheck,
    Button_Retry,
    Button_RetryBig,
    Button_StartApply,
    Button_StartInstall,
    Button_StartUninstall,
    Button_StartUpdate,
    Component_BepInEx,
    Component_Detecting,
    Component_MetaMystia,
    Component_ResourceExample,
    ConsoleOption,
    ConsoleOptionNote,
    Diag_Cancelled,
    Diag_Config_OverCount,
    Diag_Config_OverScan,
    Diag_Config_Summary,
    Diag_Config_TooLarge,
    Diag_Item_BepInExConfig,
    Diag_Item_BepInExErrorLog,
    Diag_Item_BepInExLog,
    Diag_Item_ManagerInfo,
    Diag_Item_RollbackJournal,
    Diag_Item_Truncated,
    Diag_Item_UnityLog,
    Diag_Item_UnityOutputData,
    Diag_Item_UnityOutputRoot,
    Diag_Item_WerReport,
    Dialog_CancelLoginConfirm,
    Dialog_CancelLoginContent,
    Dialog_CancelLoginKeep,
    Dialog_CancelLoginTitle,
    Dialog_DisableBepInExTitle,
    Dialog_ExitConfirm,
    Dialog_ExitContent,
    Dialog_ExitTitle,
    Dialog_GameRunning,
    Dialog_GameRunningManage,
    Dialog_NeedNetwork,
    Dialog_PickFolderInvalid,
    Dialog_PickFolderTitle,
    Dialog_StopDownloadConfirm,
    Dialog_StopDownloadContent,
    Dialog_StopDownloadKeep,
    Dialog_StopDownloadTitle,
    Directory_ChangeHint,
    Directory_Detecting,
    Directory_StatusTitle,
    Directory_Title,
    Entry_BepInExUnknown,
    Entry_BepInExVersion,
    Entry_Disabled,
    Entry_MetaMystiaDisabled,
    Entry_MetaMystiaVersion,
    Entry_ResourceExampleDisabled,
    Entry_ResourceExampleVersion,
    Err_BackupDirFailed,
    Err_BackupFailed,
    Err_BepInExRequired,
    Err_BuildDownloadRequestFailed,
    Err_CleanupTargetFailed,
    Err_ClientDisabled,
    Err_ClientInvalid,
    Err_ConfigNotHttps,
    Err_ConfigReadFailed,
    Err_ConfigTempWriteFailed,
    Err_ConfigWriteFailed,
    Err_ConflictDisabled,
    Err_ConflictFile,
    Err_ConnectFailed,
    Err_CopyFileFailed,
    Err_CopyTempFailed,
    Err_CreateDirFailed,
    Err_CreateFileFailed,
    Err_CreateTempFileFailed,
    Err_DeleteFailed,
    Err_DeleteOldCopyFailed,
    Err_DeleteRemainsDir,
    Err_DeleteRemainsFile,
    Err_DiagCreateFailed,
    Err_DiagFinishFailed,
    Err_DiagPackDirMissing,
    Err_DiagWriteFailed,
    Err_DllNotInRelease,
    Err_DownloadChecksumFailed,
    Err_DownloadCompleted,
    Err_DownloadFileFailed,
    Err_DownloadKeyInvalid,
    Err_DownloadLoginRequired,
    Err_DownloadResume,
    Err_DownloadServiceBroken,
    Err_DownloadStalled,
    Err_DownloadThreadEnded,
    Err_ElevateFailed,
    Err_ExtractFailed,
    Err_FallbackBepInEx,
    Err_FallbackBepInExTimeout,
    Err_FallbackGithubDownload,
    Err_FallbackGithubLink,
    Err_FeatureDisabled,
    Err_FetchConfigFailed,
    Err_FileInUse,
    Err_FileNotFound,
    Err_GameRunning,
    Err_GithubHttpError,
    Err_GithubTagMismatch,
    Err_GithubTagParseFailed,
    Err_GithubUrlMissing,
    Err_HashComputeFailed,
    Err_HashCreateFailed,
    Err_HashTooLong,
    Err_HttpStatusForOp,
    Err_InstallFailedName,
    Err_InvalidComponentPath,
    Err_InvalidDisabledPath,
    Err_InvalidFileName,
    Err_InvalidVersion,
    Err_Io,
    Err_JournalParseFailed,
    Err_JournalReadFailed,
    Err_JournalReplaceFailed,
    Err_JournalWriteFailed,
    Err_JournalWriteSimple,
    Err_KeyRequestHttp,
    Err_Label_AuthorizeOrigin,
    Err_Label_GithubReleaseApi,
    Err_Label_KeysUrl,
    Err_Label_Manager,
    Err_Label_SelfUpdateApi,
    Err_Label_SelfUpdateEntry,
    Err_Label_SessionUrl,
    Err_Label_VersionConfig,
    Err_LoginAs,
    Err_LoginBuildRequestFailed,
    Err_LoginCancelled,
    Err_LoginFailedDetail,
    Err_LoginIntro,
    Err_LoginInvalid,
    Err_LoginManualUrl,
    Err_LoginOffline,
    Err_LoginParseResponseFailed,
    Err_LoginReadResponseFailed,
    Err_LoginTimeout,
    Err_LoginWaiting,
    Err_LoopbackAcceptFailed,
    Err_LoopbackListenFailed,
    Err_LoopbackReadPortFailed,
    Err_LoopbackSetOptionFailed,
    Err_ManagerDirUnknown,
    Err_ModNotInstalled,
    Err_Network,
    Err_NotInGameRoot,
    Err_Op_DownloadBepInEx,
    Err_Op_DownloadDll,
    Err_Op_DownloadFile,
    Err_Op_DownloadResourceEx,
    Err_Op_FetchDownloadConfig,
    Err_Op_FetchDownloadLink,
    Err_Op_FetchVersionInfo,
    Err_Op_Login,
    Err_Op_QueryBepInEx,
    Err_Op_QueryGithub,
    Err_Op_RequestKey,
    Err_OpenBrowserFailed,
    Err_OpenFileFailed,
    Err_OpenZipFailed,
    Err_Other,
    Err_Page_BadParams,
    Err_Page_BadRequest,
    Err_Page_Cancelled,
    Err_Page_CancelledBody,
    Err_Page_Done,
    Err_Page_DoneBody,
    Err_Page_NotFound,
    Err_Page_UnreadableBody,
    Err_Page_UnreadableTitle,
    Err_ParseConfigFailed,
    Err_ParseJsonFailed,
    Err_Permission,
    Err_PluginDirMissing,
    Err_ProcessList,
    Err_ProcessListAcquireFailed,
    Err_ProcessListReadFailed,
    Err_RandomGenerateFailed,
    Err_RandomLengthTooLong,
    Err_ReadConfigFailed,
    Err_ReadFileFailed,
    Err_ReadKeyResponseFailed,
    Err_ReadResponseFailed,
    Err_ReadThreadEnded,
    Err_ReadZipEntryFailed,
    Err_ReadZipFailed,
    Err_RenameFromFailed,
    Err_RenameOrCopyFailed,
    Err_RenameTargetFailed,
    Err_RequestFailed,
    Err_RequestInvalid,
    Err_RequestTimeout,
    Err_RestoreFailed,
    Err_ResumeInvalid,
    Err_RetryConfigInvalid,
    Err_RetryUnexpectedEnd,
    Err_RollbackDirFailed,
    Err_ScanNoVersion,
    Err_ScanVersionFromName,
    Err_ScanVersionParse,
    Err_ScopeFailed,
    Err_SelfUpdateUrlMissing,
    Err_ServerHttp,
    Err_ServerInternal,
    Err_ServerOther,
    Err_ServerTemporary,
    Err_ServerUnavailable,
    Err_ShaProviderFailed,
    Err_ShareLinkInvalid,
    Err_SlowDownload,
    Err_SlowSpeedDetail,
    Err_SourceMissing,
    Err_SsoUnreachable,
    Err_SyncFileFailed,
    Err_TicketInvalid,
    Err_TooManyKeys,
    Err_TooManyRequests,
    Err_UiReplyMismatch,
    Err_UninstallIncomplete,
    Err_UninstallRemaining,
    Err_UnsafeZipPath,
    Err_UserBlocked,
    Err_UserCancelled,
    Err_UserDeleted,
    Err_UserDisabled,
    Err_UserNotFound,
    Err_VersionInfoMalformed,
    Err_VersionUnavailable,
    Err_WriteFileFailed,
    Err_WriteTempFileFailed,
    Err_ZipSymlink,
    Error_Internal,
    Error_Log,
    Error_SelfUpdate,
    Error_SelfUpdateCanContinue,
    Error_Unknown,
    Finish_Group_ComponentStatus,
    Finish_Group_InstalledContent,
    Finish_Group_UninstallResult,
    Finish_Group_UpdateResult,
    Finish_Note_Applied,
    Finish_Note_ConsoleOnly,
    Finish_Note_Exported,
    Finish_Note_Loading,
    Finish_Note_NoChange,
    Finish_Note_StartGame,
    Finish_Note_UninstallFull,
    Finish_Note_UninstallLight,
    Finish_Title_Applied,
    Finish_Title_Exported,
    Finish_Title_Installed,
    Finish_Title_Latest,
    Finish_Title_Uninstalled,
    Finish_Title_Updated,
    Hint_GameRunning,
    Installed_List,
    Installed_NoGamePath,
    Installed_None,
    Installed_ScanFailed,
    Job_Completed,
    Job_Failed,
    Job_Row_Manager,
    Job_Waiting,
    Label_Language,
    ListSeparator,
    Log_AutoRecovered,
    Log_CleanupResidue,
    Log_ConfigDirCreateFailed,
    Log_DiagnosticsExported,
    Log_DirManyFiles,
    Log_Endpoint_BepInExPrimary,
    Log_Endpoint_FileService,
    Log_FreeSpaceLow,
    Log_InvalidDllName,
    Log_InvalidZipName,
    Log_Label_GameDir,
    Log_Label_TempDir,
    Log_PreflightUnreachable,
    Log_RollbackIncompleteDetail,
    Log_RollbackInstallDone,
    Log_RollbackInstallIncomplete,
    Log_RollbackUpgradeDone,
    Log_RollbackUpgradeIncomplete,
    Log_SelfUpdateDownloadFailed,
    Log_SelfUpdateLaunchFailed,
    Log_SelfUpdateLaunchFailedPath,
    Log_SelfUpdateWriteFailed,
    Log_SelfUpdateWriteFailedPath,
    Log_Step_DownloadFiles,
    Log_Step_FetchVersion,
    Log_Step_InstallFiles,
    Log_TempDirCreateFailed,
    Manage_Disable,
    Manage_Enable,
    Manage_NoPath,
    Manage_Note,
    Manage_Status_DisablePending,
    Manage_Status_Disabled,
    Manage_Status_EnablePending,
    Manage_Status_Enabled,
    Manage_Status_NotInstalled,
    ManagerUpdate_Confirm,
    ManagerUpdate_Content,
    ManagerUpdate_Exited,
    ManagerUpdate_Needed,
    Net_Fetching,
    Net_Loading,
    Net_NeedNetwork,
    Net_Offline,
    Net_Retrying,
    Notes_ConfirmHint,
    Notes_Empty,
    Notes_FetchFailed,
    Notes_LoadingVersion,
    Notes_Placeholder,
    Notes_Title,
    Notes_TitleWithVersion,
    Notes_Unavailable,
    OpHint_Applying,
    OpHint_CheckingUpdate,
    OpHint_Cleanup,
    OpHint_ConsoleSetting,
    OpHint_Downloading,
    OpHint_Exporting,
    OpHint_Installing,
    OpHint_Login,
    OpHint_Preparing,
    OpHint_SelfUpdate,
    OpHint_UninstallingFull,
    OpHint_UninstallingLight,
    Option_DiagnosticsNote,
    Option_DiagnosticsTitle,
    Option_InstallNote,
    Option_InstallTitle,
    Option_ManageNote,
    Option_ManageTitle,
    Option_UninstallNote,
    Option_UninstallTitle,
    Page_ApplyChanges,
    Page_ChooseComponents,
    Page_ChooseDirectory,
    Page_ChooseOperation,
    Page_ChooseUninstallMode,
    Page_ChooseUpdates,
    Page_Done,
    Page_DownloadAndInstall,
    Page_DownloadAndUpdate,
    Page_ExportDiagnostics,
    Page_ManageComponents,
    Page_ReleaseNotes,
    Page_Uninstall,
    Page_UpdateManager,
    Picker_Latest,
    Picker_MetaMystiaTitle,
    Picker_ResourceExampleTitle,
    Progress_Details,
    ResourceNote,
    Status_CancelingLogin,
    Status_OfficialSite,
    Status_StoppingDownload,
    Step_ApplySettings,
    Step_CheckUpdate,
    Step_Progress,
    Suffix_BodyDisabledParen,
    Suffix_DisabledParen,
    Suffix_FrameworkDisabledParen,
    Summary_AllLatest,
    Summary_Installs,
    Summary_Updates,
    Summary_UpdatesAndInstalls,
    Uninstall_FullNote,
    Uninstall_FullTitle,
    Uninstall_LightNote,
    Uninstall_LightTitle,
    Value_Deleted,
    Value_Kept,
    Value_NoAvailableVersion,
    Value_NotChosen,
    Value_NotInstalled,
    Value_NotUpgraded,
    Value_Unknown,
    VersionLabel_BepInExUnknown,
    VersionLabel_InstalledLatest,
    VersionLabel_NotInstalled,
    VersionLabel_Selected,
    VersionLabel_SelectedSame,
    VersionLabel_Update,
    VersionLabel_WillInstall,
    Version_Unknown,
}

/// 支持的语言；顺序即界面上语言下拉框的顺序。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    /// 简体中文
    ZhCn,
    /// 繁体中文
    ZhTw,
    /// 英语
    En,
    /// 日语
    Ja,
    /// 韩语
    Ko,
}

impl Lang {
    /// 全部语言，顺序与下拉框一致。
    pub const ALL: [Self; 5] = [Self::ZhCn, Self::ZhTw, Self::En, Self::Ja, Self::Ko];

    /// 语言自称，直接显示在下拉框里。
    pub const fn native_name(self) -> &'static str {
        match self {
            Self::ZhCn => "简体中文",
            Self::ZhTw => "繁體中文",
            Self::En => "English",
            Self::Ja => "日本語",
            Self::Ko => "한국어",
        }
    }

    /// 解析语言代码；`zh-Hans`、`zh-Hant`、`ja-JP` 等变体也可识别。
    pub fn from_code(code: &str) -> Option<Self> {
        let mut parts = code.trim().to_lowercase();
        parts = parts.replace('_', "-");
        let mut segments = parts.split('-');
        match segments.next()? {
            "zh" => {
                let rest: Vec<&str> = segments.collect();
                if rest.contains(&"hant") || rest.iter().any(|s| ["tw", "hk", "mo"].contains(s)) {
                    Some(Self::ZhTw)
                } else {
                    Some(Self::ZhCn)
                }
            }
            "en" => Some(Self::En),
            "ja" => Some(Self::Ja),
            "ko" => Some(Self::Ko),
            _ => None,
        }
    }

    fn index(self) -> u8 {
        u8::try_from(Self::ALL.iter().position(|lang| *lang == self).unwrap_or(0)).unwrap_or(0)
    }
}

/// 当前语言；`init` 前默认简体中文。
static CURRENT: AtomicU8 = AtomicU8::new(0);

/// 读取当前语言。
pub fn current() -> Lang {
    Lang::ALL
        .get(usize::from(CURRENT.load(Ordering::Relaxed)))
        .copied()
        .unwrap_or(Lang::ZhCn)
}

/// 切换语言；仅对本次运行生效，不写入磁盘。
pub fn set_current(lang: Lang) {
    CURRENT.store(lang.index(), Ordering::Relaxed);
}

/// 初始化语言：按系统语言选择，无法识别时回退英语。
pub fn init() {
    let lang = crate::platform::system_locale_name()
        .and_then(|name| Lang::from_code(&name))
        .unwrap_or(Lang::En);
    CURRENT.store(lang.index(), Ordering::Relaxed);
}

/// 取词条；当前语言缺失时回退简体中文与英语，再缺失时返回键名便于排查。
#[must_use]
pub fn t(id: TextId) -> &'static str {
    lookup(current(), id)
        .or_else(|| lookup(Lang::ZhCn, id))
        .or_else(|| lookup(Lang::En, id))
        .unwrap_or_else(|| id.name())
}

/// 取译文并替换 `{name}` 占位符。
#[must_use]
pub fn tf(id: TextId, args: &[(&str, &str)]) -> String {
    let mut text = t(id).to_string();
    for (name, value) in args {
        text = text.replace(&format!("{{{name}}}"), value);
    }
    text
}

/// 指定语言的词条；布局测量需要跨语言取文本时使用。
#[must_use]
pub fn t_in(lang: Lang, id: TextId) -> &'static str {
    lookup(lang, id).unwrap_or_else(|| id.name())
}

fn lookup(lang: Lang, id: TextId) -> Option<&'static str> {
    table(lang).get(id.name()).map(String::as_str)
}

/// 单一语言的词条表。
type Table = HashMap<String, String>;

static TABLES: [OnceLock<Table>; 5] = [const { OnceLock::new() }; 5];

fn table(lang: Lang) -> &'static Table {
    TABLES[usize::from(lang.index())].get_or_init(|| match lang {
        Lang::ZhCn => parse(include_str!("i18n/zh-CN.json")),
        Lang::ZhTw => parse(include_str!("i18n/zh-TW.json")),
        Lang::En => parse(include_str!("i18n/en.json")),
        Lang::Ja => parse(include_str!("i18n/ja.json")),
        Lang::Ko => parse(include_str!("i18n/ko.json")),
    })
}

fn parse(raw: &str) -> Table {
    serde_json::from_str(raw).expect("i18n 翻译表必须是合法的 JSON 对象")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 五种语言必须恰好覆盖全部文案键，不多不少。
    #[test]
    fn tables_cover_all_text_ids() {
        for lang in Lang::ALL {
            let table = table(lang);
            for id in TextId::ALL {
                assert!(
                    table.contains_key(id.name()),
                    "语言 {} 缺少词条 {}",
                    lang.native_name(),
                    id.name()
                );
            }
            for key in table.keys() {
                assert!(
                    TextId::ALL.iter().any(|id| id.name() == key),
                    "语言 {} 存在多余词条 {key}",
                    lang.native_name()
                );
            }
        }
    }
}
