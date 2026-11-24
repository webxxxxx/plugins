# 通用 Rust 交叉编译镜像：Linux（gnu / musl，x86_64 / armv7 / aarch64）+ Windows（msvc）。
# 除了 macOS（要 Xcode，只能在 mac 上编），本项目组的 Rust 产物都能在这里出。
#
# 镜像里只有**编译依赖**，Rust 工具链由宿主机提供（tools/docker/run.sh 把宿主机的
# ~/.rustup 和 ~/.cargo/bin 挂进来）：
#   clang / llvm / libclang   bindgen（BoringSSL、各种 *-sys）要用
#   nasm                      BoringSSL 在 x86_64 Windows 上的汇编
#   musl-tools                容器内直接编 x86_64-unknown-linux-musl
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
# 构建：
#   docker build -f tools/docker/rust-cross.Dockerfile -t rust-cross:1.98.1 tools/docker
# 用法见 tools/docker/README.md；日常用 tools/docker/run.sh 和 tools/pack.sh。
# bookworm 的 clang 只有 14，而 xwin 带下来的 MSVC STL 要求 Clang ≥ 19，所以用 trixie
FROM debian:trixie-slim

ARG APT_MIRROR=mirrors.aliyun.com

ENV DEBIAN_FRONTEND=noninteractive

# 编译依赖。clang/llvm/libclang：bindgen；nasm：x86_64 Windows 汇编；
# musl-tools：容器内直接编 x86_64 musl。
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
