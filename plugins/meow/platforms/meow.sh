#!/bin/sh
# meow：包装 vendor/meow-rs 编译出来的二进制。
#
# 两个文件，各管各的：
#   <插件目录>/config   meow 自己的配置（订阅、代理组、规则），meow 就是用这份启动的；
#                       宿主的 editconfig 改的也是它。我们只在启动时改受管的几个键
#                       （mixed-port / tun / dns / tproxy-port），其余原样保留。
#                       第一次安装从包里的 payload/config.yaml 生成。
#   <插件目录>/state    我们自己的：只缓存代理类型（proxy-mode），页面上的选择器写它。
#   <插件目录>/data     运行数据：meow 的 geo 库、pid、`-d` 指过来的缓存。
#                       宿主升级时保留它；升级前的老位置在 <data>/meow，启动时搬进来。
#
# 运行形态由 state 里的 proxy-mode 决定（缺省 off），受管键按形态自动写进 meow 的 config。
# 缺省值不能是 pc：刚装好既没有订阅、也没有可用的出口，这时要是在 tun / tproxy 形态下
# 起来，整机流量就被导进一个转不出去的内核，表现就是断网。页面上选了 tun / tproxy 才
# 接管流量，选择记在 state 里，升级和重启都还认。
# 三种形态：
#   off    不接管流量：只开 mixed-port 当普通 HTTP/SOCKS 代理，tproxy / tun 都不开。
#   pc     本机透明代理。Linux：tproxy-port + routing-mark（meow 自己装 nft 规则）；
#          Windows：tun + fake-ip（见 meow.ps1）。页面上的「tun」就是它。
#   router 网关：iptables 规则由 meow-fw.sh 成对管理，meow 只在 0.0.0.0:<tproxy-port>
#          上开 tproxy 入口（需要 root + iptables；没有 nf_tables 的内核也能用）。
#          页面上的「tproxy」就是它。
#
# 宿主监督的是 exec（exec.args = start）：start 一直不返回，直到看护进程退出；
# stop 在进程退出之后跑，只做收尾（撤规则、删 pidfile）；status 按需探测。
# 路径全部从脚本位置推导，数据落在 <插件目录>/data。
# 清单里 exec.ready 是 notify：宿主先发一条 svcd.ping，收到回复才算启动成功，之后
# 这条路就是能力通道（ready: exec 的 stdout 只进日志，页面调不到任何能力）。
#
# start 现在是「看护进程」而不是把 meow exec 掉：meow 是自己的子进程，通道能力跑在
# 后台（页面 A 的代理类型选择器调它），孩子退出后前台循环按 config 里的新形态重启。
# 这样页面改 proxy-mode 不需要额外进程，也不必重启插件。
set -u

HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
ROOT=$(CDPATH= cd -- "$HERE/.." && pwd)          # payload/：yacd、cn_domains 在这一层
# 宿主给插件的三个目录之一是 SVCD_PLUGIN_DIR（安装目录）。手跑时没有它，就按脚本
# 自己的位置往上三层推（<插件>/<版本>/payload/bin/meow.sh）。
PLUGIN_ROOT=$(CDPATH= cd -- "${SVCD_PLUGIN_DIR:-$HERE/../../..}" && pwd)
# 插件的数据只能落在插件目录里：<插件目录>/data。宿主升级、卸载（不带 --purge）都
# 保留它；meow 的 geo 库、pid、`-d` 指过来的缓存全在这里。
DATA="$PLUGIN_ROOT/data"
PIDFILE="$DATA/meow.pid"
CONFIG="$PLUGIN_ROOT/config"                     # meow 的配置：meow 就是用它启动的（宿主的 editconfig 改它）
LEGACY_CONFIG="$PLUGIN_ROOT/meow.yaml"           # 1.2/1.3 的布局：meow 的配置曾放这里
TEMPLATE="$ROOT/config.yaml"                     # 包里的出厂模板
STATE="$PLUGIN_ROOT/state"                       # 我们自己的：只缓存代理类型（+ router 的可选覆盖）
FW="$HERE/meow-fw.sh"
SDK="$ROOT/lib/svcd-plugin.sh"                   # 宿主 SDK：通道循环与能力分发

# 包里带了一份精简的 Country.mmdb（130 KB）。数据目录里还没有就放进去：很多路由器
# 直连不通 GitHub，meow 自己下 7 MB 的库会失败，而 GEOIP 规则没有库整份配置都起不来。
# 已经有了就不动——那可能是 meow 自己更新过的。
seed_geodata() {
    [ -f "$ROOT/Country.mmdb" ] || return 0
    [ -f "$DATA/Country.mmdb" ] && return 0
    _seed="$DATA/Country.mmdb.tmp.$$"
    if cp "$ROOT/Country.mmdb" "$_seed" 2>/dev/null && mv "$_seed" "$DATA/Country.mmdb" 2>/dev/null; then
        echo "meow: 已把包里的 Country.mmdb 放进 $DATA" >&2
    else
        rm -f "$_seed"
    fi
    return 0
}

# 1.2/1.3 把数据放在插件目录之外，两种老位置：默认根布局下是 <data>/meow（插件目录的
# 上上级同名目录），手跑时是 <插件目录>/.data/meow。第一次启动搬进来：geo 库跟着走。
migrate_legacy() {
    [ -d "$DATA" ] && return 0
    for old in "$PLUGIN_ROOT/../../meow" "$PLUGIN_ROOT/.data/meow"; do
        [ -d "$old" ] || continue
        if mv "$old" "$DATA" 2>/dev/null; then
            echo "meow: 旧数据 $old 已搬到 $DATA" >&2
        fi
        return 0
    done
    return 0
}

migrate_legacy
mkdir -p "$DATA" 2>/dev/null || true
seed_geodata

# 能力通道（页面 A 的代理类型选择器）用 SDK；sync 会把 sdk/shell 的副本刷到 payload/lib。
if [ -f "$SDK" ]; then . "$SDK"; fi

# ---- 我们的状态（只缓存代理类型） ----

# 顶层标量：只认行首的 `键: 值`，去掉行尾注释、引号和首尾空白。
our() {
    sed -n "s/^$1:[[:space:]]*\([^#]*\).*/\1/p" "$STATE" 2>/dev/null \
        | head -n 1 \
        | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//' -e 's/^"//' -e 's/"$//' -e "s/^'//" -e "s/'$//"
}

# 我们的设置里取一个键，缺省用第二个参数。
our_or() {
    _value=$(our "$1")
    if [ -z "$_value" ] || [ "$_value" = null ]; then
        printf '%s' "$2"
    else
        printf '%s' "$_value"
    fi
}

mode_of() {
    _mode=$(our proxy-mode)
    case "$_mode" in
        '') printf 'off' ;;
        off|pc|router) printf '%s' "$_mode" ;;
        *) echo "meow: 缓存里的 proxy-mode 只能是 off、pc 或 router（现在是 '$_mode'）" >&2; return 1 ;;
    esac
}

# 页面上的三个选项（代理类型）与实际形态的对应：tun ↔ pc，tproxy ↔ router。
label_of() {
    case "${1:-}" in
        pc) printf 'tun' ;;
        router) printf 'tproxy' ;;
        *) printf 'off' ;;
    esac
}

mode_of_label() {
    case "${1:-}" in
        off) printf 'off' ;;
        tun) printf 'pc' ;;
        tproxy) printf 'router' ;;
        *) return 1 ;;
    esac
}

# 网关形态（iptables + tproxy 入口）只有 Linux 有。
is_linux() {
    case "$(uname -s 2>/dev/null)" in
        Linux) return 0 ;;
    esac
    return 1
}

is_port() {
    case "${1:-}" in
        ''|*[!0-9]*) return 1 ;;
    esac
    [ "$1" -ge 1 ] && [ "$1" -le 65535 ]
}

# ---- meow 配置的小编辑 ----
# 只做三件事：改顶层标量、删顶层键、改块内的标量。注释、顺序和缩进都原样保留，
# 因为这份文件里还有订阅和规则，不能被重新序列化。

# set_key <文件> <键> <值>：有就替换那一行，没有就追加到末尾。
set_key() {
    _tmp="$1.tmp.$$"
    awk -v key="$2" -v val="$3" '
        BEGIN { done = 0 }
        $0 ~ "^" key ":[ \t]*" { print key ": " val; done = 1; next }
        { print }
        END { if (!done) print key ": " val }
    ' "$1" > "$_tmp" && mv "$_tmp" "$1"
}

# del_key <文件> <键>：删掉键本身和它下面的缩进行（块也跟着走）。
del_key() {
    _tmp="$1.tmp.$$"
    awk -v key="$2" '
        BEGIN { skip = 0 }
        /^[^ \t#]/ {
            skip = ($0 ~ ("^" key ":[ \t]*"))
            if (skip) next
            print; next
        }
        { if (!skip) print }
    ' "$1" > "$_tmp" && mv "$_tmp" "$1"
}

# set_block_key <文件> <块> <键> <值>：块里那一行替换，块里没有就补一行；
# 连块都没有就把整块追加到末尾。
set_block_key() {
    _tmp="$1.tmp.$$"
    awk -v block="$2" -v key="$3" -v val="$4" '
        function flush() {
            if (inblock && !done) { print "  " key ": " val; done = 1 }
        }
        BEGIN { inblock = 0; seen = 0; done = 0 }
        /^[^ \t#]/ {
            flush()
            inblock = ($0 ~ ("^" block ":[ \t]*$"))
            if (inblock) seen = 1
            print; next
        }
        {
            if (inblock) {
                if ($0 ~ ("^[ \t]+" key ":[ \t]*")) { print "  " key ": " val; done = 1; next }
            }
            print
        }
        END {
            flush()
            if (!seen) {
                print ""
                print block ":"
                print "  " key ": " val
            }
        }
    ' "$1" > "$_tmp" && mv "$_tmp" "$1"
}

# 我们的全部状态就这一份：代理类型。配置在 <插件目录>/config（meow 自己的那份）。
write_state() {
    {
        echo "# meow 插件的状态：只记代理类型，页面上的 off / tun / tproxy 就是它。"
        echo "# meow 的配置在 <插件目录>/config（订阅、代理组、规则、端口都在那里）。"
        echo "proxy-mode: ${1:-off}"
    } > "$STATE"
}

set_mode_cache() {
    if [ ! -f "$STATE" ]; then
        write_state "$1"
        return 0
    fi
    set_key "$STATE" proxy-mode "$1"
}

# 老布局里我们的键混在 meow 的配置里：搬进 state，再从配置里删掉。
migrate_state() {
    for _key in proxy-mode lan-interface dns-port; do
        _value=$(sed -n "s/^${_key}:[[:space:]]*//p" "$1" 2>/dev/null | head -n 1 |
            sed -e 's/[[:space:]]*#.*$//' -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//')
        [ -n "$_value" ] || continue
        if [ "$_key" = proxy-mode ]; then
            case "$_value" in
                off|pc|router) set_mode_cache "$_value" ;;
                *) echo "meow: 迁移时忽略未知的 proxy-mode '$_value'" >&2 ;;
            esac
        else
            if [ ! -f "$STATE" ]; then write_state "$(mode_of 2>/dev/null || printf off)"; fi
            set_key "$STATE" "$_key" "$_value"
        fi
    done
    return 0
}

# meow 不认识我们的键（proxy-mode 现在住在 state，另外两个是 router 的可选项）。
strip_our_keys() {
    del_key "$1" proxy-mode
    del_key "$1" lan-interface
    del_key "$1" dns-port
}

# 保证 <插件目录>/config 是 meow 的配置且干净：
#   1.2/1.3 把 meow 的配置放在 meow.yaml，搬回 config，旧开关搬进 state；
#   更老的布局把开关和订阅写在一起，把我们那几个键挪走。
prepare_config() {
    if [ -f "$LEGACY_CONFIG" ]; then
        [ -f "$CONFIG" ] && migrate_state "$CONFIG"
        mv "$LEGACY_CONFIG" "$CONFIG" || return 1
        echo "meow: 已把 meow.yaml 搬回 config（代理类型记在 $STATE）" >&2
    fi
    if [ ! -f "$CONFIG" ]; then
        [ -f "$TEMPLATE" ] || { echo 'meow: payload/config.yaml missing' >&2; return 1; }
        sed -e "s#@DATA@#$DATA#g" "$TEMPLATE" > "$CONFIG" || return 1
    fi
    # 老版本生成的配置里可能还留着字面量的 @DATA@（那时只在首次生成时替换一次）。
    # 留着它 meow 会拿 "@DATA@/Country.mmdb" 去开 GeoIP 库，加载失败后整份配置都起不
    # 来，页面上添加订阅的接口跟着报 400。每次启动补一遍，已经是真路径就什么都不做。
    if grep -q '@DATA@' "$CONFIG" 2>/dev/null; then
        _tmp="$CONFIG.tmp.$$"
        sed -e "s#@DATA@#$DATA#g" "$CONFIG" > "$_tmp" 2>/dev/null || { rm -f "$_tmp"; return 1; }
        mv "$_tmp" "$CONFIG" || { rm -f "$_tmp"; return 1; }
        echo "meow: 配置里的 @DATA@ 已替换成 $DATA" >&2
    fi
    # 更老的布局把我们的键和订阅写在一起：先搬进 state，再从配置里摘掉。
    migrate_state "$CONFIG"
    strip_our_keys "$CONFIG"
    if [ ! -f "$STATE" ]; then
        write_state "$(mode_of 2>/dev/null || printf off)"
    fi
    return 0
}

# router 形态要一个 tproxy 入口：有就沿用（端口以它为准），没有就补一条。
# 补的时候插在 listeners: 下面；连 listeners 块都没有就整块追加。
ensure_tproxy_listener() {
    _port=$1
    [ -n "$(tproxy_entry)" ] && return 0
    if grep -qE '^listeners:[ \t]*$' "$CONFIG"; then
        _tmp="$CONFIG.tmp.$$"
        awk -v port="$_port" '
            { print }
            /^listeners:[ \t]*$/ && !done {
                print "  - name: tproxy"
                print "    type: tproxy"
                print "    port: " port
                print "    listen: 0.0.0.0"
                print "    firewall: false"
                done = 1
            }
        ' "$CONFIG" > "$_tmp" && mv "$_tmp" "$CONFIG"
        return 0
    fi
    {
        echo ""
        echo "listeners:"
        echo "  - name: tproxy"
        echo "    type: tproxy"
        echo "    port: $_port"
        echo "    listen: 0.0.0.0"
        echo "    firewall: false"
    } >> "$CONFIG"
    return 0
}

# 按形态把受管键写进 meow 的配置。这是“启动即用”的实现：你只需要在页面上选代理类型，
# tun / fake-ip / tproxy-port / routing-mark 都由这里补齐。
# 端口以 meow 的配置为准（用户改成别的就跟着走），没有才用默认值。
yaml_number() {
    _value=$(sed -n "s/^$1:[[:space:]]*\([0-9][0-9]*\).*/\1/p" "$CONFIG" 2>/dev/null | head -n 1)
    printf '%s' "${_value:-$2}"
}

# 透明代理端口：顶层 tproxy-port，或者 listeners 里那条 tproxy 的端口。
tproxy_port() {
    _value=$(sed -n 's/^tproxy-port:[[:space:]]*\([0-9][0-9]*\).*/\1/p' "$CONFIG" 2>/dev/null | head -n 1)
    [ -n "$_value" ] || _value=$(tproxy_entry | awk '{print $1}')
    printf '%s' "${_value:-7893}"
}

apply_settings() {
    _mode=$1
    _port=$(yaml_number mixed-port 7890)
    is_port "$_port" || { echo "meow: mixed-port 不是端口：'$_port'" >&2; return 1; }
    set_key "$CONFIG" mixed-port "$_port"
    # 订阅里带进来的 tun 块要关掉：三种形态的透明代理都不靠订阅自带的 tun。
    if grep -qE '^tun:' "$CONFIG"; then
        set_block_key "$CONFIG" tun enable false
    fi

    case "$_mode" in
    off)
        # 不接管：只留 mixed-port。tproxy-port 一去掉，meow 就不装 nft 规则。
        del_key "$CONFIG" tproxy-port
        del_key "$CONFIG" routing-mark
        ;;
    pc)
        _tproxy=$(tproxy_port)
        is_port "$_tproxy" || { echo "meow: tproxy-port 不是端口：'$_tproxy'" >&2; return 1; }
        set_key "$CONFIG" tproxy-port "$_tproxy"
        _mark=$(yaml_number routing-mark 9527)
        case "$_mark" in
            *[!0-9]*) echo "meow: routing-mark 只能是数字：'$_mark'" >&2; return 1 ;;
        esac
        set_key "$CONFIG" routing-mark "$_mark"
        ;;
    router)
        # 入口是 listeners 里的 tproxy 项，顶层 tproxy-port 会和它抢端口。
        del_key "$CONFIG" tproxy-port
        del_key "$CONFIG" routing-mark
        set_block_key "$CONFIG" dns enable true
        _dns=$(our_or dns-port 1053)
        is_port "$_dns" || { echo "meow: dns-port 不是端口：'$_dns'" >&2; return 1; }
        set_block_key "$CONFIG" dns listen "0.0.0.0:$_dns"
        _tproxy=$(tproxy_port)
        is_port "$_tproxy" || { echo "meow: tproxy-port 不是端口：'$_tproxy'" >&2; return 1; }
        ensure_tproxy_listener "$_tproxy"
        ;;
    *)
        echo "meow: 未知形态 '$_mode'" >&2
        return 1
        ;;
    esac
    return 0
}

# ---- router 形态 ----

# listeners: 里第一个 type: tproxy 的条目 → "端口 firewall listen"。
# 只支持块写法（- name: … 换行 port: …），一行流式 map 不认。
tproxy_entry() {
    awk '
    function clean(s) { sub(/[ \t]*#.*$/, "", s); gsub(/^[ \t]+|[ \t]+$/, "", s); gsub(/"/, "", s); return s }
    function value(line, key,   s) {
        s = line
        sub(/^[ \t]*/, "", s)
        sub(/^-[ \t]*/, "", s)
        if (index(s, key ":") != 1) return ""
        sub("^" key ":[ \t]*", "", s)
        return clean(s)
    }
    BEGIN { inblock = 0; done = 0; type = ""; port = ""; fw = ""; listen = "" }
    /^[^ \t#]/ { inblock = ($0 ~ /^listeners:[ \t]*$/); next }
    !inblock { next }
    /^[ \t]*-[ \t]*/ {
        if (type == "tproxy") { print port, fw, listen; done = 1; exit }
        type = ""; port = ""; fw = ""; listen = ""
    }
    { v = value($0, "type");     if (v != "") type = v }
    { v = value($0, "port");     if (v != "") port = v }
    { v = value($0, "firewall"); if (v != "") fw = v }
    { v = value($0, "listen");   if (v != "") listen = v }
    END { if (!done && type == "tproxy") print port, fw, listen }
    ' "$CONFIG"
}

# dns.listen → "地址 端口"；没有或解析不出就什么都不输出。
dns_listen() {
    awk '
    function clean(s) { sub(/[ \t]*#.*$/, "", s); gsub(/^[ \t]+|[ \t]+$/, "", s); gsub(/"/, "", s); return s }
    BEGIN { inblock = 0 }
    /^[^ \t#]/ { inblock = ($0 ~ /^dns:[ \t]*$/); next }
    !inblock { next }
    /^[ \t]+listen:[ \t]*/ {
        v = $0
        sub(/^[ \t]+listen:[ \t]*/, "", v)
        v = clean(v)
        if (v == "") next
        i = match(v, /:[^:]*$/)
        if (i == 0) next
        addr = substr(v, 1, i - 1)
        port = substr(v, i + 1)
        gsub(/^\[|\]$/, "", addr)
        print addr, port
        exit
    }' "$CONFIG"
}

# dns.enable → true/false；没有就什么都不输出（meow 默认不开 DNS）。
dns_enable() {
    awk '
    BEGIN { inblock = 0 }
    /^[^ \t#]/ { inblock = ($0 ~ /^dns:[ \t]*$/); next }
    !inblock { next }
    /^[ \t]+enable:[ \t]*/ {
        v = $0
        sub(/^[ \t]+enable:[ \t]*/, "", v)
        sub(/[ \t]*#.*$/, "", v)
        gsub(/^[ \t]+|[ \t]+$/, "", v)
        gsub(/"/, "", v)
        print v
        exit
    }' "$CONFIG"
}

load_router_params() {
    LAN_IF=$(our_or lan-interface br0)
    set -- $(tproxy_entry)
    TPROXY_PORT=${1:-}
    TPROXY_FIREWALL=${2:-}
    TPROXY_LISTEN=${3:-}
    set -- $(dns_listen)
    DNS_ADDR=${1:-}
    DNS_PORT=${2:-}
    DNS_ENABLE=$(dns_enable)
    MARK=$(our routing-mark)
    case "$MARK" in null) MARK='' ;; esac
}

router_hint() {
    echo "meow: proxy-mode: router 需要一份网关配置，当前配置不满足：" >&2
    echo "  $*" >&2
    echo "  参考 README「网关形态（router）」里的配置片段。" >&2
}

validate_router() {
    if ! is_port "${TPROXY_PORT:-}"; then
        router_hint "config 的 listeners: 里没有 type: tproxy 的入口（内置入口时要写 port）"
        return 1
    fi
    if [ "$TPROXY_FIREWALL" != false ]; then
        router_hint "tproxy 入口必须写 firewall: false —— 路由器内核没有 nf_tables，meow 内置防火墙起不来"
        return 1
    fi
    case "$TPROXY_LISTEN" in
        *:*) router_hint "tproxy 入口的 listen 是 IPv6（$TPROXY_LISTEN）；网关规则只做 v4 REDIRECT，写 0.0.0.0"; return 1 ;;
        127.*|::1|localhost)
            router_hint "tproxy 入口的 listen 是 '$TPROXY_LISTEN'，LAN 的 REDIRECT 到不了；写 0.0.0.0"
            return 1 ;;
    esac
    case "$DNS_ENABLE" in
        true) ;;
        *) echo "meow: dns.enable 不是 true，跳过 DNS 劫持（LAN 客户端的 DNS 直连）" >&2; DNS_PORT='' ;;
    esac
    if is_port "${DNS_PORT:-}"; then
        case "$DNS_ADDR" in
            127.*|::1|localhost)
                router_hint "dns.listen 绑在 '$DNS_ADDR'，LAN 的 DNS 劫持会打到没人监听的地址；改成 0.0.0.0:$DNS_PORT"
                return 1 ;;
            *:*)
                router_hint "dns.listen 是 IPv6（$DNS_ADDR）；LAN 的 v4 DNS 劫持到不了，改成 0.0.0.0:$DNS_PORT"
                return 1 ;;
        esac
    else
        echo "meow: 没有可用的 dns.listen，跳过 DNS 劫持（只劫持 TCP）" >&2
        DNS_PORT=''
    fi
    if [ -z "$MARK" ]; then
        echo "meow: config 里没有 routing-mark，跳过 SO_MARK 放行规则" >&2
    else
        case "$MARK" in
            *[!0-9]*) router_hint "routing-mark 不是数字：'$MARK'"; return 1 ;;
        esac
    fi
    case "$(sed -n 's/^ipv6:[[:space:]]*\([a-z]*\).*/\1/p' "$CONFIG" | head -n 1)" in
        true) echo "meow: ipv6: true 而规则只做 v4 REDIRECT，LAN 客户端的 AAAA 会绕过代理；网关形态建议 ipv6: false" >&2 ;;
    esac
    [ -e "/sys/class/net/$LAN_IF" ] || { router_hint "LAN 网口 '$LAN_IF' 不存在（改 config 里的 lan-interface）"; return 1; }
    return 0
}

# 规则参数打包成 meow-fw.sh 的参数（函数内的 positional 参数不影响调用方）。
# up / down / status 都从这一处拼参数，顺序与 --dns / --mark 的取舍只写一次。
fw_call() {
    set -- "$1" --lan "$LAN_IF"
    if [ -n "${TPROXY_PORT:-}" ]; then set -- "$@" --tproxy "$TPROXY_PORT"; fi
    if [ -n "${DNS_PORT:-}" ]; then set -- "$@" --dns "$DNS_PORT"; fi
    if [ -n "${MARK:-}" ]; then set -- "$@" --mark "$MARK"; fi
    sh "$FW" "$@"
}

# 拆规则不看端口：meow-fw.sh 只按 --lan 找自建链，改过端口也能清干净。
fw_clear() {
    [ -f "$FW" ] || return 0
    # 不看 proxy-mode、也不看当前端口：只按 --lan 找自建链，切形态后也能清干净。
    sh "$FW" down --lan "$(our_or lan-interface br0)" >/dev/null 2>&1
    return 0
}

# pidfile 里的 pid 是不是我们的 meow。宿主负责终止：强杀（SIGKILL）或断电之后 pidfile
# 会留下来，而那个 pid 很可能已经被系统复用给别的进程——只看 kill -0 会把它当成自己
# （status 谎报 running、切代理类型时还会杀错人）。
running() {
    _pid=$(cat "$PIDFILE" 2>/dev/null) || return 1
    [ -n "$_pid" ] || return 1
    kill -0 "$_pid" 2>/dev/null || return 1
    # 有 /proc 就按命令行认（NUL 分隔，grep -F 直接当字节串搜）。
    if [ -r "/proc/$_pid/cmdline" ]; then
        grep -qF -- "$HERE/meow" "/proc/$_pid/cmdline" 2>/dev/null
        return
    fi
    # macOS 没有 /proc：退回 ps。ps 也没有就只能信 kill -0，和以前一样。
    _argv=$(ps -o args= -p "$_pid" 2>/dev/null) || return 0
    case "$_argv" in
    *"$HERE/meow"*) return 0 ;;
    esac
    return 1
}

# /proc/net/tcp 里本地端口是十六进制，状态 0A = LISTEN（busybox 不一定有 netstat）。
# 网关形态要求 listener 绑 0.0.0.0，所以只看 v4 表。
port_listening() {
    _hex=$(printf '%04X' "$1" 2>/dev/null) || return 1
    grep -qE "^ *[0-9]+: [0-9A-F]+:$_hex [0-9A-F]+:[0-9A-F]+ 0A" /proc/net/tcp 2>/dev/null
}

report_ports() {
    for key in mixed-port port socks-port; do
        value=$(sed -n "s/^${key}:[[:space:]]*\([0-9][0-9]*\).*/\1/p" "$CONFIG" | head -n 1)
        [ -n "$value" ] && printf ',"%s":%s' "$key" "$value"
    done
    value=$(sed -n 's/^tproxy-port:[[:space:]]*\([0-9][0-9]*\).*/\1/p' "$CONFIG" | head -n 1)
    if [ -z "$value" ]; then
        value=$(tproxy_entry | awk '{print $1}')
    fi
    [ -n "$value" ] && printf ',"tproxy-port":%s' "$value"
    return 0
}

firewall_state() {
    [ -f "$FW" ] || { printf 'unknown'; return 0; }
    if sh "$FW" check --lan "$(our_or lan-interface br0)" >/dev/null 2>&1; then
        printf 'up'
    else
        printf 'down'
    fi
}

# 起一个 meow 子进程，pidfile 记下它。router 形态还要等 tproxy 端口起来再挂规则
# （规则先挂、进程没起，客户端会拿到 connection refused）。
start_child() {
    _mode=$1
    if [ "$_mode" = router ] && [ ! -f "$FW" ]; then
        echo "meow: 缺少规则脚本 $FW" >&2
        return 1
    fi
    if [ "$_mode" = off ]; then
        echo "meow: 代理类型是 off —— 只开 mixed-port，不接管系统流量（页面上选 tun / tproxy 才接管）" >&2
    fi
    echo "meow starting mode=$_mode (config=$CONFIG)" >&2
    # stdin 接 /dev/null：宿主的通道口只留在看护进程里，meow 不该继承它。
    # -d 是 meow 的家目录：缓存、规则提供者的缓存都落在这里，所以指到插件自己的
    # data/，不要指 payload（那是版本目录，升级时会被换掉）。
    "$HERE/meow" -d "$DATA" -f "$CONFIG" --ext-ui "$ROOT/yacd" >&2 < /dev/null &
    mon_pid=$!
    echo "$mon_pid" > "$PIDFILE"
    [ "$_mode" = router ] || return 0
    # 厂商网络服务可能在别处把 ip_forward 刷回 0（meow-fw.sh up 里也会再设一次）。
    echo 1 > /proc/sys/net/ipv4/ip_forward 2>/dev/null \
        || echo "meow: 无法打开 ip_forward" >&2

    # 端口就绪之后再挂规则。
    n=0
    while [ "$n" -lt 60 ]; do
        port_listening "$TPROXY_PORT" && break
        if ! kill -0 "$mon_pid" 2>/dev/null; then
            echo "meow: 进程在监听前退出（配置或端口有问题）" >&2
            rm -f "$PIDFILE"
            return 1
        fi
        sleep 1
        n=$((n + 1))
    done
    if [ "$n" -ge 60 ]; then
        echo "meow: 60s 内 :$TPROXY_PORT 没有监听" >&2
        kill "$mon_pid" 2>/dev/null
        wait "$mon_pid" 2>/dev/null
        rm -f "$PIDFILE"
        return 1
    fi

    if ! fw_call up; then
        echo "meow: 装 nat 规则失败，停止 meow（不留半套规则）" >&2
        fw_clear
        kill "$mon_pid" 2>/dev/null
        wait "$mon_pid" 2>/dev/null
        rm -f "$PIDFILE"
        return 1
    fi

    echo "meow router ready: lan=$LAN_IF tproxy=:$TPROXY_PORT dns=:${DNS_PORT:-off}" >&2
    return 0
}

# 收掉子进程和它留下的规则。幂等：宿主跑 lifecycle stop 时这里可能已经收过一遍。
cleanup_child() {
    [ -n "${mon_pid:-}" ] || { rm -f "$PIDFILE"; return 0; }
    kill "$mon_pid" 2>/dev/null
    rm -f "$PIDFILE"
    # 进程没了就别留规则：宁可直连，也别让 LAN 一直打到死端口。
    fw_clear
    mon_pid=''
    return 0
}

# 前台看护：孩子退出后，如果 config 里的 proxy-mode 变了就是页面上的主动切换，按新形态
# 重来一次；没变就是真退出，把 rc 交给宿主（宿主据此重启或报错）。
supervise() {
    while :; do
        MODE=$(mode_of) || return 1
        apply_settings "$MODE" || return 1
        if [ "$MODE" = router ]; then
            load_router_params
            validate_router || return 1
        elif ! "$HERE/meow" -d "$DATA" -f "$CONFIG" -t >/dev/null 2>&1 < /dev/null; then
            # pc/off 形态先做一次配置校验，失败就别起一个秒退进程。
            echo 'meow: config validation failed (run meow -t to see details)' >&2
            return 1
        fi
        start_child "$MODE" || { cleanup_child; return 1; }
        wait "$mon_pid"
        _rc=$?
        echo "meow exited rc=$_rc (mode=$MODE)" >&2
        cleanup_child
        _next=$(mode_of 2>/dev/null) || _next=''
        if [ -n "$_next" ] && [ "$_next" != "$MODE" ]; then
            echo "meow: proxy-mode $MODE → $_next，按新形态重启" >&2
            continue
        fi
        return "$_rc"
    done
}

# ---- 能力：代理类型（页面 A 的顶部选择器） ----
# 不带参数是读，带 mode 是写。写只改 config 的 proxy-mode 并结束孩子进程，
# 重启由前台看护循环负责——所以一次调用里不会出现「配置已改、进程还没换」的状态。

# meow 的面板密钥（subscription 常常自带一个 secret）。它就是 YACD 要的那串；页面把它
# 传给 iframe，面板就不用每次手输。列表写法取第一个元素。
secret_of() {
    _line=$(sed -n 's/^secret:[[:space:]]*//p' "$CONFIG" 2>/dev/null | head -n 1)
    printf '%s' "$_line" |
        sed -e 's/[[:space:]]*#.*$//' -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//' \
            -e 's/^\[//' -e 's/\]$//' -e 's/,.*$//' \
            -e 's/^"//' -e 's/"$//' -e "s/^'//" -e "s/'$//"
}

# 读：{"mode":"off|tun|tproxy","proxy-mode":"off|pc|router","running":bool,"secret":...}
report_mode() {
    _mode=$(mode_of 2>/dev/null) || _mode=off
    _running=false
    running && _running=true
    _ports=''
    [ -f "$CONFIG" ] && _ports=$(report_ports)
    if is_linux; then
        _allowed='"off","tun","tproxy"'
    else
        _allowed='"off","tun"'
    fi
    _secret=''
    [ -f "$CONFIG" ] && _secret=$(secret_of | svcd_json_escape)
    printf '{"mode":"%s","proxy-mode":"%s","running":%s,"allowed":[%s],"secret":"%s"%s}\n' \
        "$(label_of "$_mode")" "$_mode" "$_running" "$_allowed" "$_secret" "$_ports"
}

# 写：把页面上的 off/tun/tproxy 落成 proxy-mode，持久化在 config 里。
do_set_mode() {
    _want=$1
    _target=$(mode_of_label "$_want") || {
        echo "meow: 代理类型只能是 off / tun / tproxy（收到 '$_want'）" >&2
        return 1
    }
    if [ "$_target" = router ] && ! is_linux; then
        echo "meow: tproxy（网关形态）只能在 Linux 上开" >&2
        return 1
    fi
    _now=$(mode_of 2>/dev/null) || _now=''
    if [ "$_now" = "$_target" ] && running; then
        report_mode
        return 0
    fi
    # 只写我们自己的缓存；meow 的配置由看护循环按新形态重写受管键。
    set_mode_cache "$_target" || return 1
    # 孩子活着就叫它退出：前台看护循环醒来会看到新的 proxy-mode，按新形态重启。
    _pid=$(cat "$PIDFILE" 2>/dev/null || true)
    if [ -n "$_pid" ]; then
        kill "$_pid" 2>/dev/null || true
    fi
    printf '{"mode":"%s","changing":true}\n' "$_want"
}

handle() {
    case "$1" in
    mode) handle_mode "$2" ;;
    *) echo "unknown tool: $1" >&2; return 1 ;;
    esac
}

handle_mode() {
    _want=$(svcd_field "${1:-}" mode)
    if [ -z "$_want" ]; then
        report_mode
        return 0
    fi
    do_set_mode "$_want"
}

# 宿主 `svc status` 的一行摘要。
svcd_status() {
    _mode=$(mode_of 2>/dev/null) || _mode=off
    if running; then
        printf '%s (mode=%s)' 'running' "$(label_of "$_mode")"
    else
        printf '%s (mode=%s)' 'meow is not running' "$(label_of "$_mode")"
    fi
}

cleanup_on_exit() {
    trap '' TERM INT HUP EXIT
    [ -n "${CHANNEL:-}" ] && kill "$CHANNEL" 2>/dev/null
    cleanup_child
}

# ---- 生命周期 ----

do_start() {
    prepare_config || return 1
    if running; then
        echo "already running (pid $(cat "$PIDFILE"))" >&2
        return 1
    fi
    rm -f "$PIDFILE"
    if [ ! -f "$SDK" ]; then
        echo "meow: 缺少通道 SDK（$SDK），页面上的代理类型开关不可用" >&2
        return 1
    fi

    SELF=$$
    trap 'cleanup_on_exit' EXIT
    trap 'exit 0' TERM INT HUP
    # 后台作业的 stdin 会被 shell 接到 /dev/null，所以先把宿主的通道口存成 fd 5 再给它。
    exec 5<&0
    # 通道能力跑在后台：它只改写 config 并结束孩子，重启交给下面的前台循环。
    # SDK 收到 svcd.shutdown 会自己 exit，所以「通道断了就叫醒看护循环」只能挂在 EXIT 上。
    ( trap 'kill -TERM "$SELF" 2>/dev/null' EXIT; svcd_plugin_serve handle <&5 ) &
    CHANNEL=$!
    supervise
    rc=$?
    exit "$rc"
}

# 宿主负责终止（Unix 进程组 SIGTERM → 10 秒 → SIGKILL；Windows 直接强杀整棵树），stop
# 跑到这里时看护进程和 meow 都已经不在了：它只收尾，不杀进程。宿主强杀或断电之后
# pidfile 会留下来，而那个 pid 可能已经被复用给别的进程，所以只删不按它发信号。
do_stop() {
    # 先撤规则：撤完 LAN 立刻回到直连，不会打到一个没人监听的端口。
    # 不看 proxy-mode：从 router 切回 pc 之后也要能把旧规则清掉。
    if [ -f "$FW" ] && [ "$(firewall_state 2>/dev/null)" = up ]; then
        LAN_IF=$(our_or lan-interface br0)
        sh "$FW" down --lan "$LAN_IF" >/dev/null 2>&1
    fi
    if running; then
        echo "meow: pid $(cat "$PIDFILE") 还在跑（宿主应该已经结束了它）" >&2
    fi
    rm -f "$PIDFILE"
    echo "stopped"
}

do_status() {
    mode=$(mode_of 2>/dev/null) || mode=off
    fw=''
    if [ "$mode" = router ]; then
        state=$(firewall_state)
        fw=$(printf ',"firewall":"%s"' "$state")
        if running && [ "$state" != up ]; then
            echo "meow: running but nat rules are down (sh $0 fw up 或重跑 start)" >&2
        elif ! running && [ "$state" = up ]; then
            echo "meow: not running but nat rules are still up (sh $0 fw down)" >&2
        fi
    fi
    ports=''
    if [ -f "$CONFIG" ]; then
        ports=$(report_ports)
    fi
    proxy=$(printf ',"proxy":"%s"' "$(label_of "$mode")")
    if running; then
        printf '{"running":true,"pid":%s,"mode":"%s"%s,"controller":"127.0.0.1:9090"%s%s}\n' \
            "$(cat "$PIDFILE")" "$mode" "$proxy" "$fw" "$ports"
    else
        # 不要在这里删 pidfile：宿主可能在 start 写到一半时先跑 status，
        # 删掉会把刚起来的进程变成“看不见”。
        printf '{"running":false,"mode":"%s"%s,"controller":"127.0.0.1:9090"%s%s}\n' \
            "$mode" "$proxy" "$fw" "$ports"
    fi
}

# 调试用：只按当前配置跑规则脚本，不动 meow 进程。
do_fw() {
    prepare_config || return 1
    _mode=$(mode_of) || return 1
    apply_settings "$_mode" || return 1
    load_router_params
    _cmd=${1:-status}
    if [ "$_cmd" = up ]; then
        validate_router || return 1
    fi
    fw_call "$_cmd"
}

# 调试用：打印解析出来的形态与参数，不启动任何东西。
# 会先把受管键写一遍（和 start 一样），所以看到的就是 meow 实际要用的配置。
do_mode() {
    prepare_config || return 1
    echo "meow  : $CONFIG"
    echo "state : $STATE"
    _mode=$(mode_of) || return 1
    echo "mode  : $_mode"
    apply_settings "$_mode" || return 1
    # 只有网关形态才需要这些参数；pc 形态去读 lan-interface 只会多一条告警。
    [ "$_mode" = router ] || return 0
    load_router_params
    echo "lan   : ${LAN_IF:-}"
    echo "tproxy: ${TPROXY_PORT:-} (firewall=${TPROXY_FIREWALL:-?} listen=${TPROXY_LISTEN:-?})"
    echo "dns   : ${DNS_ADDR:-} ${DNS_PORT:-}"
    echo "mark  : ${MARK:-}"
}

case "${1:-}" in
    start)    do_start ;;
    stop)     do_stop ;;
    status)   do_status ;;
    mode)     do_mode ;;
    set-mode) shift; do_set_mode "${1:-}" ;;
    fw)       shift; do_fw "$@" ;;
    *)        echo "usage: $0 start|stop|status|mode|set-mode off|tun|tproxy|fw [up|down|status]" >&2; exit 2 ;;
esac
