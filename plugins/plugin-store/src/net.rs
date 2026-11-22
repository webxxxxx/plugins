//! Small HTTPS surface: catalog text, package downloads, and a DNS-over-HTTPS
//! fallback for networks that poison GitHub's asset host.
//!
//! The direct path is tried first and is all a healthy network needs. Only on
//! failure does the client ask Cloudflare/Google for the real address and retry
//! with a fixed resolver; TLS still validates the original host name, so the
//! fallback cannot silently redirect the package to another origin. The host
//! verifies the package signature before install regardless.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;

const MAX_TEXT_BYTES: u64 = 4 * 1024 * 1024;
const MAX_PACKAGE_BYTES: u64 = 256 * 1024 * 1024;
const USER_AGENT: &str = concat!("plugin-store/", env!("CARGO_PKG_VERSION"));

fn builder() -> ureq::AgentBuilder {
    let mut builder = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(15))
        .timeout_read(Duration::from_secs(30))
        .timeout_write(Duration::from_secs(30))
        .redirects(6)
        .user_agent(USER_AGENT);
    if let Some(proxy) = proxy_from_env() {
        builder = builder.proxy(proxy);
    }
    builder
}

fn agent() -> ureq::Agent {
    builder().build()
}

fn proxy_from_env() -> Option<ureq::Proxy> {
    for key in [
        "HTTPS_PROXY",
        "https_proxy",
        "ALL_PROXY",
        "all_proxy",
        "HTTP_PROXY",
        "http_proxy",
    ] {
        let Ok(value) = std::env::var(key) else {
            continue;
        };
        if value.trim().is_empty() {
            continue;
        }
        if let Ok(proxy) = ureq::Proxy::new(value.trim()) {
            return Some(proxy);
        }
    }
    None
}

/// Fetch the first URL that works. Both the body and the URL that produced it
/// are returned so the UI can say where the list came from.
pub fn fetch_text(urls: &[&str]) -> Result<(String, String), String> {
    let mut errors = Vec::new();
    let direct = agent();
    for url in urls {
        match text_from(&direct, url) {
            Ok(text) => return Ok((text, (*url).to_string())),
            Err(error) => errors.push(error),
        }
    }

    let hosts = unique_hosts(urls);
    if let Some(agent) = agent_with_doh(&hosts) {
        for url in urls {
            match text_from(&agent, url) {
                Ok(text) => return Ok((text, (*url).to_string())),
                Err(error) => errors.push(error),
            }
        }
    }
    Err(errors.join("；"))
}

fn text_from(agent: &ureq::Agent, url: &str) -> Result<String, String> {
    let response = agent
        .get(url)
        .set("Accept", "text/plain, text/markdown, */*")
        .call()
        .map_err(|error| format!("{url}: {error}"))?;
    let mut reader = response.into_reader().take(MAX_TEXT_BYTES + 1);
    let mut bytes = Vec::new();
    reader
        .read_to_end(&mut bytes)
        .map_err(|error| format!("{url}: read: {error}"))?;
    if bytes.len() as u64 > MAX_TEXT_BYTES {
        return Err(format!("{url}: text is too large"));
    }
    String::from_utf8(bytes).map_err(|error| format!("{url}: not UTF-8: {error}"))
}

/// Download a package to dest, resuming from a complete dest is not needed:
/// only a fully renamed file is ever treated as cached. A .part file is always
/// restarted.
pub fn download(
    url: &str,
    dest: &Path,
    size_hint: u64,
    progress: &mut dyn FnMut(u64, Option<u64>),
    cancelled: &dyn Fn() -> bool,
) -> Result<u64, String> {
    if let Ok(metadata) = fs::metadata(dest) {
        if metadata.is_file() && metadata.len() > 0 {
            if looks_like_html(dest) {
                let _ = fs::remove_file(dest);
            } else {
                return Ok(metadata.len());
            }
        }
    }
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", parent.display()))?;
    }
    let part = part_path(dest);
    let _ = fs::remove_file(&part);

    let mut urls = Vec::new();
    if let Some(mirror) = mirror_template() {
        urls.push(apply_mirror(&mirror, url));
    }
    urls.push(url.to_string());

    let mut errors = Vec::new();
    let direct = agent();
    for candidate in &urls {
        if cancelled() {
            let _ = fs::remove_file(&part);
            return Err("安装已取消".into());
        }
        match download_one(
            &direct, candidate, &part, dest, size_hint, progress, cancelled,
        ) {
            Ok(size) => return Ok(size),
            Err(error) => {
                let _ = fs::remove_file(&part);
                errors.push(error);
            }
        }
    }

    let mut hosts = unique_hosts(&urls.iter().map(String::as_str).collect::<Vec<_>>());
    if url.contains("github.com/") {
        hosts.push("release-assets.githubusercontent.com".to_string());
        hosts.push("objects.githubusercontent.com".to_string());
    }
    if let Some(agent) = agent_with_doh(&hosts) {
        for candidate in &urls {
            if cancelled() {
                let _ = fs::remove_file(&part);
                return Err("安装已取消".into());
            }
            match download_one(
                &agent, candidate, &part, dest, size_hint, progress, cancelled,
            ) {
                Ok(size) => return Ok(size),
                Err(error) => {
                    let _ = fs::remove_file(&part);
                    errors.push(error);
                }
            }
        }
    }

    Err(if errors.is_empty() {
        "没有可用的下载地址".into()
    } else {
        errors.join("；")
    })
}

fn download_one(
    agent: &ureq::Agent,
    url: &str,
    part: &Path,
    dest: &Path,
    size_hint: u64,
    progress: &mut dyn FnMut(u64, Option<u64>),
    cancelled: &dyn Fn() -> bool,
) -> Result<u64, String> {
    let response = agent
        .get(url)
        .set("Accept", "application/octet-stream, */*")
        .call()
        .map_err(|error| format!("{url}: {error}"))?;
    let total = response
        .header("content-length")
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0);
    let mut reader = response.into_reader();
    let mut file = File::create(part).map_err(|error| format!("{}: {error}", part.display()))?;
    let mut downloaded = 0u64;
    let mut buffer = [0u8; 64 * 1024];

    loop {
        if cancelled() {
            let _ = fs::remove_file(part);
            return Err("安装已取消".into());
        }
        let read = reader
            .read(&mut buffer)
            .map_err(|error| format!("{url}: read: {error}"))?;
        if read == 0 {
            break;
        }
        downloaded = downloaded.saturating_add(read as u64);
        if downloaded > MAX_PACKAGE_BYTES {
            let _ = fs::remove_file(part);
            return Err(format!(
                "{url}: 包超过 {} MiB",
                MAX_PACKAGE_BYTES / 1024 / 1024
            ));
        }
        file.write_all(&buffer[..read])
            .map_err(|error| format!("{}: {error}", part.display()))?;
        progress(downloaded, total.or((size_hint > 0).then_some(size_hint)));
    }
    file.flush()
        .map_err(|error| format!("{}: {error}", part.display()))?;
    file.sync_all()
        .map_err(|error| format!("{}: {error}", part.display()))?;
    drop(file);

    if downloaded == 0 {
        let _ = fs::remove_file(part);
        return Err(format!("{url}: 下载到 0 字节"));
    }
    if let Some(expected) = total {
        if downloaded != expected {
            let _ = fs::remove_file(part);
            return Err(format!(
                "{url}: 下载不完整（{downloaded} / {expected} 字节）"
            ));
        }
    }
    if looks_like_html(part) {
        let _ = fs::remove_file(part);
        return Err(format!(
            "{url}: 返回的是 HTML 而不是 .spk（下载地址需要代理或 DNS 回退）"
        ));
    }
    fs::rename(part, dest).map_err(|error| format!("{}: {error}", dest.display()))?;
    Ok(downloaded)
}

fn looks_like_html(path: &Path) -> bool {
    let Ok(mut file) = File::open(path) else {
        return false;
    };
    let mut head = [0u8; 512];
    let Ok(read) = file.read(&mut head) else {
        return false;
    };
    let head = head[..read].to_ascii_lowercase();
    head.windows(5).any(|window| window == b"<html")
        || head.windows(9).any(|window| window == b"<!doctype")
}

fn part_path(dest: &Path) -> PathBuf {
    PathBuf::from(format!("{}.part", dest.display()))
}

fn mirror_template() -> Option<String> {
    std::env::var("PLUGIN_STORE_MIRROR")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn apply_mirror(template: &str, url: &str) -> String {
    if template.contains("{url}") {
        template.replace("{url}", url)
    } else if template.ends_with('/') {
        format!("{template}{url}")
    } else {
        format!("{template}/{url}")
    }
}

fn unique_hosts(urls: &[&str]) -> Vec<String> {
    let mut hosts = Vec::new();
    for url in urls {
        if let Some(host) = host_of(url) {
            if !hosts.iter().any(|candidate| candidate == &host) {
                hosts.push(host);
            }
        }
    }
    hosts
}

fn host_of(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let authority = rest.split('/').next()?;
    let authority = authority.rsplit('@').next()?;
    let host = authority.split(':').next()?.trim();
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

fn agent_with_doh(hosts: &[String]) -> Option<ureq::Agent> {
    let mut resolved: HashMap<String, Vec<SocketAddr>> = HashMap::new();
    for host in hosts {
        if host.parse::<IpAddr>().is_ok() {
            continue;
        }
        if let Ok(addresses) = doh_resolve(host) {
            if !addresses.is_empty() {
                resolved.insert(host.clone(), addresses);
            }
        }
    }
    if resolved.is_empty() {
        return None;
    }
    Some(
        builder()
            .resolver(move |netloc: &str| {
                // ureq hands the resolver "host:port", not a bare host.
                let (host, port) = match netloc.rsplit_once(':') {
                    Some((host, port)) => (host, port.parse::<u16>().unwrap_or(443)),
                    None => (netloc, 443),
                };
                if let Some(addresses) = resolved.get(host) {
                    return Ok(addresses
                        .iter()
                        .map(|address| SocketAddr::new(address.ip(), port))
                        .collect());
                }
                netloc
                    .to_socket_addrs()
                    .map(|addresses| addresses.collect())
            })
            .build(),
    )
}

fn doh_resolve(host: &str) -> Result<Vec<SocketAddr>, String> {
    let mut errors = Vec::new();
    for endpoint in [
        format!("https://cloudflare-dns.com/dns-query?name={host}&type=A"),
        format!("https://dns.google/resolve?name={host}&type=A"),
    ] {
        match doh_query(&endpoint) {
            Ok(addresses) if !addresses.is_empty() => return Ok(addresses),
            Ok(_) => errors.push(format!("{endpoint}: 没有 A 记录")),
            Err(error) => errors.push(error),
        }
    }
    Err(errors.join("；"))
}

fn doh_query(url: &str) -> Result<Vec<SocketAddr>, String> {
    let response = agent()
        .get(url)
        .set("Accept", "application/dns-json")
        .call()
        .map_err(|error| format!("{url}: {error}"))?;
    let mut body = String::new();
    response
        .into_reader()
        .take(MAX_TEXT_BYTES)
        .read_to_string(&mut body)
        .map_err(|error| format!("{url}: read: {error}"))?;
    let value: Value =
        serde_json::from_str(&body).map_err(|error| format!("{url}: JSON: {error}"))?;
    let mut addresses = Vec::new();
    if let Some(answers) = value.get("Answer").and_then(Value::as_array) {
        for answer in answers {
            if answer.get("type").and_then(Value::as_u64) != Some(1) {
                continue;
            }
            if let Some(ip) = answer
                .get("data")
                .and_then(Value::as_str)
                .and_then(|text| text.parse::<Ipv4Addr>().ok())
            {
                addresses.push(SocketAddr::new(IpAddr::V4(ip), 443));
            }
        }
    }
    Ok(addresses)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_hosts_and_applies_mirrors() {
        assert_eq!(
            host_of("https://github.com/webxxxxx/plugins/releases/download/v1/x.spk"),
            Some("github.com".into())
        );
        assert_eq!(
            host_of("https://user:pass@example.com:443/x"),
            Some("example.com".into())
        );
        assert_eq!(
            apply_mirror("https://gh.example/{url}", "https://github.com/x"),
            "https://gh.example/https://github.com/x"
        );
        assert_eq!(
            apply_mirror("https://gh.example/", "https://github.com/x"),
            "https://gh.example/https://github.com/x"
        );
    }

    #[test]
    fn downloads_an_http_file_atomically() {
        use std::io::{Read as _, Write as _};
        use std::net::TcpListener;
        use std::thread;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let body = b"spk-payload".to_vec();
        let server_body = body.clone();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 1024];
            let _ = stream.read(&mut request);
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                server_body.len()
            )
            .unwrap();
            stream.write_all(&server_body).unwrap();
        });

        let dest = std::env::temp_dir().join(format!(
            "plugin-store-download-{}-{}.spk",
            std::process::id(),
            address.port()
        ));
        let _ = fs::remove_file(&dest);
        let part = part_path(&dest);
        let _ = fs::remove_file(&part);

        let mut progress = |_, _| {};
        let cancelled = || false;
        let size = download(
            &format!("http://{address}/package.spk"),
            &dest,
            0,
            &mut progress,
            &cancelled,
        )
        .unwrap();
        assert_eq!(size, body.len() as u64);
        assert_eq!(fs::read(&dest).unwrap(), body);
        assert!(!part.exists());

        let _ = fs::remove_file(dest);
        server.join().unwrap();
    }

    #[test]
    fn recognises_an_html_error_page() {
        let path =
            std::env::temp_dir().join(format!("plugin-store-html-{}.tmp", std::process::id()));
        std::fs::write(&path, b"<!DOCTYPE html><html>blocked</html>").unwrap();
        assert!(looks_like_html(&path));
        std::fs::write(&path, b"ustar\0\0").unwrap();
        assert!(!looks_like_html(&path));
        let _ = std::fs::remove_file(path);
    }
}
