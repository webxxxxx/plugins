---
name: svcd-plugin
description: 新增或修改 svcd 插件前读：支持哪两种语言、SDK 与 Web toolkit 怎么用、以及会让包直接失效的边界（stdout、端口、打包根、能力清单）。
---

# 写 svcd 插件

插件是一个**常驻进程**：stdout 上跑 NDJSON JSON-RPC，向宿主暴露若干「能力」；浏览器 UI 由宿主反代，插件自己不开端口。你的工作只有一件事——实现能力逻辑，其余由 SDK 承担。

## 三步走

### 1. 起项目

```bash
plugin-template <name> --lang rust|shell [--webui]
```

完成判据：`package/manifest.json` 与 `build.json` 存在，且目录里**没有** `target/`、`dist/`。

### 2. 只写 handler

Rust：

```rust
impl Plugin for MyPlugin {
    const CAPABILITIES: &'static [&'static str] = &["echo", "stats"];

    fn call(&mut self, name: &str, arguments: &Value) -> ToolOutcome {
        match name {
            "echo" => ToolOutcome::text(arguments["message"].as_str().unwrap_or("")),
            "stats" => ToolOutcome::text(format!("ticks={}", self.ticks)),
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
plugin-build          # 构建 + 打包（默认未签名，本地开发用）
plugin install <spk>  # 首装不自启
plugin start <name>
```

完成判据：`plugin-build` 打印 `packed … (unsigned)`，且随后 `verify` 输出 `ok: true`。

未签名包要求宿主 `svcd.conf` 里 `allow_insecure_install = on`（改完重启 svcd）。发布走 CI，用发布钥签名。

## 支持的语言

| 语言 | 模板 | 目标机需要什么 | 适合 |
|---|---|---|---|
| `rust` | `templates/rust` | 无（编译成静态二进制） | 需要状态、并发、JSON 处理的插件 |
| `shell` | `templates/shell` | 目标机有 `sh` | 包装已有命令、输出是文本 |

两种语言共用同一套契约和同一个 Web toolkit；**前端不认识后端语言**。

加第三种语言不需要改工具：在 `templates/<lang>/` 放模板，并写一份 `build.json`，`plugin-build` 就会照着做。

## 契约速查

| 项 | 值 |
|---|---|
| 通道 | stdout（协议）+ stderr（日志），一帧一行 |
| 方法 | `svcd.ping`、`svcd.status`、`svcd.shutdown`、`tools/call` |
| 结果形状 | `{"content":[{"type":"text","text":"…"}],"isError":false}` |
| 帧上限 | 1 MiB；无握手；`protocol: 1` |
| 错误码 | `-32700` 解析、`-32601` 方法不存在、`-32602` 未知能力、`-32603` 过载 |
| 推送 | `emit(method, params)` → 宿主转成 SSE / WebSocket 给 UI |
| 宿主注入 | `SVCD_PLUGIN_ID`、`SVCD_PLUGIN_VERSION`、`SVCD_CONFIG`、`SVCD_DATA_DIR`、`SVCD_LOG_DIR`、`SVCD_HTTP_PORT`、`SVCD_TARGET` |

`svcd.ping` / `svcd.shutdown` 由 SDK 自动应答，不要自己实现。

## toolkit

### Rust（`sdk/rust`，crate `svcd-plugin`）

| API | 作用 |
|---|---|
| `svcd_plugin::serve(Context::from_env(), plugin)` | 协议循环、有界工作池、取消、按 id 乱序回复 |
| `Plugin::call(&mut self, name, args)` | 你的能力实现 |
| `Plugin::status(&self)` | `svc status` 的一行摘要 |
| `ToolOutcome::text / error` | 结果；`error` 对应 `isError: true` |
| `Context::config_path / config_json / data_dir / log_dir / id / version` | 宿主给你的环境 |
| `Context::cancel()` → `Cancel::{is_cancelled, sleep, ticker}` | 后台循环必须观察的取消令牌 |
| `svcd_plugin::emit(method, params)` | 推送给 UI |
| `log_info! / log_warn! / log_error!` | 写 stderr（宿主捕获并轮转） |
| `declared_capabilities(path)` | 读 manifest 的能力清单，用于 drift test |

### Shell（`sdk/shell`，随包拷贝）

| API | 作用 |
|---|---|
| `svcd_plugin_serve <handler>` | 协议循环；自动应答 ping/status/shutdown |
| `<handler> <name> <args-json>` | 你的能力实现；stdout 即结果，非 0 退出即 `isError` |
| `svcd_notify <method> <json>` | 推送 |
| `svcd_status` | 可覆盖，供 `svc status` |

`arguments` 在 shell 里是**原样 JSON 字符串**（没有解析器）。参数是扁平字符串时直接 `case`/`sed` 取；要真正解析 JSON 就换 Rust。

### Web（`sdk/web`，浏览器 ESM）

```js
import { pluginUI } from './plugin-ui.mjs';
const ui = pluginUI();

await ui.invoke('echo', { message: 'hi' });          // POST ./rpc
ui.subscribe({ onMessage(method, params) {} });      // SSE ./events
ui.subscribeWebSocket({ onMessage() {} });           // WebSocket ./ws（同样是推送）
```

| 通道 | 路径 | 用途 |
|---|---|---|
| 方法调用 | `POST ./rpc` | 调能力 |
| SSE | `./events` | 服务端推送，带 `id: <seq>`，另有 `event: reset` |
| WebSocket | `./ws` | 同一路推送的另一种承载 |

## 边界

**stdout 只写协议。** 日志、调试输出、`print` / `console.log` 一律去 stderr——它是宿主的日志通道，会写进 `<logs>/<id>.log` 并轮转。Rust 用 `log_info!`，shell 用 `echo … >&2`。

**UI 放 `package/payload/web/`。** 宿主把整棵树反代到 `/plugin-ui/<id>/`，页面用**相对路径** `fetch('rpc')`；插件进程不自起任何 HTTP 或 WebSocket 服务。

**源码留在 `package/` 外面。** `package/` 是打包根，打包器不过滤任何文件：放进去的 `src/`、`Cargo.toml`、`.gitkeep` 都会进 `.spk`。构建产物由 `build.json` 的 `artifact` 复制进 `payload/bin/`。

**能力清单一处改全。** 加/删能力时同时改 manifest 的 `capabilities[]` 和代码分支；Rust 用 `cargo test` 兜底，shell 靠 `case` 与清单人工对齐。

**`platform` 由打包器盖章。** 源 manifest 不写 `platform`、不写 `files[]`、不写 `signature.json`——它们都是派生的。

**插件是常驻进程。** 没有 oneshot 语义；要"跑一次"，写成常驻 + 一个触发能力，或用 core 服务。

**依赖保持最小。** Rust 只用 `serde` / `serde_json`；shell 只用 POSIX。SDK 不提供 HTTP 客户端、数据库或调度器——需要就自己加，但先确认体积可接受。

**vendored 副本别手改。** `package/payload/lib/svcd-plugin.sh` 和 `payload/web/plugin-ui.mjs` 是 `sdk/` 的副本；改完 `sdk/` 后跑 `node tools/plugin.mjs sync`。

## 做不到的事

- **插件之间互相调能力**：宿主没有开放这条通道（daemon socket 只有 `svc.*` 管理方法）。
- **自起 HTTP 服务**：单端口模型由宿主独占。
- **宿主级定时调度**：没有 cron；定时器在进程内，重启即归零。
- **权限隔离**：插件以安装者权限运行，隔离是未决问题。
- **跨目标构建**：`plugin-build --platform` 只改 manifest 的盖章；真正的交叉编译需要目标工具链（Rust：`rustup target` + zig）。

## 常见错误 → 症状

| 症状 | 原因 |
|---|---|
| 装上了但 `start` 超时 | 脚本插件没写 `"ready": "exec"`（缺省 `notify` 会等 ping） |
| MCP 里有这个工具，调用却报错 | manifest 与代码分支漂移 |
| 包在目标机上装不上 | `platform` 与本机三元组不等 |
| Windows 上启动 `exit=9009` | `interpreter.name` 写了 `python3`（那是 0 字节存根，用 `python` 或 `py`） |
| Debian 上装不上 | 给 `sh` 写了 `interpreter.version`（dash 的 `--version` 会报错） |
| 包里出现源码 | 源码或占位文件放进了 `package/` |
| 改了 SDK 但插件没变 | vendored 副本没同步（`node tools/plugin.mjs sync`） |
