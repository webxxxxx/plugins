# Rust 交叉编译镜像（本项目组通用）

一个镜像 + 两个脚本，解决「在 Windows 上不想装 rust/cmake/MSVC/libclang，又要把 Rust 产物
编到 Linux 和 Windows」这件事：

| 脚本 | 干什么 |
| --- | --- |
| `tools/docker/docker-build.sh` | 在镜像里跑**任意命令**（cargo build、插件的 build.sh、plugin-tools 自己），任何仓库都能用 |
| `tools/docker/image.sh` | 按目标挑镜像变体（gnu → bookworm 老 glibc，其余 → trixie），缺了现建，把镜像名打到 stdout |
| `tools/cross-env.sh` | 目标三元组 → C 交叉工具链的**唯一一张表**：插件的 build.sh 和容器入口都 source 它 |
| `tools/pack.sh` | 打包插件：编 + staging + bundle，产物统一拷到 `dist/` |

`docker-build.sh` 以前叫 `run.sh`，旧名字现在还留着一个转发脚本，老调用不会失效。

## 能出哪些目标

| 目标 | 怎么来的 |
| --- | --- |
| `x86_64-unknown-linux-gnu` | 镜像里 gcc。**用 bookworm 变体**（glibc 2.36）编，产物才能在 Debian 12 / Ubuntu 22.04 上跑 |
| `x86_64-unknown-linux-musl` | 宿主 `/usr/soft/x86_64-linux-musl-cross`，挂进容器共用；没有整份工具链时才退到镜像里的 `musl-gcc`（musl-tools），纯 C 够用、带 C++ 依赖的（meow）不够 |
| `aarch64-unknown-linux-musl` | 宿主 `/usr/soft/aarch64-linux-musl-cross`，挂进容器共用 |
| `armv7-unknown-linux-musleabihf` | 宿主 `/usr/soft/arm-linux-musleabihf-cross`，同上 |
| `x86_64-pc-windows-msvc` | `cargo-xwin` + MSVC CRT/SDK（第一次编时下到 `build_workspace/xwin`，约 1.1G，之后复用） |
| macOS 两个目标 | **不在镜像里**：要 Xcode，只能在 mac 上编（CI 的 macOS runner） |

镜像里有 clang 19 / llvm / libclang（bindgen 用）、nasm（BoringSSL 的 x86_64 Windows
汇编）、musl-tools、ninja、unzip/python3。**Rust 工具链不在镜像里**：`docker-build.sh` 把宿主机的
`~/.rustup` 和 `~/.cargo/bin` 挂进去，版本由各仓库的 `rust-toolchain.toml` 决定，
`rustup target add` 也只在宿主机做一次。

基础镜像两个变体，同一个 Dockerfile，只换 `BASE_IMAGE`：

| 变体 | 底子 | 谁用 |
| --- | --- | --- |
| `rust-cross:1.98.1` | debian **trixie**，glibc 2.41 + clang 19 | musl 目标、windows-msvc |
| `rust-cross:1.98.1-bookworm` | debian **bookworm**，glibc 2.36 + clang 14 | `x86_64-unknown-linux-gnu` |

为什么分开：动态链接的产物把「我要 GLIBC_2.38 的符号」刻在 ELF 的 `.gnu.version_r` 里，
loader 在老 glibc 上直接拒绝执行——trixie 编出来的 gnu 包在 Debian 12（2.36）上就是
`version 'GLIBC_2.38' not found`。musl 是 static-pie，跟宿主 glibc 无关，所以留在 trixie。
反过来的约束在 MSVC 那边：xwin 带下来的 MSVC STL 要求 Clang ≥ 19，bookworm 只有 14，
所以 windows 目标也只能 trixie。

平时不用记这些：`tools/docker/image.sh <triple>` 按目标挑，缺了现建（第一次几分钟）；
`pack.sh` 每个平台打之前都会问它一次。

## 挂载：路径都在 WSL 里，别用 /mnt/d 当编译目录

```
<项目根>                     → /work        源码；产物写回 out/、dist/
/usr/soft                    → /usr/soft    宿主的交叉工具链，容器和宿主机共用一份
/usr/soft/build_workspace    → 同一路径     cargo 的 registry 缓存 + 每个项目的 target
```

`build_workspace` 落在 WSL 的 ext4 上：`/mnt/d` 走 drvfs，编译又慢、又容易把 WSL 那点内存吃光
（meow 在 /mnt/d 上编过一次就直接 OOM）。

`/work` 挂的是**项目根**，默认取你当前目录所属的 git 顶层 —— 在哪个仓库里敲就在哪个仓库里编，
`plugin-manager` 这种别的仓库直接进去用，不用搬脚本：`-r` 换根、`-w` 往下走子目录。

目录结构：

```
/usr/soft/build_workspace/
├── cargo/                 registry + 国内镜像配置（docker-build.sh 缺了就写）
├── <项目名>/target/       每个项目各自的 target（projects: meow、plugin-tools、…）
```

## 国内加速

| 环节 | 默认 | 换法 |
| --- | --- | --- |
| 基础镜像 | 走宿主机 `/etc/docker/daemon.json` 的 `registry-mirrors` | 改 daemon.json 后 `systemctl restart docker` |
| apt | 阿里云 | `--build-arg APT_MIRROR=mirrors.tuna.tsinghua.edu.cn` |
| rustup / crates.io | rsproxy.cn | `--build-arg CARGO_MIRROR=https://mirrors.ustc.edu.cn/rust-static` |
| MSVC SDK | 预下载进镜像 | 一次性的，之后离网也能编 |

## 用法

### 一次性：构建镜像

```sh
sh tools/docker/image.sh                            # → rust-cross:1.98.1（trixie，musl / msvc）
sh tools/docker/image.sh x86_64-unknown-linux-gnu   # → rust-cross:1.98.1-bookworm（gnu）
```

两个都可以手建（`--build-arg BASE_IMAGE=debian:bookworm-slim` 那份见 Dockerfile 头注释），
平时交给 `image.sh` 就行，缺哪个建哪个。

### 编一个二进制

```sh
# 本项目里编插件：用它自己的 build.sh（SVCD_DOCKER=1 时它自己会进来，这里手动写全）
sh tools/docker/docker-build.sh -r "$PWD" -w plugins/meow -n meow -- \
    sh plugins/meow/build.sh meow 1.3.0 aarch64-unknown-linux-musl

# 编工具本身，例如 plugin-tools（产物在 /usr/soft/build_workspace/plugin-tools/target/release）
sh tools/docker/docker-build.sh -r "$PWD" -w tools/cli -n plugin-tools -- cargo build --release --locked

# 别的仓库：进那个仓库直接敲，项目根自动跟过去（把 tools/docker 加进 PATH 就能省掉路径）
cd ~/src/plugin-manager
sh /mnt/d/learn/harmess_workspace/plugins/tools/docker/docker-build.sh -- \
    cargo build --release --target armv7-unknown-linux-musleabihf
```

第三条不用写 `-r`/`-w`/`-p`：项目根取当前目录（这里是 plugin-manager 的 git 顶层），
`--target` 从命令里认出来，容器里由 `tools/cross-env.sh` 套上 `CC_armv7_unknown_linux_musleabihf`、
`CARGO_TARGET_ARMV7_UNKNOWN_LINUX_MUSLEABIHF_LINKER` 和 `/usr/soft/...-cross/bin` 的 PATH
（`tools/` 会挂成 `/svcd-tools`，所以别的仓库也能用这张表）。

windows 目标不一样，要用 `cargo xwin build`（链接器是 lld-link + 容器里的 MSVC SDK）：

```sh
sh tools/docker/docker-build.sh -r "$PWD" -- cargo xwin build --release --target x86_64-pc-windows-msvc
```

### 打包插件

```sh
sh tools/pack.sh                                 # meow：windows-msvc + linux-gnu
sh tools/pack.sh meow all                        # 除 mac 外全打（5 个目标，测试用）
sh tools/pack.sh meow linux                      # 只打 linux 四个（gnu + 三个 musl）
sh tools/pack.sh meow x86_64-unknown-linux-musl  # 加一个平台
sh tools/pack.sh sshd                            # 别的插件（build.sh 支持 SVCD_DOCKER 即可）
```

`all` / `linux` / `musl` 从插件的 `.plugin.json` 展开平台清单；darwin 两个要 Xcode，
`all` 会把它们跳过并说明。

`tools/pack.sh` 的流程：检查 docker / 镜像 / plugin-tools（缺了在容器里现编）/ 宿主
`plugin` 二进制 → 逐平台 `SVCD_DOCKER=1 plugin-tools build --platform …` → 把
`plugins/<name>/dist/*.spk` 拷到 `dist/`。镜像按平台挑（见上文 `image.sh`），
第一次遇到某个变体会先建。

## 给别的 Rust 项目接进来

1. 项目里有一段「在容器里编」的入口（照 `plugins/meow/build.sh` 抄 6 行）：

   ```sh
   if [ "${SVCD_DOCKER:-0}" = 1 ] && [ "${SVCD_IN_DOCKER:-0}" != 1 ]; then
       REPO=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
       exec sh "$REPO/tools/docker/docker-build.sh" -r "$REPO" -w <项目在仓库里的目录> -n <项目名> -- \
           sh <同一个脚本> "$@"
   fi
   ```

   `-r` 要给全：`-w` 是「项目根下的子目录」，不跟着当前目录走（不写 `-r` 时才按当前目录的
   git 顶层推）。

   交叉目标再 source 一次那张表（容器里 `docker-build.sh` 已经替你做过了，宿主直编要自己做）：

   ```sh
   . "$REPO/tools/cross-env.sh"
   svcd_cross_apply "$PLATFORM" || die "$SVCD_CROSS_ERROR"
   ```

2. `plugin-tools build.json` 里的命令不用改，`plugin-tools` 只是执行它。

3. 纯 Rust 的 crate（比如 `tools/cli`、宿主 `svcd`）不用这套包装：站在它的仓库里
   `docker-build.sh -- cargo build --release --target …` 就行，项目根和目标都会自己认。
   只想编本仓库的子目录时用 `-w tools/cli`。

## 卡点（都是真踩过的，写在这省下次）

| 现象 | 原因 | 处理 |
| --- | --- | --- |
| `error: could not find Cargo.toml in /work` | 脚本原先把项目根算成它自己所在的仓库，在别的仓库里敲就挂错了 | 现在默认取当前目录的 git 顶层；跨仓库/子目录用 `-r`、`-w` |
| `crt1.o: relocations in generic ELF (EM: 40)` / `file in wrong format` | C 依赖（ring、openssl 之类）还是用宿主 `cc` 编的，目标文件是 x86_64 | `source tools/cross-env.sh && svcd_cross_apply <triple>` 就配好了（照 `plugins/plugin-store/build.sh` 抄两行）；或者 `SVCD_DOCKER=1` 交给容器，用的也是这张表 |
| `linker 'cc' not found`（musl / arm 目标） | 容器里没有能链接该目标的 cc | 写 `-p <triple>`（或命令里带 `--target`），脚本自动配 CC/AR/链接器和 PATH |
| `permission denied /var/run/docker.sock` | 用户不在 docker 组 | `sudo usermod -aG docker $USER`，重开 WSL |
| 拉镜像卡住 | `registry-mirrors` 里的 USTC 已停服 | 换成 `docker.m.daocloud.io` / `1panel.live` / `xuanyuan.me` / `1ms.run` |
| `rustup` 装 1.98.1 报 404 | rsproxy / tuna 都没同步这个版本（它比它们新） | 工具链走官方 `static.rust-lang.org`（国内实测 3MB/s），crates 索引才用 rsproxy |
| `missing manifest in toolchain` | 装工具链时被打断 | `rustup toolchain uninstall 1.98.1` 再 `install`（`--target` 一次带全） |
| `rustup` 只读挂载下报 `Read-only file system` | 它要往 `RUSTUP_HOME/tmp` 写临时文件 | `~/.rustup` 挂**可写**（同 uid，安全） |
| cmake 找不到 Ninja | cargo-xwin 的 C 依赖用 Ninja | 镜像里装 `ninja-build` |
| BoringSSL 的 `third_party/benchmark` configure 失败 | 交叉时它的平台探测不了；`CMAKE_TOOLCHAIN_FILE` 被 cargo-xwin 顶掉 | build.sh 把 `BUILD_TESTING OFF` 钉进 BoringSSL 的 `CMakeLists.txt`（幂等） |
| `static_assert ... expected Clang 19.0.0 or newer` | bookworm 的 clang 14 太旧 | 基础镜像换 trixie（clang 19） |
| `powershell.exe: command not found`（fetch-wintun.sh） | 容器里没有 powershell，脚本退回 python 解压 | 镜像里装 `unzip`（和 `python3`） |

## 边界

- **glibc 基线**：`linux-gnu` 包在 bookworm 变体里编（glibc 2.36），CI 上是 ubuntu-22.04
  （2.35）。要支持更老的系统（CentOS 7 之类）得再加一层老底子，或者干脆发 musl 包——
  musl 是静态的，没有这条线。
- **macOS 目标**只能 mac 上编。
- **musl 的目标**都依赖宿主的 `/usr/soft/*-cross`；别的机器上先装一份，装法：
  `sh tools/cross/install.sh <triple> /usr/soft`（x86_64 也走这条）。x86_64 少了它会退到
  镜像里的 `musl-gcc`，只是 musl-tools 的 C++ 是宿主机 glibc 那一套，BoringSSL 一定链不上。
- 镜像只负责**编译**；清单合并、staging、bundle 仍在宿主机跑（`plugin-tools` + 宿主
  `plugin`），和 CI 走同一条路，产物因此完全一致。
- 容器以当前 uid/gid 跑（`--user`），`out/`、`dist/` 里的文件属主不会变成 root。
