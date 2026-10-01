//! 底层 HTTP 传输：Agent 构建、系统代理与 URL 解析。

pub mod agent;
pub mod proxy;
pub mod url;

pub use agent::{build_agent_with_timeouts, build_download_agent, build_metadata_agent};
pub use url::host_key;
