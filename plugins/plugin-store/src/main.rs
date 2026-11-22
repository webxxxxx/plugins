// plugin-store - a small catalog UI for the .spk packages hosted by this repo.
//
// It does five things: read a generated Markdown table, cache it locally,
// filter by the host's compat facts, download one package, and hand the bytes
// to the host's own /api/v1/plugins install endpoint. Signature and platform
// checks stay in the daemon.

mod catalog;
mod download;
mod net;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use svcd_plugin::{log_error, log_info, log_warn, Cancel, Context, Exit, Plugin, ToolOutcome};

const PAGE_SIZE: usize = catalog::PAGE_SIZE;
const REFRESH_AFTER: Duration = Duration::from_secs(6 * 60 * 60);
const CATALOG_URLS: &[&str] = &[
    "https://cdn.jsdelivr.net/gh/webxxxxx/plugins@main/docs/plugins.md",
    "https://raw.githubusercontent.com/webxxxxx/plugins/main/docs/plugins.md",
    "https://github.com/webxxxxx/plugins/raw/refs/heads/main/docs/plugins.md",
];
const TARGET: &str = match option_env!("SVCD_PLUGIN_TARGET") {
    Some(target) => target,
    None => "unknown",
};
/// The installed service id. svcd 0.1.3 does not export SVCD_PLUGIN_ID for an
/// exec payload, so Context::id() falls back to "plugin"; the crate name is the
/// same string the manifest is built with, and is stable across hosts.
const OWN_ID: &str = env!("CARGO_PKG_NAME");
const SNAPSHOT: &str = include_str!(concat!(env!("OUT_DIR"), "/catalog_snapshot.md"));
/// The release key that signs every package in the catalog. It is embedded
/// here so the store can offer a one-click trust import instead of asking the
/// user to copy a `.pub` file into the host config directory by hand.
const TRUST_KEY_ID: &str = "signing";
const TRUST_KEY_PEM: &str = include_str!("../../../docs/signing.pub");

#[derive(Clone)]
struct Job {
    id: u64,
    name: String,
    version: String,
    platform: String,
    device: Option<String>,
    phase: String,
    downloaded: u64,
    total: Option<u64>,
    message: String,
    done: bool,
    ok: bool,
    started_at: u64,
}

struct State {
    entries: Vec<catalog::Entry>,
    source: String,
    fetched_at: Option<u64>,
    last_error: Option<String>,
    refreshing: bool,
    job: Option<Job>,
}

struct PluginStore {
    state: Arc<Mutex<State>>,
    data_dir: PathBuf,
    temp_dir: PathBuf,
    own_id: String,
    cancel: Cancel,
}

impl Plugin for PluginStore {
    const CAPABILITIES: &'static [&'static str] = &[
        "list",
        "install",
        "install_status",
        "package_blob",
        "install_result",
        "refresh",
        "trust_key",
    ];

    fn call(&mut self, name: &str, arguments: &Value) -> ToolOutcome {
        match name {
            "list" => self.list(arguments),
            "install" => self.start_install(arguments),
            "install_status" => self.install_status(),
            "package_blob" => self.package_blob(arguments),
            "install_result" => self.install_result(arguments),
            "refresh" => self.refresh(),
            "trust_key" => self.trust_key(),
            other => ToolOutcome::error(format!("unknown tool: {other}")),
        }
    }

    fn status(&self) -> Option<String> {
        let state = self.state.lock().ok()?;
        Some(format!(
            "{} 个包 · {} · {}",
            state.entries.len(),
            TARGET,
            state.source
        ))
    }
}

impl PluginStore {
    fn list(&self, arguments: &Value) -> ToolOutcome {
        let page = arguments
            .get("page")
            .and_then(|value| value.as_u64().or_else(|| value.as_str()?.parse().ok()))
            .unwrap_or(1)
            .max(1) as usize;
        let query = arguments
            .get("query")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .chars()
            .take(100)
            .collect::<String>();
        let scope = arguments
            .get("scope")
            .and_then(Value::as_str)
            .unwrap_or("device");
        if scope != "device" && scope != "all" {
            return ToolOutcome::error("scope 只能是 device 或 all");
        }
        // The host's GET /api/v1/compat is the authoritative source. The
        // compile-time target stays as the fallback for a page that cannot
        // reach the API (or a standalone test).
        let target = arguments
            .get("platform")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| {
                !value.is_empty()
                    && value.len() <= 128
                    && value.chars().all(|character| {
                        character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
                    })
            })
            .unwrap_or(TARGET);
        let device = arguments
            .get("device")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty() && value.len() <= 128);

        let state = match self.state.lock() {
            Ok(state) => state,
            Err(_) => return ToolOutcome::error("状态锁不可用"),
        };
        let page = catalog::select(&state.entries, target, device, scope, &query, page);
        let items: Vec<Value> = page
            .items
            .iter()
            .map(|entry| entry_json(entry, &self.data_dir))
            .collect();
        let stale = state
            .fetched_at
            .map(|fetched| now_secs().saturating_sub(fetched) > REFRESH_AFTER.as_secs())
            .unwrap_or(true);
        let body = json!({
            "items": items,
            "total": page.total,
            "page": page.page,
            "pages": page.pages,
            "page_size": PAGE_SIZE,
            "target": target,
            "device": device,
            "scope": scope,
            "source": state.source.clone(),
            "fetched_at": state.fetched_at,
            "stale": stale,
            "refreshing": state.refreshing,
            "error": state.last_error.clone(),
            "job": state.job.as_ref().map(job_json),
        });
        ToolOutcome::text(body.to_string())
    }

    fn start_install(&self, arguments: &Value) -> ToolOutcome {
        let (name, version, platform) = match (
            required(arguments, "name"),
            required(arguments, "version"),
            required(arguments, "platform"),
        ) {
            (Ok(name), Ok(version), Ok(platform)) => (name, version, platform),
            (Err(error), _, _) | (_, Err(error), _) | (_, _, Err(error)) => {
                return ToolOutcome::error(error)
            }
        };
        let device = arguments
            .get("device")
            .and_then(Value::as_str)
            .unwrap_or("");
        if name == self.own_id {
            return ToolOutcome::error("不能从自己的页面里更新插件商店本身");
        }

        let entry = {
            let state = match self.state.lock() {
                Ok(state) => state,
                Err(_) => return ToolOutcome::error("状态锁不可用"),
            };
            if let Some(job) = state.job.as_ref() {
                if !job.done && job.phase != "ready" {
                    return ToolOutcome::error(format!(
                        "已有安装任务在运行：{}（{}）",
                        job.name, job.phase
                    ));
                }
            }
            match state
                .entries
                .iter()
                .find(|entry| {
                    entry.name == name
                        && entry.version == version
                        && entry.platform == platform
                        && entry.device.as_deref().unwrap_or("") == device
                })
                .cloned()
            {
                Some(entry) => entry,
                None => {
                    return ToolOutcome::error("目录里找不到这个包，请先刷新列表");
                }
            }
        };

        let id = now_millis();
        let job = Job {
            id,
            name: entry.name.clone(),
            version: entry.version.clone(),
            platform: entry.platform.clone(),
            device: entry.device.clone(),
            phase: "queued".into(),
            downloaded: 0,
            total: (entry.size_bytes > 0).then_some(entry.size_bytes),
            message: "排队中".into(),
            done: false,
            ok: false,
            started_at: now_secs(),
        };
        {
            let mut state = match self.state.lock() {
                Ok(state) => state,
                Err(_) => return ToolOutcome::error("状态锁不可用"),
            };
            state.job = Some(job.clone());
        }
        emit_job(&job);

        let state = Arc::clone(&self.state);
        let data_dir = self.data_dir.clone();
        let cancel = self.cancel.clone();
        let thread_job = job.clone();
        if let Err(error) = thread::Builder::new()
            .name(format!("install-{}", entry.name))
            .spawn(move || run_install(state, data_dir, entry, thread_job, cancel))
        {
            let mut guard = match self.state.lock() {
                Ok(guard) => guard,
                Err(_) => return ToolOutcome::error("安装线程启动失败"),
            };
            if let Some(current) = guard.job.as_mut() {
                if current.id == id {
                    current.phase = "error".into();
                    current.message = format!("安装线程启动失败: {error}");
                    current.done = true;
                }
            }
            return ToolOutcome::error(format!("安装线程启动失败: {error}"));
        }

        ToolOutcome::text(json!({ "job": job_json(&job) }).to_string())
    }

    fn install_status(&self) -> ToolOutcome {
        let state = match self.state.lock() {
            Ok(state) => state,
            Err(_) => return ToolOutcome::error("状态锁不可用"),
        };
        ToolOutcome::text(json!({ "job": state.job.as_ref().map(job_json) }).to_string())
    }

    /// Copy the cached package into the host's incoming directory and return it
    /// as a blob result. The browser then POSTs the blob to the existing
    /// /api/v1/plugins endpoint, so the daemon performs the signature and
    /// platform checks exactly as it does for the management page.
    fn package_blob(&self, arguments: &Value) -> ToolOutcome {
        let (name, version, platform) = match (
            required(arguments, "name"),
            required(arguments, "version"),
            required(arguments, "platform"),
        ) {
            (Ok(name), Ok(version), Ok(platform)) => (name, version, platform),
            (Err(error), _, _) | (_, Err(error), _) | (_, _, Err(error)) => {
                return ToolOutcome::error(error)
            }
        };
        let device = arguments
            .get("device")
            .and_then(Value::as_str)
            .unwrap_or("");
        let entry = {
            let state = match self.state.lock() {
                Ok(state) => state,
                Err(_) => return ToolOutcome::error("状态锁不可用"),
            };
            match state
                .entries
                .iter()
                .find(|entry| {
                    entry.name == name
                        && entry.version == version
                        && entry.platform == platform
                        && entry.device.as_deref().unwrap_or("") == device
                })
                .cloned()
            {
                Some(entry) => entry,
                None => return ToolOutcome::error("目录里找不到这个包"),
            }
        };
        let package = self.data_dir.join("downloads").join(package_file(&entry));
        if !package
            .metadata()
            .map(|meta| meta.is_file() && meta.len() > 0)
            .unwrap_or(false)
        {
            return ToolOutcome::error("文件还没有下载完成");
        }
        let incoming = self.temp_dir.join("incoming");
        if let Err(error) = fs::create_dir_all(&incoming) {
            return ToolOutcome::error(format!("{}: {error}", incoming.display()));
        }
        let target = incoming.join(package_file(&entry));
        if let Err(error) = fs::copy(&package, &target) {
            return ToolOutcome::error(format!("{}: {error}", target.display()));
        }
        let updated_job = {
            if let Ok(mut state) = self.state.lock() {
                if let Some(job) = state.job.as_mut() {
                    if job.name == entry.name
                        && job.version == entry.version
                        && job.platform == entry.platform
                        && job.device.as_deref().unwrap_or("") == device
                    {
                        job.phase = "installing".into();
                        job.message = "已交给宿主安装".into();
                        Some(job.clone())
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            }
        };
        if let Some(job) = updated_job {
            emit_job(&job);
        }
        ToolOutcome::Blob {
            path: target,
            name: package_file(&entry),
            mime_type: "application/octet-stream".into(),
        }
    }

    fn install_result(&self, arguments: &Value) -> ToolOutcome {
        let ok = arguments
            .get("ok")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let message = arguments
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or(if ok {
                "已安装并启动"
            } else {
                "安装失败"
            })
            .to_string();
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(_) => return ToolOutcome::error("状态锁不可用"),
        };
        let Some(job) = state.job.as_mut() else {
            return ToolOutcome::error("没有安装任务");
        };
        job.phase = if ok { "done".into() } else { "error".into() };
        job.message = message;
        job.done = true;
        job.ok = ok;
        let job = job.clone();
        drop(state);
        emit_job(&job);
        if ok {
            let cached = self.data_dir.join("downloads").join(package_file_parts(
                &job.name,
                &job.version,
                &job.platform,
                job.device.as_deref(),
            ));
            match fs::remove_file(&cached) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => log_warn!("删除缓存包 {} 失败: {error}", cached.display()),
            }
        }
        ToolOutcome::text(json!({ "job": job_json(&job) }).to_string())
    }

    /// The public key is public data, not a secret: the browser posts it to
    /// the host trust API after the user approves the import.
    fn trust_key(&self) -> ToolOutcome {
        if !TRUST_KEY_PEM.starts_with("-----BEGIN PUBLIC KEY-----") {
            return ToolOutcome::error("内置发布公钥格式不正确");
        }
        ToolOutcome::text(
            json!({
                "keyId": TRUST_KEY_ID,
                "pem": TRUST_KEY_PEM,
            })
            .to_string(),
        )
    }

    fn refresh(&self) -> ToolOutcome {
        let (entries, already) = {
            let mut state = match self.state.lock() {
                Ok(state) => state,
                Err(_) => return ToolOutcome::error("状态锁不可用"),
            };
            let entries = state.entries.len();
            if state.refreshing {
                (entries, true)
            } else {
                state.refreshing = true;
                (entries, false)
            }
        };

        if already {
            return ToolOutcome::text(
                json!({ "ok": true, "started": false, "entries": entries }).to_string(),
            );
        }

        let state = Arc::clone(&self.state);
        let data_dir = self.data_dir.clone();
        if let Err(error) = thread::Builder::new()
            .name("manual-refresh".into())
            .spawn(move || {
                if let Err(error) = do_refresh(&state, &data_dir) {
                    log_warn!("手动刷新插件列表失败: {error}");
                }
            })
        {
            if let Ok(mut guard) = self.state.lock() {
                guard.refreshing = false;
            }
            return ToolOutcome::error(format!("启动刷新线程失败: {error}"));
        }
        ToolOutcome::text(json!({ "ok": true, "started": true, "entries": entries }).to_string())
    }
}

fn required<'a>(arguments: &'a Value, key: &str) -> Result<&'a str, String> {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("缺少参数 {key}"))
}

fn entry_json(entry: &catalog::Entry, data_dir: &Path) -> Value {
    let file = package_file(entry);
    let cached = data_dir
        .join("downloads")
        .join(&file)
        .metadata()
        .map(|metadata| metadata.is_file() && metadata.len() > 0)
        .unwrap_or(false);
    json!({
        "name": entry.name,
        "version": entry.version,
        "platform": entry.platform,
        "size": entry.size,
        "size_bytes": entry.size_bytes,
        "url": entry.url,
        "device": entry.device,
        "file": file,
        "cached": cached,
    })
}

fn job_json(job: &Job) -> Value {
    let percent = job
        .total
        .filter(|total| *total > 0)
        .map(|total| ((job.downloaded.min(total) as u128 * 100) / total as u128) as u64);
    json!({
        "id": job.id,
        "name": job.name,
        "version": job.version,
        "platform": job.platform,
        "device": job.device,
        "phase": job.phase,
        "downloaded": job.downloaded,
        "total": job.total,
        "percent": percent,
        "message": job.message,
        "done": job.done,
        "ok": job.ok,
        "started_at": job.started_at,
    })
}

fn emit_job(job: &Job) {
    svcd_plugin::emit("plugin-store/install", json!({ "job": job_json(job) }));
}

fn publish_job(state: &Arc<Mutex<State>>, job: &Job) {
    let mut guard = match state.lock() {
        Ok(guard) => guard,
        Err(_) => return,
    };
    if guard.job.as_ref().map(|current| current.id) != Some(job.id) {
        return;
    }
    guard.job = Some(job.clone());
    drop(guard);
    emit_job(job);
}

fn run_install(
    state: Arc<Mutex<State>>,
    data_dir: PathBuf,
    entry: catalog::Entry,
    mut job: Job,
    cancel: Cancel,
) {
    let package = data_dir.join("downloads").join(package_file(&entry));
    let mut last_emit = Instant::now() - Duration::from_secs(1);

    if !package
        .metadata()
        .map(|meta| meta.is_file() && meta.len() > 0)
        .unwrap_or(false)
    {
        job.phase = "downloading".into();
        job.message = "下载中".into();
        publish_job(&state, &job);

        let mut progress = |downloaded: u64, total: Option<u64>| {
            job.downloaded = downloaded;
            job.total = total.or(job.total);
            let finished = job.total.map(|total| downloaded >= total).unwrap_or(false);
            if finished || last_emit.elapsed() >= Duration::from_millis(250) {
                last_emit = Instant::now();
                publish_job(&state, &job);
            }
        };
        let cancelled = || cancel.is_cancelled();
        if let Err(error) = net::download(
            &entry.url,
            &package,
            entry.size_bytes,
            &mut progress,
            &cancelled,
        ) {
            job.phase = "error".into();
            job.message = error;
            job.done = true;
            publish_job(&state, &job);
            return;
        }
        job.downloaded = job.total.unwrap_or(entry.size_bytes);
    }

    job.phase = "ready".into();
    job.message = "下载完成，正在交给宿主安装".into();
    job.done = false;
    job.ok = false;
    if job.total.is_none() {
        job.total = Some(job.downloaded);
    }
    publish_job(&state, &job);
}

fn package_file(entry: &catalog::Entry) -> String {
    package_file_parts(
        &entry.name,
        &entry.version,
        &entry.platform,
        entry.device.as_deref(),
    )
}

fn package_file_parts(name: &str, version: &str, platform: &str, device: Option<&str>) -> String {
    let mut file = format!(
        "{}-{}-{}",
        safe_part(name),
        safe_part(version),
        safe_part(platform)
    );
    if let Some(device) = device {
        file.push('-');
        file.push_str(&safe_part(device));
    }
    file.push_str(".spk");
    file
}

fn safe_part(value: &str) -> String {
    let cleaned: String = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                character
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "package".into()
    } else {
        cleaned
    }
}

fn initial_state(data_dir: &Path) -> State {
    let cache = data_dir.join("catalog.md");
    if let Ok(text) = fs::read_to_string(&cache) {
        match catalog::parse(&text) {
            Ok(entries) => {
                let fetched_at = cache
                    .metadata()
                    .and_then(|metadata| metadata.modified())
                    .ok()
                    .and_then(system_secs);
                return State {
                    entries,
                    source: "cache".into(),
                    fetched_at,
                    last_error: None,
                    refreshing: false,
                    job: None,
                };
            }
            Err(error) => {
                log_warn!("忽略损坏的 catalog 缓存 {}: {error}", cache.display());
            }
        }
    }
    let entries = catalog::parse(SNAPSHOT).unwrap_or_else(|error| {
        log_error!("内置插件列表无法解析: {error}");
        Vec::new()
    });
    State {
        entries,
        source: "snapshot".into(),
        fetched_at: None,
        last_error: None,
        refreshing: false,
        job: None,
    }
}

fn spawn_refresher(state: Arc<Mutex<State>>, data_dir: PathBuf, cancel: Cancel) {
    let _ = thread::Builder::new()
        .name("catalog-refresh".into())
        .spawn(move || loop {
            let needs = {
                let guard = match state.lock() {
                    Ok(guard) => guard,
                    Err(_) => return,
                };
                guard
                    .fetched_at
                    .map(|fetched| now_secs().saturating_sub(fetched) > REFRESH_AFTER.as_secs())
                    .unwrap_or(true)
            };
            if needs {
                if let Err(error) = refresh_once(&state, &data_dir) {
                    log_warn!("刷新插件列表失败: {error}");
                }
            }
            if cancel.sleep(REFRESH_AFTER) {
                break;
            }
        });
}

fn refresh_once(state: &Arc<Mutex<State>>, data_dir: &Path) -> Result<usize, String> {
    {
        let mut guard = state.lock().map_err(|_| "状态锁不可用".to_string())?;
        if guard.refreshing {
            return Err("已有刷新任务在运行".into());
        }
        guard.refreshing = true;
    }
    do_refresh(state, data_dir)
}

/// Fetch and apply a refresh. The caller already owns the refreshing flag.
fn do_refresh(state: &Arc<Mutex<State>>, data_dir: &Path) -> Result<usize, String> {
    let fetched = fetch_catalog().map(|(entries, text, url)| {
        if let Err(error) = write_cache(data_dir, &text) {
            log_warn!("写入 catalog 缓存失败: {error}");
        }
        (entries, url)
    });

    let mut guard = state.lock().map_err(|_| "状态锁不可用".to_string())?;
    guard.refreshing = false;
    match fetched {
        Ok((entries, url)) => {
            let count = entries.len();
            guard.entries = entries;
            guard.source = "network".into();
            guard.fetched_at = Some(now_secs());
            guard.last_error = None;
            drop(guard);
            log_info!("插件列表已刷新: {count} 个包（{url}）");
            svcd_plugin::emit("plugin-store/catalog", json!({ "entries": count }));
            Ok(count)
        }
        Err(error) => {
            guard.last_error = Some(error.clone());
            drop(guard);
            svcd_plugin::emit(
                "plugin-store/catalog",
                json!({ "ok": false, "error": error }),
            );
            Err(error)
        }
    }
}

fn fetch_catalog() -> Result<(Vec<catalog::Entry>, String, String), String> {
    let mut errors = Vec::new();
    for url in CATALOG_URLS {
        match net::fetch_text(&[url]) {
            Ok((text, source)) => match catalog::parse(&text) {
                Ok(entries) => return Ok((entries, text, source)),
                Err(error) => errors.push(format!("{url}: {error}")),
            },
            Err(error) => errors.push(error),
        }
    }
    Err(errors.join("；"))
}

fn write_cache(data_dir: &Path, text: &str) -> Result<(), String> {
    fs::create_dir_all(data_dir).map_err(|error| format!("{}: {error}", data_dir.display()))?;
    let temporary = data_dir.join("catalog.md.tmp");
    let target = data_dir.join("catalog.md");
    fs::write(&temporary, text).map_err(|error| format!("{}: {error}", temporary.display()))?;
    #[cfg(windows)]
    let _ = fs::remove_file(&target);
    fs::rename(&temporary, &target).map_err(|error| format!("{}: {error}", target.display()))
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn system_secs(time: SystemTime) -> Option<u64> {
    time.duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_secs())
}

fn main() -> Exit {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("download") {
        if let Err(error) = download::run(&args[1..]) {
            eprintln!("plugin-store download: {error}");
            std::process::exit(1);
        }
        return;
    }

    let context = Context::from_env();
    let own_id = OWN_ID.to_string();
    let data_root = context
        .data_dir()
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("plugin-store"));
    let data_dir = data_root.join(OWN_ID);
    let state = Arc::new(Mutex::new(initial_state(&data_dir)));
    log_info!(
        "plugin-store 启动: target={TARGET}, 内置/缓存 {} 个包, data={}",
        state.lock().map(|guard| guard.entries.len()).unwrap_or(0),
        data_dir.display()
    );
    let temp_dir = context
        .temp_dir()
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("plugin-store-temp"));
    let cancel = context.cancel();
    spawn_refresher(Arc::clone(&state), data_dir.clone(), cancel.clone());

    svcd_plugin::serve(
        context,
        PluginStore {
            state,
            data_dir,
            temp_dir,
            own_id,
            cancel,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declared_capabilities_match_the_manifest() {
        let mut declared = svcd_plugin::declared_capabilities(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/package/manifest.json"
        ))
        .expect("read the manifest");
        declared.sort();

        let mut implemented: Vec<String> = PluginStore::CAPABILITIES
            .iter()
            .map(|name| name.to_string())
            .collect();
        implemented.sort();

        assert_eq!(
            declared, implemented,
            "manifest and code disagree on capabilities"
        );
    }

    #[test]
    fn embedded_trust_key_is_a_public_key() {
        assert!(TRUST_KEY_PEM.starts_with("-----BEGIN PUBLIC KEY-----"));
        assert!(TRUST_KEY_PEM.contains("-----END PUBLIC KEY-----"));
        assert_eq!(TRUST_KEY_ID, "signing");
    }

    #[test]
    fn package_names_are_sanitized() {
        assert_eq!(safe_part("meow"), "meow");
        assert_eq!(safe_part("a/b c"), "a_b_c");
        assert_eq!(safe_part(""), "package");
    }

    #[test]
    fn package_file_keeps_the_device_suffix() {
        let entry = catalog::Entry {
            name: "sshd".into(),
            version: "1.0.0".into(),
            platform: "x86_64-unknown-linux-gnu".into(),
            size: "2.8 MiB".into(),
            size_bytes: 2_936_013,
            url: "https://github.com/webxxxxx/plugins/releases/download/v1/sshd-1.0.0-x86_64-unknown-linux-gnu-sr6110.spk".into(),
            device: Some("sr6110".into()),
        };
        assert_eq!(
            package_file(&entry),
            "sshd-1.0.0-x86_64-unknown-linux-gnu-sr6110.spk"
        );
    }

    #[test]
    fn builtin_snapshot_has_rows() {
        let entries = catalog::parse(SNAPSHOT).expect("snapshot parses");
        assert!(entries.len() >= 3);
    }
}
