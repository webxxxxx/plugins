#!/bin/sh
# 在 rust-cross 镜像里跑任意命令（编 meow、编 plugin-tools、编宿主 svcd，或别的 Rust 项目）。
#
#   cd ~/src/whatever && docker-build.sh -- cargo build --release
#   docker-build.sh -p armv7-unknown-linux-musleabihf -- cargo build --release
#   docker-build.sh -w plugins/meow -n meow -- sh plugins/meow/build.sh meow 1.4.0 aarch64-unknown-linux-musl
#
# 项目根（挂到 /work 的那个目录）默认取**当前目录**所属的 git 顶层，没有 git 就是当前目录：
# 在哪个仓库里敲就在哪个仓库里编。所以别的仓库也能直接用，不用搬脚本、不用软链。
#
# Rust 工具链不装在镜像里，而是把宿主机的挂进去（版本由各仓库的 rust-toolchain.toml 决定，
# 目标也要在宿主机上装好：rustup target add <triple>）。
#
# 挂载（路径都在 WSL 里，别用 /mnt/d 当编译目录：drvfs 又慢又容易把内存吃光）：
#   <项目根>                  → /work        源码；产物写回 out/、dist/
#   ~/.rustup                 → /rustup      宿主机的工具链（可写：rustup 解析 pin 时要落临时文件）
#   ~/.cargo/bin              → /rustbin     cargo / rustup / cargo-xwin 的入口
#   /usr/soft                 → /usr/soft    宿主的交叉工具链（aarch64 / armv7 musl），共用
#   /usr/soft/build_workspace → 同一路径     registry 缓存、每个项目的 target、xwin 缓存
#
# 选项：
#   -r DIR    项目根（→ /work）：默认当前目录的 git 顶层
#   -w DIR    项目根下干活的位置（默认根），target 落在 <workspace>/<NAME>/target
#   -n NAME   构建工作目录名，默认取 -w 那个目录的名字
#   -p TRIPLE 目标平台：容器里按 tools/cross-env.sh 配好 CC / AR / 链接器和 PATH，
#             省得 cargo 报「linker cc not found」（不写就从命令里的 --target 读）
#   -t IMAGE  镜像（默认 rust-cross:1.98.1）
#   -j N      并行度（默认 4）
set -eu

SELF=$0
SELF_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
SCRIPT_REPO=$(CDPATH= cd -- "$SELF_DIR/../.." && pwd)
TOOLS_DIR=$(CDPATH= cd -- "$SELF_DIR/.." && pwd) # cross-env.sh 在 tools/ 下，容器里挂成 /svcd-tools
WORKSPACE=${SVCD_BUILD_WORKSPACE:-/usr/soft/build_workspace}
IMAGE=${SVCD_CROSS_IMAGE:-rust-cross:1.98.1}
JOBS=${JOBS:-4}
ROOT=
DIR=.
NAME=
TRIPLE=

usage() {
    sed -n '2,/^set -eu/p' "$SELF" | sed -e 's/^# \{0,1\}//' -e '${/^set -eu/d;}'
}

while [ $# -gt 0 ]; do
    case "$1" in
    -r | --root) ROOT=$2; shift 2 ;;
    -w | --workdir) DIR=$2; shift 2 ;;
    -n | --name) NAME=$2; shift 2 ;;
    -t | --image) IMAGE=$2; shift 2 ;;
    -j | --jobs) JOBS=$2; shift 2 ;;
    -p | --platform | --target) TRIPLE=$2; shift 2 ;;
    -h | --help) usage; exit 0 ;;
    --) shift; break ;;
    -*) echo "docker-build: 未知选项 $1（要跑的命令写在 -- 后面）" >&2; exit 2 ;;
    *) break ;;
    esac
done
[ $# -gt 0 ] || {
    echo "docker-build: 缺命令，例如：docker-build.sh -- cargo build --release" >&2
    exit 2
}

# ---- 项目根：默认跟着你站的仓库走 ----
if [ -z "$ROOT" ]; then
    ROOT=$(git -C "$PWD" rev-parse --show-toplevel 2>/dev/null || true)
    [ -n "$ROOT" ] || ROOT=$PWD
fi
[ -d "$ROOT" ] || { echo "docker-build: 项目根不存在：$ROOT" >&2; exit 2; }
ROOT=$(CDPATH= cd -- "$ROOT" && pwd)

case "$DIR" in
/*) WORKDIR=$(CDPATH= cd -- "$DIR" && pwd) || { echo "docker-build: 没有目录 $DIR" >&2; exit 2; } ;;
*) WORKDIR=$(CDPATH= cd -- "$ROOT/$DIR" && pwd) || { echo "docker-build: 项目根 $ROOT 下没有 $DIR" >&2; exit 2; } ;;
esac
case "$WORKDIR" in
"$ROOT") DIR=.
    NAME=${NAME:-$(basename "$ROOT")} ;;
"$ROOT"/*) DIR=${WORKDIR#"$ROOT"/}
    NAME=${NAME:-$(basename "$WORKDIR")} ;;
*) echo "docker-build: 工作目录 $WORKDIR 不在项目根 $ROOT 里（项目根是 -r 指定的）" >&2
    exit 2 ;;
esac

# ---- 从命令里认出 --target，顺手做两个检查 ----
_IS_CARGO=0
_WANT_TARGET=
for _a in "$@"; do
    case "$_a" in
    --target=*) [ -n "$TRIPLE" ] || TRIPLE=${_a#--target=} ;;
    --target) _WANT_TARGET=1 ;;
    cargo | */cargo) _IS_CARGO=1 ;;
    *) if [ "${_WANT_TARGET:-}" = 1 ]; then [ -n "$TRIPLE" ] || TRIPLE=$_a; _WANT_TARGET=; fi ;;
    esac
done

if [ "$_IS_CARGO" = 1 ] && [ ! -f "$WORKDIR/Cargo.toml" ]; then
    echo "docker-build: $WORKDIR 里没有 Cargo.toml" >&2
    echo "             挂进去的是 $ROOT → /work；-r 指项目根、-w 往下走子目录" >&2
    echo "             （这个脚本自己所在的仓库是 $SCRIPT_REPO）" >&2
    exit 2
fi

# ---- 目标平台：交叉工具链的表在 cross-env.sh，容器里再套上去 ----
# 宿主这边不认识三元组也没关系：把 tools/ 挂成 /svcd-tools，容器里 source 同一份表，
# 用 svcd_cross_exec 把 CC_/AR_/CARGO_TARGET_*_LINKER 和 PATH 配好再跑命令。
if [ -n "${TRIPLE:-}" ]; then
    case "$TRIPLE" in
    *-pc-windows-msvc)
        echo "docker-build: windows-msvc 要用 cargo xwin build，不是 cargo build（cargo-xwin 在 /rustbin，XWIN_CACHE_DIR 指到 $WORKSPACE/xwin）" >&2
        ;;
    esac
fi

command -v docker >/dev/null 2>&1 || {
    echo "docker-build: 找不到 docker（WSL：apt install docker.io && sudo usermod -aG docker \$USER，重开会话）" >&2
    exit 1
}
docker image inspect "$IMAGE" >/dev/null 2>&1 || {
    echo "docker-build: 还没有镜像 $IMAGE" >&2
    echo "             先构建：docker build -f $SCRIPT_REPO/tools/docker/rust-cross.Dockerfile -t $IMAGE $SCRIPT_REPO/tools/docker" >&2
    exit 1
}

# 宿主机工具链：容器里没有 rustc，全部从这里来
RUSTUP_HOME_HOST=${RUSTUP_HOME:-$HOME/.rustup}
CARGO_BIN_HOST=${CARGO_BIN:-$HOME/.cargo/bin}
[ -d "$RUSTUP_HOME_HOST/toolchains" ] || {
    echo "docker-build: 宿主机没有 rustup 工具链（$RUSTUP_HOME_HOST）" >&2
    echo "             装一个：curl https://sh.rustup.rs -sSf | sh -s -- -y --profile minimal" >&2
    exit 1
}
[ -x "$CARGO_BIN_HOST/cargo" ] || {
    echo "docker-build: 找不到 $CARGO_BIN_HOST/cargo" >&2
    exit 1
}

mkdir -p "$WORKSPACE/cargo" "$WORKSPACE/$NAME/target"
# cargo 的 registry 缓存和镜像配置都放工作目录：第二次编译快很多
if [ ! -f "$WORKSPACE/cargo/config.toml" ]; then
    printf '%s\n' \
        '[source.crates-io]' \
        'replace-with = "rsproxy-sparse"' \
        '' \
        '[source.rsproxy-sparse]' \
        'registry = "sparse+https://rsproxy.cn/index/"' \
        '' \
        '[net]' \
        'git-fetch-with-cli = true' \
        >"$WORKSPACE/cargo/config.toml"
fi

# /usr/soft 挂进同名路径：build.sh 里的 /usr/soft/*-cross 探测因此不用改。
SOFT=
if [ -d /usr/soft ]; then SOFT="-v /usr/soft:/usr/soft"; fi

CONTAINER_PATH="/rustbin:$WORKSPACE/cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"

echo "docker-build: $ROOT → /work，在 $DIR 里干活${TRIPLE:+，目标 $TRIPLE}（$NAME）" >&2

# shellcheck disable=SC2086 # $SOFT、$ENVS 是刻意留白的参数串
exec docker run --rm \
    --user "$(id -u):$(id -g)" \
    -e HOME=/tmp \
    -w "/work/$DIR" \
    -v "$ROOT:/work" \
    -v "$RUSTUP_HOME_HOST:/rustup" \
    -v "$CARGO_BIN_HOST:/rustbin:ro" \
    -v "$WORKSPACE:$WORKSPACE" \
    $SOFT \
    -e PATH="$CONTAINER_PATH" \
    -e RUSTUP_HOME=/rustup \
    -e CARGO_HOME="$WORKSPACE/cargo" \
    -e CARGO_TARGET_DIR="$WORKSPACE/$NAME/target" \
    -e XWIN_CACHE_DIR="$WORKSPACE/xwin" \
    -e XWIN_ACCEPT_LICENSE=1 \
    -e SVCD_IN_DOCKER=1 \
    -e JOBS="$JOBS" \
    -v "$TOOLS_DIR:/svcd-tools:ro" \
    "$IMAGE" \
    sh -c '. /svcd-tools/cross-env.sh && svcd_cross_exec "$0" "$@"' "${TRIPLE:-}" "$@"
