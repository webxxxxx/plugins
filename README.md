# 插件仓库

一期的两种插件模板（Rust / Shell）、它们共用的协议壳，以及本地打包与校验的工具。
插件作者只写核心逻辑，其余在 SDK 里。

## 结构

```
sdk/rust/         crate `svcd-plugin` —— 协议循环、并发、取消、MCP 返回形状
sdk/shell/        POSIX sh 协议壳（随插件拷进包里）
sdk/web/          浏览器侧 toolkit（ESM，无构建步骤）
templates/rust/   模板源
templates/shell/  模板源
samples/          脚手架空跑出来的标准示例（plugin-template ... --force 重新生成）
tools/            plugin-template.mjs（新建/构建/打包未签名包）、plugin.mjs（devkey/pack/dev/check/sync）
```

模板是唯一真源；`samples/` 是它的实例，改了模板就用 `--force` 重新生成，别手改。

## 三条命令

```bash
node tools/plugin-template.mjs hello --lang rust --webui   # 新建 + 构建 + 打包（未签名）
node tools/plugin.mjs dev                                  # 打包（签名）+ 安装 + 启动
node tools/plugin.mjs devkey                               # 需要签名打包时：生成开发钥
```

`node tools/plugin.mjs pack --platform <triple>` 单独打包；`check <file.spk>` 走 `plugin verify`。

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

已知还需要宿主侧放宽的两点（会挡流水线）：

1. `plugin verify` 要求签名钥在 `trust.d` —— CI runner 上没有。
2. `plugin verify` 要求 `platform` 等于本机三元组 —— x86 runner 验不了 aarch64 包。

详见 `templates/rust/README.md` 的「验证」一节。
