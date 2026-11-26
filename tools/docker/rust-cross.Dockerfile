# 通用 Rust 交叉编译镜像：Linux（gnu / musl，x86_64 / armv7 / aarch64）+ Windows（msvc）。
# 除了 macOS（要 Xcode，只能在 mac 上编），本项目组的 Rust 产物都能在这里出。
#
# 镜像里只有**编译依赖**，Rust 工具链由宿主机提供（tools/docker/run.sh 把宿主机的
# ~/.rustup 和 ~/.cargo/bin 挂进来）：
#   clang / llvm / libclang   bindgen（BoringSSL、各种 *-sys）要用
#   nasm                      BoringSSL 在 x86_64 Windows 上的汇编
#   musl-tools                没有整份工具链时兜底编 x86_64-unknown-linux-musl（只够纯 C）
#   cmake / ninja / perl / build-essential  （cargo-xwin 的 C 依赖走 Ninja）
#   unzip / python3                          （meow 的 build.rs 要解 wintun 压缩包）
#
# 为什么不把 rustup 塞进镜像：工具链版本由各仓库的 rust-toolchain.toml 决定，而且
# `rustup target add` 一次要下几百 MB —— 放在宿主机上只下一次，容器每次直接用。
# Windows 的 MSVC SDK 也不进镜像：第一次编 msvc 时 cargo-xwin 会自动下到
# /usr/soft/build_workspace/xwin，之后复用。
#
# 国内加速（都能用 --build-arg 换）：
#   APT_MIRROR    debian 源，默认阿里云
#   基础镜像走宿主机 /etc/docker/daemon.json 的 registry-mirrors
#
# 构建（两个变体，同一个 Dockerfile，差别只有基础镜像的 glibc 版本）：
#   docker build -f tools/docker/rust-cross.Dockerfile -t rust-cross:1.98.1 tools/docker
#   docker build -f tools/docker/rust-cross.Dockerfile --build-arg BASE_IMAGE=debian:bookworm-slim \
#       -t rust-cross:1.98.1-bookworm tools/docker
# 平时不用手敲：tools/docker/image.sh <triple> 会按目标挑变体，缺了就现建。
# 用法见 tools/docker/README.md；日常用 tools/docker/run.sh 和 tools/pack.sh。
#
# 为什么两个变体：
#   trixie（默认）  glibc 2.41 + clang 19 —— musl 目标、windows-msvc。xwin 带下来的
#                   MSVC STL 要求 Clang ≥ 19，而 bookworm 的 clang 只有 14。
#   bookworm        glibc 2.36 + clang 14 —— x86_64-unknown-linux-gnu 用它编，产物才能
#                   在 Debian 12 / Ubuntu 22.04 这些老 glibc 上跑（trixie 编出来的要
#                   2.38/2.39 的符号，Debian 12 是 2.36，loader 直接拒绝）。
ARG BASE_IMAGE=debian:trixie-slim
FROM ${BASE_IMAGE}

ARG APT_MIRROR=mirrors.aliyun.com

ENV DEBIAN_FRONTEND=noninteractive

# 编译依赖。clang/llvm/libclang：bindgen；nasm：x86_64 Windows 汇编；
# musl-tools：没有 musl.cc 那份 x86_64 工具链时兜底（它的 C++ 是宿主机 glibc 的，
# BoringSSL 那种 C++ 依赖只能靠宿主 /usr/soft/x86_64-linux-musl-cross）。
RUN set -eux; \
    sed -i "s|deb.debian.org|${APT_MIRROR}|g; s|security.debian.org|${APT_MIRROR}|g" \
        /etc/apt/sources.list.d/debian.sources; \
    apt-get update; \
    apt-get install -y --no-install-recommends \
        build-essential cmake perl curl xz-utils pkg-config ca-certificates git \
        clang llvm libclang-dev lld nasm musl-tools ninja-build unzip python3 file; \
    rm -rf /var/lib/apt/lists/*

WORKDIR /work
CMD ["sh", "-c", "echo '用 tools/docker/run.sh 跑命令；见 tools/docker/README.md'; exit 2"]
