// plugin-store - a small catalog UI for the .spk packages hosted by this repo.
//
// It does six things: read a generated Markdown table, cache it locally,
// filter by the host's compat facts, download one package, hand the bytes
// to the host's own /api/v1/plugins install endpoint, and look up a GitHub
// release asset so svcd can upgrade itself without linking TLS. Signature and
// platform checks stay in the daemon.

mod catalog;
mod download;
mod net;
mod release;
mod settings;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use svcd_plugin::{log_error, log_info, log_warn, Cancel, Context, Exit, Plugin, ToolOutcome};

const PAGE_SIZE: usize = catalog::PAGE_SIZE;
const REFRESH_AFTER: Duration = Duration::from_secs(6 * 60 * 60);
/// Where the generated catalog is fetched from, in order.
///
/// GitHub raw first: it serves the branch as it is, with no quota. Then two
/// stateless raw proxies (`raw.githack.com`, `cdn.statically.io`) for networks
/// where GitHub is blocked. jsDelivr sits last of the mirrors: its branch URLs
/// are cached for a week (`max-age=604800`) and in practice it kept serving
/// last week's table even after the release workflow purged it
/// (`https://purge.jsdelivr.net/...`), sometimes as a 301 back to raw. It is
/// still worth trying when nothing else answers, because a stale list is
/// better than no list — `catalog::is_older` keeps it from overwriting a newer
/// one.
///
/// The GitHub REST API is deliberately not a source: it is the same network as
/// raw (so it does not add reachability), it caps unauthenticated callers at 60
/// requests an hour per IP, and the Markdown file is generated from it anyway.
const CATALOG_URLS: &[&str] = &[
    "https://raw.githubusercontent.com/webxxxxx/plugins/main/docs/plugins.md",
    "https://raw.githack.com/webxxxxx/plugins/main/docs/plugins.md",
    "https://cdn.statically.io/gh/webxxxxx/plugins/main/docs/plugins.md",
    "https://cdn.jsdelivr.net/gh/webxxxxx/plugins@main/docs/plugins.md",
    "https://fastly.jsdelivr.net/gh/webxxxxx/plugins@main/docs/plugins.md",
    "https://gcore.jsdelivr.net/gh/webxxxxx/plugins@main/docs/plugins.md",
    "https://github.com/webxxxxx/plugins/raw/refs/heads/main/docs/plugins.md",
];

/// One place a catalog can come from, with the parser its bytes need.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Source {
    url: String,
    format: catalog::Format,
}

impl Source {
    /// A list fetched over https must not point at `http://` downloads; a list
    /// fetched over http is already plaintext, so its http links are allowed.
    fn requires_https(&self) -> bool {
        self.url.starts_with("https://")
    }
}

/// The configured source first, the other one as a safety net.
///
/// `catalog_source` sets priority, not exclusivity: a typo in `qiniu_base` or a
/// network where GitHub is unreachable must degrade the store to the other
/// list, never to an empty page. `catalog::is_older` still decides whether an
/// answer is fresh enough to replace what is already in hand.
fn catalog_sources(settings: &settings::Settings) -> Vec<Source> {
    let github = || {
        CATALOG_URLS
            .iter()
            .map(|url| Source {
                url: (*url).to_string(),
                format: catalog::Format::Markdown,
            })
            .collect::<Vec<_>>()
    };
    let qiniu = || {
        settings
            .qiniu_base
            .as_ref()
            .map(|base| {
                vec![Source {
                    url: format!("{base}/index.json"),
                    format: catalog::Format::Json,
                }]
            })
            .unwrap_or_default()
    };
    let mut sources = match settings.catalog_source {
        settings::CatalogSource::Github => github(),
        settings::CatalogSource::Qiniu => qiniu(),
    };
    sources.extend(match settings.catalog_source {
        settings::CatalogSource::Github => qiniu(),
        settings::CatalogSource::Qiniu => github(),
    });
    sources
}

/// The cache shape the configured source writes.
fn cache_format(source: settings::CatalogSource) -> catalog::Format {
    match source {
        settings::CatalogSource::Github => catalog::Format::Markdown,
        settings::CatalogSource::Qiniu => catalog::Format::Json,
    }
}

fn other_format(format: catalog::Format) -> catalog::Format {
    match format {
        catalog::Format::Markdown => catalog::Format::Json,
        catalog::Format::Json => catalog::Format::Markdown,
    }
}

const TARGET: &str = match option_env!("SVCD_PLUGIN_TARGET") {
    Some(target) => target,
    None => "unknown",
};
/// The installed service id. svcd 0.1.3 does not export SVCD_PLUGIN_ID for an
/// exec payload, so Context::id() falls back to "plugin"; the crate name is the
/// same string the manifest is built with, and is stable across hosts.
const OWN_ID: &str = env!("CARGO_PKG_NAME");
const SNAPSHOT: &str = include_str!(concat!(env!("OUT_DIR"), "/catalog_snapshot.md"));

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
    /// The URL the current entries came from; `None` for cache and snapshot.
    source_url: Option<String>,
    fetched_at: Option<u64>,
    last_error: Option<String>,
    refreshing: bool,
    job: Option<Job>,
}

struct PluginStore {
    state: Arc<Mutex<State>>,
    /// The user's config file: GitHub token and release-cache window.
    settings: settings::Settings,
    data_dir: PathBuf,
    /// Transient package downloads live under the plugin root, not beside the
    /// host's data root.
    download_dir: PathBuf,
    temp_dir: PathBuf,
    own_id: String,
    cancel: Cancel,
    /// GitHub REST root; only tests point this somewhere else.
    api_base: String,
}

impl Plugin for PluginStore {
    const CAPABILITIES: &'static [&'static str] = &[
        "list",
        "install",
        "install_status",
        "install_cancel",
        "package_blob",
        "install_result",
        "refresh",
        "release_asset",
    ];

    fn call(&mut self, name: &str, arguments: &Value) -> ToolOutcome {
        match name {
            "list" => self.list(arguments),
            "install" => self.start_install(arguments),
            "install_status" => self.install_status(),
            "install_cancel" => self.cancel_install(),
            "package_blob" => self.package_blob(arguments),
            "install_result" => self.install_result(arguments),
            "refresh" => self.refresh(),
            "release_asset" => self.release_asset(arguments),
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
        // One row per install slot: `variants` are the artifacts that share an
        // id, a version and a platform (a device-restricted build next to the
        // portable one). The page picks the variant that matches what is
        // installed; `name` / `version` / `platform` are the row's own facts.
        let items: Vec<Value> = page
            .items
            .iter()
            .map(|row| {
                json!({
                    "name": row.name,
                    "version": row.version,
                    "platform": row.platform,
                    "variants": row
                        .variants
                        .iter()
                        .map(|entry| entry_json(entry, &self.download_dir))
                        .collect::<Vec<Value>>(),
                })
            })
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
            "self": self.own_id,
            "device": device,
            "scope": scope,
            "source": state.source.clone(),
            "source_url": state.source_url.clone(),
            "catalog_source": self.settings.catalog_source.as_str(),
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

        let id = next_job_id();
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
        let download_dir = self.download_dir.clone();
        let cancel = self.cancel.clone();
        let thread_job = job.clone();
        if let Err(error) = thread::Builder::new()
            .name(format!("install-{}", entry.name))
            .spawn(move || run_install(state, download_dir, entry, thread_job, cancel))
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

    /// Delete the current job: stop an in-flight download, remove the cached
    /// package, and drop the job so the page starts from a clean state. Once
    /// the bytes are with the host there is nothing left to call back, so the
    /// installing phase refuses.
    fn cancel_install(&self) -> ToolOutcome {
        let job = {
            let mut state = match self.state.lock() {
                Ok(state) => state,
                Err(_) => return ToolOutcome::error("状态锁不可用"),
            };
            let job = match state.job.as_ref() {
                Some(job) => job.clone(),
                None => return ToolOutcome::error("没有可删除的安装任务"),
            };
            if !can_delete(&job) {
                return ToolOutcome::error("安装已经交给宿主，无法删除");
            }
            state.job = None;
            job
        };
        // Clearing the job is what makes the download thread stop between read
        // chunks; its handle may take a moment to close on Windows.
        let package = self.download_dir.join(package_file_parts(
            &job.name,
            &job.version,
            &job.platform,
            job.device.as_deref(),
        ));
        remove_download(&package);
        // The transfer belongs to the download thread, which may still hold the
        // handle (Windows): best effort here, the thread removes its own file.
        let transfer = job_transfer_path(&self.download_dir, &job);
        remove_if_idle(&transfer);
        remove_if_idle(&net::part_path(&transfer));
        svcd_plugin::emit("plugin-store/install", json!({ "job": null }));
        ToolOutcome::text(json!({ "ok": true }).to_string())
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
        let package = self.download_dir.join(package_file(&entry));
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
        if let Err(error) = hand_off(&package, &target) {
            return ToolOutcome::error(error);
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
        // 安装一结束就删掉下载副本：`download_dir` 是传输暂存，不是仓库。
        // 只在成功时删，失败时留一份「重试不用再下载」会把小卷占满——34 MB
        // 的 OpenWrt 上，一次失败的 15.7 MiB 安装就让 temp 一直不清。
        let downloaded = self.download_dir.join(package_file_parts(
            &job.name,
            &job.version,
            &job.platform,
            job.device.as_deref(),
        ));
        remove_download(&downloaded);
        // The transfer is named after the job, so a newer download of the same
        // package keeps its own file.
        let transfer = job_transfer_path(&self.download_dir, &job);
        remove_download(&transfer);
        remove_download(&net::part_path(&transfer));
        ToolOutcome::text(json!({ "job": job_json(&job) }).to_string())
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
        let settings = self.settings.clone();
        if let Err(error) = thread::Builder::new()
            .name("manual-refresh".into())
            .spawn(move || {
                if let Err(error) = do_refresh(&state, &data_dir, &settings) {
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

    /// Look up the newest stable GitHub release, optionally downloading one of
    /// its assets. This is how svcd reaches GitHub for its own upgrade while
    /// staying TLS-free.
    ///
    /// `download: false` returns `{tag, version, assets:[{name,size}], sumsAsset}`.
    /// The host picks its archive from `assets` by target suffix and refuses the
    /// upgrade when `sumsAsset` is null. `download: true` needs `asset` (an exact
    /// file name) and returns exactly one blob, because the host materializes a
    /// single `$blobPath` per call; `SHA256SUMS` is fetched with a second call.
    /// Downloads deliberately skip `PLUGIN_STORE_MIRROR`: a mirror may serve
    /// plugins, not the daemon itself.
    fn release_asset(&self, arguments: &Value) -> ToolOutcome {
        let repo = match optional(arguments, "repo") {
            Ok(Some(repo)) => repo,
            Ok(None) => release::DEFAULT_REPO,
            Err(error) => return ToolOutcome::error(error),
        };
        let download = match arguments.get("download") {
            None => false,
            Some(Value::Bool(value)) => *value,
            Some(_) => return ToolOutcome::error("download 只能是 true 或 false"),
        };
        let asset_name = match optional(arguments, "asset") {
            Ok(asset) => asset,
            Err(error) => return ToolOutcome::error(error),
        };
        // Validate the download target before the API call, so a missing name
        // never burns a request from the anonymous quota.
        let download_target = if download {
            match asset_name {
                Some(name) => Some(name),
                None => return ToolOutcome::error("download=true 时必须给 asset 文件名"),
            }
        } else {
            None
        };

        let release = match self.daemon_release(repo) {
            Ok(release) => release,
            Err(error) => return ToolOutcome::error(error),
        };
        let Some(asset_name) = download_target else {
            return ToolOutcome::text(release::release_json(&release).to_string());
        };
        let asset = match release::download_asset(&release, asset_name) {
            Ok(asset) => asset,
            Err(error) => return ToolOutcome::error(error),
        };

        let cancel = self.cancel.clone();
        match release::download_to(&self.temp_dir, asset, &move || cancel.is_cancelled()) {
            Ok(path) => ToolOutcome::Blob {
                path,
                name: asset.name.clone(),
                mime_type: "application/octet-stream".into(),
            },
            Err(error) => ToolOutcome::error(error),
        }
    }
}

impl PluginStore {
    /// svcd's release, from whichever source the user configured.
    ///
    /// A mirror is preferred when `catalog_source` picks it, but never blindly:
    /// a mirror without an `svcd` section falls back to GitHub, and the fallback
    /// is logged — the two sources can disagree about what is newest, and that
    /// disagreement is exactly what a user needs to see.
    fn daemon_release(&self, repo: &str) -> Result<release::Release, String> {
        let mirror = self.settings.catalog_source == settings::CatalogSource::Qiniu
            && self.settings.qiniu_base.is_some();
        if mirror && repo == release::DEFAULT_REPO {
            match self.mirrored_svcd() {
                Ok(release) => {
                    log_info!(
                        "svcd {} 来自镜像（{}）；宿主仍按同源 SHA256SUMS 校验字节",
                        release.version,
                        self.settings.qiniu_base.as_deref().unwrap_or_default()
                    );
                    return Ok(release);
                }
                Err(error) => log_warn!("镜像里取不到 svcd（{error}），改用 GitHub"),
            }
        }
        self.github_svcd(repo)
    }

    /// The mirror's `index.json`, parsed into the release the host understands.
    ///
    /// No GitHub token is attached: this host is not GitHub, and a PAT must not
    /// travel to a mirror.
    fn mirrored_svcd(&self) -> Result<release::Release, String> {
        let base = self
            .settings
            .qiniu_base
            .as_deref()
            .ok_or("没有配置 qiniu_base")?;
        let url = format!("{}/index.json", base.trim_end_matches('/'));
        let body = net::fetch_json(
            &url,
            None,
            Some(self.data_dir.as_path()),
            self.settings.cache_ttl,
        )?;
        release::from_index(&body)
    }

    /// A read-only token lifts the anonymous 60/hour to 5000/hour, and the cache
    /// answers a repeat from disk: within the window no request is made at all,
    /// after it the ETag turns the check into a free 304.
    fn github_svcd(&self, repo: &str) -> Result<release::Release, String> {
        let token = net::github_token(self.settings.github_token.as_deref(), &self.data_dir);
        let ttl = self.settings.cache_ttl;
        release::fetch_at(&self.api_base, repo, &|url| {
            net::fetch_json(url, token.as_deref(), Some(self.data_dir.as_path()), ttl)
        })
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

/// An optional string argument: absent means `None`, but a present non-string
/// or an empty string is an error rather than a silent fallback.
fn optional<'a>(arguments: &'a Value, key: &str) -> Result<Option<&'a str>, String> {
    match arguments.get(key) {
        None => Ok(None),
        Some(Value::String(value)) if !value.trim().is_empty() => Ok(Some(value.trim())),
        Some(_) => Err(format!("{key} 必须是非空字符串")),
    }
}

/// Where this plugin may keep its own files: `<plugin dir>/data`.
///
/// The host exports `SVCD_PLUGIN_DIR` (`<plugins>/<id>`), and that directory is
/// what survives an upgrade — the version directories under it are replaced.
/// A binary started by hand has no such variable, so its own path is the next
/// best evidence: `<plugin>/<version>/payload/bin/<exe>` walks up to the plugin
/// directory. Neither applies outside a package (cargo test, a bare `cargo run`),
/// and then the system temp directory keeps the files out of the source tree.
fn plugin_data_dir(exe: Option<&Path>, plugin_dir: Option<&Path>, id: &str) -> PathBuf {
    if let Some(root) = plugin_dir {
        return root.join("data");
    }
    if let Some(root) = exe.and_then(installed_plugin_root) {
        if root.file_name().and_then(|name| name.to_str()) == Some(id) {
            return root.join("data");
        }
    }
    std::env::temp_dir().join(id)
}

/// The plugin directory, when it is known at all — settings live in its
/// `config`, which the host owns and `editconfig` writes.
fn plugin_root(exe: Option<&Path>, plugin_dir: Option<&Path>, id: &str) -> Option<PathBuf> {
    if let Some(root) = plugin_dir {
        return Some(root.to_path_buf());
    }
    exe.and_then(installed_plugin_root)
        .filter(|root| root.file_name().and_then(|name| name.to_str()) == Some(id))
}

/// `<data>/plugins/<id>` from the running payload's path.
fn installed_plugin_root(exe: &Path) -> Option<PathBuf> {
    let bin = exe.parent()?;
    let payload = bin.parent()?;
    let version = payload.parent()?;
    if bin.file_name()? != "bin" || payload.file_name()? != "payload" {
        return None;
    }
    version.parent().map(Path::to_path_buf)
}

fn entry_json(entry: &catalog::Entry, download_dir: &Path) -> Value {
    let file = package_file(entry);
    let cached = download_dir
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

/// Once the bytes are with the host the install cannot be called back; before
/// that a delete cancels the download and removes its files.
fn can_delete(job: &Job) -> bool {
    job.phase != "installing"
}

/// A download belongs to the job the page is showing. Deleting that job clears
/// `state.job`, which is how the download thread learns to stop.
fn job_is_current(state: &Arc<Mutex<State>>, id: u64) -> bool {
    state
        .lock()
        .map(|state| state.job.as_ref().map(|job| job.id) == Some(id))
        .unwrap_or(false)
}

/// Remove a downloaded package or its `.part`, tolerating a download thread
/// that is still closing the handle: Windows refuses to unlink an open file.
/// Move the downloaded package into the host's incoming directory.
///
/// The download lives under the plugin data root and `$SVCD_TEMP_DIR/incoming`
/// is normally the same filesystem, so a rename is enough — and it matters:
/// copying a 15.7 MiB package needs a second 15.7 MiB, which a small `/data`
/// volume does not have; a failed copy also used to leave a partial file in
/// `incoming/` that nothing cleaned. A cross-device rename falls back to copy,
/// and any partial target is removed on failure.
fn hand_off(package: &Path, target: &Path) -> Result<(), String> {
    match fs::remove_file(target) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("{}: {error}", target.display())),
    }
    if fs::rename(package, target).is_ok() {
        return Ok(());
    }
    match fs::copy(package, target) {
        Ok(_) => {
            remove_download(package);
            Ok(())
        }
        Err(error) => {
            let _ = fs::remove_file(target);
            Err(format!("{}: {error}", target.display()))
        }
    }
}

fn remove_download(path: &Path) {
    for attempt in 0..20 {
        match fs::remove_file(path) {
            Ok(()) => return,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
            Err(error) if attempt == 19 => {
                log_warn!("删除下载文件 {} 失败: {error}", path.display());
            }
            Err(_) => thread::sleep(Duration::from_millis(100)),
        }
    }
}

/// The file one job writes into. The package name stays readable, but the job
/// id makes the path unique, so a job deleted mid-transfer can only ever remove
/// its own bytes — never the `.part` a newer job is writing (#18).
fn transfer_path(dir: &Path, file: &str, job_id: u64) -> PathBuf {
    dir.join(format!("{file}.{job_id}.download"))
}

fn download_path(dir: &Path, entry: &catalog::Entry, job_id: u64) -> PathBuf {
    transfer_path(dir, &package_file(entry), job_id)
}

fn job_transfer_path(dir: &Path, job: &Job) -> PathBuf {
    transfer_path(
        dir,
        &package_file_parts(
            &job.name,
            &job.version,
            &job.platform,
            job.device.as_deref(),
        ),
        job.id,
    )
}

/// Remove the transfer a job owns. Never the cache: a newer job may already be
/// handing that file to the host.
fn discard_transfer(dir: &Path, entry: &catalog::Entry, job_id: u64) {
    let transfer = download_path(dir, entry, job_id);
    remove_download(&transfer);
    remove_download(&net::part_path(&transfer));
}

/// Unlink without retrying. A transfer the download thread still holds open
/// (Windows) is removed by that thread once it notices the delete.
fn remove_if_idle(path: &Path) {
    let _ = fs::remove_file(path);
}

/// Clear transfers left by a process that died mid-download, and the shared
/// `.part` files older versions wrote. Only runs while nothing is downloading.
fn sweep_transfers(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.ends_with(".download") && !name.ends_with(".part") {
            continue;
        }
        let path = entry.path();
        if let Err(error) = fs::remove_file(&path) {
            log_warn!("清理残留下载文件 {} 失败: {error}", path.display());
        }
    }
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
    download_dir: PathBuf,
    entry: catalog::Entry,
    mut job: Job,
    cancel: Cancel,
) {
    let package = download_dir.join(package_file(&entry));
    let mut last_emit = Instant::now() - Duration::from_secs(1);
    let job_id = job.id;

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
        // #18: the transfer is this job's alone. Deleting the job must not make
        // the next download fail at 100% because a shared `.part` went away
        // under it, so the bytes go to `<file>.<id>.download` and only the
        // finished file takes the shared cache name.
        let transfer = download_path(&download_dir, &entry, job_id);
        let cancelled = || cancel.is_cancelled() || !job_is_current(&state, job_id);
        if let Err(error) = net::download(
            &entry.url,
            &transfer,
            entry.size_bytes,
            &mut progress,
            &cancelled,
        ) {
            discard_transfer(&download_dir, &entry, job_id);
            if !job_is_current(&state, job_id) {
                // The page deleted this job while the download was running.
                // Leave no bytes behind instead of publishing a stale card.
                return;
            }
            job.phase = "error".into();
            job.message = error;
            job.done = true;
            publish_job(&state, &job);
            return;
        }
        // A delete that landed just as the last byte arrived must not resurrect
        // the job with a "ready" card or leave the package behind.
        if !job_is_current(&state, job_id) {
            discard_transfer(&download_dir, &entry, job_id);
            return;
        }
        if let Err(error) = fs::rename(&transfer, &package) {
            job.phase = "error".into();
            job.message = format!("{}: {error}", package.display());
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

fn initial_state(data_dir: &Path, settings: &settings::Settings) -> State {
    let preferred = cache_format(settings.catalog_source);
    // A cached http list is fine while the configured source is http, and
    // stale the moment it is https again.
    let require_https = match settings.catalog_source {
        settings::CatalogSource::Github => true,
        settings::CatalogSource::Qiniu => settings
            .qiniu_base
            .as_deref()
            .is_none_or(|base| base.starts_with("https://")),
    };
    // Either cache is valid data; the configured one is merely preferred, so a
    // user who switches sources still starts from the last known list.
    for format in [preferred, other_format(preferred)] {
        if let Some(state) = cached_state(data_dir, format, require_https) {
            return state;
        }
    }
    let entries = catalog::parse(SNAPSHOT).unwrap_or_else(|error| {
        log_error!("内置插件列表无法解析: {error}");
        Vec::new()
    });
    State {
        entries,
        source: "snapshot".into(),
        source_url: None,
        fetched_at: None,
        last_error: None,
        refreshing: false,
        job: None,
    }
}

fn cached_state(data_dir: &Path, format: catalog::Format, require_https: bool) -> Option<State> {
    let cache = data_dir.join(format!("catalog.{}", format.extension()));
    let text = fs::read_to_string(&cache).ok()?;
    match format.parse(&text) {
        Ok(entries) => {
            if let Err(error) = catalog::check_scheme(&entries, require_https) {
                log_warn!("忽略 catalog 缓存 {}: {error}", cache.display());
                return None;
            }
            let fetched_at = cache
                .metadata()
                .and_then(|metadata| metadata.modified())
                .ok()
                .and_then(system_secs);
            Some(State {
                entries,
                source: "cache".into(),
                source_url: None,
                fetched_at,
                last_error: None,
                refreshing: false,
                job: None,
            })
        }
        Err(error) => {
            log_warn!("忽略损坏的 catalog 缓存 {}: {error}", cache.display());
            None
        }
    }
}

fn spawn_refresher(
    state: Arc<Mutex<State>>,
    data_dir: PathBuf,
    cancel: Cancel,
    settings: settings::Settings,
) {
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
                if let Err(error) = refresh_once(&state, &data_dir, &settings) {
                    log_warn!("刷新插件列表失败: {error}");
                }
            }
            if cancel.sleep(REFRESH_AFTER) {
                break;
            }
        });
}

fn refresh_once(
    state: &Arc<Mutex<State>>,
    data_dir: &Path,
    settings: &settings::Settings,
) -> Result<usize, String> {
    {
        let mut guard = state.lock().map_err(|_| "状态锁不可用".to_string())?;
        if guard.refreshing {
            return Err("已有刷新任务在运行".into());
        }
        guard.refreshing = true;
    }
    do_refresh(state, data_dir, settings)
}

/// Fetch and apply a refresh. The caller already owns the refreshing flag.
fn do_refresh(
    state: &Arc<Mutex<State>>,
    data_dir: &Path,
    settings: &settings::Settings,
) -> Result<usize, String> {
    let current = match state.lock() {
        Ok(guard) => guard.entries.clone(),
        Err(_) => return Err("状态锁不可用".into()),
    };
    let fetched = fetch_catalog(&current, settings);

    let mut guard = state.lock().map_err(|_| "状态锁不可用".to_string())?;
    guard.refreshing = false;
    match fetched {
        Ok(fetched) => {
            let Fetched {
                entries,
                text,
                url,
                format,
            } = fetched;
            // The list may have been replaced while this fetch was in flight.
            if catalog::is_older(&entries, &guard.entries) {
                let error = format!("{url} 返回的列表比当前这份旧，已忽略");
                guard.last_error = Some(error.clone());
                drop(guard);
                log_warn!("{error}");
                return Err(error);
            }
            if let Err(error) = write_cache(data_dir, &text, format) {
                log_warn!("写入 catalog 缓存失败: {error}");
            }
            let count = entries.len();
            guard.entries = entries;
            guard.source = "network".into();
            guard.source_url = Some(url.clone());
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

/// First source that answers with a list at least as new as `current`.
///
/// A mirror can serve a week-old copy (jsDelivr caches a branch for that long),
/// so a stale answer is not a failure to install — it is a reason to ask the
/// next source instead of stopping here.
/// A catalog that passed the freshness check, with the bytes to cache.
#[derive(Debug)]
struct Fetched {
    entries: Vec<catalog::Entry>,
    text: String,
    url: String,
    format: catalog::Format,
}

fn fetch_catalog(current: &[catalog::Entry], settings: &settings::Settings) -> Result<Fetched, String> {
    fetch_from(current, &catalog_sources(settings))
}

/// Ask the sources in order; the first one that parses and is not older wins.
fn fetch_from(current: &[catalog::Entry], sources: &[Source]) -> Result<Fetched, String> {
    let mut errors = Vec::new();
    for source in sources {
        match net::fetch_text(&[source.url.as_str()]) {
            Ok((text, url)) => match source.format.parse(&text) {
                Ok(entries) => {
                    if let Err(error) = catalog::check_scheme(&entries, source.requires_https()) {
                        errors.push(format!("{url}: {error}"));
                        continue;
                    }
                    if catalog::is_older(&entries, current) {
                        // Worth a line of its own: a stale mirror is why the
                        // configured source loses to the fallback, and that is
                        // invisible when the fallback succeeds.
                        log_warn!("{url}: 返回的列表比当前这份旧，换下一个源");
                        errors.push(format!("{url}: 返回的列表比当前这份旧，换下一个源"));
                        continue;
                    }
                    return Ok(Fetched {
                        entries,
                        text,
                        url,
                        format: source.format,
                    });
                }
                Err(error) => errors.push(format!("{url}: {error}")),
            },
            Err(error) => errors.push(error),
        }
    }
    Err(errors.join("；"))
}

fn write_cache(data_dir: &Path, text: &str, format: catalog::Format) -> Result<(), String> {
    fs::create_dir_all(data_dir).map_err(|error| format!("{}: {error}", data_dir.display()))?;
    let name = format!("catalog.{}", format.extension());
    let temporary = data_dir.join(format!("{name}.tmp"));
    let target = data_dir.join(&name);
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

/// Job ids are wall-clock milliseconds; a second job that starts in the same
/// millisecond still needs its own id, because the id is what tells a finishing
/// transfer whether it still owns the job (and names its transfer file).
fn next_job_id() -> u64 {
    static LAST: AtomicU64 = AtomicU64::new(0);
    loop {
        let last = LAST.load(Ordering::Relaxed);
        let next = now_millis().max(last.saturating_add(1));
        if LAST
            .compare_exchange(last, next, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
        {
            return next;
        }
    }
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
    let exe = std::env::current_exe().ok();
    let plugin_dir = context.plugin_dir().map(PathBuf::from);
    let data_dir = plugin_data_dir(exe.as_deref(), plugin_dir.as_deref(), OWN_ID);
    // The catalog cache and the in-flight downloads both live in `data/`, which
    // the host keeps across upgrades; the version directories do not survive.
    let download_dir = data_dir.join("temp");
    // Nothing can be downloading at startup, so any transfer still here belongs
    // to a process that died mid-download (or to a version that shared `.part`).
    sweep_transfers(&download_dir);
    // Settings first: they decide which cache shape to read and which source
    // the refresher asks. The host's `config` sits in the plugin directory, not
    // in `data/`; `SVCD_CONFIG` is only exported to config-check runs.
    let config = context
        .config_path()
        .map(PathBuf::from)
        .or_else(|| plugin_root(exe.as_deref(), plugin_dir.as_deref(), OWN_ID).map(|root| root.join("config")));
    let settings = settings::Settings::load(config.as_deref(), &data_dir);
    let state = Arc::new(Mutex::new(initial_state(&data_dir, &settings)));
    log_info!(
        "plugin-store 启动: target={TARGET}, 内置/缓存 {} 个包, data={}, downloads={}",
        state.lock().map(|guard| guard.entries.len()).unwrap_or(0),
        data_dir.display(),
        download_dir.display()
    );
    let temp_dir = context
        .temp_dir()
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("plugin-store-temp"));
    let cancel = context.cancel();
    spawn_refresher(
        Arc::clone(&state),
        data_dir.clone(),
        cancel.clone(),
        settings.clone(),
    );

    svcd_plugin::serve(
        context,
        PluginStore {
            state,
            settings,
            data_dir,
            download_dir,
            temp_dir,
            own_id,
            cancel,
            api_base: release::API.to_string(),
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
    fn the_configured_source_comes_first_and_the_other_stays_a_fallback() {
        // github only: every URL is a Markdown mirror, and nothing else.
        let urls = catalog_sources(&settings::Settings::default());
        assert_eq!(urls.len(), CATALOG_URLS.len());
        assert!(urls
            .iter()
            .all(|source| source.format == catalog::Format::Markdown));

        // qiniu chosen: index.json first, GitHub behind it.
        let qiniu = settings::Settings {
            catalog_source: settings::CatalogSource::Qiniu,
            qiniu_base: Some("https://dl.example.com".into()),
            ..settings::Settings::default()
        };
        let urls = catalog_sources(&qiniu);
        assert_eq!(urls[0].url, "https://dl.example.com/index.json");
        assert_eq!(urls[0].format, catalog::Format::Json);
        assert_eq!(urls[1].format, catalog::Format::Markdown);
        assert_eq!(urls.len(), CATALOG_URLS.len() + 1);

        // qiniu chosen but not configured: GitHub only, no empty first entry.
        let broken = settings::Settings {
            catalog_source: settings::CatalogSource::Qiniu,
            ..settings::Settings::default()
        };
        let urls = catalog_sources(&broken);
        assert_eq!(urls.len(), CATALOG_URLS.len());
        assert_eq!(urls[0].format, catalog::Format::Markdown);

        // github chosen with a base: the mirror is still worth a try when raw
        // is blocked, so it goes last.
        let both = settings::Settings {
            qiniu_base: Some("https://dl.example.com".into()),
            ..settings::Settings::default()
        };
        let urls = catalog_sources(&both);
        assert_eq!(urls[0].format, catalog::Format::Markdown);
        assert_eq!(
            urls.last().map(|source| source.url.as_str()),
            Some("https://dl.example.com/index.json")
        );
    }

    #[test]
    fn a_qiniu_source_is_fetched_parsed_and_kept_in_its_own_format() {
        use std::io::{Read as _, Write as _};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let body = r#"{"schema":1,"packages":[{"plugin":"meow","version":"1.0.2",
            "platform":"x86_64-unknown-linux-gnu","size":2048,
            "url":"https://dl.example.com/meow-1.0.2-x86_64-unknown-linux-gnu.spk"}]}"#
            .to_string();
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
            stream.write_all(server_body.as_bytes()).unwrap();
        });

        let sources = vec![Source {
            url: format!("http://{address}/index.json"),
            format: catalog::Format::Json,
        }];
        let fetched = fetch_from(&[], &sources).expect("the local index answers");
        server.join().unwrap();

        assert_eq!(fetched.format, catalog::Format::Json);
        assert_eq!(fetched.text, body);
        assert_eq!(fetched.url, format!("http://{address}/index.json"));
        assert_eq!(fetched.entries.len(), 1);
        assert_eq!(fetched.entries[0].name, "meow");
        assert_eq!(fetched.entries[0].size, "2.0 KiB");
    }

    #[test]
    fn an_http_source_may_point_at_http_packages() {
        use std::io::{Read as _, Write as _};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let body = r#"{"schema":1,"packages":[{"plugin":"meow","version":"1.0.2",
            "platform":"x86_64-unknown-linux-gnu","size":1024,
            "url":"http://127.0.0.1:1/meow-1.0.2-x86_64-unknown-linux-gnu.spk"}]}"#
            .to_string();
        let server_body = body.clone();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 1024];
            let _ = stream.read(&mut request);
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                server_body.len(),
                server_body
            )
            .unwrap();
        });

        // The source itself is http, so plaintext packages are the same
        // bargain and are not refused.
        let sources = vec![Source {
            url: format!("http://{address}/index.json"),
            format: catalog::Format::Json,
        }];
        let fetched = fetch_from(&[], &sources).expect("plaintext mirror is usable");
        server.join().unwrap();
        assert!(fetched.entries[0].url.starts_with("http://"));

        // The same bytes cached while the user was on http become unusable the
        // moment `qiniu_base` goes back to https.
        let dir = std::env::temp_dir().join(format!(
            "plugin-store-http-cache-{}-{}",
            std::process::id(),
            now_millis()
        ));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("catalog.json"), &body).unwrap();
        assert!(cached_state(&dir, catalog::Format::Json, false).is_some());
        assert!(cached_state(&dir, catalog::Format::Json, true).is_none());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_source_that_answers_with_the_wrong_shape_is_skipped() {
        use std::io::{Read as _, Write as _};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        // A Markdown table behind an `index.json` URL: rejected, not guessed.
        let body = "# 插件下载\n\n| 插件 | 版本 |\n| --- | --- |\n";
        let server_body = body.to_string();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 1024];
            let _ = stream.read(&mut request);
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                server_body.len(),
                server_body
            )
            .unwrap();
        });

        let sources = vec![Source {
            url: format!("http://{address}/index.json"),
            format: catalog::Format::Json,
        }];
        let error = fetch_from(&[], &sources).unwrap_err();
        server.join().unwrap();
        assert!(error.contains("JSON"), "{error}");
    }

    #[test]
    fn switching_the_source_prefers_its_own_cache_but_keeps_the_other() {
        let dir = std::env::temp_dir().join(format!(
            "plugin-store-cache-{}-{}",
            std::process::id(),
            now_millis()
        ));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("catalog.json"),
            r#"{"schema":1,"packages":[{"plugin":"from-qiniu","version":"2.0.0",
                "platform":"x86_64-unknown-linux-gnu","size":1024,
                "url":"https://dl.example.com/from-qiniu-2.0.0-x86_64-unknown-linux-gnu.spk"}]}"#,
        )
        .unwrap();
        fs::write(
            dir.join("catalog.md"),
            "| 插件 | 版本 | 平台 | 大小 | 下载地址 |\n| --- | --- | --- | ---: | --- |\n| from-github | 1.0.0 | x86_64-unknown-linux-gnu | 1.0 KiB | [a](https://github.com/o/r/releases/download/v1/from-github-1.0.0-x86_64-unknown-linux-gnu.spk) |\n",
        )
        .unwrap();

        let qiniu = settings::Settings {
            catalog_source: settings::CatalogSource::Qiniu,
            ..settings::Settings::default()
        };
        let state = initial_state(&dir, &qiniu);
        assert_eq!(state.source, "cache");
        assert_eq!(state.entries[0].name, "from-qiniu");

        let github = settings::Settings::default();
        let state = initial_state(&dir, &github);
        assert_eq!(state.entries[0].name, "from-github");

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn data_lives_under_the_plugin_root() {
        // The installed payload path is <plugin>/<version>/payload/bin/<exe>.
        let installed = Path::new("/data/plugins/plugin-store/0.1.8/payload/bin/plugin-store");
        assert_eq!(
            plugin_data_dir(Some(installed), None, "plugin-store"),
            Path::new("/data/plugins/plugin-store/data"),
            "a payload walks up to its plugin directory"
        );
        // The host exports the plugin directory; it wins over the payload path.
        assert_eq!(
            plugin_data_dir(
                Some(installed),
                Some(Path::new("/srv/plugins/plugin-store")),
                "plugin-store"
            ),
            Path::new("/srv/plugins/plugin-store/data"),
            "SVCD_PLUGIN_DIR is where the host put us"
        );
        // A binary built by hand is not inside a package; nothing may land next
        // to it in the source tree.
        let hand_built = Path::new("/repo/target/release/plugin-store");
        assert_eq!(
            plugin_data_dir(Some(hand_built), None, "plugin-store"),
            std::env::temp_dir().join("plugin-store")
        );
        assert_eq!(
            plugin_data_dir(None, None, "plugin-store"),
            std::env::temp_dir().join("plugin-store")
        );
        // Settings live in the plugin directory, not in data/.
        assert_eq!(
            plugin_root(Some(installed), None, "plugin-store"),
            Some(Path::new("/data/plugins/plugin-store").to_path_buf())
        );
        assert_eq!(plugin_root(Some(hand_built), None, "plugin-store"), None);
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

    fn job(phase: &str) -> Job {
        Job {
            id: 1,
            name: "meow".into(),
            version: "1.0.1".into(),
            platform: "x86_64-unknown-linux-gnu".into(),
            device: None,
            phase: phase.into(),
            downloaded: 0,
            total: None,
            message: String::new(),
            done: false,
            ok: false,
            started_at: 0,
        }
    }

    #[test]
    fn delete_is_allowed_until_the_host_owns_the_install() {
        assert!(can_delete(&job("queued")));
        assert!(can_delete(&job("downloading")));
        assert!(can_delete(&job("ready")));
        assert!(can_delete(&job("error")));
        assert!(can_delete(&job("done")));
        assert!(!can_delete(&job("installing")));
    }

    #[test]
    fn a_download_stops_when_its_job_is_deleted() {
        let state = Arc::new(Mutex::new(State {
            entries: Vec::new(),
            source: "test".into(),
            source_url: None,
            fetched_at: None,
            last_error: None,
            refreshing: false,
            job: Some(job("downloading")),
        }));
        assert!(job_is_current(&state, 1));
        state.lock().unwrap().job = None;
        assert!(!job_is_current(&state, 1));
    }

    #[test]
    fn a_failed_install_drops_the_downloaded_package() {
        let dir = std::env::temp_dir().join(format!(
            "plugin-store-failed-{}-{}",
            std::process::id(),
            now_millis()
        ));
        fs::create_dir_all(&dir).unwrap();
        let job = Job {
            id: 7,
            name: "hysteria".into(),
            version: "16.0.0".into(),
            platform: "armv7-unknown-linux-musleabihf".into(),
            device: None,
            phase: "installing".into(),
            downloaded: 15,
            total: Some(15),
            message: String::new(),
            done: false,
            ok: false,
            started_at: 0,
        };
        let store = store_with(&dir, job);
        let package = dir.join("hysteria-16.0.0-armv7-unknown-linux-musleabihf.spk");
        fs::write(&package, b"spk").unwrap();
        let transfer = dir.join("hysteria-16.0.0-armv7-unknown-linux-musleabihf.spk.7.download");
        fs::write(&transfer, b"spk").unwrap();
        fs::write(net::part_path(&transfer), b"part").unwrap();
        // A newer job's transfer must survive: cleanup owns one job, not a path.
        let newer = dir.join("hysteria-16.0.0-armv7-unknown-linux-musleabihf.spk.8.download");
        fs::write(&newer, b"spk").unwrap();

        let outcome = store.install_result(&json!({"ok": false, "message": "安装失败"}));
        assert!(
            !matches!(outcome, svcd_plugin::ToolOutcome::Error(_)),
            "install_result must succeed for a job that exists"
        );
        assert!(
            !package.exists(),
            "a failed install must not keep the downloaded package"
        );
        assert!(!transfer.exists(), "its transfer belongs to the same job");
        assert!(
            !net::part_path(&transfer).exists(),
            "its .part belongs to the same transfer"
        );
        assert!(
            newer.exists(),
            "a newer job's transfer is not ours to delete"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// #18: a deleted job must not unlink the bytes of the job after it.
    ///
    /// 删除 clears `state.job`, but the download thread only notices at its next
    /// chunk — seconds later on a stalled link. The next job starts meanwhile,
    /// and the old thread's path-based cleanup used to unlink the shared
    /// `<file>.spk.part`: the new download climbed to 100% and then
    /// `rename(part, dest)` failed with "No such file or directory".
    #[test]
    fn a_deleted_download_does_not_break_the_next_one() {
        use std::io::{Read as _, Write as _};
        use std::net::TcpListener;
        use std::sync::Condvar;

        const CHUNK: usize = 4096;
        let body: Vec<u8> = (0..CHUNK * 2).map(|index| (index % 251) as u8).collect();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        // Each connection sends one chunk, then waits for this test to release
        // it, so the first download can sit in a read while the second starts.
        let gate = Arc::new((Mutex::new(Vec::<usize>::new()), Condvar::new()));
        let server_gate = Arc::clone(&gate);
        let serve_body = body.clone();
        let server = thread::spawn(move || {
            let mut index = 0usize;
            for connection in listener.incoming().take(2) {
                let Ok(mut stream) = connection else {
                    return;
                };
                index += 1;
                let gate = Arc::clone(&server_gate);
                let chunk = serve_body.clone();
                let position = index - 1;
                thread::spawn(move || {
                    let mut request = [0u8; 1024];
                    let _ = stream.read(&mut request);
                    let headers = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        chunk.len()
                    );
                    if stream.write_all(headers.as_bytes()).is_err()
                        || stream.write_all(&chunk[..CHUNK]).is_err()
                    {
                        return;
                    }
                    let _ = stream.flush();
                    let (lock, ready) = &*gate;
                    let mut released = lock.lock().unwrap();
                    while !released.contains(&position) {
                        released = ready.wait(released).unwrap();
                    }
                    drop(released);
                    let _ = stream.write_all(&chunk[CHUNK..]);
                    let _ = stream.flush();
                });
            }
        });

        let dir = std::env::temp_dir().join(format!(
            "plugin-store-race-{}-{}",
            std::process::id(),
            now_millis()
        ));
        fs::create_dir_all(&dir).unwrap();
        let store = store_with(&dir, job("done"));
        {
            let mut state = store.state.lock().unwrap();
            state.job = None;
            state.entries = vec![catalog::Entry {
                name: "hysteria".into(),
                version: "1.0.0".into(),
                platform: "android-arm64".into(),
                size: "15.7 MiB".into(),
                size_bytes: body.len() as u64,
                url: format!("http://{address}/package.spk"),
                device: None,
            }];
        }

        let first = start(&store);
        wait_for(&store, first, |job| job.downloaded > 0);
        // Give the first thread time to park in its read.
        thread::sleep(Duration::from_millis(100));
        assert!(matches!(store.cancel_install(), ToolOutcome::Text(_)));

        let second = start(&store);
        // Progress is published every 250 ms, so wait for the phase and give the
        // second thread time to create its part and park in its first read.
        wait_for(&store, second, |job| job.phase == "downloading");
        thread::sleep(Duration::from_millis(150));

        // The first thread wakes up here and runs its cleanup, then the second
        // transfer is allowed to finish.
        release(&gate, 0);
        thread::sleep(Duration::from_millis(200));
        release(&gate, 1);

        let finished = wait_for(&store, second, |job| job.done || job.phase == "ready");
        assert_eq!(
            finished.phase, "ready",
            "重新下载必须完成，而不是报 os error 2：{}",
            finished.message
        );
        assert_eq!(
            fs::read(dir.join("hysteria-1.0.0-android-arm64.spk")).unwrap(),
            body,
            "缓存里必须是完整的包"
        );

        server.join().unwrap();
        let _ = fs::remove_dir_all(&dir);
    }

    fn start(store: &PluginStore) -> u64 {
        let outcome = store.start_install(&json!({
            "name": "hysteria",
            "version": "1.0.0",
            "platform": "android-arm64"
        }));
        assert!(
            !matches!(outcome, ToolOutcome::Error(_)),
            "start_install must accept a new job"
        );
        store
            .state
            .lock()
            .unwrap()
            .job
            .as_ref()
            .map(|job| job.id)
            .expect("a job is published before the thread starts")
    }

    fn wait_for(store: &PluginStore, id: u64, mut check: impl FnMut(&Job) -> bool) -> Job {
        let mut last = None;
        for _ in 0..600 {
            if let Some(job) = store.state.lock().unwrap().job.clone() {
                if job.id == id && check(&job) {
                    return job;
                }
                last = Some(job_json(&job));
            }
            thread::sleep(Duration::from_millis(10));
        }
        panic!("job {id} never reached the expected state; last seen: {last:?}");
    }

    fn release(gate: &(Mutex<Vec<usize>>, std::sync::Condvar), index: usize) {
        let (lock, ready) = gate;
        lock.lock().unwrap().push(index);
        ready.notify_all();
    }

    fn package_entry(name: &str, version: &str, platform: &str) -> catalog::Entry {
        catalog::Entry {
            name: name.into(),
            version: version.into(),
            platform: platform.into(),
            size: "15.7 MiB".into(),
            size_bytes: 15,
            url: "https://example.com/x.spk".into(),
            device: None,
        }
    }

    /// #291: the handoff moves the download instead of copying it (a second
    /// 15.7 MiB does not fit a small `/data`), and a stale target from an
    /// earlier attempt is replaced rather than left next to the real package.
    #[test]
    fn package_blob_moves_the_download_into_incoming() {
        let dir = std::env::temp_dir().join(format!(
            "plugin-store-blob-{}-{}",
            std::process::id(),
            now_millis()
        ));
        fs::create_dir_all(&dir).unwrap();
        let job = Job {
            id: 1,
            name: "hysteria".into(),
            version: "16.0.0".into(),
            platform: "armv7-unknown-linux-musleabihf".into(),
            device: None,
            phase: "ready".into(),
            downloaded: 3,
            total: Some(3),
            message: String::new(),
            done: false,
            ok: false,
            started_at: 0,
        };
        let store = store_with(&dir, job);
        store.state.lock().unwrap().entries.push(package_entry(
            "hysteria",
            "16.0.0",
            "armv7-unknown-linux-musleabihf",
        ));
        let package = dir.join("hysteria-16.0.0-armv7-unknown-linux-musleabihf.spk");
        fs::write(&package, b"spk").unwrap();
        let incoming = dir.join("incoming");
        fs::create_dir_all(&incoming).unwrap();
        let stale = incoming.join("hysteria-16.0.0-armv7-unknown-linux-musleabihf.spk");
        fs::write(&stale, b"stale").unwrap();

        let outcome = store.package_blob(&json!({
            "name": "hysteria",
            "version": "16.0.0",
            "platform": "armv7-unknown-linux-musleabihf"
        }));
        match outcome {
            svcd_plugin::ToolOutcome::Blob { path, .. } => {
                assert_eq!(
                    fs::read(&path).unwrap(),
                    b"spk",
                    "the stale target must be replaced by the download"
                );
            }
            _ => panic!("package_blob must hand the package over as a blob"),
        }
        assert!(
            !package.exists(),
            "the download is moved into incoming, not copied"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    fn store_with(dir: &Path, job: Job) -> PluginStore {
        PluginStore {
            settings: settings::Settings::default(),
            state: Arc::new(Mutex::new(State {
                entries: Vec::new(),
                source: "test".into(),
                source_url: None,
                fetched_at: None,
                last_error: None,
                refreshing: false,
                job: Some(job),
            })),
            data_dir: dir.to_path_buf(),
            download_dir: dir.to_path_buf(),
            temp_dir: dir.to_path_buf(),
            own_id: "plugin-store".into(),
            cancel: Cancel::default(),
            api_base: release::API.into(),
        }
    }

    #[test]
    fn list_marks_the_store_itself() {
        let dir = std::env::temp_dir().join(format!(
            "plugin-store-list-{}-{}",
            std::process::id(),
            now_millis()
        ));
        fs::create_dir_all(&dir).unwrap();
        let store = store_with(&dir, job("done"));
        {
            let mut state = store.state.lock().unwrap();
            state.job = None;
            state.entries = vec![catalog::Entry {
                name: "plugin-store".into(),
                version: "0.1.9".into(),
                platform: "x86_64-unknown-linux-gnu".into(),
                size: "1.8 MiB".into(),
                size_bytes: 1_873_408,
                url: "https://example.test/plugin-store-0.1.9.spk".into(),
                device: None,
            }];
        }

        match store.list(&json!({ "scope": "all", "platform": "x86_64-unknown-linux-gnu" })) {
            ToolOutcome::Text(text) => {
                let body: Value = serde_json::from_str(&text).expect("list returns JSON");
                assert_eq!(body["self"], "plugin-store");
                assert_eq!(body["total"], 1);
            }
            _ => panic!("expected a text result"),
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn cancel_install_clears_the_job_and_its_files() {
        let dir = std::env::temp_dir().join(format!(
            "plugin-store-cancel-{}-{}",
            std::process::id(),
            now_millis()
        ));
        fs::create_dir_all(&dir).unwrap();
        let store = store_with(&dir, job("downloading"));
        let package = dir.join("meow-1.0.1-x86_64-unknown-linux-gnu.spk");
        fs::write(&package, b"spk").unwrap();
        let transfer = dir.join("meow-1.0.1-x86_64-unknown-linux-gnu.spk.1.download");
        fs::write(&transfer, b"spk").unwrap();
        fs::write(net::part_path(&transfer), b"spk").unwrap();

        match store.cancel_install() {
            ToolOutcome::Text(text) => assert!(text.contains("\"ok\":true"), "{text}"),
            _ => panic!("expected a text result"),
        }
        assert!(store.state.lock().unwrap().job.is_none());
        assert!(!package.exists());
        assert!(!transfer.exists());
        assert!(!net::part_path(&transfer).exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn cancel_install_is_refused_once_the_host_installs() {
        let dir = std::env::temp_dir().join(format!(
            "plugin-store-installing-{}-{}",
            std::process::id(),
            now_millis()
        ));
        fs::create_dir_all(&dir).unwrap();
        let store = store_with(&dir, job("installing"));

        match store.cancel_install() {
            ToolOutcome::Error(text) => assert!(text.contains("无法删除"), "{text}"),
            _ => panic!("expected an error result"),
        }
        assert_eq!(
            store.state.lock().unwrap().job.as_ref().map(|job| job.id),
            Some(1)
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn remove_download_deletes_packages_and_part_files() {
        let dir = std::env::temp_dir().join(format!(
            "plugin-store-remove-{}-{}",
            std::process::id(),
            now_millis()
        ));
        fs::create_dir_all(&dir).unwrap();
        let package = dir.join("meow-1.0.1-x86_64-unknown-linux-gnu.spk");
        fs::write(&package, b"spk").unwrap();
        let part = net::part_path(&package);
        fs::write(&part, b"spk").unwrap();

        remove_download(&package);
        remove_download(&part);
        assert!(!package.exists());
        assert!(!part.exists());
        // Deleting an already-gone file is not an error.
        remove_download(&package);
        let _ = fs::remove_dir_all(&dir);
    }

    /// #18: transfers left by a process that died mid-download (or the shared
    /// `.part` older versions wrote) are cleared at startup; the cache is data,
    /// so it stays.
    #[test]
    fn startup_clears_transfers_but_keeps_the_cache() {
        let dir = std::env::temp_dir().join(format!(
            "plugin-store-sweep-{}-{}",
            std::process::id(),
            now_millis()
        ));
        fs::create_dir_all(&dir).unwrap();
        let cache = dir.join("meow-1.0.1-x86_64-unknown-linux-gnu.spk");
        let transfer = dir.join("meow-1.0.1-x86_64-unknown-linux-gnu.spk.7.download");
        let part = dir.join("meow-1.0.1-x86_64-unknown-linux-gnu.spk.7.download.part");
        let legacy = dir.join("meow-1.0.1-x86_64-unknown-linux-gnu.spk.part");
        for path in [&cache, &transfer, &part, &legacy] {
            fs::write(path, b"spk").unwrap();
        }

        sweep_transfers(&dir);

        assert!(cache.exists(), "the cache is not a transfer");
        assert!(!transfer.exists());
        assert!(!part.exists());
        assert!(!legacy.exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn two_jobs_never_share_an_id() {
        let first = next_job_id();
        let second = next_job_id();
        assert!(second > first, "{second} must outrank {first}");
    }

    #[test]
    fn release_asset_rejects_bad_arguments_before_touching_the_network() {
        let dir = std::env::temp_dir().join(format!(
            "plugin-store-release-args-{}-{}",
            std::process::id(),
            now_millis()
        ));
        fs::create_dir_all(&dir).unwrap();
        let store = store_with(&dir, job("done"));

        for arguments in [
            json!({ "download": "yes" }),
            json!({ "repo": 7 }),
            json!({ "asset": 3 }),
            json!({ "download": true }),
            json!({ "download": true, "asset": "  " }),
        ] {
            match store.release_asset(&arguments) {
                ToolOutcome::Error(text) => assert!(!text.is_empty(), "{arguments}"),
                ToolOutcome::Text(text) => panic!("expected an error for {arguments}: {text}"),
                ToolOutcome::Blob { .. } => panic!("expected an error for {arguments}"),
            }
        }

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn release_asset_can_read_the_daemon_from_the_mirror() {
        use std::io::{Read as _, Write as _};
        use std::net::TcpListener;
        use std::sync::atomic::{AtomicBool, Ordering};

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let archive = b"svcd-zip-payload".to_vec();
        let sums = b"aa11  svcd-0.1.5-x86_64-pc-windows-msvc.zip\n".to_vec();
        let index = json!({
            "schema": 1,
            "svcd_sums_url": format!("http://{address}/SHA256SUMS"),
            "svcd": [{
                "version": "0.1.5",
                "target": "x86_64-pc-windows-msvc",
                "file": "svcd-0.1.5-x86_64-pc-windows-msvc.zip",
                "size": archive.len(),
                "url": format!("http://{address}/archive.zip")
            }]
        })
        .to_string();
        let stop = Arc::new(AtomicBool::new(false));
        let server_stop = Arc::clone(&stop);
        let (server_archive, server_sums, server_index) =
            (archive.clone(), sums.clone(), index.clone());
        let server = thread::spawn(move || {
            listener.set_nonblocking(true).unwrap();
            while !server_stop.load(Ordering::SeqCst) {
                let Ok((mut stream, _)) = listener.accept() else {
                    thread::sleep(std::time::Duration::from_millis(5));
                    continue;
                };
                let mut request = [0u8; 2048];
                let read = stream.read(&mut request).unwrap_or(0);
                let request = String::from_utf8_lossy(&request[..read]);
                let path = request.split_whitespace().nth(1).unwrap_or("/");
                let body = if path.ends_with("/index.json") {
                    server_index.clone().into_bytes()
                } else if path.ends_with("/SHA256SUMS") {
                    server_sums.clone()
                } else {
                    server_archive.clone()
                };
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(&body);
            }
        });

        let dir = std::env::temp_dir().join(format!(
            "plugin-store-mirror-svcd-{}-{}",
            std::process::id(),
            address.port()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let store = PluginStore {
            settings: settings::Settings {
                catalog_source: settings::CatalogSource::Qiniu,
                qiniu_base: Some(format!("http://{address}")),
                ..settings::Settings::default()
            },
            ..store_with(&dir, job("done"))
        };

        match store.release_asset(&json!({})) {
            ToolOutcome::Text(text) => {
                let value: Value = serde_json::from_str(&text).unwrap();
                assert_eq!(value["tag"], "v0.1.5");
                assert_eq!(value["version"], "0.1.5");
                assert_eq!(value["sumsAsset"], "SHA256SUMS");
                assert_eq!(value["assets"].as_array().unwrap().len(), 2);
            }
            _ => panic!("the metadata call must answer with JSON text"),
        }

        match store.release_asset(
            &json!({"download": true, "asset": "svcd-0.1.5-x86_64-pc-windows-msvc.zip"}),
        ) {
            ToolOutcome::Blob { path, name, .. } => {
                assert_eq!(name, "svcd-0.1.5-x86_64-pc-windows-msvc.zip");
                assert_eq!(fs::read(path).unwrap(), archive);
            }
            _ => panic!("download must answer with a blob"),
        }

        stop.store(true, Ordering::SeqCst);
        server.join().unwrap();
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_mirror_without_svcd_falls_back_to_github() {
        use std::io::{Read as _, Write as _};
        use std::net::TcpListener;
        use std::sync::atomic::{AtomicBool, Ordering};

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let index = json!({"schema": 1, "packages": [], "svcd": []}).to_string();
        let github = json!({
            "tag_name": "v0.2.0",
            "draft": false,
            "prerelease": false,
            "assets": [{
                "name": "svcd-0.2.0-x86_64-pc-windows-msvc.zip",
                "size": 10,
                "browser_download_url": format!("http://{address}/a.zip")
            }]
        })
        .to_string();
        let stop = Arc::new(AtomicBool::new(false));
        let server_stop = Arc::clone(&stop);
        let (server_index, server_github) = (index.clone(), github.clone());
        let server = thread::spawn(move || {
            listener.set_nonblocking(true).unwrap();
            while !server_stop.load(Ordering::SeqCst) {
                let Ok((mut stream, _)) = listener.accept() else {
                    thread::sleep(std::time::Duration::from_millis(5));
                    continue;
                };
                let mut request = [0u8; 2048];
                let read = stream.read(&mut request).unwrap_or(0);
                let request = String::from_utf8_lossy(&request[..read]);
                let path = request.split_whitespace().nth(1).unwrap_or("/");
                let body = if path.ends_with("/index.json") {
                    server_index.clone().into_bytes()
                } else {
                    server_github.clone().into_bytes()
                };
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(&body);
            }
        });

        let dir = std::env::temp_dir().join(format!(
            "plugin-store-mirror-fallback-{}-{}",
            std::process::id(),
            address.port()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let store = PluginStore {
            settings: settings::Settings {
                catalog_source: settings::CatalogSource::Qiniu,
                qiniu_base: Some(format!("http://{address}")),
                ..settings::Settings::default()
            },
            api_base: format!("http://{address}"),
            ..store_with(&dir, job("done"))
        };

        match store.release_asset(&json!({})) {
            ToolOutcome::Text(text) => {
                let value: Value = serde_json::from_str(&text).unwrap();
                // The mirror had nothing, so the answer came from GitHub.
                assert_eq!(value["tag"], "v0.2.0");
                assert_eq!(
                    value["assets"][0]["name"],
                    "svcd-0.2.0-x86_64-pc-windows-msvc.zip"
                );
            }
            _ => panic!("the metadata call must answer with JSON text"),
        }

        stop.store(true, Ordering::SeqCst);
        server.join().unwrap();
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn release_asset_returns_exactly_one_blob_per_download() {
        use std::io::{Read as _, Write as _};
        use std::net::TcpListener;
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::thread;
        use std::time::Duration;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let archive = b"zip-payload".to_vec();
        let sums = b"aa11  svcd-0.1.5-x86_64-pc-windows-msvc.zip\n".to_vec();
        let release_body = json!({
            "tag_name": "v0.1.5",
            "draft": false,
            "prerelease": false,
            "assets": [
                {
                    "name": "SHA256SUMS",
                    "size": sums.len(),
                    "browser_download_url": format!("http://{address}/download/SHA256SUMS")
                },
                {
                    "name": "svcd-0.1.5-x86_64-pc-windows-msvc.zip",
                    "size": archive.len(),
                    "browser_download_url": format!("http://{address}/download/archive.zip")
                }
            ]
        })
        .to_string();
        let stop = Arc::new(AtomicBool::new(false));
        let server_stop = Arc::clone(&stop);
        let server_archive = archive.clone();
        let server_sums = sums.clone();
        let server = thread::spawn(move || {
            listener.set_nonblocking(true).unwrap();
            while !server_stop.load(Ordering::SeqCst) {
                let Ok((mut stream, _)) = listener.accept() else {
                    thread::sleep(Duration::from_millis(5));
                    continue;
                };
                let mut request = [0u8; 2048];
                let read = stream.read(&mut request).unwrap_or(0);
                let request = String::from_utf8_lossy(&request[..read]);
                let path = request.split_whitespace().nth(1).unwrap_or("/");
                let (body, content_type) = if path.ends_with("/releases/latest") {
                    (release_body.clone().into_bytes(), "application/json")
                } else if path.ends_with("/SHA256SUMS") {
                    (server_sums.clone(), "text/plain")
                } else {
                    (server_archive.clone(), "application/octet-stream")
                };
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(&body);
            }
        });

        let dir = std::env::temp_dir().join(format!(
            "plugin-store-release-tool-{}-{}",
            std::process::id(),
            address.port()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let store = PluginStore {
            api_base: format!("http://{address}"),
            ..store_with(&dir, job("done"))
        };

        match store.release_asset(&json!({})) {
            ToolOutcome::Text(text) => {
                let value: Value = serde_json::from_str(&text).unwrap();
                assert_eq!(value["tag"], "v0.1.5");
                assert_eq!(value["version"], "0.1.5");
                assert_eq!(value["sumsAsset"], "SHA256SUMS");
                assert_eq!(value["assets"].as_array().unwrap().len(), 2);
            }
            _ => panic!("the metadata call must answer with JSON text"),
        }

        match store.release_asset(&json!({
            "download": true,
            "asset": "svcd-0.1.5-x86_64-pc-windows-msvc.zip"
        })) {
            ToolOutcome::Blob {
                path,
                name,
                mime_type,
            } => {
                assert_eq!(name, "svcd-0.1.5-x86_64-pc-windows-msvc.zip");
                assert_eq!(mime_type, "application/octet-stream");
                assert_eq!(fs::read(path).unwrap(), archive);
            }
            _ => panic!("a download must be exactly one blob"),
        }

        match store.release_asset(&json!({ "download": true, "asset": "SHA256SUMS" })) {
            ToolOutcome::Blob { path, .. } => assert_eq!(fs::read(path).unwrap(), sums),
            _ => panic!("the checksum list must be exactly one blob"),
        }

        stop.store(true, Ordering::SeqCst);
        server.join().unwrap();
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn builtin_snapshot_has_rows() {
        let entries = catalog::parse(SNAPSHOT).expect("snapshot parses");
        assert!(entries.len() >= 3);
    }
}
