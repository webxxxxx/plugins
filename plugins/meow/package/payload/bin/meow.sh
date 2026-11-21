#!/bin/sh
# meow：包装 vendor/meow-rs 编译出来的二进制。
#
# 宿主把 lifecycle.start 当成常驻前台进程：start 直接 exec 到 meow，
# stop/status 执行后退出。路径全部从脚本位置推导，数据目录用 SVCD_DATA_DIR。
set -u

HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
ROOT=$(CDPATH= cd -- "$HERE/.." && pwd)          # payload/：yacd、cn_domains 在这一层
PLUGIN_ROOT=$(CDPATH= cd -- "$HERE/../../.." && pwd)
BASE=${SVCD_DATA_DIR:-$PLUGIN_ROOT/.data}
DATA="$BASE/meow"
PIDFILE="$DATA/meow.pid"
CONFIG="$PLUGIN_ROOT/config"
DATA_CONFIG="$DATA/config.yaml"

mkdir -p "$DATA" 2>/dev/null || true

# 配置直接使用安装后的活动配置，订阅/API 的写回才会出现在控制台里。
# 只有源码 checkout 没有活动配置时，才用包内模板生成一份数据目录副本。
prepare_config() {
    if [ ! -f "$CONFIG" ]; then
        template="$PLUGIN_ROOT/package/config/config"
        [ -f "$template" ] || { echo 'meow: config.yaml missing' >&2; return 1; }
        if [ ! -f "$DATA_CONFIG" ]; then
            sed -e "s#@DATA@#$DATA#g" "$template" > "$DATA_CONFIG" || return 1
        fi
        CONFIG="$DATA_CONFIG"
    elif grep -q '@DATA@' "$CONFIG"; then
        # 旧布局：订阅数据可能已经写进数据目录的副本；先把它迁回活动配置。
        if [ -f "$DATA_CONFIG" ] && grep -q '^subscriptions:' "$DATA_CONFIG"; then
            cp "$DATA_CONFIG" "$CONFIG" || return 1
        fi
        if grep -q '@DATA@' "$CONFIG"; then
            tmp="$CONFIG.tmp.$$"
            sed -e "s#@DATA@#$DATA#g" "$CONFIG" > "$tmp" && mv "$tmp" "$CONFIG" || return 1
        fi
    fi
    printf '%s' "$CONFIG"
}

running() {
    [ -f "$PIDFILE" ] && kill -0 "$(cat "$PIDFILE" 2>/dev/null)" 2>/dev/null
}

report_ports() {
    cfg="$1"
    [ -f "$cfg" ] || return 0
    for key in mixed-port port socks-port; do
        value=$(sed -n "s/^${key}:[[:space:]]*\([0-9][0-9]*\).*/\1/p" "$cfg" | head -n 1)
        [ -n "$value" ] && printf ',"%s":%s' "$key" "$value"
    done
}

do_start() {
    cfg=$(prepare_config) || return 1
    if running; then
        echo "already running (pid $(cat "$PIDFILE"), port 7890)" >&2
        return 1
    fi
    # 先做一次配置校验；失败就不要 exec，宿主会看到 start 失败而不是一个秒退进程。
    if ! "$HERE/meow" -d "$ROOT" -f "$cfg" -t >/dev/null 2>&1; then
        echo 'meow: config validation failed (run with -t to see details)' >&2
        return 1
    fi
    echo $$ > "$PIDFILE"
    echo "meow starting (data=$ROOT, config=$cfg)" >&2
    exec "$HERE/meow" -d "$ROOT" -f "$cfg" --ext-ui "$ROOT/yacd"
}

do_stop() {
    if running; then
        pid=$(cat "$PIDFILE")
        kill "$pid" 2>/dev/null || true
        n=0
        while kill -0 "$pid" 2>/dev/null && [ "$n" -lt 5 ]; do
            n=$((n + 1))
            sleep 1
        done
        kill -9 "$pid" 2>/dev/null || true
        rm -f "$PIDFILE"
        echo "stopped (pid $pid)"
    else
        rm -f "$PIDFILE"
        echo "not running"
    fi
}

do_status() {
    cfg="$CONFIG"
    if running; then
        printf '{"running":true,"pid":%s,"controller":"127.0.0.1:9090"%s}\n' "$(cat "$PIDFILE")" "$(report_ports "$cfg")"
    else
        # 不要在这里删 pidfile：宿主可能在 start 写到一半时先跑 status，
        # 删掉会把刚起来的进程变成“看不见”。
        printf '{"running":false,"controller":"127.0.0.1:9090"%s}\n' "$(report_ports "$cfg")"
    fi
}

case "${1:-}" in
start)  do_start ;;
stop)   do_stop ;;
status) do_status ;;
*)      echo "usage: $0 start|stop|status" >&2; exit 2 ;;
esac
