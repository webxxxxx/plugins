# vendored 源码（sshfs 依赖链）

按仓库惯例：`vendor/` 放**源码**，打包前由 `build.sh` 编译；这里没有预编译件。
配方来自 `sr6110_rebuild` 分支 `fix/wg-lan-autodetect`（提交 `8185609`，2026-11-05）的
`thirdparty/sshfs/build.sh` 与 `thirdparty/dropbear/build.sh`，只把工具链换成
`tools/cross-env.sh`、依赖前缀落到 `out/prefix`。

| tarball | 版本 | 许可证 | 用途 |
|---|---|---|---|
| `pcre2-10.42.tar.bz2` | 10.42 | BSD-3-Clause | glib 的正则依赖 |
| `libffi-3.4.4.tar.gz` | 3.4.4 | MIT | glib 的 `GObject` 闭包 |
| `zlib-1.3.1.tar.gz` | 1.3.1 | zlib | glib 的 `gio` 硬依赖（上游构建机 staging 里有，这里入库） |
| `glib-2.78.4.tar.xz` | 2.78.4 | LGPL-2.1-or-later | sshfs 用的 `GIO`/`GOutputStream` |
| `fuse-3.16.2.tar.gz` | 3.16.2 | LGPL-2.1（`util/` 为 GPL-2） | libfuse3 + `fusermount3` |
| `sshfs-3.7.3.tar.gz` | 3.7.3 | GPL-2.0 | 挂载程序本体 |
| `dropbear-2022.83.tar.bz2` | 2022.83 | MIT | `dbclient`（sshfs 的 SSH 客户端）+ `dropbearkey` |

许可证全文在各自 tarball 里（`COPYING` / `LICENSE` / `LICENCE`），不再单独抄一份。

## 构建顺序

`build.sh` 严格按依赖顺序、每次从干净的 `out/build/` 开始：

1. pcre2 → `./configure --host=aarch64-linux --disable-shared --enable-static`
2. libffi → 同上，另加 `--disable-exec-static-tramp --disable-builddir`（musl + drvfs 的坑）
3. zlib → `CC=… ./configure --static`
4. glib → `meson setup --cross-file … --wrap-mode=nodownload -Dnls=disabled -Dlibmount=disabled -Dselinux=disabled -Dtests=false -Dman=false`
5. libfuse3 → `-Dutils=true -Duseroot=false -Dinitscriptdir= -Dc_link_args=-static`
6. sshfs → `--default-library=static -Dc_link_args=-static`
7. dropbear → `./configure --disable-{zlib,syslog,lastlog,utmp,utmpx,wtmp,wtwmpx,pututline,pututxline}`，`make LDFLAGS=-static`

前五步装进 `out/prefix`（`PKG_CONFIG_LIBDIR` 指向它）；sshfs / fusermount3 / dbclient /
dropbearkey 拷到 `out/`，再由 `build.json` 的 artifact 和 `build.sh` 放进 `payload/bin/`。

`--wrap-mode=nodownload` 是故意的：缺依赖时直接失败，不去 wrapdb 下载（glib 当初就因为
zlib 缺失差点下载一个不在仓库里的版本）。

## 宿主依赖

交叉工具链：`tools/cross-env.sh` 找 `/usr/soft/aarch64-linux-musl-cross`（或用
`AARCH64_CROSS` 指过去；`tools/cross/install.sh aarch64-unknown-linux-musl` 负责安装）。

宿主工具：`meson`、`ninja`、`pkg-config`、`make`、`tar`、`xz`。Debian/Ubuntu：

```sh
sudo apt install meson ninja-build pkg-config build-essential
# 或者：pip install --user meson
```

`glib` 的 meson 需要目标 `zlib.pc`/`libffi.pc`/`libpcre2-8.pc`，都来自上一步装进
`out/prefix` 的静态库；**不要**让 `PKG_CONFIG_PATH` 指到宿主目录，否则会把 x86_64 的
`.pc` 带进来（跨文件里 `pkgconfig` 走 `PKG_CONFIG_LIBDIR`）。

## 平台

`build.sh` 的 `PLATFORM` 表决定 `--host`、meson 的 `host_machine` 和是否全静态：

| 三元组 | autoconf host | meson cpu | 链接 |
|---|---|---|---|
| `x86_64-unknown-linux-musl` | `x86_64-linux` | `x86_64` | 全静态 |
| `aarch64-unknown-linux-musl` | `aarch64-linux` | `aarch64` | 全静态 |
| `armv7-unknown-linux-musleabihf` | `arm-linux` | `armv7` | 全静态 |
| `x86_64-unknown-linux-gnu` | `x86_64-linux` | `x86_64` | 动态 glibc |

gnu 动态链接是有意的：静态 glibc 的 `getaddrinfo` 运行时还要 dlopen `libnss_dns.so`，
自包含包里没有它，域名解析会失败。所以 gnu 包要求宿主的 glibc 版本，CI 在 ubuntu-22.04
上编（glibc 2.35 基线）。

## prefix 说明

上游把 prefix 设成 `~/sshfs-static` 是为了和设备上已部署的二进制字节级一致。这里用
`out/prefix`：`fusermount3` 的默认查找路径会跟着变，但 libfuse 在直接 `mount(2)` 失败后
会按名字 `execvp("fusermount3")`，而 `sshfs.sh` 已经把 `payload/bin` 放进 `PATH`，
所以插件里不依赖那个编译期前缀。
