#!/bin/sh
# hysteria：用 mini-sb-agent（sing-box 内核）跑一个 Hysteria2 服务端。
#
# 宿主监督的就是这个脚本（exec.args = start），所以 start 直接 exec 内核；
# stop 在进程退出之后跑，只做收尾；status 按需探测。TLS 证书和密码在首次启动时
# 生成到数据目录。
set -u

HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
# 宿主给插件的三个目录之一是 SVCD_PLUGIN_DIR（安装目录）。手跑时没有它，就按脚本
# 自己的位置往上三层推（<插件>/<版本>/payload/bin/hysteria.sh）。
PLUGIN_ROOT=$(CDPATH= cd -- "${SVCD_PLUGIN_DIR:-$HERE/../../..}" && pwd)
# 插件的数据只能落在插件目录里：<插件目录>/data。宿主升级、卸载（不带 --purge）都
# 保留它，证书、私钥、密码不会重生成。
DATA="$PLUGIN_ROOT/data"
PIDFILE="$DATA/hysteria.pid"
CONFIG_DST="$DATA/config.json"
CERT="$DATA/cert.pem"
KEY="$DATA/key.pem"
PASSWORD_FILE="$DATA/password"

# 1.0 把数据放在插件目录之外，两种老位置：默认根布局下是 <data>/hysteria（插件目录的
# 上上级同名目录），手跑时是 <插件目录>/.data/hysteria。第一次 start 搬进来：证书和
# 密码跟着走，客户端不用重新配。
migrate_legacy() {
    [ -d "$DATA" ] && return 0
    for old in "$PLUGIN_ROOT/../../hysteria" "$PLUGIN_ROOT/.data/hysteria"; do
        [ -d "$old" ] || continue
        if mv "$old" "$DATA" 2>/dev/null; then
            echo "hysteria: 旧数据 $old 已搬到 $DATA" >&2
        fi
        return 0
    done
    return 0
}

find_config() {
    for candidate in "${SVCD_CONFIG:-}" "$PLUGIN_ROOT/config" "$PLUGIN_ROOT/package/config/config"; do
        [ -n "$candidate" ] && [ -f "$candidate" ] && { printf '%s' "$candidate"; return 0; }
    done
    return 1
}

# 证书和密码：优先用包里自带的 hysteria-certgen（Linux/macOS 都能跑，不依赖
# openssl）；手跑源码、包里还没编进它时，才退回 openssl（密码再退回 od）。
CERTGEN="$HERE/hysteria-certgen"

random_password() {
    if [ -f "$CERTGEN" ] && "$CERTGEN" hex 16 > "$PASSWORD_FILE" 2>/dev/null && [ -s "$PASSWORD_FILE" ]; then
        return 0
    fi
    if command -v openssl >/dev/null 2>&1; then
        openssl rand -hex 16 > "$PASSWORD_FILE" 2>/dev/null
        [ -s "$PASSWORD_FILE" ] && return 0
    fi
    if command -v od >/dev/null 2>&1; then
        od -An -N16 -tx1 /dev/urandom 2>/dev/null | tr -d ' \n' > "$PASSWORD_FILE"
        [ -s "$PASSWORD_FILE" ] && return 0
    fi
    rm -f "$PASSWORD_FILE"
    return 1
}

password() {
    if [ ! -s "$PASSWORD_FILE" ]; then
        rm -f "$PASSWORD_FILE"
        random_password || {
            echo 'hysteria: 生成密码失败（包里没有 hysteria-certgen，系统也没有 openssl 或 od）' >&2
            return 1
        }
    fi
    cat "$PASSWORD_FILE"
}

ensure_cert() {
    [ -s "$CERT" ] && [ -s "$KEY" ] && return 0
    rm -f "$CERT" "$KEY"
    if [ -f "$CERTGEN" ]; then
        "$CERTGEN" cert "$CERT" "$KEY" 2>/dev/null
        if [ -s "$CERT" ] && [ -s "$KEY" ]; then
            chmod 600 "$KEY" 2>/dev/null || true
            return 0
        fi
        rm -f "$CERT" "$KEY"
        echo "hysteria: hysteria-certgen 没生成出证书（$CERTGEN），改用 openssl" >&2
    fi
    if command -v openssl >/dev/null 2>&1; then
        openssl req -x509 -newkey rsa:2048 -sha256 -days 3650 -nodes \
            -keyout "$KEY" -out "$CERT" -subj '/CN=svcd-hysteria' >/dev/null 2>&1 || return 1
        [ -s "$CERT" ] && [ -s "$KEY" ] || return 1
        chmod 600 "$KEY" 2>/dev/null || true
        return 0
    fi
    echo 'hysteria: 生成自签证书失败（包里没有 hysteria-certgen，系统也没有 openssl）' >&2
    return 1
}

prepare_config() {
    mkdir -p "$DATA" 2>/dev/null || { echo "hysteria: 建不了数据目录 $DATA" >&2; return 1; }
    src=$(find_config) || { echo 'hysteria: config.json missing' >&2; return 1; }
    # 只有模板里的占位符才替换；用户自己写死证书路径/密码时原样保留。
    secret=$(password) || return 1
    if grep -q -e '@CERT@' -e '@KEY@' "$src" 2>/dev/null; then
        ensure_cert || return 1
    fi
    sed -e "s#@PASSWORD@#$secret#g" -e "s#@CERT@#$CERT#g" -e "s#@KEY@#$KEY#g" \
        "$src" > "$CONFIG_DST" || return 1
    printf '%s' "$CONFIG_DST"
}

# pidfile 里的 pid 是不是我们的内核。宿主负责终止：强杀（SIGKILL）或断电之后 pidfile
# 会留下来，而那个 pid 很可能已经被系统复用给别的进程——只看 kill -0 会把它当成自己
# （status 谎报 running、start 说 already running）。
running() {
    _pid=$(cat "$PIDFILE" 2>/dev/null) || return 1
    [ -n "$_pid" ] || return 1
    kill -0 "$_pid" 2>/dev/null || return 1
    # 有 /proc 就按命令行认（NUL 分隔，grep -F 直接当字节串搜）。
    if [ -r "/proc/$_pid/cmdline" ]; then
        grep -qF -- "$HERE/mini-sb-agent" "/proc/$_pid/cmdline" 2>/dev/null
        return
    fi
    # macOS 没有 /proc：退回 ps。ps 也没有就只能信 kill -0，和以前一样。
    _argv=$(ps -o args= -p "$_pid" 2>/dev/null) || return 0
    case "$_argv" in
    *"$HERE/mini-sb-agent"*) return 0 ;;
    esac
    return 1
}

do_start() {
    migrate_legacy
    cfg=$(prepare_config) || return 1
    if running; then
        echo "already running (pid $(cat "$PIDFILE"), port 8443)" >&2
        return 1
    fi
    echo $$ > "$PIDFILE"
    echo "hysteria starting (config=$cfg)" >&2
    exec "$HERE/mini-sb-agent" -config "$cfg"
}

# 宿主负责终止（Unix 进程组 SIGTERM → 10 秒 → SIGKILL），stop 跑到这里时内核已经不
# 在了：它只收尾，不杀进程。pidfile 可能是强杀留下的，那个 pid 甚至已经被复用给别的
# 进程，所以只删不按它发信号。
do_stop() {
    rm -f "$PIDFILE"
    echo "stopped"
}

do_status() {
    if running; then
        printf '{"running":true,"pid":%s,"port":8443}\n' "$(cat "$PIDFILE")"
    else
        printf '{"running":false,"port":8443}\n'
    fi
}

case "${1:-}" in
start)  do_start ;;
stop)   do_stop ;;
status) do_status ;;
*)      echo "usage: $0 start|stop|status" >&2; exit 2 ;;
esac
