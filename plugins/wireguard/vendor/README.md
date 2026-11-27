# vendored 源码

按仓库惯例：`vendor/` 放**源码**，打包前由 `build.sh` 编译（和 `meow`、`sshd` 一样）。
只有一份例外——`prebuilt/` 里的 SR6110 内核模块。

| 目录 | 内容 | 版本 / 来源 |
|---|---|---|
| `gotatun-0.9.2/` | 用户态 WireGuard（`gotatun` + `gotatun-cli` 工作区） | [mullvad/gotatun](https://github.com/mullvad/gotatun) 0.9.2，commit `3e25b63`（2026-09-16） |
| `wireguard-tools/` | `wg` 命令行工具 | 上游 `a998407`（2026-05-06），`wg --version` 报 `v1.0.20260223-5-ga998407` |
| `prebuilt/aarch64-unknown-linux-musl/wireguard.ko` | SR6110 的内核模块 | wireguard-linux-compat 1.0.20220627，厂商 5.4.196 内核 + ABI 对齐补丁（取 `sr6110_rebuild` 的 `thirdparty/data/bin/wireguard.ko`，提交 `7d05d16`） |

## 为什么只有 ko 是预编译的

`wireguard.ko` 必须对着带 ABI 补丁的 5.4.196 厂商内核树编，CI 里没有那棵树，也不该把
几十 MB 的内核树塞进插件仓库；它的源码在上游 `wireguard-linux-compat`，补丁在
`sr6110_rebuild`。其余二进制都能从本目录的源码编出来：

- `gotatun`：Rust，`cargo build --locked --release --bin gotatun --target <triple>`；
- `wg`：C，`make -C wireguard-tools/src CC=<triple>-cc LDFLAGS=-static PLATFORM=linux`。

`build.sh` 就是这么做的：先编 `wg` 到 `out/<triple>/wg-tool`，通用包再编 `gotatun`
进 `payload/bin/`；`sr6110` 变体只拷 `prebuilt/` 里的 ko。

## 交叉编译环境

工具链那张唯一的表是 `tools/cross-env.sh`：`build.sh` 里 `. "$REPO/tools/cross-env.sh"` +
`svcd_cross_apply "$PLATFORM"`，它会找 `/usr/soft/*-cross`（或 `CROSS_PREFIX` /
`AARCH64_CROSS` / `X86_64_MUSL_CROSS`），导出 `CC_/AR_/CARGO_TARGET_*_LINKER` 并把工具链
挂到 `PATH`。CI 里 `tools/cross/install.sh <triple>` 负责把包装到 `/usr/soft`。

有 docker 的机器可以完全不装工具链：

```sh
SVCD_DOCKER=1 plugin-tools build --platform aarch64-unknown-linux-musl --device sr6110
# 或者
tools/pack.sh wireguard all
```

`gotatun` 的默认加密后端是 `aws-lc-rs`，交叉编译要 `cmake`、`clang`、`perl`（CI 的
"Install Linux build dependencies" 步骤已装）。`Cargo.lock` 已入库，构建用 `--locked`，
不会解析出新版本。

## 许可证

- `gotatun`：MPL-2.0，见 `gotatun-0.9.2/LICENSE`（以及历史部分的 `LICENSE-CLOUDFLARE`）；
- `wireguard-tools`：GPL-2.0，见 `wireguard-tools/COPYING`；
- `wireguard.ko`：GPL-2.0（Linux 内核模块）。

两份许可证在 `LICENSE.gotatun` / `LICENSE.wireguard-tools` 也各留一份，方便随包分发时引用。
