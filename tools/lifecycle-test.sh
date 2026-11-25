#!/bin/sh
# 生命周期脚本的「收尾」契约（svcd #323 之后）：
#   - 被监督进程是 exec（exec.args = start），stop 在进程消失**之后**才跑；
#   - 所以 stop 只收尾、不杀进程，也不能按 pidfile 发信号——强杀、断电之后那个 pid
#     很可能已经被系统复用给别的进程；
#   - status 只认自己的进程（同样不吃复用 pidfile 的亏）。
# 三个插件（hysteria / sshd / meow）各跑一遍：造一个假的插件目录，把 pidfile 指向一个
# 活着但不相干的进程，然后检查 status 不认它、stop 不杀它、pidfile 被删掉。
#
# 只在 Linux 上有意义（靠 /proc 认自己的进程），macOS / Windows 上脚本会退回 kill -0。
# 用法：sh tools/lifecycle-test.sh [仓库根]
set -eu

REPO=$(CDPATH= cd -- "${1:-$(dirname -- "$0")/..}" && pwd)
WORK=$(mktemp -d "${TMPDIR:-/tmp}/lifecycle-test.XXXXXX")

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

# 冒充「pid 被复用」：一个活着、但不是插件的进程。
sleep 300 &
DECOY=$!

cleanup() {
    kill "$DECOY" 2>/dev/null || true
    rm -rf "$WORK"
}
trap cleanup EXIT INT TERM

# $1 标签，$2 插件目录，$3 入口脚本，$4 pidfile
check_stale_pid() {
    _label=$1
    _plugin=$2
    _script=$3
    _pidfile=$4
    mkdir -p "$(dirname -- "$_pidfile")"
    printf '%s\n' "$DECOY" > "$_pidfile"

    _status=$(SVCD_PLUGIN_DIR="$_plugin" sh "$_script" status 2>&1) ||
        fail "$_label: status 退出非零（$_status）"
    case "$_status" in
    *'"running":false'*) ;;
    *) fail "$_label: 复用 pid 的无关进程被当成插件自己了（$_status）" ;;
    esac
    kill -0 "$DECOY" 2>/dev/null || fail "$_label: status 就把无关进程弄没了"

    SVCD_PLUGIN_DIR="$_plugin" sh "$_script" stop >/dev/null 2>&1 ||
        fail "$_label: stop 退出非零"
    kill -0 "$DECOY" 2>/dev/null || fail "$_label: stop 把无关进程杀了"
    if [ -f "$_pidfile" ]; then
        fail "$_label: stop 之后 pidfile 还在"
    fi
}

# ---- hysteria ----
plugin="$WORK/hysteria"
bin="$plugin/1.0.3/payload/bin"
mkdir -p "$bin"
cp "$REPO/plugins/hysteria/package/payload/bin/hysteria.sh" "$bin/"
check_stale_pid hysteria "$plugin" "$bin/hysteria.sh" "$plugin/data/hysteria.pid"

# ---- sshd ----
plugin="$WORK/sshd"
bin="$plugin/1.0.0/payload/bin"
mkdir -p "$bin"
cp "$REPO/plugins/sshd/package/payload/bin/sshd.sh" "$bin/"
check_stale_pid sshd "$plugin" "$bin/sshd.sh" "$plugin/data/dropbear.pid"

# ---- meow ----
plugin="$WORK/meow"
bin="$plugin/1.3.0/payload/bin"
mkdir -p "$bin" "$plugin/1.3.0/payload/lib" "$plugin/1.3.0/payload/yacd"
cp "$REPO/plugins/meow/platforms/meow.sh" "$bin/"
check_stale_pid meow "$plugin" "$bin/meow.sh" "$plugin/data/meow.pid"

echo 'plugin lifecycle stop/status: ok'
