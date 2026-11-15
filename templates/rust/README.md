# __NAME__ — Rust 插件模板

只写核心逻辑：协议、并发、就绪探针、MCP 返回形状、取消令牌都在 `sdk/rust` 里。

## 布局

```
package/                  ← 打包根。里面有什么，包里就有什么（packer 不做过滤）
├── manifest.json         ← 手写；不写 platform / files[] / signature.json
├── payload/bin/          ← cargo 产物落到这里（git 忽略）
└── payload/web/          ← 可选 UI，宿主反代到 /plugin-ui/<id>/
src/main.rs               ← 你的代码
```

源码必须留在 `package/` 之外。`plugin bundle` 会无差别递归整个目录，放进去的
`src/`、`Cargo.toml`、`target/` 都会被打进 `.spk`。

## 命令（在仓库根或插件目录内都行）

```bash
node tools/scaffold.mjs <name> --lang rust      # 从模板新建
node tools/plugin.mjs pack                      # 构建 + 打包（默认本机三元组）
node tools/plugin.mjs pack --platform aarch64-unknown-linux-musl
node tools/plugin.mjs dev                       # 打包 + 安装 + 启动
node tools/plugin.mjs check dist/<name>-<version>-<triple>.spk
cargo test                                      # 清单与代码的能力清单是否一致
```

`plugin bundle` 需要一把签名钥；默认用 `~/.svcd-dev/dev.pem`（`node tools/plugin.mjs devkey` 生成一次），
也可以用 `--key` 指定。

## 验证（当前实现的两个限制）

`plugin verify` 目前做两件超出"包自洽性"的判断，都会挡流水线：

1. **要求签名钥在 `trust.d`** —— 本机先跑一次 `svc trust add ~/.svcd-dev/dev.pub` 才验得过；
   CI runner 上没有 trust.d，所以门禁里要先导入发布公钥。
2. **要求 `manifest.platform` 等于本机三元组** —— `platform mismatch: package aarch64-…, host x86_64-…`。
   也就是**交叉产物在本机验不了**，而 x86 runner 恰恰要产出并验证 aarch64 / Windows 包。

这两条都要放宽成"只报事实、不判结论"：输出 `platform` 与 `signature.state`，另给
`--expect-platform <triple>` 与 `--require-trust` 让调用方自己决定严格程度。在那之前，
`node tools/plugin.mjs check` 只对本机三元组的包有效。


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
5. 首装默认不自启，装完必须 `plugin start`（`node tools/plugin.mjs dev` 已代做）。
6. 含 `.ko` 的包必须声明 `--compat <device>`，否则验包拒。
7. `capabilities` 只在 `ready: "notify"` 时可用；改成 `exec` 就要把能力清单一起删。
