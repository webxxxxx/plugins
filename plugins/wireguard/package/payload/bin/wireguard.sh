#!/bin/sh
# wireguard: WireGuard 隧道（生命周期入口）
#
# 宿主监督的常驻进程是 wireguardd.sh（JSON-RPC + 看守循环），这里负责：
#   prepare         -> 生命周期 start：等 LAN、解析 LAN 口、生成密钥、迁移旧配置；
#                      必须退出，幂等，不建接口
#   up / start      -> 由 wireguardd.sh 调起：建/配 wg0（内核模块或 gotatun）、
#                      装规则、应用 peers
#   down / stop     -> 生命周期 stop 或重建：撤规则、清路由、停 gotatun、删 wg0
#   status          -> 一行 JSON，供 svc status 的 probe 显示
#
# 后端由包里的文件决定：带 wireguard.ko 的包走内核模块（sr6110），带 gotatun
# 的包走用户态实现。设置来自 <插件目录>/data/settings，首次启动写入默认值。
set -u

HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
. "$HERE/../lib/wireguard-lib.sh"

load_settings || exit 1

[ -x "$WG_TOOL" ] || {
    fail "找不到包内的 payload/bin/wg-tool"
    exit 1
}

# 等 LAN 口拿到 IPv4：网络服务在开机时可能晚几秒就绪。最多 20 秒。
wait_lan() {
    n=0
    while [ "$n" -lt 10 ]; do
        ip -4 addr show "$WG_LAN_IF" 2>/dev/null | grep -q 'inet ' && return 0
        sleep 2
        n=$((n + 1))
    done
    return 1
}

# WG_LAN_IF=auto 时要等“网桥”起来，不能一上来就退到默认路由口 —— 否则
# 开机早期会把 WAN 口当成 LAN 锁死。
lan_candidate_up() {
    for _cand in br0 br-lan; do
        has_ipv4 "$_cand" && return 0
    done
    for _cand in $(ls /sys/class/net 2>/dev/null); do
        is_lan_bridge "$_cand" || continue
        has_ipv4 "$_cand" && return 0
    done
    return 1
}

wait_lan_auto() {
    n=0
    while [ "$n" -lt 10 ]; do
        lan_candidate_up && return 0
        sleep 2
        n=$((n + 1))
    done
    return 1
}

add() {
    tbl=$1
    chain=$2
    shift 2
    iptables -t "$tbl" -C "$chain" "$@" 2>/dev/null && return 0
    iptables -t "$tbl" -I "$chain" 1 "$@" 2>/dev/null ||
        log "iptables -t $tbl -I $chain $* failed"
}

del() {
    tbl=$1
    chain=$2
    shift 2
    iptables -t "$tbl" -D "$chain" "$@" 2>/dev/null || true
}

add6() {
    ip6tables -C "$@" 2>/dev/null && return 0
    ip6tables -I "$@" 2>/dev/null || log "ip6tables -I $* failed"
}

del6() {
    ip6tables -D "$@" 2>/dev/null || true
}

rules_add() {
    add filter INPUT -i "$WG_IF" -j ACCEPT
    add filter INPUT -p udp --dport "$WG_PORT" -j ACCEPT
    add filter FORWARD -i "$WG_IF" -o "$WG_LAN_IF" -j ACCEPT
    add filter FORWARD -i "$WG_LAN_IF" -o "$WG_IF" -j ACCEPT
    add filter FORWARD -i "$WG_IF" -j ACCEPT
    add filter FORWARD -o "$WG_IF" -j ACCEPT
    add nat POSTROUTING -s "$WG_NET" -o "$WG_LAN_IF" -j MASQUERADE
    add6 INPUT -p udp --dport "$WG_PORT" -j ACCEPT
    add6 INPUT -p ipv6-icmp -j ACCEPT
}

rules_del() {
    del filter INPUT -i "$WG_IF" -j ACCEPT
    del filter INPUT -p udp --dport "$WG_PORT" -j ACCEPT
    del filter FORWARD -i "$WG_IF" -o "$WG_LAN_IF" -j ACCEPT
    del filter FORWARD -i "$WG_LAN_IF" -o "$WG_IF" -j ACCEPT
    del filter FORWARD -i "$WG_IF" -j ACCEPT
    del filter FORWARD -o "$WG_IF" -j ACCEPT
    del nat POSTROUTING -s "$WG_NET" -o "$WG_LAN_IF" -j MASQUERADE
    del6 INPUT -p udp --dport "$WG_PORT" -j ACCEPT
    del6 INPUT -p ipv6-icmp -j ACCEPT
}

rules_for_table() {
    ip rule 2>/dev/null |
        awk -v t="$WG_TABLE" '$2=="from" && $4=="lookup" && $5==t { print $3 }'
}

# 生命周期 start：只做准备，不建接口。
do_prepare() {
    ensure_keys || return 1
    migrate_legacy_peer

    if [ "${WG_SKIP_WAIT:-0}" = 1 ]; then
        log "跳过 LAN 等待（管理页面保存时重建）"
    else
        case "$WG_LAN_IF" in
        '' | auto) wait_lan_auto || log "等待 LAN 网桥的 IPv4 超时（20 秒），继续启动" ;;
        *) wait_lan || log "等待 $WG_LAN_IF 的 IPv4 超时（20 秒），继续启动" ;;
        esac
    fi
    resolve_lan_if
    return 0
}

# 内核后端：insmod 后由 netlink 建 wg0。
kernel_create() {
    if [ ! -d /sys/module/wireguard ]; then
        [ -f "$KO" ] || {
            fail "找不到包内的 payload/bin/wireguard.ko"
            return 1
        }
        insmod "$KO" 2>/dev/null || true
    fi
    if [ ! -d /sys/module/wireguard ]; then
        fail "wireguard 模块加载失败（$KO）"
        return 1
    fi
    if ! ip link show "$WG_IF" >/dev/null 2>&1; then
        if ! ip link add "$WG_IF" type wireguard; then
            fail "创建 $WG_IF 失败"
            return 1
        fi
    fi
    return 0
}

do_up() {
    do_prepare || return 1

    case "$(wg_impl)" in
    kernel)
        kernel_create || return 1
        ;;
    userspace)
        start_gotatun || return 1
        ;;
    *)
        fail "既没有 wireguard.ko 也没有 gotatun，无法启动隧道"
        return 1
        ;;
    esac

    # 厂商内核没有 max_mtu，必须在 up 之前手动设；gotatun 的 TUN 同理。
    if ! ip link set "$WG_IF" mtu "$WG_MTU"; then
        fail "设置 $WG_IF mtu 失败"
        return 1
    fi
    if ! "$WG_TOOL" set "$WG_IF" private-key "$SERVER_KEY" listen-port "$WG_PORT"; then
        fail "wg set（私钥/端口）失败"
        return 1
    fi
    ip addr add "$WG_ADDR/$PREFIX" dev "$WG_IF" 2>/dev/null || true
    if ! ip link set "$WG_IF" up; then
        fail "$WG_IF up 失败"
        return 1
    fi

    apply_peers

    # 厂商固件会把它刷回 0：内核后端的自愈每 30 秒再保证一次（用户态隧道没这个坑）。
    echo 1 > /proc/sys/net/ipv4/ip_forward 2>/dev/null || true

    rules_add
    log "启动完成：$WG_IF $WG_ADDR/$PREFIX 端口 $WG_PORT，后端 $(wg_impl)，lan=$WG_LAN_IF"
    return 0
}

do_down() {
    log "停止：撤规则与 $WG_IF（后端 $(wg_impl)）"

    rules_del

    # up 与 LAN 自愈（内核后端）加的 ip rule / 策略路由
    for old in $(rules_for_table); do
        ip rule del from "$old" lookup "$WG_TABLE" 2>/dev/null || true
    done
    ip route show table "$WG_TABLE" 2>/dev/null | while read -r route; do
        ip route del $route table "$WG_TABLE" 2>/dev/null || true
    done

    # 用户态后端：进程没了 TUN 自动消失；内核后端要显式删接口。
    stop_gotatun
    ip link set "$WG_IF" down 2>/dev/null || true
    ip link del "$WG_IF" 2>/dev/null || true

    # 看守（被监督进程）这时已经被宿主结束了，它留下的 pidfile 只删不发信号：
    # 强杀之后那个 pid 可能已经被复用给别的进程。
    rm -f "$GUARDIAN_PID"

    log "已停止"
    return 0
}

do_status() {
    _impl=$(wg_impl)
    if ! ip link show "$WG_IF" >/dev/null 2>&1; then
        printf '{"running":false,"interface":"%s","impl":"%s","reason":"no interface"}\n' \
            "$WG_IF" "$_impl"
        return 0
    fi
    if ! ip link show "$WG_IF" 2>/dev/null | grep -q 'UP'; then
        printf '{"running":false,"interface":"%s","impl":"%s","reason":"interface down"}\n' \
            "$WG_IF" "$_impl"
        return 0
    fi
    peers=$("$WG_TOOL" show "$WG_IF" allowed-ips 2>/dev/null | wc -l | tr -d ' ')
    printf '{"running":true,"interface":"%s","impl":"%s","listen_port":%s,"peers":%s,"lan":"%s"}\n' \
        "$WG_IF" "$_impl" "$WG_PORT" "$peers" "$WG_LAN_IF"
}

case "${1:-}" in
prepare) do_prepare ;;
up | start) do_up ;;
down | stop) do_down ;;
status) do_status ;;
*)
    echo "usage: $0 prepare|up|down|status" >&2
    exit 2
    ;;
esac
