//! `ureq` Agent 构建、TLS 与代理接线。

use crate::config::USER_AGENT;

use super::proxy::read_system_proxy;
use std::{result::Result as StdResult, time::Duration};
use ureq::unversioned::{
    resolver::DefaultResolver,
    transport::{
        Buffers, ConnectionDetails, Connector, DefaultConnector, NextTimeout, Transport, time,
    },
};

// 超时
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const DOWNLOAD_READ_TIMEOUT: Duration = Duration::from_secs(70);
const METADATA_TIMEOUT: Duration = Duration::from_secs(30);
const RECV_RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);

fn build_config(
    url: &str,
    connect_timeout: Option<Duration>,
    global_timeout: Option<Duration>,
    body_timeout: Option<Duration>,
) -> ureq::config::Config {
    // 只启用了 native-tls；不显式指定 provider 时会尝试使用未启用的 rustls。
    let tls_config = ureq::tls::TlsConfig::builder()
        .provider(ureq::tls::TlsProvider::NativeTls)
        .root_certs(ureq::tls::RootCerts::PlatformVerifier)
        .build();

    let mut builder = ureq::Agent::config_builder()
        .tls_config(tls_config)
        .timeout_connect(connect_timeout)
        .timeout_global(global_timeout)
        .timeout_recv_response(Some(RECV_RESPONSE_TIMEOUT))
        .timeout_recv_body(body_timeout)
        // 4xx/5xx 交由 check_response_status 判定，便于读取 Retry-After
        .http_status_as_error(false)
        .user_agent(USER_AGENT);

    if let Some(proxy) = read_system_proxy(url)
        && let Ok(p) = ureq::Proxy::new(&proxy)
    {
        builder = builder.proxy(Some(p));
    }

    builder.build()
}

/// 构建带超时与系统代理的 `ureq` Agent。
pub fn build_agent_with_timeouts(
    url: &str,
    connect_timeout: Option<Duration>,
    global_timeout: Option<Duration>,
    body_timeout: Option<Duration>,
) -> ureq::Agent {
    ureq::Agent::new_with_config(build_config(
        url,
        connect_timeout,
        global_timeout,
        body_timeout,
    ))
}

/// 构建用于读取元数据（版本、配置）的 Agent。
pub fn build_metadata_agent(url: &str) -> ureq::Agent {
    build_agent_with_timeouts(
        url,
        Some(CONNECT_TIMEOUT),
        Some(METADATA_TIMEOUT),
        Some(METADATA_TIMEOUT),
    )
}

/// 只限制单次读写时长，不限制下载总时长。
pub fn build_download_agent(url: &str) -> ureq::Agent {
    let config = build_config(url, Some(CONNECT_TIMEOUT), None, None);

    ureq::Agent::with_parts(
        config,
        StallingConnector::new(DOWNLOAD_READ_TIMEOUT),
        DefaultResolver::default(),
    )
}

#[derive(Debug)]
struct StallingConnector {
    inner: DefaultConnector,
    timeout: Duration,
}

impl StallingConnector {
    fn new(timeout: Duration) -> Self {
        Self {
            inner: DefaultConnector::new(),
            timeout,
        }
    }
}

impl Connector<()> for StallingConnector {
    type Out = Box<dyn Transport>;

    fn connect(
        &self,
        details: &ConnectionDetails,
        chained: Option<()>,
    ) -> StdResult<Option<Self::Out>, ureq::Error> {
        let Some(transport) = self.inner.connect(details, chained)? else {
            return Ok(None);
        };

        Ok(Some(Box::new(StallingTransport {
            inner: transport,
            timeout: self.timeout,
        })))
    }
}

#[derive(Debug)]
struct StallingTransport {
    inner: Box<dyn Transport>,
    timeout: Duration,
}

impl StallingTransport {
    fn cap(&self, timeout: NextTimeout) -> NextTimeout {
        let after = match timeout.after {
            time::Duration::Exact(after) => time::Duration::Exact(after.min(self.timeout)),
            time::Duration::NotHappening => time::Duration::Exact(self.timeout),
        };

        NextTimeout {
            after,
            reason: timeout.reason,
        }
    }
}

impl Transport for StallingTransport {
    fn buffers(&mut self) -> &mut dyn Buffers {
        self.inner.buffers()
    }

    fn transmit_output(
        &mut self,
        amount: usize,
        timeout: NextTimeout,
    ) -> StdResult<(), ureq::Error> {
        self.inner.transmit_output(amount, self.cap(timeout))
    }

    fn await_input(&mut self, timeout: NextTimeout) -> StdResult<bool, ureq::Error> {
        self.inner.await_input(self.cap(timeout))
    }

    fn is_open(&mut self) -> bool {
        self.inner.is_open()
    }

    fn is_tls(&self) -> bool {
        self.inner.is_tls()
    }
}
