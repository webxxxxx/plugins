#!/bin/sh
# wg-lan-fix.sh — WireGuard 客户端访问路由器 LAN 网段的自愈循环
#
# 由 wireguardd.sh 看守并以子进程运行；设置来自管理页面写出的 data/settings。
#
# 厂商固件的三个坑：
#   1) CPU 产生的帧经网桥转发（wg0 -> br0）会被数据通路丢弃，
#      必须从"实际的 LAN 口"直接发；
#   2) br0 的 rp_filter=1 会把「内网 -> wg0」的 TCP 回包丢掉；
#   3) LAN 口会换（eth1/eth2/eth3），厂商网络服务重连后还会把 rp_filter、
#      策略路由刷回原样 —— 所以每 30 秒自愈一次，不能只在开机时做一次。
set -u

HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
. "$HERE/../lib/wireguard-lib.sh"

load_settings || exit 1

port_of_if() {
    cat "/sys/class/net/$1/brport/port_no" 2>/dev/null
}

lan_nets() {
    # 不能用 `ip route show dev X`：厂商内核的 fib trie 遍历（inet_dump_fib ->
    # leaf_walk_rcu）在带硬件加速路由时会解引用野指针，直接 kernel panic
    # （Oops 96000005, Comm: ip）。改用 RTM_GETADDR（ip -4 addr show），
    # 它走 ifaddr 的 seq_file，不碰 fib trie。
    # busybox awk 没有位运算，用 mask = 2^(32-pfx)-1 的方式算网络地址。
    ip -4 addr show dev "$WG_LAN_IF" 2>/dev/null | awk '
        /inet / {
            cidr = ""
            for (i = 1; i <= NF; i++) if ($i == "inet") { cidr = $(i + 1); break }
            if (cidr == "") next
            n = split(cidr, a, "/")
            if (n != 2) next
            pfx = a[2] + 0
            if (pfx < 0 || pfx > 32) next
            m = split(a[1], o, ".")
            if (m != 4) next
            ipv = ((o[1] * 256 + o[2]) * 256 + o[3]) * 256 + o[4]
            size = 1
            for (i = 0; i < 32 - pfx; i++) size = size * 2
            net = ipv - (ipv % size)
            o1 = int(net / 16777216) % 256
            o2 = int(net / 65536) % 256
            o3 = int(net / 256) % 256
            o4 = net % 256
            printf "%d.%d.%d.%d/%d\n", o1, o2, o3, o4, pfx
        }'
}

peer_addrs() {
    "$WG_TOOL" show "$WG_IF" allowed-ips 2>/dev/null | awk '
        {
            for (i = 2; i <= NF; i++) {
                n = split($i, a, ",")
                for (j = 1; j <= n; j++)
                    if (a[j] ~ /\/32$/) { sub(/\/32$/, "", a[j]); print a[j] }
            }
        }'
}

rules_for_table() {
    ip rule 2>/dev/null |
        awk -v t="$WG_TABLE" '$2=="from" && $4=="lookup" && $5==t { print $3 }'
}

detect_port() {
    # 1) 任取 br0 ARP 表里的一个 MAC（不按网段过滤），查它学在哪个端口
    for mac in $(awk -v ifc="$WG_LAN_IF" '$6==ifc && $4!="00:00:00:00:00:00" { print $4 }' /proc/net/arp 2>/dev/null); do
        pno=$(brctl showmacs "$WG_LAN_IF" 2>/dev/null |
            awk -v m="$mac" 'tolower($2)==tolower(m) { print $1; exit }')
        [ -n "$pno" ] || continue
        for d in /sys/class/net/$WG_LAN_IF/brif/*; do
            i=${d##*/}
            case "$i" in eth*) ;; *) continue ;; esac
            n=$(port_of_if "$i")
            [ -n "$n" ] || continue
            if [ $((n)) -eq "$pno" ] 2>/dev/null; then echo "$i"; return; fi
        done
    done
    # 2) 回退：第一个有 carrier 的有线桥端口
    for d in /sys/class/net/$WG_LAN_IF/brif/*; do
        i=${d##*/}
        case "$i" in eth*) ;; *) continue ;; esac
        [ "$(cat "/sys/class/net/$i/carrier" 2>/dev/null)" = "1" ] || continue
        echo "$i"
        return
    done
}

trap 'exit 0' TERM INT
log "自愈循环启动（lan=$WG_LAN_IF table=$WG_TABLE）"
LAST=""
while true; do
    # 隧道被删掉（或内核模块被卸载）时退出；wireguardd 的看守会在 wg0 回来后重启。
    if ! ip link show "$WG_IF" >/dev/null 2>&1; then
        log "$WG_IF 不见了，退出等待宿主重启"
        exit 1
    fi

    PORT=$(detect_port)
    if [ -z "$PORT" ]; then
        sleep 30 & wait $!
        continue
    fi
    [ "$PORT" = "$LAST" ] || {
        log "LAN 口 -> $PORT"
        LAST=$PORT
    }

    # 1) rp_filter：厂商网络服务会刷回 1
    for f in all "$WG_LAN_IF" "$PORT"; do
        v=$(cat "/proc/sys/net/ipv4/conf/$f/rp_filter" 2>/dev/null)
        [ "$v" = "0" ] || echo 0 > "/proc/sys/net/ipv4/conf/$f/rp_filter" 2>/dev/null
    done

    # 2) 策略路由：每个 peer -> LAN 从实际端口直发
    PEERS=$(peer_addrs)
    if [ -n "$PEERS" ]; then
        for p in $PEERS; do
            ip rule | grep -q "from $p lookup $WG_TABLE" ||
                ip rule add from "$p" lookup "$WG_TABLE" pref 0 2>/dev/null
        done
        for old in $(rules_for_table); do
            echo " $PEERS " | grep -q " $old " ||
                ip rule del from "$old" lookup "$WG_TABLE" 2>/dev/null
        done
    fi
    for net in $(lan_nets); do
        ip route replace "$net" dev "$PORT" table "$WG_TABLE" 2>/dev/null
    done

    # 3) ARP 同步：网桥端口自身学不到 MAC，从 br0 的表搬（不按网段过滤）
    awk -v ifc="$WG_LAN_IF" '$6==ifc && $4!="00:00:00:00:00:00" { print $1, $4 }' /proc/net/arp 2>/dev/null |
        while read ip mac; do
            ip neigh replace "$ip" lladdr "$mac" dev "$PORT" nud permanent 2>/dev/null
        done

    # 4) 解析不到的地址，回 br0 上 arping 补表（也是端口选错时的自纠正来源）
    ip neigh show dev "$PORT" 2>/dev/null | while read ip rest; do
        case "$rest" in
        *INCOMPLETE* | *FAILED*) arping -c 1 -w 1 -I "$WG_LAN_IF" "$ip" >/dev/null 2>&1 ;;
        esac
    done

    sleep 30 & wait $!
done
