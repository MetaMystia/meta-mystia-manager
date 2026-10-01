//! URL 主机与端口解析。

/// 取 URL 的主机名（去掉端口与用户信息）。
pub(super) fn url_host(url: &str) -> Option<&str> {
    let (_, rest) = url.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let authority = authority.rsplit('@').next().unwrap_or(authority);
    let host = authority.strip_prefix('[').map_or_else(
        || authority.split(':').next().unwrap_or(authority),
        |rest| rest.split(']').next().unwrap_or(rest),
    );

    (!host.is_empty()).then_some(host)
}

/// 从 URL 中提取用于缓存代理与连接的 host key（含显式端口）。
pub fn host_key(url: &str) -> String {
    let Some((_, rest)) = url.split_once("://") else {
        return url.to_ascii_lowercase();
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    let authority = authority.rsplit('@').next().unwrap_or(authority);

    let (host, port) = split_authority(authority);

    let host = host.to_ascii_lowercase();

    match port {
        Some(port) => format!("{host}:{port}"),
        None => host,
    }
}

/// 拆分 authority 里的主机与显式端口，兼容 `[::1]:8080` 形式的 IPv6 字面量。
fn split_authority(authority: &str) -> (&str, Option<&str>) {
    if let Some(rest) = authority.strip_prefix('[') {
        let Some((host, tail)) = rest.split_once(']') else {
            return (rest, None);
        };

        return (host, tail.strip_prefix(':').filter(|port| !port.is_empty()));
    }

    let Some((host, port)) = authority.split_once(':') else {
        return (authority, None);
    };

    (host, (!port.is_empty()).then_some(port))
}
