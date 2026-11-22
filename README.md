# 插件仓库

四种语言的插件模板（Rust / C / Shell / PowerShell）、它们共用的协议壳，以及本地打包与校验的工具。
插件作者只写核心逻辑，其余在 SDK 里。

插件下载列表：[docs/plugins.md](docs/plugins.md)（CI 在每次发布后自动更新，只列每个插件/平台的最新稳定版）。

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

## 发布

`.github/workflows/release.yml` 处理发布。两种 tag：

- **全量发布**：`v*`（如 `v1.0.0`）构建 `plugin-tools` + `plugins/` 下全部插件；
- **单插件发布**：`<plugin>-v*`（如 `hysteria-v1.0.1`）只构建该插件，tag 版本必须等于该插件 `package/manifest.json` 的 `version`。

单插件发布示例：

```bash
# 先把 plugins/hysteria/package/manifest.json 的 version 改成 1.0.1
git tag hysteria-v1.0.1
git push origin hysteria-v1.0.1
```

tag 里的版本必须和插件 manifest 的 `version` 一致；只改插件代码、不 bump 版本时 workflow 会直接报错。

手动 `workflow_dispatch` 也可以选 `plugin=all` 或单个插件名（配合 `tag` 和 `release`）。

每次发布：

- **项目本身**：构建 `plugin-tools`，产出 Linux x86_64 gnu、Windows x86_64 msvc、macOS universal2 三份归档（仅全量发布）；
- **`plugins/` 下的插件**：自动发现 `plugins/*/.plugin.json`，按 `ci.platforms`（没有就 `platforms`）生成矩阵，逐个平台构建 `.spk`；
- **发布**：汇总 `.spk`、`plugin-tools` 归档和 `SHA256SUMS`，`gh release create/upload` 到同一个 Release；tag 带 `-`（如 `v1.0.0-rc1`、`hysteria-v1.0.1-rc1`）自动标 prerelease；
- **插件列表**：发布成功后 `update-plugin-list` job 重新生成 [docs/plugins.md](docs/plugins.md) 并提交到 `main`，只保留每个插件/平台的最新稳定版。

插件构建使用仓库存的 `secrets.PLUGIN_SIGNING_KEY`（Ed25519 PKCS#8 PEM）。没有这个 secret 时 PR 只做未签名构建；tag / 手动发布则直接失败，避免发出未签名包。

发布公钥在 [docs/release-key.pub](docs/release-key.pub)（SHA-256 指纹
`af7c736c946e2340e48cac641177514792daec9539cdbc6a8998b2a8a8e91cf6`）。
安装发布包前先信任它：

```bash
svc trust add docs/release-key.pub
```

私钥只保存在发布机上（`~/.svcd-release/release.pem`），不要提交；轮换时同时更新
GitHub secret 和这个公钥文件。

### CI 平台覆盖

每个插件的 `.plugin.json` 可以写 `ci.platforms` 覆盖发布矩阵；meow 因 BoringSSL 交叉链目前只覆盖 Linux x86_64 gnu + 两种 macOS。完整的 `platforms` 仍可用于本地构建。

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
