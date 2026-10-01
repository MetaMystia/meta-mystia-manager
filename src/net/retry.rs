//! 网络请求的退避重试策略。

use std::time::Duration;

/// 指数退避重试策略。
pub struct RetryConfig {
    /// 最大重试次数（至少 1）
    pub attempts: usize,
    /// 基础延迟（秒）
    pub base_delay_secs: u64,
    /// 延迟上限（秒）
    pub max_delay_secs: u64,
    /// 指数倍数（例如 2.0 表示每次延迟翻倍）
    pub multiplier: f64,
}

impl RetryConfig {
    /// 常规网络请求：3 次尝试，5s 起步，上限 15s。
    pub const fn network() -> Self {
        Self {
            attempts: 3,
            base_delay_secs: 5,
            max_delay_secs: 15,
            multiplier: 2.0,
        }
    }

    /// GitHub Release 说明：2 次尝试，固定 5s。
    pub const fn github_release_note() -> Self {
        Self {
            attempts: 2,
            base_delay_secs: 5,
            max_delay_secs: 5,
            multiplier: 1.0,
        }
    }

    /// 卸载重试：3 次尝试，10s 起步，上限 60s。
    pub const fn uninstall() -> Self {
        Self {
            attempts: 3,
            base_delay_secs: 10,
            max_delay_secs: 60,
            multiplier: 2.0,
        }
    }

    /// 第 `attempt` 次重试（从 0 开始）前的等待时长。
    ///
    /// 即 `base_delay_secs * multiplier^attempt`，并以 `max_delay_secs` 为上限。
    #[allow(
        clippy::cast_precision_loss,
        reason = "退避时长最多几十秒，f64 足以精确表示"
    )]
    pub fn delay(&self, attempt: usize) -> Duration {
        let exponent = i32::try_from(attempt).unwrap_or(i32::MAX);
        let secs = (self.base_delay_secs as f64 * self.multiplier.powi(exponent))
            .min(self.max_delay_secs as f64)
            .ceil();

        Duration::from_secs_f64(secs)
    }
}
