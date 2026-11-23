# 插件仓库

四种语言的插件模板（Rust / C / Shell / PowerShell）、它们共用的协议壳，以及本地打包与校验的工具。
插件作者只写核心逻辑，其余在 SDK 里。

插件下载列表：[docs/plugins.md](docs/plugins.md)（CI 在每次发布后自动更新，只列每个插件/平台的最新稳定版）。

## 下载全部发布包

插件商店自带一个批量下载子命令，复用安装时那条下载路径（代理、GitHub 资产 DNS 被污染时的 DoH 回退、`PLUGIN_STORE_MIRROR` 镜像都生效）：

```bash
cargo run --release --manifest-path plugins/plugin-store/Cargo.toml -- download
```

默认读取 `docs/plugins.md`，把其中每个 `.spk` 下载到 `dist/`。已存在的文件默认跳过；`--force` 重新下载，`--dry-run` 只打印清单，`--manifest` / `--output` 换输入和目录。下载先写临时文件再改名，中断不会留下半个包；`dist/` 已在 `.gitignore` 里。

只下某个插件或单个包：

```powershell
# 这个插件的所有平台
.\plugin-store.exe download --only meow

# 精确到一个包
.\plugin-store.exe download --only plugin-store-0.1.0-x86_64-pc-windows-msvc
```

`--only` 不区分大小写，可重复；匹配插件名、版本、平台、设备或文件名。

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
tools/cross/      musl 交叉工具链的准备脚本：`install.sh <triple>`（musl.cc 的包 + 镜像回退）
```

模板是唯一真源；由模板生成的样例用 `--force` 重新生成，别手改。`samples/hello-rust` 是手写的参考实现，不受模板约束。

## 装工具链（一次性）

工具链是一个 Rust 二进制，**不需要 Node、Python 或任何解释器**：

```bash
cd tools/cli && cargo build --release
# 产物：tools/cli/target/release/plugin-tools[.exe]，放进 PATH 即可
```

它只做编排：模板是数据（`build.json` + 目录树），打包仍调宿主的 `plugin bundle`。

## manifest 只有 schema 1

`.spk` 清单只有一种格式：`schema: 1`。包里的 `id` 是必填身份（`[a-z0-9][a-z0-9_-]{0,63}`，不能是
`svc`），安装目录、日志、MCP 工具名前缀、`/plugin-ui/<id>/` 都用它；源清单不写时打包器取项目目录名。
`name` 只是显示名，可以写中文，缺省等于 `id`。能力的 `outputSchema` 与包图标 `icons` 都是可选的，
只做声明与投影。

`plugin bundle` 恒定写 `schema: 1`（源清单写的值会被覆盖，不会降级），`plugin-tools build` 按
`<id>-<version>-<platform>.spk` 命名产物。
**不产出旧 schema 的包**：还在报 `schema_max: 3` 的老宿主装不上新产物，这是有意的——
先升级宿主，再用同一条线（main）的 `plugin-tools` 重打包。

## CI

`.github/workflows/test.yml` 在两个操作系统上跑 SDK、工具链与 Rust 样例的测试，并给四种语言各脚手架一次；`.github/workflows/build.yml` 先构建宿主的 `plugin` CLI，再用它打包每份样例并 `verify`，产物作为 artifact 上传。没有 Node、没有 daemon，跑的全是 `plugin bundle` 的本地路径。

发布工作流的 PR 构建只跑改动过的插件：`plugins/<name>/**` 命中谁就构建谁；改了 `sdk/`、`templates/` 或 `tools/cli/` 时回退到全部插件。

## 发布

`.github/workflows/release.yml` 处理发布。两种 tag：

- **全量发布**：`v*`（如 `v1.0.0`）构建 `plugin-tools` + `plugins/` 下全部插件；
- **单插件发布**：`<plugin>-v*`（如 `hysteria-v1.0.1`）只构建该插件，tag 版本必须等于该插件 `package/manifest.json` 的 `version`。

改版本、提交、打 tag、推送交给 [tools/tag.py](tools/tag.py)：

```bash
python tools/tag.py all patch             # 全量发布：plugin-tools 0.2.1 -> 0.2.2，tag v0.2.2
python tools/tag.py plugin-store patch    # 单插件发布：0.1.17 -> 0.1.18，tag plugin-store-v0.1.18
python tools/tag.py sshd 1.0.1 --dry-run  # 指定版本，只打印会改哪些文件
```

- `all` 改 `tools/cli/Cargo.toml` 和它的 `Cargo.lock`；单个插件改 `plugins/<name>/package/manifest.json`，Rust 插件连 `Cargo.toml`、`Cargo.lock` 一起改。两处必须同步，否则 CI 的 `cargo --locked` 会在第一步就拒绝构建。
- 版本号带 `-rc1` 时只有 tag 带后缀，manifest 里写 `1.0.1`（workflow 拿 `-` 之前的部分去比 manifest）。
- 动手之前会检查：在 `main` 上、没有未提交改动、tag 没被用过；改完先跑一遍 `cargo metadata --locked`，和 CI 的同一步对齐。
- `--no-push` 只在本地产提交和 tag，`--no-verify` 跳过 cargo 校验。

手动发布也行，注意 tag 里的版本必须和插件 manifest 的 `version` 一致：

```bash
# 先把 plugins/hysteria/package/manifest.json 的 version 改成 1.0.1
git tag hysteria-v1.0.1
git push origin hysteria-v1.0.1
```

只改插件代码、不 bump 版本时 workflow 会直接报错。

手动 `workflow_dispatch` 也可以选 `plugin=all` 或单个插件名（配合 `tag` 和 `release`）。

### 关掉 macOS 构建

`macos-15` 那两台 runner 经常排长队（`The job was not acquired by Runner of type hosted`），等 15 分钟就失败。可以关掉它：

- **长期**：Settings → Secrets and variables → Actions → **Variables** 新建 `BUILD_MACOS`，值 `false`（也认 `0`/`no`/`off`）。不设或设 `true` 就是原来的行为。
- **单次**：`gh workflow run release.yml -f tag=<tag> -f plugin=<名字|all> -f release=true -f build_macos=false`。

关掉之后这次发布不跑 `host tools / macos-15`、不产出 `plugin-tools` 的 universal2 归档，也不产出各插件的 `*-apple-darwin` 包。旧包不会被删：`docs/plugins.md` 和七牛 `index.json` 会保留上一版的 mac 条目，只是版本停在上一版；七牛根目录上的旧 mac 文件也不会被挪进 `history/`，因为新版本没顶掉它们。

每次发布：

- **项目本身**：构建 `plugin-tools`，产出 Linux x86_64 gnu、Windows x86_64 msvc、macOS universal2 三份归档（仅全量发布）；
- **`plugins/` 下的插件**：自动发现 `plugins/*/.plugin.json`，按 `ci.platforms`（没有就 `platforms`）生成矩阵，逐个平台构建未签名 package；
- **签名**：`sign` job 只下载上一步的 package，在 `release` environment 里用 `secrets.PLUGIN_SIGNING_KEY` 重新 `plugin bundle --key` 并 `plugin verify`；构建脚本接触不到私钥；
- **发布**：汇总已签名的 `.spk`、`plugin-tools` 归档和 `SHA256SUMS`，`gh release create/upload` 到同一个 Release；tag 的**版本部分**带 `-`（如 `v1.0.0-rc1`、`hysteria-v1.0.1-rc1`）自动标 prerelease，`hysteria-v1.0.1` 这类单插件稳定版不标；
- **插件列表**：发布成功后 `update-plugin-list` job 重新生成 [docs/plugins.md](docs/plugins.md) 并提交到 `main`，只保留每个插件/平台的最新稳定版。
- **七牛镜像**：GitHub Release 建好后再跑一个 job，把本版包推到七牛空间根目录，被本版替换掉的旧文件移到 `history/<版本>/`，然后按空间实际内容重写根目录的 `index.json`。没配 `QINIU_*` secret 时整步跳过，不影响发布。

私钥只存在于 GitHub Actions Secret（建议放 `release` environment secret）和发布机，仓库里只有公钥。建议给 `release` environment 配 required reviewers；构建 job 不注入 secret，没有它时 PR 只做未签名构建，tag / 手动发布会在 `sign` job 直接失败，避免发出未签名包。

发布公钥在 [docs/signing.pub](docs/signing.pub)（SHA-256 指纹
`af7c736c946e2340e48cac641177514792daec9539cdbc6a8998b2a8a8e91cf6`）。
安装发布包前先信任它：

```bash
svc trust add docs/signing.pub
```

`svc trust add` 会把公钥按文件名放进 trust.d；发布包的签名 keyId 是 `signing`，所以公钥文件必须命名为 `signing.pub`。`docs/release-key.pub` 只是旧链接的兼容别名。

`svcd --install` 会把发布公钥写到 `<config>/trust.d/signing.pub`；只有已经装好、但还没跑过安装步骤的机器才需要手动 `svc trust add` 一次。

私钥只保存在发布机上（`~/.svcd-release/release.pem`）和 GitHub Actions Secret，不要提交；轮换时同时更新
GitHub secret 和这个公钥文件。

### 七牛云镜像（可选）

发布成功后 `.github/scripts/sync_qiniu.py` 把产物同步到一个七牛 Kodo 空间。空间根目录就是"新装用户该看到的东西"：

```
<插件>-<版本>-<平台>[-<设备>].spk   当前版本的包
plugin-tools-<版本>-<目标>.tar.gz   当前版本的工具链（仅全量发布）
svcd-<版本>-<目标>.zip|tar.gz       守护进程本体（从 plugin-manager 的最新稳定版镜像）
signing.pub, release-key.pub        发布公钥
SHA256SUMS                          本次发布这些文件的校验和
index.json                          导航文件：根目录每个包的版本、大小、sha256、下载地址
history/<发布 tag>/<文件名>         被本版替换掉的旧文件
```

对象类型按内容给：`.spk` 存成 `application/octet-stream`、`index.json` 是 `application/json`、`SHA256SUMS` 是 `text/plain`——上传凭证带 `detectMime: -1`，否则七牛会把 `.spk` 按扩展名猜成 `application/x-tar`（`detectMime: 0` 时 `application/octet-stream` 会被刻意忽略，照样去侦测）。

`history/` 只有一种目录命名：**每次发布一个 tag 目录**，里面放这次被替换掉的包，以及这次发布的 `SHA256SUMS`。按版本分目录在这里行不通——一次全量发布里各插件带的版本号互不相同，没有单一的"版本"可以当目录名。

三条规则保证这个形状不会歪：根目录只放"该身份（插件/平台/设备，工具链是目标三元组）的最新版"；旧文件只有在本版带来**同一身份且不低于**的版本时才移进 `history/`，所以单个插件的发布会保留其它插件的当前包；`index.json` 是**按空间实际列表重算**的，不是按本地目录拼的，因此不会出现"索引里有、空间里没有"。

需要的 secret（加在**仓库级** Actions secrets：Settings → Secrets and variables → Actions；
不是 Variables（明文），也不是 `release` environment——那个只给 `sign` job）：

两个访问密钥缺任一就跳过整步，其余两项缺了会直接报错，免得半套配置静默通过。

| secret | 说明 |
| --- | --- |
| `QINIU_ACCESS_KEY` / `QINIU_SECRET_KEY` | 七牛账号密钥对（控制台「密钥管理」）。社区版是账号级权限，没有「只授权一个空间」的粒度：泄露等于整个账号可写，只放 secret 里，别落进配置或日志 |
| `QINIU_BUCKET` | 空间名 |
| `QINIU_DOMAIN` | 空间的公开访问域名，带 `http://` 或 `https://`，末尾不加斜杠；`http` 会在日志里警告 |

`index.json` 的三个数组各管一段，别混：

| 字段 | 内容 | 什么时候有 |
| --- | --- | --- |
| `packages[]` | `.spk`：`plugin` `version` `platform` `device` `file` `size` `sha256` `url` `released` | 每次发布 |
| `tools[]` | `plugin-tools` 工具链归档：`version` `target` `file` `size` `sha256` `url` `released` | 只有全量发布（`v*` tag） |
| `svcd[]` | 守护进程归档：`version` `target` `file` `size` `sha256` `url` | 每次发布都尽力镜像 `plugin-manager` 的最新稳定版 |
| `keys[]` | 根目录的发布公钥（`signing.pub`、兼容别名 `release-key.pub`） | 每次发布 |

`svcd[]` 的字节由 CI 从 `webxxxxx/plugin-manager` 的最新**稳定** release 拉下来，和插件包放在同一个根目录，`svcd_sums_url` 指向根目录的 `SHA256SUMS`（那份清单覆盖根目录所有已知哈希的文件），所以宿主拿到的 `sumsAsset` 和校验方式跟走 GitHub 时一模一样。

这里要认清代价：宿主用**同一份** `SHA256SUMS` 验归档，镜像同时拿到字节和哈希，就等于镜像可以决定守护进程跑哪一版（它不能伪造字节，因为改不了哈希，但可以让升级停在旧版本、或提供一份自洽的旧版）。要彻底解决只能给 svcd 的 release 加签名、由宿主按签名校验；在那之前，把 `catalog_source` 留在 `github` 就是"不信任镜像选版本"的开关。

导航文件可直接取：

```bash
curl -fsS https://<QINIU_DOMAIN>/index.json | jq -r '.packages[] | "\(.plugin) \(.version) \(.platform) \(.url)"'
```

七牛边缘默认把对象缓存一年（响应头 `Cache-Control: public, max-age=31536000`），所以同步脚本写完 `index.json` 会顺带刷新 `index.json` 与 `SHA256SUMS` 两条 CDN 缓存。不刷新的话，商店读到的还是上一次发布的索引——而商店的「不比手里这份旧」规则会让它**静默回落 GitHub**，现象就是"配了 qiniu，列表还是从 GitHub 来的"。刷新失败脚本只打 warning（不影响 release），这时去控制台「CDN → 刷新缓存」手动推一次即可；免费额度按条算，每次发布两条。

注意 `base_url` 的协议要和商店里填的 `qiniu_base` 一致：索引是 `http://` 而配置写 `https://`，商店会按"https 列表不许指向 http 包"整份拒掉。

本地预演不需要密钥，也不碰网络：

```bash
python3 .github/scripts/sync_qiniu.py --dir dist --tag v0.3.0 --dry-run --domain https://dl.example.com
```

它打印这次会上传什么、哪些旧文件会进 `history/`、以及重算出来的 `index.json`；加 `--listing <列表 JSON>` 可以拿一份真实的列举结果做输入。真跑（不带 `--dry-run`）需要七牛 SDK：`python3 -m pip install qiniu==7.18.0`，CI 里装在临时 venv 里，不碰系统 Python。

第一次别直接上 CI，先拿一个测试桶把密钥、域名、公开读验一遍：

```bash
export QINIU_ACCESS_KEY=… QINIU_SECRET_KEY=… QINIU_BUCKET=… QINIU_DOMAIN=https://…
python3 -m pip install qiniu==7.18.0
python3 .github/scripts/sync_qiniu.py --dir dist --tag v0.0.0-test
curl -fsS "$QINIU_DOMAIN/index.json" | jq -r '.packages[] | "\(.plugin) \(.version) \(.url)"'
```

期望输出三样：每个包一行 `uploaded …`、`index: N packages`、`done: <域名>/index.json`。把 `dist/` 里某个包复制成更高版本的名字再跑一次，就能看到 `history <旧文件> -> history/<版本>/<旧文件>`——这行出现，说明移动和归档都通了。

代价要心里有数：七牛的免费额度是每月 10GB 标准存储，**外网流出流量按量计费**（约 0.26 元/GB），不是免费下载；`index.json` 只记录根目录的当前版本，历史包永远留在 `history/` 里，需要时自己清理。

### svcd 单独发版怎么办

本仓库打 tag 时会顺带把 svcd 的最新稳定版一起镜像。但 `plugin-manager` 自己发版**不会**通知这里，
`index.json` 里的 svcd 会停在上一次本仓库发布时的版本——而商店在 `catalog_source = qiniu` 时优先读镜像，
所以它会一直看到旧版，日志里也没有异常。

`.github/workflows/mirror.yml` 补了这一环，由 `plugin-manager` 的发布流程触发：

- 它的 `release.yml` 在 `publish` 成功、且 tag 不是预发布之后，把自己刚发布的 tag 用
  `repository_dispatch`（`event_type: svcd-released`，payload 带 `tag`）送到本仓库；
- 本仓库收到后只同步 svcd：旧归档进 `history/svcd-<tag>/`，其它包原地不动，`index.json` 按桶的实际内容重写，然后刷 CDN；
- 镜像已经是那版（比如本仓库刚发过一次）就直接退出，不碰桶；
- 手动重跑：Actions 页面 Run workflow，或 `gh workflow run mirror.yml`；没有 payload 时退回「最新稳定版」。

对方仓库需要一个 `PLUGINS_MIRROR_TOKEN`：能在 `webxxxxx/plugins` 上创建 dispatch 的 token（fine-grained 给 Contents: read and write，classic 用 `repo`）。没配时它只打 `::warning::`，镜像会等本仓库下一次发布追上。
它和发布流程里的 `qiniu` job 共用 `concurrency: qiniu-mirror`，因为 `index.json` 是读-改-写，两个写入者不能重叠。

### CI 平台覆盖

每个插件的 `.plugin.json` 可以写 `ci.platforms` 覆盖发布矩阵；不写就按 `platforms` 全量构建。meow 的 Linux musl 目标用 musl cross 工具链编译 BoringSSL。

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

交叉到 musl 目标（x86_64 / aarch64 / armv7）时，先跑 `tools/cross/install.sh <triple>`：它把 musl.cc 的工具链下到 `~/.svcd-cross`（可复用），并交代 `CARGO_TARGET_<TRIPLE>_LINKER`、`CC_<triple>`、`CC` 与 `PATH`——在 CI 里写 `$GITHUB_ENV` / `$GITHUB_PATH`，本地打印 `export` 行（`eval "$(tools/cross/install.sh …)"`）。C 的 `build.json` 用 `{cc}` 取编译器（`$CC` 优先，否则 `gcc`），所以同一份配方既能本机编也能交叉编；仓库 CI、四份模板和四份样例的 workflow 都调这个脚本。

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

协议 3 再加一条通道，仍然只写两个 trait：

```rust
impl RawPlugin for MyPlugin {
    fn raw(&mut self, stream: u32, payload: &[u8], out: &Streams) {
        out.send(stream, payload).ok();   // 同一条 stream 原样回帧
    }
}
fn main() { svcd_plugin::multiplex::serve(Context::from_env(), MyPlugin) }
```

清单写 `"protocol": 3`：stream 0 还是上面那套 JSON-RPC，stream ≥ 1 是 `/raw` 订阅者的不透明
字节。写侧有界、不阻塞读循环，满了返回 `SendError::Busy`；超过上限的帧在写头之前就被拒。
C / shell / PowerShell SDK 仍然只有协议 1。

## 浏览器侧五个通道

插件不自起 HTTP。静态资源与下面四条都在 `/plugin-ui/<id>/` 下，由 svcd 反代：

| 通道 | 路径 | 用途 |
|---|---|---|
| method invoke | `POST ./rpc` | JSON-RPC `tools/call` |
| SSE | `./events` | 服务端推送，`id:<seq>` + `event: message`，另有 `event: reset` |
| WebSocket | `./ws` | 同一路推送的另一种承载，**不是**请求通道 |
| raw 流 | `./raw` | 协议 3 才有：二进制帧按 stream 配对，与上面三条共用同一个插件进程 |
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
