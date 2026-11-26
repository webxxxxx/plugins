#!/bin/sh
# 按目标挑 rust-cross 镜像的变体（缺了现建），把镜像名打到 stdout。
#
#   sh tools/docker/image.sh                           # 默认变体：trixie，glibc 2.41 + clang 19
#   sh tools/docker/image.sh x86_64-unknown-linux-gnu   # gnu 变体：bookworm，glibc 2.36 + clang 14
#
# 为什么 gnu 目标要单独一个老底子：动态链接的产物把「我要 GLIBC_2.38 的符号」刻在
# ELF 的 .gnu.version_r 里，loader 在老 glibc 上直接拒绝执行（Debian 12 是 2.36）。
# musl 目标是 static-pie，跟宿主 glibc 无关，所以照旧用 trixie；windows-msvc 那边
# xwin 带下来的 MSVC STL 要求 Clang ≥ 19，只有 trixie 够。
#
# 两个变体是同一个 Dockerfile，只换 BASE_IMAGE。想换仓库或版本：
#   SVCD_CROSS_IMAGE       默认变体的镜像名（默认 rust-cross:1.98.1）
#   SVCD_CROSS_IMAGE_GNU   gnu 变体的镜像名（默认 rust-cross:1.98.1-bookworm）
#   SVCD_CROSS_BASE / SVCD_CROSS_BASE_GNU   各自的基础镜像
set -eu

SELF_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
REPO=$(CDPATH= cd -- "$SELF_DIR/../.." && pwd)

triple=${1:-}
case "$triple" in
*-unknown-linux-gnu)
    image=${SVCD_CROSS_IMAGE_GNU:-rust-cross:1.98.1-bookworm}
    base=${SVCD_CROSS_BASE_GNU:-debian:bookworm-slim}
    ;;
*)
    image=${SVCD_CROSS_IMAGE:-rust-cross:1.98.1}
    base=${SVCD_CROSS_BASE:-debian:trixie-slim}
    ;;
esac

command -v docker >/dev/null 2>&1 || {
    echo "image: 找不到 docker（WSL：apt install docker.io && sudo usermod -aG docker \$USER，重开会话）" >&2
    exit 1
}

if ! docker image inspect "$image" >/dev/null 2>&1; then
    echo "image: 还没有 $image，从 $base 建一个（第一次几分钟）…" >&2
    # 构建上下文就 tools/docker：Dockerfile 里没有 COPY/ADD，上下文大小无所谓，
    # 用仓库根反而要把 vendor/、dist/、.git 全传一遍（几个 G，光 untar 就好几分钟）。
    docker build -f "$SELF_DIR/rust-cross.Dockerfile" \
        --build-arg "BASE_IMAGE=$base" -t "$image" "$SELF_DIR" >&2 || {
        echo "image: $image 构建失败" >&2
        exit 1
    }
fi

printf '%s\n' "$image"
