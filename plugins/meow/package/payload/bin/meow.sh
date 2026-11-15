#!/bin/sh
# meow：包装 vendor/meow-rs 编译出来的二进制。生命周期：start / stop / status。
# 所有路径相对 payload 目录解析，不依赖 /plugin/meow 这类绝对路径。
set -u
HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
ROOT=$(CDPATH= cd -- "$HERE/.." && pwd)   # payload/，config 与 yacd 在这一层
. "$HERE/../lib/svcd-plugin.sh"

DATA=${SVCD_DATA_DIR:-$ROOT/.meow}
mkdir -p "$DATA" 2>/dev/null || true
PIDFILE="$DATA/meow.pid"
LOG="$DATA/meow.log"

# config.yaml 在包里，但运行时复制一份到 DATA，并把残留的绝对路径改成 payload 的实际位置。
prepare_config() {
    src="$ROOT/config.yaml"
    dst="$DATA/config.yaml"
    [ -f "$src" ] || return 1
    sed "s#/plugin/meow#${ROOT}#g" "$src" > "$dst"
    printf '%s' "$dst"
}

running() {
    [ -f "$PIDFILE" ] && kill -0 "$(cat "$PIDFILE" 2>/dev/null)" 2>/dev/null
}

# status 里带上配置里声明的端口，读不到就省略
report_ports() {
    cfg="$1"
    [ -f "$cfg" ] || return 0
    for key in mixed-port port socks-port; do
        value=$(sed -n "s/^${key}:[[:space:]]*\\([0-9][0-9]*\\).*/\\1/p" "$cfg" | head -n 1)
        [ -n "$value" ] && printf ',"%s":%s' "$key" "$value"
    done
}

do_start() {
    cfg=$(prepare_config) || { echo 'config.yaml missing' >&2; return 1; }
    if running; then
        printf 'already running (pid %s)' "$(cat "$PIDFILE")"
        return 0
    fi
    "$HERE/meow" -d "$ROOT" -f "$cfg" >"$LOG" 2>&1 &
    echo $! > "$PIDFILE"
    sleep 1
    if running; then
        printf 'started (pid %s)' "$(cat "$PIDFILE")"
    else
        echo 'start failed: see meow.log' >&2
        return 1
    fi
}

do_stop() {
    if running; then
        kill "$(cat "$PIDFILE")" 2>/dev/null || true
        rm -f "$PIDFILE"
        printf 'stopped'
    else
        rm -f "$PIDFILE"
        printf 'not running'
    fi
}

do_status() {
    cfg="$DATA/config.yaml"
    if running; then
        printf '{"running":true,"pid":%s%s}' "$(cat "$PIDFILE")" "$(report_ports "$cfg")"
    else
        printf '{"running":false%s}' "$(report_ports "$cfg")"
    fi
}

handle() {
    case "$1" in
    *) echo "unknown tool: $1" >&2; return 1 ;;
    esac
}

# 生命周期入口：宿主按 manifest.lifecycle 带参数调用，执行完即退出。
case "${1:-}" in
start)
    do_start
    exit $?
    ;;
stop)
    do_stop
    exit $?
    ;;
status)
    do_status
    exit $?
    ;;
esac

# 无参数：作为插件进程常驻，先拉起守护进程，再进入协议循环。
do_start >&2 || true
svcd_plugin_serve handle
