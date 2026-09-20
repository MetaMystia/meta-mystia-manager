use crate::config::UninstallMode;

use clap::{ArgGroup, Parser, ValueEnum};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(name = env!("CARGO_PKG_NAME"))]
#[command(version = env!("CARGO_PKG_VERSION"))]
#[command(about = env!("CARGO_PKG_DESCRIPTION"), long_about = None)]
#[command(group(
    ArgGroup::new("operation")
        .args(&["install", "upgrade", "uninstall"])
))]
#[allow(
    clippy::struct_excessive_bools,
    reason = "命令行开关，每个布尔字段对应一个 flag"
)]
pub struct Cli {
    /// Specify the game root directory path (default: auto-detect or current directory).
    #[arg(short = 'p', long = "path", value_name = "PATH")]
    pub path: Option<PathBuf>,

    /// Skip automatic self-update check before running operations.
    /// On successful update: exits with code 100 and prints the new executable filename.
    #[arg(long)]
    pub skip_self_update: bool,

    /// Suppress descriptive output (errors still shown).
    #[arg(short = 'q', long)]
    pub quiet: bool,

    /// Install MetaMystia Mod.
    #[arg(short = 'i', long)]
    pub install: bool,

    /// Do not install ResourceExample ZIP (default: install).
    #[arg(long = "no-resourceex", requires = "install")]
    pub no_resourceex: bool,

    /// Show BepInEx console on game startup (default: false).
    #[arg(long = "with-bepinex-console", requires = "install")]
    pub with_bepinex_console: bool,

    /// Specify the MetaMystia DLL version to install.
    #[arg(long = "dll-version", value_name = "VERSION", requires = "install")]
    pub dll_version: Option<String>,

    /// Specify the ResourceExample version to install.
    #[arg(
        long = "resourceex-version",
        value_name = "VERSION",
        requires = "install"
    )]
    pub resourceex_version: Option<String>,

    /// Upgrade MetaMystia Mod.
    #[arg(short = 'u', long)]
    pub upgrade: bool,

    /// Uninstall MetaMystia Mod.
    #[arg(short = 'U', long)]
    pub uninstall: bool,

    /// Uninstall mode: light (remove MetaMystia only) or full (remove all mods).
    #[arg(long, value_enum, default_value = "light", requires = "uninstall")]
    pub mode: UninstallModeArg,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum UninstallModeArg {
    /// Remove MetaMystia files only (keep BepInEx and other mods)
    Light,
    /// Remove all mod-related files (restore to vanilla game)
    Full,
}

impl From<UninstallModeArg> for UninstallMode {
    fn from(mode: UninstallModeArg) -> Self {
        match mode {
            UninstallModeArg::Light => Self::Light,
            UninstallModeArg::Full => Self::Full,
        }
    }
}

#[derive(Clone, Debug)]
pub struct InstallConfig {
    pub install_resourceex: bool,
    pub show_bepinex_console: bool,
    pub dll_version: Option<String>,
    pub resourceex_version: Option<String>,
}

#[derive(Clone, Debug)]
pub struct CliConfig {
    pub game_path: Option<PathBuf>,
    pub operation: CliOperation,
    pub quiet: bool,
    pub skip_self_update: bool,
}

#[derive(Clone, Debug)]
pub enum CliOperation {
    Install(InstallConfig),
    Upgrade,
    Uninstall(UninstallMode),
}

impl Cli {
    /// 将命令行参数转换为 `CliConfig`
    pub fn to_config(&self) -> Option<CliConfig> {
        let operation = if self.install {
            Some(CliOperation::Install(InstallConfig {
                install_resourceex: !self.no_resourceex,
                show_bepinex_console: self.with_bepinex_console,
                dll_version: self.dll_version.clone(),
                resourceex_version: self.resourceex_version.clone(),
            }))
        } else if self.upgrade {
            Some(CliOperation::Upgrade)
        } else if self.uninstall {
            Some(CliOperation::Uninstall(self.mode.into()))
        } else {
            None
        };

        operation.map(|op| CliConfig {
            game_path: self.path.clone(),
            operation: op,
            quiet: self.quiet,
            skip_self_update: self.skip_self_update,
        })
    }
}
