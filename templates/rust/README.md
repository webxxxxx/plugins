# __NAME__ — Rust 插件模板

只写核心逻辑：协议、并发、就绪探针、MCP 返回形状、取消令牌都在 `sdk/rust` 里。

## 布局

```
package/                  ← 打包根。里面有什么，包里就有什么（packer 不做过滤）
├── manifest.json         ← 手写；schema 为 1，id 是 ASCII 身份，name 是显示名
├── payload/bin/          ← cargo 产物落到这里（git 忽略）
└── payload/web/          ← 可选 UI，宿主反代到 /plugin-ui/<id>/
src/main.rs               ← 你的代码
```

`manifest.json` 的字段分工，记这一条就够：

- `id` 是身份：`[a-z0-9][a-z0-9_-]{0,63}`，不能是 `svc`。安装目录、日志、`<id>.<能力>` 工具名、
  `/plugin-ui/<id>/` 都用它，所以只能 ASCII；模板里默认就是项目名，不写时打包器也取项目目录名。
- `name` 只是显示名：任意 UTF-8，想写中文就写中文，留空则等于 `id`。
- `capabilities[].outputSchema` 可选：只做声明与 MCP `tools/list` 投影，不做运行期校验
  （不写 = 未声明，`{}` = 任意）。本模板的 `stats` 返回 JSON，所以给了一个真实的对象 schema。
- `icons` 可选：`{"png"|"ico"|"icns": "<包内路径>"}`；`plugin-tools build` 会先校验文件存在且非空。
- `platform` / `files[]` / `signature.json` 由打包器盖章，源清单里不要写。

源码必须留在 `package/` 之外。`plugin bundle` 会无差别递归整个目录，放进去的
`src/`、`Cargo.toml`、`target/` 都会被打进 `.spk`。

`Cargo.toml` 用 path 依赖本仓库的 `sdk/rust`；如果把插件拆成独立仓库，
需要把 `svcd-plugin` 改成 git 或已发布版本的依赖。

## 命令（在仓库根或插件目录内都行）

```bash
plugin-tools template <name> --lang rust --webui   # 只创建项目
plugin-tools build                                 # 构建 + 打包（默认本机三元组，未签名）
plugin-tools build --platform aarch64-unknown-linux-musl
plugin-tools build --key ~/.svcd-dev/dev.pem       # 打成签名包
plugin-tools dev                    # 打包 + 安装 + 启动
plugin-tools check dist/<id>-<version>-<triple>.spk
cargo test                                   # 清单与代码的能力清单是否一致
```

`plugin bundle` 需要一把签名钥；默认用 `~/.svcd-dev/dev.pem`（`plugin-tools devkey` 生成一次），
也可以用 `--key` 指定。

多平台构建会执行 `cargo build --target <triple>`；先 `rustup target add <triple>`，
musl / ARM 目标还需要对应的链接器。

## 验证（当前实现的两个限制）

`plugin verify` 目前做两件超出"包自洽性"的判断，都会挡流水线：

1. **要求签名钥在 `trust.d`** —— 本机先跑一次 `svc trust add ~/.svcd-dev/dev.pub` 才验得过；
   CI runner 上没有 trust.d，所以门禁里要先导入发布公钥。
2. **要求 `manifest.platform` 等于本机三元组** —— `platform mismatch: package aarch64-…, host x86_64-…`。
   也就是**交叉产物在本机验不了**，而 x86 runner 恰恰要产出并验证 aarch64 / Windows 包。

这两条都要放宽成"只报事实、不判结论"：输出 `platform` 与 `signature.state`，另给
`--expect-platform <triple>` 与 `--require-trust` 让调用方自己决定严格程度。在那之前，
`plugin-tools check` 只对本机三元组的包有效。


## 开发机前置

```toml
# svcd.conf —— 仅开发机，改完重启 svcd
allow_insecure_install = on
```

本地包用开发钥签名，开发钥不在 `trust.d` 里，靠这个开关放行。**生产机器不要开。**

## 七个会咬人的点

1. `package/` 是唯一打包根，packer 不过滤任何文件。
2. `.spk` 必须输出到打包根之外，否则 packer 直接拒绝。
3. 包内脚本不许自建日志文件，一律写 stderr——svcd 接走后按 1 MiB 轮转。
4. 插件恒为常驻进程，没有 oneshot 语义；"跑一次"要写成常驻 + 能力触发。
5. 首装默认不自启，装完必须 `plugin start`（`plugin-tools dev` 已代做）。
6. 含 `.ko` 的包必须声明 `--compat <device>`，否则验包拒。
7. `capabilities` 只在 `ready: "notify"` 时可用；改成 `exec` 就要把能力清单一起删。
