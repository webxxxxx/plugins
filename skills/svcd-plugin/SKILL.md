---
name: svcd-plugin
description: 新增或修改 svcd 插件前读：支持哪两种语言、SDK 与 Web toolkit 怎么用、以及会让包直接失效的边界（stdout、端口、打包根、能力清单）。
---

# 写 svcd 插件

插件是一个**常驻进程**：stdout 上跑 JSON-RPC，向宿主暴露若干「能力」；浏览器 UI 由宿主反代，插件自己不开端口。你的工作只有一件事——实现能力逻辑，其余由 SDK 承担。协议 1 是一帧一行 JSON；协议 3 是 `u32 len | u32 stream_id | payload` 的多路复用帧，只有它同时具备 raw 流。

## 三步走

### 1. 起项目

```bash
plugin-tools template wireguard          # 先选平台（1-7），再按平台选语言
plugin-tools template wireguard --lang rust --platforms x86_64-unknown-linux-musl --ui web
```

语言跟着平台走（全 unix：rust/c/shell/powershell；全 Windows：rust/c/powershell；跨家族：
rust/c/script），非交互时缺参数直接报错并列出允许集——不会停下来等输入。

页面形态用 `--ui headerless|web|gui` 选，就是清单里 `ui.type` 的三个值（与 manage 的
`{type: web / gui / headerless, entry?}` 一致）：`headerless` 不带页面、清单写
`{"type": "headerless"}` 且没有 `entry`；`web` / `gui` 带模板页面与 toolkit，`entry` 是
`index.html`。值只认这三个，没有别名。

生成到哪、模板从哪来都可以指定：`--dir <path>`（相对当前目录，默认 `plugins`）、
`--template-repo <本地目录|git 地址>`（默认从 cwd 向上找，git 地址按 URL 缓存，
`--refresh` 强制重 clone）、`--conf <JSON>`（把这些参数写进文件，命令行优先）。

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

想知道这个项目会产出哪些包：`plugin-tools units`（一行一个单元：三元组、设备、产物名；`--json` 还带 `variants/<unit>/` 的 overlay）。`build`、`tools/pack.sh`、CI 矩阵读的都是它，别自己拼 `<id>-<version>-<platform>[-<device>].spk`。

未签名包要求宿主 `svcd.conf` 里 `allow_insecure_install = on`（改完重启 svcd）。发布走 CI，用发布钥签名。

## 支持的语言

| 语言 | 模板 | 目标机需要什么 | 完整示例 |
|---|---|---|---|
| `rust` | `templates/rust` | 无（编译成静态二进制） | `samples/hello-rust` |
| `c` | `templates/c` | 无（编译成静态二进制） | `samples/hello-c` |
| `shell` | `templates/shell` | 目标机有 `sh` | `samples/hello-shell` |
| `powershell` | `templates/powershell` | Windows 的 `powershell` 或 Linux/macOS 的 `pwsh` | `samples/hello-powershell` |
| `script` | `templates/script` | unix 有 `sh`、Windows 有 `powershell`；两套脚本写同一组能力 | 无（`template` 现生成） |

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

加语言不需要改工具：在 `templates/<lang>/` 放模板，并写一份 `build.json`，`plugin-tools build` 就会照着做。
例外是 `script`：它要同时 vendor 两份 SDK，所以在工具里有一条 `vendor_sdk` 分支。

`template` 先选平台、再选语言，可选语言由平台集合决定（全 unix：rust/c/shell/powershell；
全 Windows：rust/c/powershell；跨家族：rust/c/script）。非交互时一次给全参数即可，
stdin 不是终端时不会提问：

```bash
plugin-tools template wireguard --platforms x86_64-unknown-linux-musl,x86_64-pc-windows-msvc \
  --lang script --desc "wireguard plugin" --ui web --dir plugins
```

## 一个进程、一个线程

路由器上的预算很紧，插件的默认形态是 **一个进程**（Rust 通道插件的线程数由 SDK 决定，见下）。四条规矩把这件事做实：

**被监督的进程就是你的本体。** 别写「看护脚本 + 本体」两层：脚本干完准备活就 `exec` 掉自己（`exec "$HERE/meow" …`），于是 `ps` 里只有本体，`SVCD_LAST_EXIT` 是它自己的退出码，宿主按进程组停服收的也还是它。`exec` 不换 pid，所以 `exec` 之前写进 pidfile 的那个 pid 就是本体，`status` 与就绪探测都靠它认人。

**原本让看护循环干的活，宿主的生命周期各有位置。**

| 你要做的事 | 写在哪 |
| --- | --- |
| 迁移、播种数据、生成配置、校验（每次启动都做，必须幂等） | `lifecycle.start`——每次 spawn `exec` 之前跑一次、必须退出，非零或超时即本次启动失败 |
| 起来之后才能做的动作（等端口、挂路由/防火墙、注册到别处） | `lifecycle.started`——就绪之后、Running 之前跑一次，失败即启动失败（宿主停整组、跑 `stop`） |
| 收尾（撤规则、删 pidfile、按停止原因降级） | `lifecycle.stop`——进程消失之后跑，拿 `SVCD_STOP_REASON` / `SVCD_LAST_EXIT` |
| 周期任务（自愈、心跳、看孩子死没死） | 你自己的主循环里，别再起常驻子 shell |

**周期任务用「一拍」入口，不要 `&`。** 每 N 秒要做一次的脚本给一个 `--once`（跑一拍就退），由主循环驱动（`wireguardd.sh` 的看守每 30 秒叫一次 `wg-lan-fix.sh --once`）；不带参数手跑时才进循环。稳定态因此永远只有一个进程，也不会留下「父进程没了、子 shell 还在转」的孤儿。

**能力钩子只改状态。** 钩子是短命进程：改完 state / 配置受管键就返回，要按新配置重来一遍就回 `{"restart":true}`，由页面调 `ui.restartSelf()` 让宿主停整组 → `start` → 重新 spawn。钩子里起进程会变孤儿，杀进程组又是抢宿主的活。

线程方面先看你有没有通道：

- **`ready: notify`（通道插件，Rust）**：`svcd_serve` 自己开 1 条读线程、`WORKER_THREADS = 4` 条工作线程（`QUEUE_DEPTH = 64` 的有界队列），协议 3 还有一条写线程。handler 跑在工作线程里、`Plugin` 被 `Mutex` 串行化——别在 handler 里再起线程或做十几秒的阻塞活：那会占住一个工作线程、把队列顶满，然后整条协议开始延迟。
- **`ready: exec`（没有通道）**：不跑 `svcd_serve`，进程里就一条主线程。阻塞的等待放进 lifecycle 钩子（各有自己的 timeout），别放进主循环——主循环卡住，宿主只能按超时杀你。

怎么验：`ls /proc/<pid>/task | wc -l` 看线程数（`exec` 插件应该是 1），`ps -e -o pid,ppid,args | grep <id>` 看进程数（稳定态只有本体那一个）。

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
| 宿主注入 | 三个目录：`SVCD_PLUGIN_DIR`、`SVCD_LOG_DIR`、`SVCD_TEMP_DIR`；hook 另给 `SVCD_PLUGIN_ID`、`SVCD_PLUGIN_VERSION`、`SVCD_ACTION` |

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
| `Context::config_path / config_json / plugin_dir / data_dir / log_dir / temp_dir / id / version` | 宿主给你的环境（`data_dir` 就是 `<plugin_dir>/data`） |
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

**数据只写插件目录。** 插件进程只拿到三个目录变量：`SVCD_PLUGIN_DIR`（安装目录）、`SVCD_LOG_DIR`（日志，宿主写）、`SVCD_TEMP_DIR`（临时根，blob 放 `incoming/`）。自己的东西一律写 `$SVCD_PLUGIN_DIR/data/`：宿主升级、卸载（不带 `--purge`）都保留它，主机密钥、证书、geo 库因此不会丢。守护进程自己的数据根不导出给插件，别去猜它、也别读没在环境里的变量；脚本手跑时按自己的位置推插件目录（`$0` 往上三层即可）。

**UI 放 `package/payload/web/`。** 宿主把整棵树反代到 `/plugin-ui/<id>/`，页面用**相对路径** `fetch('rpc')`；插件进程不自起任何 HTTP 或 WebSocket 服务。

**源码留在 `package/` 外面。** `package/` 是打包根，打包器不过滤任何文件：放进去的 `src/`、`Cargo.toml`、`.gitkeep` 都会进 `.spk`。构建产物由 `build.json` 的 `artifact` 复制进 `payload/bin/`；同一目录里上一个平台留下的同名产物（`demo` ↔ `demo.exe`）会在构建前被清掉，否则两种架构会一起进包。

**`vendor/` 放源码，打包前编译。** 和 `meow`、`sshd` 一样：`vendor/<依赖>/` 存上游源码，`build.sh` 在打包时编译，产物落在 `out/` 或 `payload/bin/`。提交二进制会让它跟源码一起腐烂。唯一的例外是设备内核模块：它对着一棵 CI 里没有的厂商内核树编，只能预编译，并且只能用 `SHA256SUMS` 校验着放（见下面「单元专属的文件」）。`tools/check-vendor.py` 会拦住其余 ELF/`.a`。

**单元专属的文件放 `variants/<unit>/`，不要放 `package/`。** 一个包里只该有它那个单元（平台 + 设备）的东西：内核模块只对一个固件代次有效，`gotatun` 只属于通用包。`package/` 是所有包共用的，把这类文件放进去就只能靠 `build.sh` 记得在下一个包之前删掉——漏一次就进错包（打包器看到 `.ko` 还会要求声明 `compat`）。`variants/<unit>/` 和 `package/` 同形（`payload/bin/…`），打包时只按单元盖上去：

**入口脚本在 git 里要带可执行位（100755）。** 包里的文件模式来自源文件，发布流水线只会再收紧（签名时 `umask 077`），不会补上。`exec.path` 由宿主按声明的 `interpreter` 跑，缺位也能活；但脚本之间直接调（`"$HERE/wireguard.sh" up`）没有位就是 `Permission denied`——wireguard 1.2.2 在设备上就是这么挂的。两件事一起做：脚本里调兄弟脚本写成 `sh "$HERE/x.sh"`，同时 `git update-index --chmod=+x` 补上位；`tools/check-shell.py` 会检查 `payload/bin/*.sh` 与 `variants/*/payload/bin/*.sh` 在索引里是不是 100755。
| 目录 | 谁拿到 |
| --- | --- |
| `package/` | 所有包 |
| `variants/universal/` | 只给通用包（没有 `--device` 的那个） |
| `variants/<device>/` | 只给那个设备变体的包（`.plugin.json` 的 `variants[].device`） |

目录名必须是声明过的 device 或 `universal`，写错 `plugin-tools build` 直接报错。里面有预编译件时，`variants/<unit>/SHA256SUMS` 必须能校验它——`tools/check-vendor.py` 会拦。构建产物写进这两处也要在插件自己的 `.gitignore` 里忽略。

**平台差异写进 `platforms/`，不要为它加启动壳。** `.spk` 是按平台产出的（打包器盖 `platform`），而 manifest 的 `exec` / `lifecycle` / `interpreter` 各只有一个；脚本入口又必须挑解释器：POSIX 用 `sh`、Windows 用 `powershell`。需要按平台换入口时，`package/manifest.json` 只留平台无关的字段，入口写进与 `package/` 同级的 `platforms/<家族>.json`（`unix` / `windows`，一个家族一份；`template` 就生成这两份）：同名顶层键整段替换，`null` 删除，`id` / `version` 不许改。同家族里个别三元组真不一样时再加 `platforms/<三元组>.json` 当例外层，它压在家族之上；除此之外 `platforms/` 里只认 7 个三元组和这两个家族名，别的名字直接报错。`platforms/` 下还可以直接放入口脚本，构建时按扩展名分家族（`.sh` → unix，`.ps1`/`.cmd`/`.bat` → windows）复制进 `payload/bin/`，别的平台那份不进包；`platforms/lib/` 是同一件事的库版本，SDK 副本放那里、构建时按同样规则进 `payload/lib/`，所以 Linux 包里不会带 `SvcdPlugin.ps1`。`plugin-tools build` 按平台合并后再交给 `plugin bundle`，副本里缺了清单指的文件会立刻报错（没有 `platforms/` 的插件也查：Windows 上 `rust` / `c` 的产物是 `<name>.exe`，构建会把 `exec.path` 改成实际产物名写进副本，签名 job 从 tar 重打包时没有 `--exec` 可传）。发布流水线因此要从 `--stage dist/stage` 打 tar —— 签名 job 会重新 bundle 那个目录，平台清单必须在那之前就进包。

**同一三元组要出两个包时用 `variants`。** 比如 `aarch64` 既要有通用包（gotatun 用户态），又要有一个只带 `wireguard.ko`、只装到 `sr6110` 的包：`.plugin.json` 写 `"variants": [{"device": "sr6110", "platforms": ["aarch64-unknown-linux-musl"]}]`，构建单元就是“每个平台一个通用包 + 每个变体一个包”。变体的 `compat.devices` 由构建写进那一份清单（base manifest 不写，否则通用包也被一起锁死），产物名按规则带 `-<device>`。`build.json` 的 recipe 能拿到 `{device}`（通用包是空串）和同名环境变量 `SVCD_DEVICE`，用来挑该拷哪个二进制；`--device <id>` 只打那个变体，只给 `--platform` 时仍然只打通用包。变体的 `platforms` 必填——漏了会把内核模块塞进别的平台的包。为什么是“一个插件 + variants”而不是两个插件、以及预编译件的边界，见 `docs/adr/0006-one-plugin-with-variants.md`。

**能力清单一处改全。** 加/删能力时同时改 manifest 的 `capabilities[]` 和代码分支；Rust 用 `cargo test` 兜底，shell 靠 `case` 与清单人工对齐。

**能力要么走通道，要么挂短命钩子。** 一个能力只有一种投递方式：`ready: notify` 的插件有协议通道，能力由 `svcd_plugin_serve` 的 handler 回答，且**不许**声明 `hook`；`ready: exec` / `port` 的插件没有通道，因此每个能力都**必须**声明 `capabilities[].hook = {path, args?, env?, timeout?}`（包内相对路径，1..300 秒）。宿主每次 `tools/call` 起一次那个动作：`arguments` 写进 stdin 然后关掉，stdout 原文就是这次调用的文本结果（不解析 JSON，空输出就是空文本），stderr 进 `<id>-hooks.log`，超过契约内联上限算这次调用失败而不是截断。所以「一个常驻进程 + 偶尔被调一次」不必为了能力面再养一个通道 shell（`meow` / `wireguard` 就是这么改的）。

**钩子只改状态，重启交宿主。** 钩子是短命进程：改完自己那份启动期配置（state、配置文件的受管键）就返回，需要按新配置重来一遍时回 `{"restart":true}`，由页面调 `ui.restartSelf()`（`sdk/web/plugin-ui.mjs`，内部是 `POST /api/v1/services/<id>/restart`，id 从 `/plugin-ui/<id>/` 自己取）让宿主停整组 → 跑 `start` → 重新 spawn。别在钩子里起进程：孩子会挂在钩子自己的进程组下，钩子一退就成了孤儿；更别杀进程组，那是宿主的活。页面打宿主 `/api/v1` 的**写**请求是 cookie 写，宿主只认同源 `Origin` 加静态标记 `X-Svcd-Csrf: 1`（`Origin` 浏览器自己带，标记得你带）：`hostRequest` / `restartSelf` 已经在 `withCsrf` 里带上了，自己手写 `fetch('/api/v1/…')` 时写 `credentials: 'same-origin'` 和 `headers: {'X-Svcd-Csrf': '1'}`，漏了就是 `403 cookie writes require Origin and X-Svcd-Csrf`。

**`id` 是身份，`name` 是显示名。** `schema` 恒为 `1`（打包器盖章，源清单写别的值也会被覆盖），`id` 只能是 `[a-z0-9][a-z0-9_-]{0,63}`（不能是 `svc`）：安装目录、日志文件名、`<id>.<能力>` 工具名、`/plugin-ui/<id>/` 都用它。源清单省略 `id` 时取项目目录名；`name` 随便写，中文也行，缺省等于 `id`；命令与 API 只认 `id`，不按显示名解析。

**`platform` / `files[]` / `signature.json` 由打包器盖章。** 源 manifest 不写它们——都是派生的。产物文件名是 `<id>-<version>-<platform>[-<device>].spk`：声明了 `compat.devices` 的包再追加列表第一个 id（`<id>-<version>-<platform>-<device>.spk`，如 `sr6110`），商店按这个后缀认出设备。含 `.ko` 的包**必须**声明 `compat.devices`，否则打包器直接拒绝——内核模块只对某一代内核/固件有效。

**`outputSchema` 只做声明。** `capabilities[].outputSchema` 可选：写了就投影进 MCP `tools/list`，运行期不校验结果（不写 = 未声明，`{}` = 任意）。工具返回结构化 JSON 时才值得声明（模板里的 `stats` / `info` / `date` 就是示例）。

**`icons` 是可选的包图标。** 形状 `{"png"|"ico"|"icns": "<包内路径>"}`，作者自备三种格式（不做转格式/转尺寸）；构建时会先校验文件存在且非空，验包阶段再查魔数与大小。

**插件是常驻进程。** 没有 oneshot 语义；要"跑一次"，写成常驻 + 一个触发能力，或用 core 服务。

**生命周期钩子只在需要准备或收尾时用。** 被监督的永远是 `exec`：入口要带参数就写 `exec.args`（比如 `["start"]`），脚本在前台准备好资源后 `exec` 真正的进程。`lifecycle.start` 是**前置准备**，每次 spawn `exec` 之前跑一次、必须退出，非零或超时即本次启动失败；`lifecycle.stop` 在进程消失**之后**跑，只做收尾（撤全局规则、清 pidfile），终止权归宿主（Unix 进程组 SIGTERM → 10 秒 → SIGKILL，Windows 直接强杀）——不要指望在 `stop` 里杀进程或等它退出：强杀、断电留下的 pidfile 里那个 pid 可能已经被系统复用，按它发信号就是误伤。

**依赖保持最小。** Rust 只用 `serde` / `serde_json`；shell 只用 POSIX。SDK 不提供 HTTP 客户端、数据库或调度器——需要就自己加，但先确认体积可接受。

**raw 只在协议 3 里。** `protocol: 1` 的插件没有 `/raw`（宿主回 `raw_unavailable`）；要同时有
调用、推送与 raw，就用 `multiplex::serve` 并把清单改成 `protocol: 3`。C / shell / PowerShell
SDK 没有 raw 实现。

**大对象走 blob，不走 JSON。** 参数传 `{"$blob": id}`，结果返回 `$blobPath`；1 MiB 帧只装小 JSON。

**vendored 副本别手改。** `package/payload/lib/svcd-plugin.sh`（`script` 项目里是 `platforms/lib/`）、`payload/web/plugin-ui.mjs` 和 `src/svcd_plugin.h` 都是 `sdk/` 的副本；改完 `sdk/` 后跑 `plugin-tools sync`。它只刷新插件已经带着的那几份，不会给不用工具包的页面塞一份 `plugin-ui.mjs`。

## 做不到的事

- **插件之间互相调能力**：宿主没有开放这条通道（daemon socket 只有 `svc.*` 管理方法）。
- **自起 HTTP 服务**：单端口模型由宿主独占。
- **宿主级定时调度**：没有 cron；定时器在进程内，重启即归零。
- **权限隔离**：插件以安装者权限运行，隔离是未决问题。
- **跨目标构建**：`plugin-tools build --platform` 只改 manifest 的盖章和 `build.json` 里的 `{platform}`；工具链那张唯一的表是 `tools/cross-env.sh`——`build.sh` 里 `. "$REPO/tools/cross-env.sh"` 再 `svcd_cross_apply "$PLATFORM"`，别自己拼 `CC_/AR_/CARGO_TARGET_*_LINKER`。`tools/cross/install.sh <triple>` 只负责把 musl.cc 的包下到 `/usr/soft`（CI 写 `$GITHUB_ENV`，本地打印 export 行）。想在本机零安装打包，用 `tools/pack.sh <插件> [平台…]`：它在 docker 里跑构建（给 `build.sh` 的信号是 `SVCD_DOCKER=1`），宿主只要 docker 和一个**叫 `plugin`** 的 svcd 副本（svcd 按 `argv[0]` 分派，CI 的 host artifact 就是这么放的）。
- C 的 `build.json` 用 `{cc}` 取编译器（`$CC` 优先，否则 `gcc`），Rust 用 `--target {platform}` 加 `rustup target add`。

## 常见错误 → 症状

| 症状 | 原因 |
|---|---|
| 装上了但 `start` 超时 | 脚本插件没写 `"ready": "exec"`（缺省 `notify` 会等 ping） |
| `ps` 里多一个 `sh` | 入口脚本没 `exec` 掉自己（自己写了看护循环），或周期任务用 `&` 起了常驻子 shell——看「一个进程、一个线程」那一节 |
| 改了配置但进程还是老的 | 钩子只改状态，忘了让页面调 `ui.restartSelf()`（钩子不许自己重启服务） |
| 页面报 `403 cookie writes require Origin and X-Svcd-Csrf` | 手写的 `/api/v1` 写请求漏了 `X-Svcd-Csrf: 1`（SDK 的 `hostRequest` / `restartSelf` 自带） |
| `status` 报 `running` 但没在跑 | pidfile 里的 pid 被复用给别的进程了：认进程要 `kill -0` + `/proc/<pid>/cmdline` 对一下 |
| MCP 里有这个工具，调用却报错 | manifest 与代码分支漂移 |
| 包在目标机上装不上 | `platform` 与本机三元组不等 |
| Windows 上启动 `exit=9009` | `interpreter.name` 写了 `python3`（那是 0 字节存根，用 `python` 或 `py`） |
| Debian 上装不上 | 给 `sh` 写了 `interpreter.version`（dash 的 `--version` 会报错） |
| 包里出现源码 | 源码或占位文件放进了 `package/` |
| `vendor/` 里提交了二进制 | `tools/check-vendor.py` 会报错：源码放 `vendor/`、打包时编译；预编译件只能放 `vendor/prebuilt/<三元组>/` 或 `variants/<unit>/`，并带 SHA256SUMS |
| 一个包里有别的单元的文件 | 放进了 `package/`：单元专属（设备内核模块、某个后端）的放 `variants/<unit>/`，见「单元专属的文件」 |
| 设备日志里 `.../payload/bin/x.sh: Permission denied` | 入口脚本在 git 里没有可执行位（100644）；`git update-index --chmod=+x`，并且脚本之间改成 `sh "$HERE/x.sh"` 调用 |
| 本机打包报找不到 `cc` / `<triple>-gcc` | 构建脚本没走 `tools/cross-env.sh`，或目标工具链没装（`tools/cross/install.sh <triple>`） |
| 装不上，提示重打包 | 包不是 `schema: 1` 或没有 `id`——用新版 `plugin-tools build` 重打 |
| 目录名变成中文/乱码 | 把显示名写进了 `id`；中文只能放 `name` |
| 改了 SDK 但插件没变 | vendored 副本没同步（`plugin-tools sync`） |
| UI 的 `/raw` 连不上（`raw_unavailable`） | 插件不是 `protocol: 3`，或还在用协议 1 的 `serve` |
| raw 帧发出去没人收 | 订阅者已经关了这条 stream；宿主不通知插件，按请求生产 |
