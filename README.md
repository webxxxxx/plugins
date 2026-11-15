# 插件仓库

四种语言的插件模板（Rust / C / Shell / PowerShell）、它们共用的协议壳，以及本地打包与校验的工具。
插件作者只写核心逻辑，其余在 SDK 里。

## 结构

```
sdk/rust/         crate `svcd-plugin` —— 协议循环、并发、取消、MCP 返回形状、文件收发
sdk/c/            单头文件协议壳
sdk/shell/        POSIX sh 协议壳（随插件拷进包里）
sdk/powershell/   PowerShell 协议壳（随插件拷进包里）
sdk/web/          浏览器侧 toolkit（ESM，无构建步骤）：invoke / SSE / WS / 上传下载
templates/        rust / c / shell / powershell 四种模板源
samples/hello-rust             Rust 参考实现：五能力 + 配置 + 通知 + UI
samples/hello-c           C 同款（单头文件 SDK）
samples/hello-shell       Shell 同款（POSIX sh + awk）
samples/hello-powershell  PowerShell 同款
skills/           给 AI 的插件开发手册（语言、toolkit、边界）
tools/cli/        Rust 工具链：单二进制 plugin-tools（template / build / dev / devkey / sync / check）
```

模板是唯一真源；由模板生成的样例用 `--force` 重新生成，别手改。`samples/hello-rust` 是手写的参考实现，不受模板约束。

## 装工具链（一次性）

工具链是一个 Rust 二进制，**不需要 Node、Python 或任何解释器**：

```bash
cd tools/cli && cargo build --release
# 产物：tools/cli/target/release/plugin-tools[.exe]，放进 PATH 即可
```

它只做编排：模板是数据（`build.json` + 目录树），打包仍调宿主的 `plugin bundle`。

## CI

`.github/workflows/test.yml` 在两个操作系统上跑 SDK、工具链与 Rust 样例的测试，并给四种语言各脚手架一次；`.github/workflows/build.yml` 先构建宿主的 `plugin` CLI，再用它打包每份样例并 `verify`，产物作为 artifact 上传。没有 Node、没有 daemon，跑的全是 `plugin bundle` 的本地路径。

## 两条命令

```bash
plugin-tools template wireguard                 # 数字菜单：语言 1-4 → 平台 1-7
cd plugins/wireguard && plugin-tools build      # 构建 + 打包（默认未签名）
```

非交互（CI 用）：

```bash
plugin-tools template wireguard --lang c --platforms x86_64-unknown-linux-musl,x86_64-pc-windows-msvc --webui
```

`plugin-tools build` 不给 `--key` 时产出未签名包，宿主需要 `allow_insecure_install = on`；
发布路径是 CI 用发布钥调 `plugin bundle --key`。

`build` 不给 `--platform` 时按 `.plugin.json` 的 `platforms` 逐个打；没写 `platforms` 的插件视为 7 个平台通用。本地开发用 `--platform <本机三元组>` 只打一个。`check <file.spk>` 走 `plugin verify`。

每个插件（含四份样例和四份模板）都带自己的 `.github/workflows/build.yml`：平台列表从 `.plugin.json` 读，没写就 7 个全build，每个平台在自己的 runner 上构建并上传 `.spk`。插件独立成仓库后直接就是它的 CI。

## 插件只写核心逻辑

Rust：

```rust
impl Plugin for MyPlugin {
    const CAPABILITIES: &'static [&'static str] = &["echo"];
    fn call(&mut self, name: &str, arguments: &Value) -> ToolOutcome {
        match name { "echo" => ToolOutcome::text("hi"), other => ToolOutcome::error(other) }
    }
}
fn main() { svcd_plugin::serve(Context::from_env(), MyPlugin) }
```

Shell：

```sh
handle() { case "$1" in uname) uname -a ;; *) return 1 ;; esac }
svcd_plugin_serve handle
```

就绪探针、关闭请求、帧上限、并发调度、取消令牌、MCP 结果形状都在 SDK 里。

## 浏览器侧四个通道

插件不自起 HTTP。静态资源与下面三条都在 `/plugin-ui/<id>/` 下，由 svcd 反代：

| 通道 | 路径 | 用途 |
|---|---|---|
| method invoke | `POST ./rpc` | JSON-RPC `tools/call` |
| SSE | `./events` | 服务端推送，`id:<seq>` + `event: message`，另有 `event: reset` |
| WebSocket | `./ws` | 同一路推送的另一种承载，**不是**请求通道 |
| 静态资源 | `./` | 包内 `payload/web/` |

```js
import { pluginUI } from './plugin-ui.mjs';
const ui = pluginUI();
await ui.invoke('echo', { message: 'hi' });          // → "hi"
ui.subscribe({ onMessage: (method, params) => {} }); // 推送
```

## 目录约定：`package/` 是唯一打包根

打包器无差别递归，**没有忽略规则**：`package/` 里有什么，包里就有什么。
所以源码、`Cargo.toml`、`target/`、占位文件都必须留在它外面。

构建由插件根目录的 `build.json` 声明，驱动里没有任何语言分支：

```json
{ "build": { "command": ["cargo", "build", "--release"],
             "artifact": "target/release/{name}" } }
```

加一门语言 = 加一个模板目录 + 一份 `build.json`，`plugin-tools build` 不用改。

已知还需要宿主侧放宽的两点（会挡流水线）：

1. `plugin verify` 要求签名钥在 `trust.d` —— CI runner 上没有。
2. `plugin verify` 要求 `platform` 等于本机三元组 —— x86 runner 验不了 aarch64 包。

详见 `templates/rust/README.md` 的「验证」一节。
