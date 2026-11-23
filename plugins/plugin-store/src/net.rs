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

use serde_json::{json, Value};

const MAX_TEXT_BYTES: u64 = 4 * 1024 * 1024;
pub(crate) const MAX_DOWNLOAD_BYTES: u64 = 256 * 1024 * 1024;
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
        // A mirror may cache a branch for days (jsDelivr: seven). Ask every
        // edge to revalidate; the catalog is small and correctness beats it.
        .set("Cache-Control", "no-cache")
        .set("Pragma", "no-cache")
        .set("Accept", "text/plain, text/markdown, */*")
        .call()
        .map_err(|error| format!("{url}: {error}"))?;
    read_body(response.into_reader(), url)
}

fn read_body(reader: impl Read, url: &str) -> Result<String, String> {
    let mut reader = reader.take(MAX_TEXT_BYTES + 1);
    let mut bytes = Vec::new();
    reader
        .read_to_end(&mut bytes)
        .map_err(|error| format!("{url}: read: {error}"))?;
    if bytes.len() as u64 > MAX_TEXT_BYTES {
        return Err(format!("{url}: text is too large"));
    }
    String::from_utf8(bytes).map_err(|error| format!("{url}: not UTF-8: {error}"))
}

/// ETag + body of the last successful release lookup. GitHub answers a
/// matching `If-None-Match` with 304, which does not count against the quota,
/// so a repeated check costs nothing after the first one.
const RELEASE_CACHE: &str = "release-cache.json";
/// Optional read-only PAT. It lives next to `catalog.md` in the plugin data
/// directory so a service install does not need a machine-wide environment
/// variable; the `/api/v1` face never sees it.
pub(crate) const TOKEN_FILE: &str = "github-token";
pub(crate) const MAX_TOKEN_BYTES: u64 = 512;

/// Read a read-only GitHub token: the plugin's own config file, then
/// `GITHUB_TOKEN`, then `GH_TOKEN`, then `<data_dir>/github-token`. An
/// authenticated request gets 5000 requests per hour instead of the anonymous
/// 60, which is the difference between a shared NAT address working and not.
/// The value is never logged.
pub(crate) fn github_token(configured: Option<&str>, data_dir: &Path) -> Option<String> {
    token_choice(
        configured,
        std::env::var("GITHUB_TOKEN").ok(),
        std::env::var("GH_TOKEN").ok(),
        data_dir,
    )
}

fn token_choice(
    configured: Option<&str>,
    primary: Option<String>,
    secondary: Option<String>,
    data_dir: &Path,
) -> Option<String> {
    configured
        .and_then(clean_token)
        .or_else(|| primary.as_deref().and_then(clean_token))
        .or_else(|| secondary.as_deref().and_then(clean_token))
        .or_else(|| {
            let path = data_dir.join(TOKEN_FILE);
            if fs::metadata(&path).ok()?.len() > MAX_TOKEN_BYTES {
                return None;
            }
            fs::read_to_string(&path)
                .ok()
                .and_then(|text| clean_token(&text))
        })
}

pub(crate) fn clean_token(value: &str) -> Option<String> {
    let token = value.trim();
    if token.is_empty()
        || token.len() as u64 > MAX_TOKEN_BYTES
        || token.chars().any(char::is_control)
    {
        return None;
    }
    Some(token.to_string())
}

struct CachedRelease {
    url: String,
    etag: String,
    body: String,
    /// When the 200 that produced this body arrived (unix seconds). A cache
    /// written by an older build has no stamp and simply misses the TTL.
    stored_at: u64,
}

fn cache_file(data_dir: &Path) -> PathBuf {
    data_dir.join(RELEASE_CACHE)
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn read_cache(path: &Path, url: &str) -> Option<CachedRelease> {
    let text = fs::read_to_string(path).ok()?;
    let value: Value = serde_json::from_str(&text).ok()?;
    let cached_url = value.get("url")?.as_str()?;
    // The cache is only valid for the repository that produced it; `repo` is a
    // call argument, so a second repository must not reuse this ETag.
    if cached_url != url {
        return None;
    }
    Some(CachedRelease {
        url: cached_url.to_string(),
        etag: value.get("etag")?.as_str()?.to_string(),
        body: value.get("body")?.as_str()?.to_string(),
        stored_at: value.get("stored_at").and_then(Value::as_u64).unwrap_or(0),
    })
}

fn write_cache(path: &Path, cached: &CachedRelease) {
    let Some(parent) = path.parent() else {
        return;
    };
    if fs::create_dir_all(parent).is_err() {
        return;
    }
    let temporary = path.with_extension("json.tmp");
    let value = json!({
        "url": cached.url,
        "etag": cached.etag,
        "body": cached.body,
        "stored_at": cached.stored_at,
    });
    let Ok(text) = serde_json::to_string(&value) else {
        return;
    };
    if fs::write(&temporary, text).is_ok() && fs::rename(&temporary, path).is_err() {
        let _ = fs::remove_file(&temporary);
    }
}

/// Fetch one JSON document (the GitHub API) as text. Same DoH fallback as
/// [`fetch_text`], but a rate-limit or a missing repo is reported as itself
/// instead of being retried against another resolver.
///
/// With `data_dir` the last body and its ETag are reused through
/// `If-None-Match`; with `auth` the request is authenticated. A 304 is not a
/// failure and does not consume quota.
pub(crate) fn fetch_json(
    url: &str,
    auth: Option<&str>,
    data_dir: Option<&Path>,
    cache_ttl: Duration,
) -> Result<String, String> {
    let cache = data_dir.map(cache_file);
    let cached = cache.as_deref().and_then(|path| read_cache(path, url));
    // A fresh body answers without touching the network at all. That is what
    // keeps a burst of "check for updates" clicks (or a host poller) from
    // spending quota even before GitHub can answer 304.
    if let Some(cached) = &cached {
        if !cache_ttl.is_zero() && now_secs().saturating_sub(cached.stored_at) < cache_ttl.as_secs()
        {
            return Ok(cached.body.clone());
        }
    }
    let etag = cached.as_ref().map(|cached| cached.etag.as_str());

    let direct = agent();
    let outcome = match json_from(&direct, url, auth, etag) {
        Ok(reply) => Ok(reply),
        // A 403/404 is an answer, not a broken resolver: retrying it through
        // DoH would burn another request without changing anything.
        Err(JsonFailure::Status(message)) => Err(message),
        Err(JsonFailure::Transport(first)) => match agent_with_doh(&unique_hosts(&[url])) {
            Some(agent) => match json_from(&agent, url, auth, etag) {
                Ok(reply) => Ok(reply),
                Err(JsonFailure::Status(message)) => Err(message),
                Err(JsonFailure::Transport(second)) => Err(format!("{first}；{second}")),
            },
            None => Err(first),
        },
    };

    match outcome {
        Ok(JsonReply::NotModified) => match cached {
            Some(cached) => Ok(cached.body),
            None => Err("GitHub 返回 304，但本地没有缓存".into()),
        },
        Ok(JsonReply::Body { body, etag }) => {
            if let (Some(path), Some(etag)) = (cache.as_deref(), etag) {
                write_cache(
                    path,
                    &CachedRelease {
                        url: url.to_string(),
                        etag,
                        body: body.clone(),
                        stored_at: now_secs(),
                    },
                );
            }
            Ok(body)
        }
        Err(message) => Err(token_hint(message, auth, data_dir)),
    }
}

/// Append the one action that fixes the anonymous quota, so the console error
/// says where the token goes instead of only asking the user to wait.
fn token_hint(message: String, auth: Option<&str>, data_dir: Option<&Path>) -> String {
    if auth.is_some() || !message.contains("限流") {
        return message;
    }
    let location = data_dir
        .map(|dir| dir.join(TOKEN_FILE).display().to_string())
        .unwrap_or_else(|| TOKEN_FILE.to_string());
    format!(
        "{message}；把只读 GitHub token 填进配置（`plugin editconfig plugin-store` 的 github_token，\
         或写 {location}、设置 GITHUB_TOKEN）可把配额提到 5000 次/小时"
    )
}

enum JsonFailure {
    Status(String),
    Transport(String),
}

enum JsonReply {
    Body { body: String, etag: Option<String> },
    NotModified,
}

fn json_from(
    agent: &ureq::Agent,
    url: &str,
    auth: Option<&str>,
    etag: Option<&str>,
) -> Result<JsonReply, JsonFailure> {
    let mut request = agent
        .get(url)
        .set("Accept", "application/vnd.github+json")
        .set("X-GitHub-Api-Version", "2022-11-28");
    if let Some(token) = auth {
        request = request.set("Authorization", &format!("Bearer {token}"));
    }
    if let Some(etag) = etag {
        request = request.set("If-None-Match", etag);
    }
    let response = match request.call() {
        Ok(response) => response,
        Err(ureq::Error::Status(status, response)) => {
            let remaining = response.header("x-ratelimit-remaining").map(str::to_string);
            let reset = response.header("x-ratelimit-reset").map(str::to_string);
            let mut body = String::new();
            let _ = response
                .into_reader()
                .take(4 * 1024)
                .read_to_string(&mut body);
            let message = serde_json::from_str::<Value>(&body).ok().and_then(|value| {
                value
                    .get("message")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            });
            return Err(JsonFailure::Status(github_http_error(
                status,
                message.as_deref(),
                remaining.as_deref(),
                reset.as_deref(),
                auth.is_some(),
            )));
        }
        Err(ureq::Error::Transport(error)) => {
            return Err(JsonFailure::Transport(format!("{url}: {error}")))
        }
    };
    if response.status() == 304 {
        return Ok(JsonReply::NotModified);
    }
    let etag = response.header("etag").map(str::to_string);
    let body = read_body(response.into_reader(), url).map_err(JsonFailure::Transport)?;
    Ok(JsonReply::Body { body, etag })
}

fn github_http_error(
    status: u16,
    message: Option<&str>,
    remaining: Option<&str>,
    reset: Option<&str>,
    authenticated: bool,
) -> String {
    let message = message
        .map(str::trim)
        .filter(|message| !message.is_empty())
        .unwrap_or("没有说明");
    let rate_limited = status == 429
        || remaining.and_then(|value| value.trim().parse::<u64>().ok()) == Some(0)
        || message.to_ascii_lowercase().contains("rate limit")
        || message.to_ascii_lowercase().contains("abuse");
    if rate_limited {
        let mut text = format!("GitHub API 限流（HTTP {status}）：{message}");
        if let Some(remaining) = remaining.map(str::trim).filter(|value| !value.is_empty()) {
            text.push_str(&format!("；本小时剩余 {remaining} 次"));
        }
        if let Some(reset) = reset.map(str::trim).filter(|value| !value.is_empty()) {
            text.push_str(&format!("；配额重置于 {reset}（Unix 时间）"));
        }
        if authenticated {
            text.push_str("；GitHub 认证请求每小时 5000 次，稍后重试或换出口 IP 即可");
        } else {
            text.push_str("；GitHub 匿名请求每小时 60 次，稍后重试或换出口 IP 即可");
        }
        text
    } else if status == 404 {
        format!("GitHub API 返回 404：仓库或 release 不存在（{message}）")
    } else if status == 401 {
        format!(
            "GitHub token 无效或被吊销（HTTP 401）：{message}；用 `plugin editconfig plugin-store` 检查 github_token，或看 GITHUB_TOKEN / GH_TOKEN"
        )
    } else if status == 403 {
        format!("GitHub API 拒绝访问（HTTP 403）：{message}")
    } else {
        format!("GitHub API 返回 HTTP {status}：{message}")
    }
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
    let mirror = mirror_template();
    download_with(url, dest, size_hint, mirror.as_deref(), progress, cancelled)
}

/// The same download without the `PLUGIN_STORE_MIRROR` hop. The self-upgrade
/// path uses this: a mirror may serve the packages, but it must never choose
/// the bytes of the daemon that verifies it.
pub fn download_direct(
    url: &str,
    dest: &Path,
    size_hint: u64,
    progress: &mut dyn FnMut(u64, Option<u64>),
    cancelled: &dyn Fn() -> bool,
) -> Result<u64, String> {
    download_with(url, dest, size_hint, None, progress, cancelled)
}

fn download_with(
    url: &str,
    dest: &Path,
    size_hint: u64,
    mirror: Option<&str>,
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

    let urls = candidates(url, mirror);

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
        if downloaded > MAX_DOWNLOAD_BYTES {
            let _ = fs::remove_file(part);
            return Err(format!(
                "{url}: 文件超过 {} MiB",
                MAX_DOWNLOAD_BYTES / 1024 / 1024
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

    // The last chunk may complete while the user is deleting the job.
    if cancelled() {
        let _ = fs::remove_file(part);
        return Err("安装已取消".into());
    }
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
            "{url}: 返回的是 HTML 而不是文件（下载地址需要代理或 DNS 回退）"
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

pub(crate) fn part_path(dest: &Path) -> PathBuf {
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

/// The URL list in try order. `None` is the upgrade path: the original URL is
/// the only candidate, so a mirror cannot swap the daemon's bytes.
fn candidates(url: &str, mirror: Option<&str>) -> Vec<String> {
    let mut urls = Vec::new();
    if let Some(mirror) = mirror.map(str::trim).filter(|mirror| !mirror.is_empty()) {
        urls.push(apply_mirror(mirror, url));
    }
    urls.push(url.to_string());
    urls
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

    #[test]
    fn explains_rate_limiting_and_missing_repos() {
        let limited = github_http_error(
            403,
            Some("API rate limit exceeded for 1.2.3.4."),
            Some("0"),
            Some("1763800000"),
            false,
        );
        assert!(limited.contains("限流"), "{limited}");
        assert!(limited.contains("API rate limit exceeded"), "{limited}");
        assert!(limited.contains("1763800000"), "{limited}");
        assert!(limited.contains("匿名请求每小时 60 次"), "{limited}");

        // The same headers with a token in place say the 5000/hour budget is
        // the one that ran out, so the hint does not tell the user to add one.
        let authed = github_http_error(
            403,
            Some("API rate limit exceeded"),
            Some("0"),
            Some("1763800000"),
            true,
        );
        assert!(authed.contains("认证请求每小时 5000 次"), "{authed}");

        let missing = github_http_error(404, Some("Not Found"), None, None, false);
        assert!(missing.contains("404"), "{missing}");
        assert!(missing.contains("Not Found"), "{missing}");

        // A plain refusal is not a rate limit, even though GitHub uses 403 for
        // both.
        let forbidden = github_http_error(403, Some("Forbidden"), None, Some("1763800000"), false);
        assert!(!forbidden.contains("限流"), "{forbidden}");
        assert!(forbidden.contains("拒绝访问"), "{forbidden}");
    }

    #[test]
    fn candidates_list_the_mirror_first_only_when_one_is_given() {
        assert_eq!(
            candidates("https://github.com/x", Some("https://gh.example/{url}")),
            vec![
                "https://gh.example/https://github.com/x".to_string(),
                "https://github.com/x".to_string()
            ]
        );
        assert_eq!(
            candidates("https://github.com/x", None),
            vec!["https://github.com/x".to_string()]
        );
    }

    #[test]
    fn maps_a_real_rate_limit_response_to_a_readable_error() {
        use std::io::{Read as _, Write as _};
        use std::net::TcpListener;
        use std::thread;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 2048];
            let _ = stream.read(&mut request);
            let body = r#"{"message":"API rate limit exceeded for 1.2.3.4."}"#;
            write!(
                stream,
                "HTTP/1.1 403 Forbidden\r\nContent-Type: application/json\r\nContent-Length: {}\r\nx-ratelimit-remaining: 0\r\nx-ratelimit-reset: 1763800000\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        });

        let error = fetch_json(
            &format!("http://{address}/repos/webxxxxx/plugin-manager/releases/latest"),
            None,
            None,
            Duration::ZERO,
        )
        .unwrap_err();

        assert!(error.contains("限流"), "{error}");
        assert!(error.contains("API rate limit exceeded"), "{error}");
        assert!(error.contains("本小时剩余 0 次"), "{error}");
        assert!(error.contains("1763800000"), "{error}");
        server.join().unwrap();
    }

    /// Read one request head from a test server connection.
    fn request_head(stream: &mut std::net::TcpStream) -> String {
        use std::io::Read as _;

        let mut bytes = Vec::new();
        let mut buffer = [0u8; 1024];
        while !bytes.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = stream.read(&mut buffer).unwrap();
            if read == 0 {
                break;
            }
            bytes.extend_from_slice(&buffer[..read]);
        }
        String::from_utf8_lossy(&bytes).into_owned()
    }

    #[test]
    fn an_etag_turns_a_repeated_check_into_a_cached_304() {
        use std::io::Write as _;
        use std::net::TcpListener;
        use std::thread;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut first, _) = listener.accept().unwrap();
            let head = request_head(&mut first).to_ascii_lowercase();
            assert!(!head.contains("if-none-match"), "{head}");
            let body = r#"{"tag_name":"v0.1.5"}"#;
            write!(
                first,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nETag: \"rel-1\"\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();

            let (mut second, _) = listener.accept().unwrap();
            let head = request_head(&mut second).to_ascii_lowercase();
            assert!(head.contains("if-none-match: \"rel-1\""), "{head}");
            write!(
                second,
                "HTTP/1.1 304 Not Modified\r\nETag: \"rel-1\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )
            .unwrap();
        });

        let dir = std::env::temp_dir().join(format!("plugin-store-etag-{}", address.port()));
        let _ = fs::remove_dir_all(&dir);
        let url = format!("http://{address}/repos/webxxxxx/plugin-manager/releases/latest");
        let first = fetch_json(&url, None, Some(&dir), Duration::ZERO).unwrap();
        let second = fetch_json(&url, None, Some(&dir), Duration::ZERO).unwrap();
        assert_eq!(first, second);
        assert!(dir.join(RELEASE_CACHE).is_file());
        server.join().unwrap();
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_fresh_cache_answers_without_a_request() {
        let dir = std::env::temp_dir().join(format!("plugin-store-ttl-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let url = "http://127.0.0.1:1/repos/webxxxxx/plugin-manager/releases/latest";
        fs::write(
            cache_file(&dir),
            json!({
                "url": url,
                "etag": "\"rel-9\"",
                "body": r#"{"tag_name":"v9.9.9"}"#,
                "stored_at": now_secs(),
            })
            .to_string(),
        )
        .unwrap();

        // Inside the window nothing is dialled; the body comes off disk.
        let body = fetch_json(url, None, Some(&dir), Duration::from_secs(600)).unwrap();
        assert!(body.contains("v9.9.9"), "{body}");

        // A zero window means "always revalidate", and this endpoint refuses.
        assert!(fetch_json(url, None, Some(&dir), Duration::ZERO).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_configured_token_is_sent_as_a_bearer_header() {
        use std::io::Write as _;
        use std::net::TcpListener;
        use std::thread;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let head = request_head(&mut stream).to_ascii_lowercase();
            assert!(head.contains("authorization: bearer read-only"), "{head}");
            let body = r#"{"tag_name":"v0.1.5"}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        });

        let url = format!("http://{address}/repos/webxxxxx/plugin-manager/releases/latest");
        let body = fetch_json(&url, Some("read-only"), None, Duration::ZERO).unwrap();
        assert!(body.contains("v0.1.5"), "{body}");
        server.join().unwrap();
    }

    #[test]
    fn a_token_file_is_read_trimmed_and_never_beats_the_environment() {
        let dir = std::env::temp_dir().join(format!("plugin-store-token-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(TOKEN_FILE), b"  file-token\n").unwrap();

        assert_eq!(
            token_choice(None, None, None, &dir).as_deref(),
            Some("file-token")
        );
        assert_eq!(
            token_choice(None, Some("env-token".into()), None, &dir).as_deref(),
            Some("env-token")
        );
        assert_eq!(
            token_choice(None, Some("   ".into()), Some("gh-token".into()), &dir).as_deref(),
            Some("gh-token")
        );
        // The config file is the documented knob, so it wins over the
        // environment and over the legacy token file.
        assert_eq!(
            token_choice(Some("config-token"), Some("env-token".into()), None, &dir).as_deref(),
            Some("config-token")
        );
        // A control character invalidates the value instead of reaching a header.
        assert_eq!(
            token_choice(None, Some("bad\nx".into()), None, &dir).as_deref(),
            Some("file-token")
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_rate_limited_answer_points_at_the_token_file() {
        use std::io::Write as _;
        use std::net::TcpListener;
        use std::thread;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let body = r#"{"message":"API rate limit exceeded for 1.2.3.4."}"#;
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let _ = request_head(&mut stream);
            write!(
                stream,
                "HTTP/1.1 403 Forbidden\r\nContent-Type: application/json\r\nContent-Length: {}\r\nx-ratelimit-remaining: 0\r\nx-ratelimit-reset: 1795401688\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        });

        let dir = std::env::temp_dir().join(format!("plugin-store-limited-{}", address.port()));
        let url = format!("http://{address}/repos/webxxxxx/plugin-manager/releases/latest");
        let error = fetch_json(&url, None, Some(&dir), Duration::ZERO).unwrap_err();
        assert!(error.contains("限流"), "{error}");
        assert!(error.contains("github-token"), "{error}");
        assert!(error.contains(dir.to_string_lossy().as_ref()), "{error}");
        assert!(error.contains("5000"), "{error}");
        server.join().unwrap();
    }
}
