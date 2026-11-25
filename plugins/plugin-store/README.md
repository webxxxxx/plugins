# plugin-store — 插件商店

一个 Rust 插件：读取 docs/plugins.md 的发布表，只显示当前设备平台可安装的包，支持按名称搜索、每页 50 个、下载后安装。

## 界面

- 设备平台与 device id 来自宿主 GET /api/v1/compat（plugin-manager 的同一份 HostFacts）；
  只显示 platform 精确匹配的包，没有「全部平台」开关。
- 包名带 device 标识时按 device 过滤：优先解析文件名
  <id>-<version>-<platform>-<device>.spk 的平台后后缀（`id` 是插件的 ASCII 身份；`name` 只是显示名），也兼容 device 放在包名/文件名
  version 两侧的写法；只有宿主的 device id 相同才显示，无标识视为该 platform 的通用包。
  当前 docs 生成器也已支持平台后的 device 后缀，避免设备包在列表里被丢掉。
- 每页固定 50 条；搜索只匹配插件名，输入即筛（250 ms 防抖）。
- 已安装版本低于目录版本时，操作列显示「升级」；相同或更高时显示「已安装」并禁用。名字后面只在装了**别的**版本时标「已安装 x.y.z」，与目录同版本时不再重复标（操作列已经说明）。
- 插件商店自己也在列表里，带「自己」标记。它的「升级」会下载自己的新包并交给宿主替换：宿主停掉旧进程后由页面启动新版本并自动刷新。升级期间本页的安装任务会随旧进程消失，其他标签页会收到任务清空。
- 列表来自本地缓存或内置快照，启动后先在后台刷新；「刷新列表」只触发后台任务，不会卡住页面。
- 安装按钮先由插件下载 .spk 到数据目录，再通过宿主现成的 API 安装并启动：

      package_blob  ->  宿主把文件变成 $blob
      GET  /api/v1/disk?id={id}           装之前问空间（旧版占用 + 新包大小）
      GET  /api/v1/blobs/{id}             取回字节
      DELETE /api/v1/blobs/{id}           字节到手后立刻释放宿主那份
      POST /api/v1/plugins                与 management 页面相同的安装入口
      POST /api/v1/services/{id}/start    首装不自启，装完启动

  插件不调 CLI、不自起 HTTP、不碰宿主 socket；签名和平台校验仍由 svcd 完成。
- **装到哪个目录不弹框**：只有一份插件根（或就只有默认根）时没有可选项；多根时宿主自己按
  「装得下且剩余空间最小」挑（svcd 的 `plugins_dir` 列表）。页面从不提示选路径。
- **唯一会弹的框是空间不够**（升级时旧版还在，新包解到同一卷会满）：文案固定为
  「空间不够，需要先卸载旧版再安装 / 安装需要 X，现在空闲 Y， 建议先导出备份，如果升级失败，
  重新上传安装。」三个按钮：取消、导出、卸载并安装。导出走 `GET /api/v1/export/{id}/spk`，
  由宿主流式生成下载（不占设备磁盘）；卸载并安装先 `DELETE /api/v1/plugins/{id}`（不 purge，
  配置保留）再走正常安装。`GET /api/v1/disk?id=` 的 `plugin.bytes` 是旧版占用，
  `freeBytes` 是那一卷的空闲；`freeBytes < 新包 + 旧版` 才弹，`freeBytes < 新包` 则直接报
  「空间不够」——那种情况卸载也装不下。

## 数据与下载

列表有两个来源，由配置里的 `catalog_source` 选优先级：GitHub 的 `docs/plugins.md`
（下面那串地址）或七牛空间根目录的 `index.json`（`qiniu_base`）。选中的先问，另一个排在它后面
当兜底——所以 `qiniu_base` 写错或 GitHub 不可达时，商店退化到另一份列表，而不是变成空页。

GitHub 这份的地址按顺序尝试，取**第一个不比手里这份旧**的：

1. https://raw.githubusercontent.com/webxxxxx/plugins/main/docs/plugins.md
2. https://raw.githack.com/webxxxxx/plugins/main/docs/plugins.md
3. https://cdn.statically.io/gh/webxxxxx/plugins/main/docs/plugins.md
4. https://cdn.jsdelivr.net/gh/webxxxxx/plugins@main/docs/plugins.md
5. https://fastly.jsdelivr.net/gh/webxxxxx/plugins@main/docs/plugins.md
6. https://gcore.jsdelivr.net/gh/webxxxxx/plugins@main/docs/plugins.md
7. https://github.com/webxxxxx/plugins/raw/refs/heads/main/docs/plugins.md

raw 排第一：它给的就是分支当前内容，没有配额。2、3 是两个无状态的 raw 代理，GitHub 不通时
它们通常还能用。jsDelivr 放在最后：它的分支地址按 `max-age=604800` 缓存一周，实测即便发布
流程调了 `purge.jsdelivr.net` 清理，它仍可能继续发一周前的表（有时干脆 301 回 raw）——
留着它只是「别的源全都不通时至少还有一份旧表」。

插件侧还有两道保险：请求带 `Cache-Control` / `Pragma: no-cache`；抓回来的表如果每一行都
不比手里这份新（`catalog::is_older`，按 `x.y.z` 数值比较），就跳过这个源去问下一个，
全都不行才报「返回的列表比当前这份旧」。所以镜像再旧也只会让列表停在较新的那一份上。

不用 GitHub Releases API 当数据源：它和 raw 是同一张网（不通的时候一起不通，不解决可达性），
未认证调用还有每小时 60 次的配额，而 `docs/plugins.md` 本来就是从那些 release 生成的，
走 API 只是把同一份数据重新解析一遍。

成功结果写到插件根目录的 <data_dir>/plugins/plugin-store/catalog.md，6 小时后后台再刷新；首次启动/离线时用编译时嵌入的 docs/plugins.md 快照。插件根目录从运行中的 payload 路径推出（<data>/plugins/<id>/<version>/payload/bin/<exe> 往上四级）——svcd 不给插件注入 SVCD_DATA_DIR；手动运行二进制时才退回 SVCD_DATA_DIR，再退回系统临时目录。页面顶部的「列表」会显示这条在线结果来自哪个域名。

下载的 .spk 暂存在同一插件根目录的 temp 下：<data_dir>/plugins/plugin-store/temp/；交给宿主时用 rename 移进 `$SVCD_TEMP_DIR/incoming`（同一文件系统，不再占第二份 15 MiB；跨设备才降级复制，失败会删掉半成品）。安装结束（成功或失败）就删除——它是传输暂存而不是仓库，小容量卷上留一份十几 MiB 的失败包会把盘占满，重试时重新下载。下载中或下载完成待安装时，任务卡片上的「删除」会停止下载、删掉对应的 .spk（含 .part 临时文件）并清空任务状态；已经交给宿主安装（installing）后不能删除。下载顺序：

1. 直接下载（自动跟随 GitHub release 的 302）；
2. 如果 release-assets.githubusercontent.com 被 DNS 污染/无法连接，查询 Cloudflare/Google DNS-over-HTTPS 得到真实地址，用固定 resolver 重试；TLS 仍按原主机名校验；
3. 可选的镜像模板：PLUGIN_STORE_MIRROR，例如
   PLUGIN_STORE_MIRROR=https://gh.example/{url}，{url} 会替换为原始下载地址。

下载完成后仍由宿主验签，第三方镜像无法绕过签名。

### 命令行批量下载

同一个二进制也能把 `docs/plugins.md` 里的包全部拉到 `dist/`，供本地安装或归档：

```bash
cargo run --release --manifest-path plugins/plugin-store/Cargo.toml -- download
```

它直接调用上面的 `net::download`，所以代理、DoH 回退和 `PLUGIN_STORE_MIRROR` 都会生效。默认读 `docs/plugins.md`、写 `dist/`；已存在文件跳过，`--force` 重下，`--dry-run` 只看清单，`--manifest` / `--output` 可换路径。`--only meow` 只下这个插件的包，`--only <完整文件名>` 只下一个包；不区分大小写，可重复。

## 配置

安装时宿主把包里的 `config/config.json` 复制成插件的活动配置，并用 `SVCD_CONFIG` 指给插件。
之后用宿主的命令编辑，改完 `plugin restart plugin-store` 生效：

```bash
plugin editconfig plugin-store --print           # 看当前值
plugin editconfig plugin-store --file cfg.json   # 整体替换
plugin editconfig plugin-store --reset           # 回到模板
```

```json
{
  "catalog_source": "github",
  "qiniu_base": "",
  "github_token": "github_pat_…",
  "release_cache_minutes": 15
}
```

- `catalog_source`：`github` 或 `qiniu`，决定先问哪一份列表，默认 `github`。另一个来源不会消失，
  只排在后面兜底。
- `qiniu_base`：七牛空间的公开域名，例如 `https://dl.example.com`（`http` 和 `https` 都行，
  末尾斜杠可有可无，粘 `…/index.json` 也认）。只有配了它，七牛来源才存在；配了 `qiniu_base`
  却仍留 `catalog_source: "github"`，七牛就只是 GitHub 全都不通时的最后一条路。
  用 `http` 时解析规则跟着 base 走：base 是 http，索引里的 http 链接就放行（并记一条警告）；
  base 是 https，索引里出现 http 链接会被拒绝——抓列表走的是 https，就不该把下载降级回明文。
  包本身有签名，http 能保证"字节没被换"，保证不了"目录没被改"；能开证书就别停在 http。
- `github_token`：只读 PAT（读公开 release，`public_repo` 就够）。留空表示不认证。
  宿主写入的活动配置在 Linux 上是 0640；插件不会把 token 打进日志、错误文案或 `/list` 结果。
- `release_cache_minutes`：`release_asset` 结果的重用窗口，默认 15，上限 1440；
  设 0 表示每次检查都带 `If-None-Match` 校验。

优先级：配置文件 > `GITHUB_TOKEN` > `GH_TOKEN` > `<data_dir>/github-token`。配置文件给服务
安装用，环境变量留给临时调试。配置解析失败只记一条警告，插件仍按默认值继续跑。

两个来源各写各的缓存：`catalog.md` 存 GitHub 那份表，`catalog.json` 存七牛那份索引；启动时优先读
配置所选来源的缓存，读不到再读另一份。所以切换来源不会让页面先空一次，也不会把两种格式喂给同一个
解析器。`list` 结果里的 `catalog_source` 回显当前选择，`source_url` 回显这份列表实际来自哪个地址。

切到七牛的一步操作（改完 `plugin restart plugin-store`）：

```bash
plugin editconfig plugin-store --print > cfg.json   # 拿到当前值
# 编辑 cfg.json：catalog_source=qiniu、qiniu_base=https://你的空间域名
plugin editconfig plugin-store --file cfg.json
plugin restart plugin-store
```

## 自升级查询：release_asset

svcd 自己不链 TLS，检查与下载 svcd 本体由 plugin-store 代劳。`release_asset`
查的是 `webxxxxx/plugin-manager` 的最新**稳定** release（prerelease/draft 直接拒绝），
`repo` 省略时就用它。

来源跟着 `catalog_source` 走：配成 `qiniu` 时先读七牛 `index.json` 的 `svcd[]`
（`svcd_sums_url` 指向根目录的 `SHA256SUMS`），镜像里没有 `svcd` 小节才退回 GitHub，
退回会记一条 `log_warn!`。镜像这一路不带 `github_token`——那不是 GitHub 的域名，
PAT 不该出现在那里。

```jsonc
// 1) 只查：返回全部资产与校验清单是否存在，不产生下载流量
release_asset({ "download": false })
-> {
     "tag": "v0.1.5",
     "version": "0.1.5",
     "assets": [
       { "name": "SHA256SUMS", "size": 767 },
       { "name": "svcd-0.1.5-x86_64-pc-windows-msvc.zip", "size": 1234567 }
     ],
     "sumsAsset": "SHA256SUMS"      // 没有就是 null，宿主据此拒绝升级
   }

// 2) 按精确文件名取一个文件；每个调用只回一个 blob
release_asset({ "download": true, "asset": "SHA256SUMS" })
release_asset({ "download": true, "asset": "svcd-0.1.5-x86_64-pc-windows-msvc.zip" })
-> { "$blobPath": "…", "name": "…", "mimeType": "application/octet-stream" }
```

- 宿主侧流程：`download: false` 拿版本与资产清单 → 按 target 后缀挑归档、`sumsAsset`
  为 null 直接拒绝 → `download: true` 分别取归档与 `SHA256SUMS` → 用 SUMS 里该行
  的 sha256 校验归档 → 才走 `svcd upgrade --file`。
- `download: true` 的结果只有 blob，**不带** `tag`/`version`/`assets`；元数据一律来自
  第一次查询。`asset` 必须是 `assets[].name` 里的精确名字，未知名字报错并列出候选。
- 列出可下载的资产（GitHub 标记为 `uploaded` 且带文件名/大小/下载地址的那些）；单文件超过
  256 MiB 直接拒绝，不截断。
- 查询带 `User-Agent: plugin-store/<version>`，匿名配额 60 次/小时；403/429 只有在
  确实是限流（剩余 0、HTTP 429，或文案含 rate limit/abuse）时才报限流，否则报「拒绝访问」，
  都给出可读错误而不是裸 403。
- 认证：读取顺序是**配置文件里的 `github_token`** → `GITHUB_TOKEN` → `GH_TOKEN` →
  插件数据目录下的 `github-token`（一行文本，建议 0600）。有 token 就带
  `Authorization: Bearer …`，配额从匿名 60 次/小时提到 5000 次/小时；token 只发给
  `api.github.com`，不写日志。401 明确报「token 无效或被吊销」，不会静默退回匿名。
- 缓存：第一次成功查询把响应体、`ETag` 和写入时间存进 `<data_dir>/release-cache.json`
  （按 URL 区分）。`release_cache_minutes`（默认 15 分钟）内的重复查询**不发请求**，直接从
  磁盘回；超出窗口才带 `If-None-Match`，GitHub 的 304 不计配额。窗口设 0 表示每次都校验。
  `download: true` 不读缓存，永远取最新字节。
- 下载复用直连 + DoH 回退与代理，但**不走** `PLUGIN_STORE_MIRROR`：那个前缀代理可以决定插件包，
  不该再无条件决定 svcd 的字节。走七牛那条是显式选择（`catalog_source`），代价见下：镜像同时
  提供字节和 `SHA256SUMS`，它能决定「升到哪一版」，伪造不了字节。版本不缓存，每次升级都重新查、
  重新下；失败时清掉 `.part`。
- 不做签名；校验链由宿主用 `SHA256SUMS` 的 sha256 完成，插件只负责把字节交回。

本地手测（2026-11-23，Windows）：

- `cargo test -- --ignored`：真网用例 `finds_the_latest_stable_release_of_the_daemon`
  在共享出口 IP 上命中了 GitHub 匿名限流，返回可读错误
  `GitHub API 限流（HTTP 403）：API rate limit exceeded ...；本小时剩余 0 次；配额重置于 ...`；
  配额可用时该用例会断言 `sumsAsset == "SHA256SUMS"` 且资产多于一个。
- 归档字节往返由 `downloads_an_asset_into_the_incoming_directory_byte_for_byte`
  用本地 HTTP 假 transport 覆盖：下载文件与响应体逐字节一致，`.part` 已清理，路径在
  `incoming/` 下；不依赖外网。限流响应另有一条本地假 403 用例，直接驱动 `net::fetch_json`
  验证 `x-ratelimit-*` 头与可读文案。
- 安装收尾：`a_failed_install_drops_the_downloaded_package` 断言失败报告后 `.spk` 与 `.part` 都不在了。
- 认证与缓存：`an_etag_turns_a_repeated_check_into_a_cached_304`、
  `a_configured_token_is_sent_as_a_bearer_header`、`a_token_file_is_read_trimmed_and_never_beats_the_environment`
  与 `a_rate_limited_answer_points_at_the_token_file` 都用本地 TCP 假服务器覆盖，
  断言实际发出的 `If-None-Match` / `Authorization` 头与落到错误文案里的 token 路径。
- 构建：`plugin-tools build --platform x86_64-pc-windows-msvc` 产出
  `dist/plugin-store-<version>-x86_64-pc-windows-msvc.spk`（未签名本地包），`plugin verify`
  返回 `ok: true`、`signature.state: "missing"`；发布包需在发布流程里用发布钥签名。

## 构建

    # 仓库根目录；需要宿主 plugin 在 PATH，或设置 SVCD_PLUGIN_BIN
    plugin-tools build --platform x86_64-pc-windows-msvc
    cargo test

发布时按 .plugin.json 的 7 个平台各自构建。build.rs 会：

- 把 docs/plugins.md 复制成编译期快照（include_str!）；
- 把 cargo 的 TARGET 写进 SVCD_PLUGIN_TARGET，作为设备过滤依据。

## 边界

- 目录格式只认 `docs/plugins.md` 的 Markdown 表和七牛的 `index.json`（`schema: 1`）；表头、缺字段、
  下载地址不是 https 的 `.spk` 都会拒绝解析并保留旧缓存。
- 目录里没有 Windows 包时，Windows 上的默认页是空的，这是正确结果。
- platform 与文件名 device 后缀是列表侧过滤；manifest 里完整的 compat.devices 仍会在安装时由 svcd 再次校验。
- 安装包必须已签名并被宿主的 trust.d 信任；未签名包需要开发机的 allow_insecure_install = on。
  发布包的签名 keyId 是 `signing`，公钥文件必须叫 `signing.pub`。发布公钥由 `svcd --install`
  写到 `<config>/trust.d/signing.pub`，商店自己不再分发公钥。
- 插件不能更新自己（页面会拒绝 plugin-store 自身的安装），避免安装过程中把自己重启掉。