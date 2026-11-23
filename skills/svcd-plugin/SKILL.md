---
name: svcd-plugin
description: 新增或修改 svcd 插件前读：支持哪两种语言、SDK 与 Web toolkit 怎么用、以及会让包直接失效的边界（stdout、端口、打包根、能力清单）。
---

# 写 svcd 插件

插件是一个**常驻进程**：stdout 上跑 JSON-RPC，向宿主暴露若干「能力」；浏览器 UI 由宿主反代，插件自己不开端口。你的工作只有一件事——实现能力逻辑，其余由 SDK 承担。协议 1 是一帧一行 JSON；协议 3 是 `u32 len | u32 stream_id | payload` 的多路复用帧，只有它同时具备 raw 流。

## 三步走

### 1. 起项目

```bash
plugin-tools template wireguard          # 数字菜单选语言（1-4）与平台（1-7）
plugin-tools template wireguard --lang rust --platforms x86_64-unknown-linux-musl --webui
```

完成判据：`package/manifest.json`、`build.json`、`.plugin.json` 都存在，且目录里**没有** `target/`、`dist/`、`out/`。清单里 `schema` 写成 `1`（打包器也会恒定盖成 `1`），`id` 是身份（不写则取项目目录名）。

### 2. 只写 handler

Rust：

```rust
impl Plugin for MyPlugin {
    const CAPABILITIES: &'static [&'static str] = &["echo", "get_stats"];

    fn call(&mut self, name: &str, arguments: &Value) -> ToolOutcome {
        match name {
            "echo" => ToolOutcome::text(arguments["message"].as_str().unwrap_or("")),
            "get_stats" => ToolOutcome::text(format!("ticks={}", self.ticks)),
            other => ToolOutcome::error(format!("unknown tool: {other}")),
        }
    }
}
```

Shell：

```sh
handle() {                    # $1 = 能力名, $2 = 原样 arguments JSON
    case "$1" in
    uname) uname -a ;;
    *)     echo "unknown tool: $1" >&2; return 1 ;;
    esac
}
svcd_plugin_serve handle
```

完成判据：`package/manifest.json` 的 `capabilities[].name` 与代码里的分支**一一对应**；Rust 模板自带 `cargo test` 会断言这件事。

### 3. 打包与安装

```bash
plugin-tools build          # 构建 + 打包（默认未签名，本地开发用）
plugin install <spk>  # 首装不自启
plugin start <id>
```

完成判据：`plugin-tools build` 打印 `packed … (unsigned)`，且随后 `verify` 输出 `ok: true`（`"schema": 1`、`"id"` 是 ASCII；`"displayName"` 就是清单里的 `name`，应当是你的中文名）。

未签名包要求宿主 `svcd.conf` 里 `allow_insecure_install = on`（改完重启 svcd）。发布走 CI，用发布钥签名。

## 支持的语言

| 语言 | 模板 | 目标机需要什么 | 完整示例 |
|---|---|---|---|
| `rust` | `templates/rust` | 无（编译成静态二进制） | `samples/hello-rust` |
| `c` | `templates/c` | 无（编译成静态二进制） | `samples/hello-c` |
| `shell` | `templates/shell` | 目标机有 `sh` | `samples/hello-shell` |
| `powershell` | `templates/powershell` | Windows 的 `powershell` 或 Linux/macOS 的 `pwsh` | `samples/hello-powershell` |

四个示例实现**同一组能力**（`echo` / `get_stats` / `count_text` / `get_file_info` /
`create_report`），可以逐行对照；每份 README 末尾都有一张「演示了宿主哪些能力」的表：
方法调用、文件上传（`$blob`）、大文件上传（`$file`）、文件下载（`$blobPath`）、
SSE、WebSocket、日志——以及它们分别由哪个能力触发。SDK 提供的能力面也是对等的：

| 能力 | Rust | C | Shell | PowerShell |
|---|---|---|---|---|
| 协议循环 + ping/status/shutdown | `serve` | `svcd_serve` | `svcd_plugin_serve` | `Invoke-SvcdPlugin` |
| 文本结果 / 错误 | `ToolOutcome::text/error` | `svcd_text/error` | stdout + 退出码 | 返回字符串 / `@{Text,Error}` |
| 大结果（`$blobPath`） | `ToolOutcome::Blob` | `svcd_blob` | `SVCD_BLOB_PATH` | `@{BlobPath=…}` |
| 日志 → stderr | `log_info!` 等 | `svcd_log` | `echo >&2` | `Write-SvcdLog` |
| 配置读取 | `Context::config_json` | `svcd_read_file` | `svcd_config_get` | `Get-SvcdConfig` |
| `$blob` 输入 | `blob_argument` | `svcd_string(args,"path")` | `svcd_field` | `$Arguments.input.path` |
| `$file` 文件包 | `file_argument` + `PartReader` | `svcd_json_numbers` + `fopen` | `svcd_json_numbers` | `Get-SvcdFileParts` |
| 临时目录 | `Context::temp_dir` | `context.temp_dir` | `$SVCD_TEMP_DIR` | `$Context.TempDir` |

两种语言共用同一套契约和同一个 Web toolkit；**前端不认识后端语言**。

加第三种语言不需要改工具：在 `templates/<lang>/` 放模板，并写一份 `build.json`，`plugin-tools build` 就会照着做。

## 契约速查

| 项 | 值 |
|---|---|
| 通道 | stdout（协议）+ stderr（日志）；协议 1 一帧一行，协议 3 见下 |
| 协议 | `protocol: 1` 换行 JSON-RPC；`protocol: 3` stream 0 是同一套 JSON-RPC，stream ≥ 1 是 raw |
| 方法 | `svcd.ping`、`svcd.status`、`svcd.shutdown`、`tools/call` |
| 结果形状 | `{"content":[{"type":"text","text":"…"}],"isError":false}` |
| 帧上限 | stream 0 / JSON 1 MiB；raw 默认 1 MiB（`ws_raw_max_bytes`）；无握手 |
| 错误码 | `-32700` 解析、`-32601` 方法不存在、`-32602` 未知能力、`-32603` 过载 |
| 推送 | `emit(method, params)` → 宿主转成 SSE / WebSocket 给 UI |
| 宿主注入 | `SVCD_PLUGIN_ID`、`SVCD_PLUGIN_VERSION`、`SVCD_CONFIG`、`SVCD_DATA_DIR`、`SVCD_LOG_DIR`、`SVCD_HTTP_PORT`、`SVCD_TARGET` |

`svcd.ping` / `svcd.shutdown` 由 SDK 自动应答，不要自己实现。

## toolkit

### Rust（`sdk/rust`，crate `svcd-plugin`）

| API | 作用 |
|---|---|
| `svcd_plugin::serve(Context::from_env(), plugin)` | 协议 1 循环、有界工作池、取消、按 id 乱序回复 |
| `multiplex::serve(ctx, plugin)` | 协议 3：stream 0 的 JSON-RPC + raw 流，一条管道 |
| `multiplex::RawPlugin::{raw, raw_closed}` | raw 帧回调（`stream`、`payload`、`&Streams`）与关流钩子 |
| `Streams::send / close` | 往 raw 流写帧（有界、非阻塞）；`close` 立刻让这条流拒后续帧，并尽力通知宿主 |
| `Streams::send_json(value)` | 从 raw 回调里往 stream 0 发一条 JSON-RPC（一般是通知/进度） |
| `Plugin::call(&mut self, name, args)` | 你的能力实现 |
| `Plugin::status(&self)` | `svc status` 的一行摘要 |
| `ToolOutcome::text / error` | 结果；`error` 对应 `isError: true` |
| `Context::config_path / config_json / data_dir / log_dir / temp_dir / id / version` | 宿主给你的环境 |
| `Context::cancel()` → `Cancel::{is_cancelled, sleep, ticker}` | 后台循环必须观察的取消令牌 |
| `svcd_plugin::emit(method, params)` | 推送给 UI |
| `log_info! / log_warn! / log_error!` | 写 stderr（宿主捕获并轮转） |
| `declared_capabilities(path)` | 读 manifest 的能力清单，用于 drift test |
| `blob::blob_argument(args, "input")` | 取宿主解析好的 `$blob` 参数（`{path,size}`） |
| `file::file_argument(args, "input")` → `FileArg::open()` | 取 `$file` 文件包，按 part 顺序流式读 |
| `BlobWriter::create(&ctx, name, mime)` | 把大结果写进 `incoming/`，`finish()` 返回 `ToolOutcome::Blob` |
| `FileStore` | 插件**自己的**持久文件（`data_dir` 下），不是传输通道 |

### 协议 3（只有 Rust）

manifest 写 `"protocol": 3`，`main` 用 `multiplex::serve`。帧是
`u32 payload_len | u32 stream_id | payload`：

- **stream 0**：还是上面那套 JSON-RPC（ping、`tools/call`、`notifications/*`）。宿主在
  stream 0 上发 `svcd.ping`，SDK 自动回，回完才算启动成功。
- **stream ≥ 1**：不透明字节，宿主按 stream 配给 `/raw` 的 WebSocket 订阅者。插件不知道
  谁在订阅，只知道帧从哪条 stream 来——回帧就用同一条 stream。

写侧不阻塞读循环：`Streams::send` 进有界队列，满时返回 `SendError::Busy`；超过上限的帧
在写头之前就拒，不会出现半帧。反过来，宿主发来的 raw 帧在插件 raw 线程落后超过 64 帧时会被
丢掉（读循环不能被一条慢流拖住）——raw 本来就不保证送达，需要可靠就自己带序号重传。上限用 `multiplex::serve_with(..., Limits { raw_frame_max, .. })`
跟宿主的 `ws_raw_max_bytes` 对齐。

**关流走控制流。** stream `0xFFFFFFFF` 是控制流，payload 是一个小 JSON 对象，目前只有
`{"op":"close","stream":N}`：宿主在 `/raw` 订阅者离开时给插件发它（触发
`RawPlugin::raw_closed`），插件用 `Streams::close(stream)` 发它让宿主把该 WebSocket 干净关闭
（1000）并释放订阅。本地 `close` 之后该流立刻拒后续帧（`SendError::Closed`）。插件往已经被
关掉的流写数据时，宿主不再静默丢弃，而是回一个 close 让插件知道。

### Shell（`sdk/shell`，随包拷贝）

| API | 作用 |
|---|---|
| `svcd_plugin_serve <handler>` | 协议循环；自动应答 ping/status/shutdown |
| `<handler> <name> <args-json>` | 你的能力实现；stdout 即结果，非 0 退出即 `isError` |
| `svcd_notify <method> <json>` | 推送 |
| `svcd_status` | 可覆盖，供 `svc status` |

`arguments` 在 shell 里是**原样 JSON 字符串**（没有解析器）。参数是扁平字符串时直接 `case`/`sed` 取；要真正解析 JSON 就换 Rust。

### C（`sdk/c`，单头文件）

| API | 作用 |
|---|---|
| `svcd_serve(&ctx, handler, user, capabilities)` | 协议循环；自动应答 ping/status/shutdown |
| `handler(name, args_json, user)` | 你的能力实现；返回 `svcd_text()` / `svcd_error()` |
| `svcd_string(args_json, "key")` | 取一个扁平字符串字段（调用方 free） |
| `svcd_raw_value(args_json, "key")` | 取一个嵌套对象/数组的原始 JSON |
| `svcd_log("info", "…")` | 写 stderr |
| `svcd_notify(method, params_json)` | 推送 |
| `svcd_blob(path, name, mime)` | 返回大结果（`$blobPath`）；`ctx.temp_dir` 是写入点 |

头文件里是**扁平 JSON 扫描器**，不是通用解析器。需要真解析 JSON 就换 Rust。

### PowerShell（`sdk/powershell`，随包拷贝）

| API | 作用 |
|---|---|
| `Invoke-SvcdPlugin -Handler {…} -Capabilities @(…)` | 协议循环 |
| handler `param($Name, $Arguments)` | 你的能力实现；返回字符串或 `@{Text=…;Error=$true}` |
| `Write-SvcdLog 'info' '…'` | 写 stderr |
| `Send-SvcdNotification -Method … -Params …` | 推送 |
| 返回 `@{BlobPath=…;Name=…;MimeType=…}` | 大结果（`$blobPath`） |
| `Get-SvcdContext` | `SVCD_*` 环境（含 `TempDir`） |

`interpreter.name` 在 Windows 写 `powershell`，Linux/macOS 写 `pwsh`；**不要写 `version`**
（`powershell --version` 不是合法参数，探测会失败）。

### Web（`sdk/web`，浏览器 ESM）

```js
import { pluginUI } from './plugin-ui.mjs';
const ui = pluginUI();

await ui.invoke('echo', { message: 'hi' });          // POST ./rpc
const r = await ui.invokeResult('create_report');           // 保留 structuredContent
ui.subscribe({ onMessage(method, params, msg, seq) {} });  // SSE，自动续传
const socket = ui.subscribeWebSocket({ onMessage() {} });
await socket.invoke('get_stats');                        // 同一条 WS 上发 tools/call
const meta = await ui.uploadBlob(file);              // 整体上传（≤512 MiB）
const big = await ui.uploadFilePackage(file);        // part 会话（≤16 GiB）
await ui.invoke('count_text', { input: ui.blobArg(meta.id) });
await ui.invoke('get_file_info', { input: ui.fileArg(big.id) });
await ui.downloadBlob(meta.id, { filename: 'x.bin' });   // Range/流式
await ui.downloadFile(big.id, { filename: 'x.iso' });
```

| 通道 | 路径 | 用途 |
|---|---|---|
| 方法调用 | `POST ./rpc` | 调能力 |
| SSE | `./events` | 服务端推送；断线重连带 `Last-Event-ID` 补发，窗口外的先收到 `event: reset` |
| WebSocket | `./ws` | 双向：推送 + 同一连接上的 `tools/call` |

**大对象**永远不要塞进 JSON 帧（单帧上限 1 MiB）。宿主有两级传输面：

| 级别 | 上限 | 参数形状 | 说明 |
|---|---|---|---|
| blob | 512 MiB | `{"$blob": id}` → `{$blob, path, size}` | 单个对象；整体上传或分片会话；下载支持 Range |
| file 包 | 16 GiB | `{"$file": id}` → `{$file, dir, manifest, size, name, mimeType}` | parts + manifest；id 是整文件 SHA-256；最多 10000 个 part |

- **上传**：`POST /api/v1/blobs`（流式）或 `POST /api/v1/{blobs,file}/uploads` 的 S3 风格 part 会话（`PUT .../parts/{n}`、`GET .../uploads/{id}` 查进度、`POST .../complete`）。
- **下载**：`GET /api/v1/blobs/{id}`、`GET /api/v1/file/{id}`，都支持 `Range` / `HEAD` / 条件请求。
- **读文件包**：`file_argument(args, "input")` → `FileArg::open()` 顺序流式读所有 part，别整读进内存。
- 两套会话不通用：拿 blob 的 id 去打 file 的 part 接口会 409，反之亦然。
- 临时文件默认 2 小时 TTL。完整语义见 `plugin-manager/docs/plugin-blob.md`。

`FileStore` 是另一回事：那是插件**自己**要长期保存的文件（`data_dir` 下），不用于和调用方传大对象。

## 边界

**stdout 只写协议。** 日志、调试输出、`print` / `console.log` 一律去 stderr——它是宿主的日志通道，会写进 `<logs>/<id>.log` 并轮转。Rust 用 `log_info!`，shell 用 `echo … >&2`。

**UI 放 `package/payload/web/`。** 宿主把整棵树反代到 `/plugin-ui/<id>/`，页面用**相对路径** `fetch('rpc')`；插件进程不自起任何 HTTP 或 WebSocket 服务。

**源码留在 `package/` 外面。** `package/` 是打包根，打包器不过滤任何文件：放进去的 `src/`、`Cargo.toml`、`.gitkeep` 都会进 `.spk`。构建产物由 `build.json` 的 `artifact` 复制进 `payload/bin/`。

**能力清单一处改全。** 加/删能力时同时改 manifest 的 `capabilities[]` 和代码分支；Rust 用 `cargo test` 兜底，shell 靠 `case` 与清单人工对齐。

**`id` 是身份，`name` 是显示名。** `schema` 恒为 `1`（打包器盖章，源清单写别的值也会被覆盖），`id` 只能是 `[a-z0-9][a-z0-9_-]{0,63}`（不能是 `svc`）：安装目录、日志文件名、`<id>.<能力>` 工具名、`/plugin-ui/<id>/` 都用它。源清单省略 `id` 时取项目目录名；`name` 随便写，中文也行，缺省等于 `id`；命令与 API 只认 `id`，不按显示名解析。

**`platform` / `files[]` / `signature.json` 由打包器盖章。** 源 manifest 不写它们——都是派生的。产物文件名是 `<id>-<version>-<platform>.spk`。

**`outputSchema` 只做声明。** `capabilities[].outputSchema` 可选：写了就投影进 MCP `tools/list`，运行期不校验结果（不写 = 未声明，`{}` = 任意）。工具返回结构化 JSON 时才值得声明（模板里的 `stats` / `info` / `date` 就是示例）。

**`icons` 是可选的包图标。** 形状 `{"png"|"ico"|"icns": "<包内路径>"}`，作者自备三种格式（不做转格式/转尺寸）；构建时会先校验文件存在且非空，验包阶段再查魔数与大小。

**插件是常驻进程。** 没有 oneshot 语义；要"跑一次"，写成常驻 + 一个触发能力，或用 core 服务。

**依赖保持最小。** Rust 只用 `serde` / `serde_json`；shell 只用 POSIX。SDK 不提供 HTTP 客户端、数据库或调度器——需要就自己加，但先确认体积可接受。

**raw 只在协议 3 里。** `protocol: 1` 的插件没有 `/raw`（宿主回 `raw_unavailable`）；要同时有
调用、推送与 raw，就用 `multiplex::serve` 并把清单改成 `protocol: 3`。C / shell / PowerShell
SDK 没有 raw 实现。

**大对象走 blob，不走 JSON。** 参数传 `{"$blob": id}`，结果返回 `$blobPath`；1 MiB 帧只装小 JSON。

**vendored 副本别手改。** `package/payload/lib/svcd-plugin.sh` 和 `payload/web/plugin-ui.mjs` 是 `sdk/` 的副本；改完 `sdk/` 后跑 `plugin-tools sync`。它只刷新插件已经带着的那几份，不会给不用工具包的页面塞一份 `plugin-ui.mjs`。

## 做不到的事

- **插件之间互相调能力**：宿主没有开放这条通道（daemon socket 只有 `svc.*` 管理方法）。
- **自起 HTTP 服务**：单端口模型由宿主独占。
- **宿主级定时调度**：没有 cron；定时器在进程内，重启即归零。
- **权限隔离**：插件以安装者权限运行，隔离是未决问题。
- **跨目标构建**：`plugin-tools build --platform` 只改 manifest 的盖章；真正的交叉编译需要目标工具链（Rust：`rustup target` + zig）。

## 常见错误 → 症状

| 症状 | 原因 |
|---|---|
| 装上了但 `start` 超时 | 脚本插件没写 `"ready": "exec"`（缺省 `notify` 会等 ping） |
| MCP 里有这个工具，调用却报错 | manifest 与代码分支漂移 |
| 包在目标机上装不上 | `platform` 与本机三元组不等 |
| Windows 上启动 `exit=9009` | `interpreter.name` 写了 `python3`（那是 0 字节存根，用 `python` 或 `py`） |
| Debian 上装不上 | 给 `sh` 写了 `interpreter.version`（dash 的 `--version` 会报错） |
| 包里出现源码 | 源码或占位文件放进了 `package/` |
| 装不上，提示重打包 | 包不是 `schema: 1` 或没有 `id`——用新版 `plugin-tools build` 重打 |
| 目录名变成中文/乱码 | 把显示名写进了 `id`；中文只能放 `name` |
| 改了 SDK 但插件没变 | vendored 副本没同步（`plugin-tools sync`） |
| UI 的 `/raw` 连不上（`raw_unavailable`） | 插件不是 `protocol: 3`，或还在用协议 1 的 `serve` |
| raw 帧发出去没人收 | 订阅者已经关了这条 stream；宿主不通知插件，按请求生产 |
