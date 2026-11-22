# plugin-store — 插件商店

一个 Rust 插件：读取 docs/plugins.md 的发布表，只显示当前设备平台可安装的包，支持按名称搜索、每页 50 个、下载后安装。

## 界面

- 设备平台与 device id 来自宿主 GET /api/v1/compat（plugin-manager 的同一份 HostFacts）；
  只显示 platform 精确匹配的包，没有「全部平台」开关。
- 包名带 device 标识时按 device 过滤：优先解析文件名
  <name>-<version>-<platform>-<device>.spk 的平台后后缀，也兼容 device 放在包名/文件名
  version 两侧的写法；只有宿主的 device id 相同才显示，无标识视为该 platform 的通用包。
  当前 docs 生成器也已支持平台后的 device 后缀，避免设备包在列表里被丢掉。
- 每页固定 50 条；搜索只匹配插件名，输入即筛（250 ms 防抖）。
- 已安装版本低于目录版本时，操作列显示「升级」；相同或更高时显示「已安装」并禁用。
- 插件商店自己也在列表里，带「自己」标记。它的「升级」会下载自己的新包并交给宿主替换：宿主停掉旧进程后由页面启动新版本并自动刷新。升级期间本页的安装任务会随旧进程消失，其他标签页会收到任务清空。
- 列表来自本地缓存或内置快照，启动后先在后台刷新；「刷新列表」只触发后台任务，不会卡住页面。
- 安装按钮先由插件下载 .spk 到数据目录，再通过宿主现成的 API 安装并启动：

      package_blob  ->  宿主把文件变成 $blob
      GET  /api/v1/blobs/{id}             取回字节
      POST /api/v1/plugins                与 management 页面相同的安装入口
      POST /api/v1/services/{id}/start    首装不自启，装完启动

  插件不调 CLI、不自起 HTTP、不碰宿主 socket；签名和平台校验仍由 svcd 完成。

## 数据与下载

列表地址按顺序尝试：

1. https://cdn.jsdelivr.net/gh/webxxxxx/plugins@main/docs/plugins.md （通常最快）
2. https://raw.githubusercontent.com/webxxxxx/plugins/main/docs/plugins.md
3. https://github.com/webxxxxx/plugins/raw/refs/heads/main/docs/plugins.md

成功结果写到插件根目录的 <data_dir>/plugins/plugin-store/catalog.md，6 小时后后台再刷新；首次启动/离线时用编译时嵌入的 docs/plugins.md 快照。插件根目录从运行中的 payload 路径推出（<data>/plugins/<id>/<version>/payload/bin/<exe> 往上四级）——svcd 不给插件注入 SVCD_DATA_DIR；手动运行二进制时才退回 SVCD_DATA_DIR，再退回系统临时目录。

下载的 .spk 暂存在同一插件根目录的 temp 下：<data_dir>/plugins/plugin-store/temp/；安装成功后删除，失败时保留以便重试。下载中或下载完成待安装时，任务卡片上的「删除」会停止下载、删掉对应的 .spk（含 .part 临时文件）并清空任务状态；已经交给宿主安装（installing）后不能删除。下载顺序：

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

## 自升级查询：release_asset

svcd 自己不链 TLS，检查与下载 svcd 本体由 plugin-store 代劳。`release_asset`
查 GitHub 仓库的最新**稳定** release（`/releases/latest`，prerelease/draft 直接拒绝），
按文件名通配选**恰好一个**资产：

```json
{ "repo": "webxxxxx/plugin-manager", "platform": "x86_64-pc-windows-msvc",
  "pattern": "svcd-*-{platform}.zip", "download": false }
```

- `download: false`（默认）只查，返回
  `{ "tag": "v0.1.5", "version": "0.1.5", "asset": { "name": "svcd-0.1.5-x86_64-pc-windows-msvc.zip", "size": 1234567 } }`，
  不产生下载流量；`version` 是 tag 去掉前导 `v`。
- `download: true` 把匹配到的资产下载到宿主 `SVCD_TEMP_DIR/incoming/`，以**单个**
  `$blob` 交回，不返回磁盘路径，**也不带 `tag`/`version`/`asset`**（一个结果只装一个 blob）。
  宿主先调一次 `download: false` 拿版本，再用第二次调用取校验清单：
  `{ "repo": "...", "pattern": "SHA256SUMS", "download": true }`。
- 同一 release 里 0 个匹配或多个匹配都报错，并列出候选文件名；超过 256 MiB 直接拒绝，不截断。
- 查询带 `User-Agent: plugin-store/<version>`，匿名配额 60 次/小时；403/429 返回带剩余次数
  与重置时间的可读错误，而不是裸 403。
- 下载复用直连 + DoH 回退与代理，但**不走** `PLUGIN_STORE_MIRROR`：镜像可以决定插件包，
  不能决定 svcd 本体的字节。版本不缓存，每次升级都重新查、重新下；失败时清掉 `.part`。
- 不做签名；校验链由宿主用 `SHA256SUMS` 里的 sha256 完成。清单本身也走同一个能力
  （`pattern: "SHA256SUMS"`），所以宿主不需要理解 GitHub 的分页/资产结构。

本地手测（2026-11-23，Windows）：

- `cargo test -- --ignored`：真网用例 `finds_the_latest_stable_release_of_the_daemon`
  在共享出口 IP 上命中了 GitHub 匿名限流，返回可读错误
  `GitHub API 限流（HTTP 403）：API rate limit exceeded ...；本小时剩余 0 次；配额重置于 ...`；
  配额可用时该用例会断言最新 tag 与 `SHA256SUMS` 资产。
- 归档字节往返由 `downloads_an_asset_into_the_incoming_directory_byte_for_byte`
  用本地 HTTP 假 transport 覆盖：下载文件与响应体逐字节一致，`.part` 已清理，路径在
  `incoming/` 下；不依赖外网。限流响应另有一条本地假 403 用例，直接驱动 `net::fetch_json`
  验证 `x-ratelimit-*` 头与可读文案。
- 构建：`plugin-tools build --platform x86_64-pc-windows-msvc` 产出
  `dist/plugin-store-0.1.10-x86_64-pc-windows-msvc.spk`（未签名本地包），`plugin verify`
  返回 `ok: true`、`signature.state: "missing"`；发布包需在发布流程里用发布钥签名。

## 构建

    # 仓库根目录；需要宿主 plugin 在 PATH，或设置 SVCD_PLUGIN_BIN
    plugin-tools build --platform x86_64-pc-windows-msvc
    cargo test

发布时按 .plugin.json 的 7 个平台各自构建。build.rs 会：

- 把 docs/plugins.md 复制成编译期快照（include_str!）；
- 把 cargo 的 TARGET 写进 SVCD_PLUGIN_TARGET，作为设备过滤依据。

## 边界

- 目录格式只认 docs/plugins.md 的 Markdown 表；表头/下载单元格错误会拒绝解析并保留旧缓存。
- 目录里没有 Windows 包时，Windows 上的默认页是空的，这是正确结果。
- platform 与文件名 device 后缀是列表侧过滤；manifest 里完整的 compat.devices 仍会在安装时由 svcd 再次校验。
- 安装包必须已签名并被宿主的 trust.d 信任；未签名包需要开发机的 allow_insecure_install = on。
  发布包的签名 keyId 是 `signing`，公钥文件必须叫 `signing.pub`。发布公钥由 `svcd --install`
  写到 `<config>/trust.d/signing.pub`，商店自己不再分发公钥。
- 插件不能更新自己（页面会拒绝 plugin-store 自身的安装），避免安装过程中把自己重启掉。