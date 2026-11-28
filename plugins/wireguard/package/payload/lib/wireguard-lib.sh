#!/bin/sh
# wireguard-lib.sh — wireguard 插件共用的路径、设置、密钥与 peer 存储。
#
# 调用方先设置 HERE（payload/bin），再 source 本文件。wireguard.sh（生命周期）、
# wireguardd.sh（被监督的看守进程）和 wg-lan-fix.sh（自愈循环）都从这里取同一份状态。
#
# 所有插件文件都落在插件目录内：
#   <插件目录>/data/settings    设置（管理页面写，0600）
#   <插件目录>/data/wg/         服务端密钥
#   <插件目录>/data/peers.tsv   peer 列表（含客户端私钥，0600）
#
# 设置文件是 shell 可 source 的 KEY=value；管理页面通过 capability 写它。

: "${HERE:?HERE must be set before sourcing wireguard-lib.sh}"

PLUGIN_ROOT=$(CDPATH= cd -- "${SVCD_PLUGIN_DIR:-$HERE/../../..}" && pwd)
DATA="$PLUGIN_ROOT/data"
SETTINGS="$DATA/settings"
CONF="$PLUGIN_ROOT/config"
PEERS="$DATA/peers.tsv"
WGDIR="$DATA/wg"
SERVER_KEY="$WGDIR/server.key"
SERVER_PUB="$WGDIR/server.pub"

WG_IF=wg0
WG_TOOL="$HERE/wg-tool"
KO="$HERE/wireguard.ko"
GOTATUN="$HERE/gotatun"
GOTATUN_PID="$DATA/gotatun.pid"
# 被监督进程（看守）的 pid：能力钩子是另一个进程，只能靠它知道看守还在不在。
GUARDIAN_PID="$DATA/wireguardd.pid"
WG_SOCK="/var/run/wireguard/$WG_IF.sock"

PATH=/etc/sr/bin:/kmodule/bin:/sbin:/usr/sbin:/bin:/usr/bin
export PATH

log() { echo "wireguard: $*" >&2; }
# 生命周期脚本里 fail 只写日志；在能力钩子（WIREGUARD_CAPABILITY=1）里同时写到
# stdout，宿主的 tools/call 才会把这段话作为 isError 的文本带给管理页面。
fail() {
    log "$*"
    [ "${WIREGUARD_CAPABILITY:-0}" = 1 ] && printf '%s\n' "$*"
    return 1
}

# 首次启动/管理页面没填时的缺省值。管理页面写出的 settings 覆盖它们。
settings_defaults() {
    # auto：优先 br0，其次 br-lan，再退到任意带 IPv4 的网桥 / 默认路由口。
    WG_LAN_IF=auto
    WG_PORT=51820
    WG_NET=10.99.0.0/24
    WG_ADDR=10.99.0.1
    WG_MTU=1420
    WG_TABLE=200
    WG_ENDPOINT=
    WG_DNS=
    WG_ALLOWED_IPS=0.0.0.0/0,::/0
    WG_KEEPALIVE=25
    # auto：包里有 wireguard.ko 就用内核模块（sr6110），否则用 gotatun。
    WG_IMPL=auto
    # 自愈只属于内核后端（带 wireguard.ko 的包）：厂商固件会把 rp_filter / 策略路由
    # 刷回去，所以要周期性修一遍。auto/on 都是开，off 关；gotatun 的用户态隧道没有
    # 这个坑，守护循环只在 kernel 分支里叫 wg-lan-fix.sh，这个开关对它不起作用。
    WG_LAN_FIX=auto
}

# 把当前 WG_* 变量写成 settings（原子替换）。
write_settings() {
    mkdir -p "$DATA" 2>/dev/null || {
        fail "建不了数据目录 $DATA"
        return 1
    }
    _tmp="$SETTINGS.$$"
    {
        printf 'WG_LAN_IF=%s\n' "$WG_LAN_IF"
        printf 'WG_PORT=%s\n' "$WG_PORT"
        printf 'WG_NET=%s\n' "$WG_NET"
        printf 'WG_ADDR=%s\n' "$WG_ADDR"
        printf 'WG_MTU=%s\n' "$WG_MTU"
        printf 'WG_TABLE=%s\n' "$WG_TABLE"
        printf 'WG_ENDPOINT=%s\n' "$WG_ENDPOINT"
        printf 'WG_DNS=%s\n' "$WG_DNS"
        printf 'WG_ALLOWED_IPS=%s\n' "$WG_ALLOWED_IPS"
        printf 'WG_KEEPALIVE=%s\n' "$WG_KEEPALIVE"
        printf 'WG_IMPL=%s\n' "$WG_IMPL"
        printf 'WG_LAN_FIX=%s\n' "$WG_LAN_FIX"
    } > "$_tmp" 2>/dev/null || {
        rm -f "$_tmp"
        fail "写不了 $SETTINGS"
        return 1
    }
    chmod 600 "$_tmp" 2>/dev/null || true
    mv "$_tmp" "$SETTINGS" 2>/dev/null || {
        rm -f "$_tmp"
        fail "替换不了 $SETTINGS"
        return 1
    }
    return 0
}

# 先放默认值，再 source settings；首次运行时把默认值落盘。
load_settings() {
    settings_defaults
    # 1.0 的配置在 <插件目录>/config；只在还没有 settings 时读一次，用来做升级迁移。
    if [ ! -f "$SETTINGS" ] && [ -f "$CONF" ]; then
        . "$CONF" 2>/dev/null || true
    fi
    if [ -f "$SETTINGS" ]; then
        . "$SETTINGS" || {
            fail "读不了设置 $SETTINGS"
            return 1
        }
    fi
    case "$WG_NET" in
    */*) ;;
    *) WG_NET="$WG_NET/24" ;;
    esac
    PREFIX=${WG_NET##*/}
    case "$WG_IMPL" in
    auto | kernel | userspace) ;;
    *) WG_IMPL=auto ;;
    esac
    case "$WG_LAN_FIX" in
    auto | on | off) ;;
    *) WG_LAN_FIX=auto ;;
    esac
    if [ ! -f "$SETTINGS" ]; then
        write_settings || return 1
    fi
    return 0
}

# ---- 后端：内核模块 / gotatun -------------------------------------------
#
# 后端由“这个包里带了什么”决定，不看宿主碰巧加载了什么模块：
#   带 wireguard.ko 的包（sr6110 变体）-> kernel
#   带 gotatun 的包（通用包）          -> userspace
# 同一条代码路径服务两种包；WG_IMPL 可以在 settings 里强制（排障用）。
impl_kernel() {
    case "$WG_IMPL" in
    kernel) return 0 ;;
    userspace) return 1 ;;
    esac
    [ -f "$KO" ]
}

impl_userspace() {
    case "$WG_IMPL" in
    userspace) return 0 ;;
    kernel) return 1 ;;
    esac
    [ -x "$GOTATUN" ]
}

# kernel | userspace | none
wg_impl() {
    if impl_kernel; then
        printf 'kernel'
    elif impl_userspace; then
        printf 'userspace'
    else
        printf 'none'
    fi
}

# LAN 口解析（WG_LAN_IF=auto）：br0 → br-lan → 任意带 IPv4 的网桥 → 默认
# 路由口。默认路由读 /proc/net/route：厂商内核上 `ip route` 的 fib 遍历会
# 直接 panic（见 wg-lan-fix.sh 的注释）。
has_ipv4() {
    ip -4 addr show "$1" 2>/dev/null | grep -q 'inet '
}

# 容器/虚拟化建的网桥不是 LAN；桌面 Linux 上 docker0 往往比真实网桥先起来。
is_lan_bridge() {
    case "$1" in
    docker* | virbr* | lxc* | veth* | tap* | tun*) return 1 ;;
    esac
    [ -d "/sys/class/net/$1/bridge" ]
}

resolve_lan_if() {
    case "$WG_LAN_IF" in
    '' | auto) ;;
    *) return 0 ;;
    esac
    for _cand in br0 br-lan; do
        if has_ipv4 "$_cand"; then
            WG_LAN_IF=$_cand
            return 0
        fi
    done
    for _cand in $(ls /sys/class/net 2>/dev/null); do
        is_lan_bridge "$_cand" || continue
        if has_ipv4 "$_cand"; then
            WG_LAN_IF=$_cand
            return 0
        fi
    done
    _cand=$(awk '$2 == "00000000" { print $1; exit }' /proc/net/route 2>/dev/null)
    if [ -n "$_cand" ]; then
        WG_LAN_IF=$_cand
        return 0
    fi
    WG_LAN_IF=br0
}

# ---- gotatun 进程 --------------------------------------------------------
#
# 由 wireguardd.sh 经 wireguard.sh up 拉起：它在被监督进程的进程组里，宿主
# 停服时按组 SIGTERM 能一起收走。UAPI 落在 /var/run/wireguard/<if>.sock，
# wg-tool 对非 netlink 接口会自动退到这个 socket。

gotatun_pid() {
    [ -f "$GOTATUN_PID" ] || return 1
    _pid=$(cat "$GOTATUN_PID" 2>/dev/null) || return 1
    [ -n "$_pid" ] || return 1
    printf '%s' "$_pid"
}

# pid 复用保护：/proc/<pid>/exe 必须还是本插件那个 gotatun。
is_our_gotatun() {
    [ -n "${1:-}" ] || return 1
    kill -0 "$1" 2>/dev/null || return 1
    [ "$(readlink -f "/proc/$1/exe" 2>/dev/null)" = "$(readlink -f "$GOTATUN" 2>/dev/null)" ]
}

gotatun_alive() {
    _pid=$(gotatun_pid) || return 1
    is_our_gotatun "$_pid"
}

start_gotatun() {
    [ -x "$GOTATUN" ] || {
        fail "包里没有 gotatun：这个包走用户态后端，但二进制不在"
        return 1
    }
    if [ ! -e /dev/net/tun ] && command -v modprobe >/dev/null 2>&1; then
        modprobe tun 2>/dev/null || true
    fi
    if [ ! -e /dev/net/tun ]; then
        fail "没有 /dev/net/tun：内核缺 TUN 支持，gotatun 起不来"
        return 1
    fi
    gotatun_alive && return 0
    mkdir -p "$DATA" /var/run/wireguard 2>/dev/null || true
    rm -f "$WG_SOCK" 2>/dev/null || true
    # stdout 是宿主的协议通道：gotatun 的日志一律改道 stderr，由宿主写进
    # <logs>/wireguard.log。--disable-drop-privileges 是旧平台脚本的用法，
    # 保留 root 才能设 fwmark / 改路由。
    "$GOTATUN" --foreground --disable-drop-privileges --verbosity error \
        "$WG_IF" 1>&2 &
    _pid=$!
    printf '%s\n' "$_pid" > "$GOTATUN_PID" 2>/dev/null || true
    _n=0
    while [ "$_n" -lt 10 ]; do
        [ -S "$WG_SOCK" ] && return 0
        kill -0 "$_pid" 2>/dev/null || break
        sleep 1
        _n=$((_n + 1))
    done
    fail "gotatun 没有在 10 秒内起来（$WG_IF，UAPI $WG_SOCK）"
    stop_gotatun
    return 1
}

stop_gotatun() {
    _pid=$(gotatun_pid 2>/dev/null) || _pid=
    if is_our_gotatun "$_pid"; then
        kill -TERM "$_pid" 2>/dev/null || true
        _n=0
        while [ "$_n" -lt 5 ] && kill -0 "$_pid" 2>/dev/null; do
            sleep 1
            _n=$((_n + 1))
        done
        kill -KILL "$_pid" 2>/dev/null || true
    fi
    rm -f "$GOTATUN_PID" "$WG_SOCK" 2>/dev/null || true
    return 0
}

has_key() {
    [ -s "$1" ] && grep -q '[^[:space:]]' "$1" 2>/dev/null
}

# 服务端密钥；缺失/为空就生成一套。peer 密钥在 add_peer 时按需生成。
ensure_keys() {
    mkdir -p "$WGDIR" 2>/dev/null || {
        fail "建不了密钥目录 $WGDIR"
        return 1
    }
    chmod 700 "$WGDIR" 2>/dev/null || true
    if ! has_key "$SERVER_KEY"; then
        rm -f "$SERVER_KEY"
        if ! "$WG_TOOL" genkey > "$SERVER_KEY" 2>/dev/null || ! has_key "$SERVER_KEY"; then
            rm -f "$SERVER_KEY"
            fail "生成服务端私钥失败（$WG_TOOL genkey）"
            return 1
        fi
        chmod 600 "$SERVER_KEY" 2>/dev/null || true
        log "首次启动：已生成服务端私钥 $SERVER_KEY"
    fi
    if ! "$WG_TOOL" pubkey < "$SERVER_KEY" > "$SERVER_PUB" 2>/dev/null || ! has_key "$SERVER_PUB"; then
        rm -f "$SERVER_PUB"
        fail "从 $SERVER_KEY 推导服务端公钥失败（$WG_TOOL pubkey）"
        return 1
    fi
    chmod 644 "$SERVER_PUB" 2>/dev/null || true
    return 0
}

peers_init() {
    mkdir -p "$DATA" 2>/dev/null || return 1
    [ -f "$PEERS" ] || : > "$PEERS" 2>/dev/null || return 1
    chmod 600 "$PEERS" 2>/dev/null || true
    return 0
}

# 1.0 只有一套对端密钥（data/wg/peer.key + peer.pub）；首次启动把它迁进 peers.tsv。
migrate_legacy_peer() {
    [ -f "$PEERS" ] && [ -s "$PEERS" ] && return 0
    [ -s "$WGDIR/peer.key" ] && [ -s "$WGDIR/peer.pub" ] || return 0
    peers_init || return 0
    _addr=$(peer_next_addr) || return 0
    _pub=$(cat "$WGDIR/peer.pub" 2>/dev/null) || _pub=""
    _priv=$(cat "$WGDIR/peer.key" 2>/dev/null) || _priv=""
    [ -n "$_pub" ] && [ -n "$_priv" ] || return 0
    _created=$(date +%s 2>/dev/null || echo 0)
    printf '1\t旧客户端\t%s\t%s\t%s\t%s\n' "$_addr" "$_pub" "$_priv" "$_created" > "$PEERS"
    chmod 600 "$PEERS" 2>/dev/null || true
    log "迁移 1.0 的对端密钥到 $PEERS（地址 $_addr）"
}

# $1 = peer id；命中时设置 P_ID P_NAME P_ADDR P_PUB P_PRIV P_CREATED。
peer_find() {
    P_ID= P_NAME= P_ADDR= P_PUB= P_PRIV= P_CREATED=
    [ -f "$PEERS" ] || return 1
    _want=$1
    while IFS="$(printf '\t')" read -r id name addr pub priv created; do
        [ "$id" = "$_want" ] || continue
        P_ID=$id P_NAME=$name P_ADDR=$addr P_PUB=$pub P_PRIV=$priv P_CREATED=$created
        return 0
    done < "$PEERS"
    return 1
}

# 下一个 peer id（最大 id + 1）。
peer_next_id() {
    [ -f "$PEERS" ] || {
        printf '1'
        return 0
    }
    awk -F '\t' 'BEGIN { m = 0 } { if ($1 + 0 > m) m = $1 + 0 } END { print m + 1 }' "$PEERS"
}

# 下一个隧道地址（仅支持 /24，避开服务端地址与已分配地址）。
peer_next_addr() {
    [ "$PREFIX" = 24 ] || return 1
    _base=$(printf '%s' "$WG_NET" | awk -F '[./]' '{ print $1 "." $2 "." $3 }')
    [ -n "$_base" ] || return 1
    _host=$(printf '%s' "$WG_ADDR" | awk -F '.' '{ print $4 }')
    _used=" $_host "
    if [ -f "$PEERS" ]; then
        while IFS="$(printf '\t')" read -r id name addr pub priv created; do
            [ -n "$addr" ] || continue
            _n=$(printf '%s' "$addr" | awk -F '.' '{ print $4 }')
            [ -n "$_n" ] && _used="$_used $_n "
        done < "$PEERS"
    fi
    _n=2
    while [ "$_n" -le 254 ]; do
        case "$_used" in
        *" $_n "*) ;;
        *)
            printf '%s.%s' "$_base" "$_n"
            return 0
            ;;
        esac
        _n=$((_n + 1))
    done
    return 1
}

# 把 peers.tsv 里的 peer 全部应用到 wg0（start 时调用）。
apply_peers() {
    [ -f "$PEERS" ] || return 0
    while IFS="$(printf '\t')" read -r id name addr pub priv created; do
        [ -n "$pub" ] && [ -n "$addr" ] || continue
        "$WG_TOOL" set "$WG_IF" peer "$pub" allowed-ips "$addr/32" 2>/dev/null ||
            log "应用 peer $name 失败（$WG_TOOL set $WG_IF peer …）"
    done < "$PEERS"
    return 0
}

# 单个 peer 即时应用/删除（wg0 不在时失败，下次 start 会补上）。
apply_peer() {   # $1=public key, $2=address
    "$WG_TOOL" set "$WG_IF" peer "$1" allowed-ips "$2/32" 2>/dev/null
}

remove_peer_from_kernel() {   # $1=public key
    "$WG_TOOL" set "$WG_IF" peer "$1" remove 2>/dev/null
}

# 按 id 生成客户端 WireGuard 配置；输出到 stdout。
peer_config() {
    peer_find "$1" || {
        fail "找不到 peer $1"
        return 1
    }
    [ -n "$P_PRIV" ] || {
        fail "peer $P_NAME 没有客户端私钥，无法生成配置"
        return 1
    }
    [ -n "$WG_ENDPOINT" ] || {
        fail "先在设置页填写“客户端连接地址”"
        return 1
    }
    _server_pub=$(cat "$SERVER_PUB" 2>/dev/null) || _server_pub=""
    [ -n "$_server_pub" ] || {
        fail "服务端公钥还没生成"
        return 1
    }
    _endpoint=$WG_ENDPOINT
    case "$_endpoint" in
    *:*:*) ;; # IPv6 字面量（应写成 [addr]:port）
    *:*) ;;   # 已经带了端口
    *) _endpoint="$_endpoint:$WG_PORT" ;;
    esac
    printf '[Interface]\n'
    printf 'PrivateKey = %s\n' "$P_PRIV"
    printf 'Address = %s/32\n' "$P_ADDR"
    printf 'MTU = %s\n' "$WG_MTU"
    if [ -n "$WG_DNS" ]; then
        printf 'DNS = %s\n' "$WG_DNS"
    fi
    printf '\n[Peer]\n'
    printf 'PublicKey = %s\n' "$_server_pub"
    printf 'Endpoint = %s\n' "$_endpoint"
    printf 'AllowedIPs = %s\n' "$WG_ALLOWED_IPS"
    printf 'PersistentKeepalive = %s\n' "$WG_KEEPALIVE"
    return 0
}
