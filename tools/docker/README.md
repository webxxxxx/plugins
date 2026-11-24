# Rust 交叉编译镜像（本项目组通用）

一个镜像 + 两个脚本，解决「在 Windows 上不想装 rust/cmake/MSVC/libclang，又要把 Rust 产物
编到 Linux 和 Windows」这件事：

| 脚本 | 干什么 |
| --- | --- |
| `tools/docker/run.sh` | 在镜像里跑**任意命令**（cargo build、插件的 build.sh、plugin-tools 自己） |
| `tools/pack.sh` | 打包插件：编 + staging + bundle，产物统一拷到 `dist/` |

## 能出哪些目标

| 目标 | 怎么来的 |
| --- | --- |
| `x86_64-unknown-linux-gnu` | 镜像里 gcc |
| `x86_64-unknown-linux-musl` | 镜像里 `musl-gcc`（debian 的 musl-tools） |
| `aarch64-unknown-linux-musl` | 宿主 `/usr/soft/aarch64-linux-musl-cross`，挂进容器共用 |
| `armv7-unknown-linux-musleabihf` | 宿主 `/usr/soft/arm-linux-musleabihf-cross`，同上 |
| `x86_64-pc-windows-msvc` | `cargo-xwin` + MSVC CRT/SDK（第一次编时下到 `build_workspace/xwin`，约 1.1G，之后复用） |
| macOS 两个目标 | **不在镜像里**：要 Xcode，只能在 mac 上编（CI 的 macOS runner） |

镜像里有 clang 19 / llvm / libclang（bindgen 用）、nasm（BoringSSL 的 x86_64 Windows
汇编）、musl-tools、ninja、unzip/python3。**Rust 工具链不在镜像里**：`run.sh` 把宿主机的
`~/.rustup` 和 `~/.cargo/bin` 挂进去，版本由各仓库的 `rust-toolchain.toml` 决定，
`rustup target add` 也只在宿主机做一次。

基础镜像是 debian **trixie**（clang 19）：bookworm 只有 clang 14，而 xwin 带下来的 MSVC
STL 要求 Clang ≥ 19。

## 挂载：路径都在 WSL 里，别用 /mnt/d 当编译目录

```
<仓库>                       → /work        源码；产物写回 out/、dist/
/usr/soft                    → /usr/soft    宿主的交叉工具链，容器和宿主机共用一份
/usr/soft/build_workspace    → 同一路径     cargo 的 registry 缓存 + 每个项目的 target
```

`build_workspace` 落在 WSL 的 ext4 上：`/mnt/d` 走 drvfs，编译又慢、又容易把 WSL 那点内存吃光
（meow 在 /mnt/d 上编过一次就直接 OOM）。

目录结构：

```
/usr/soft/build_workspace/
├── cargo/                 registry + 国内镜像配置（run.sh 缺了就写）
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
docker build -f tools/docker/rust-cross.Dockerfile -t rust-cross:1.98.1 .
```

### 编一个二进制

```sh
# 直接在仓库根跑 cargo
sh tools/docker/run.sh -- cargo build --release --target x86_64-pc-windows-msvc

# 用插件自己的 build.sh（它在 SVCD_DOCKER=1 时会自己进来，这里手动写全）
sh tools/docker/run.sh -w plugins/meow -n meow -- sh plugins/meow/build.sh meow 1.3.0 aarch64-unknown-linux-musl

# 编工具本身，例如 plugin-tools（产物在 /usr/soft/build_workspace/plugin-tools/target/release）
sh tools/docker/run.sh -w tools/cli -n plugin-tools -- cargo build --release --locked
```

### 打包插件

```sh
sh tools/pack.sh                                 # meow：windows-msvc + linux-gnu
sh tools/pack.sh meow x86_64-unknown-linux-musl  # 加一个平台
sh tools/pack.sh sshd                            # 别的插件（build.sh 支持 SVCD_DOCKER 即可）
```

`tools/pack.sh` 的流程：检查 docker / 镜像 / plugin-tools（缺了在容器里现编）/ 宿主
`plugin` 二进制 → 逐平台 `SVCD_DOCKER=1 plugin-tools build --platform …` → 把
`plugins/<name>/dist/*.spk` 拷到 `dist/`。

## 给别的 Rust 项目接进来

1. 项目里有一段「在容器里编」的入口（照 `plugins/meow/build.sh` 抄 6 行）：

   ```sh
   if [ "${SVCD_DOCKER:-0}" = 1 ] && [ "${SVCD_IN_DOCKER:-0}" != 1 ]; then
       REPO=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
       exec sh "$REPO/tools/docker/run.sh" -w <项目在仓库里的目录> -n <项目名> -- \
           sh <同一个脚本> "$@"
   fi
   ```

2. `plugin-tools build.json` 里的命令不用改，`plugin-tools` 只是执行它。

3. 纯 Rust 的 crate（比如 `tools/cli`、宿主 `svcd`）不用这套包装，直接
   `sh tools/docker/run.sh -w <dir> -- cargo build --release --target …` 就行。

## 卡点（都是真踩过的，写在这省下次）

| 现象 | 原因 | 处理 |
| --- | --- | --- |
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

- **macOS 目标**只能 mac 上编。
- **armv7 / aarch64 的 musl** 依赖宿主的 `/usr/soft/*-cross`；别的机器上要自己解一份
  同名工具链到 `/usr/soft`，或者用 `SVCD_BUILD_WORKSPACE` 之外的自定义镜像。
- 镜像只负责**编译**；清单合并、staging、bundle 仍在宿主机跑（`plugin-tools` + 宿主
  `plugin`），和 CI 走同一条路，产物因此完全一致。
- 容器以当前 uid/gid 跑（`--user`），`out/`、`dist/` 里的文件属主不会变成 root。
