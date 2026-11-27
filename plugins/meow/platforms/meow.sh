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

# meow 的 geo 库和 rule-provider 的表都只在它的「家目录」（-d，也就是 <插件目录>/data）
# 里找：file 型 provider 的 path 还受 resolve_contained 约束，指不到数据目录外面。所以
# 放在 config 旁边（<插件目录>/<名字>，手搭的那套就是这么放的）也得搬进来才算数：
#   Country.mmdb      精简版（130 KB）：很多路由器直连不通 GitHub，meow 自己下 7 MB 会失败，
#                     而 GEOIP 规则没有库整份配置都起不来；
#   cn_domains.yaml   file 型 rule-provider 的表（古典写法，DOMAIN-SUFFIX,…）；
#   GeoLite2-ASN.mmdb / geosite.dat  见下面空的占位。
# 数据目录里已经有了就不动——那可能是 meow 自己更新过的。
seed_data_files() {
    for _name in Country.mmdb cn_domains.yaml GeoLite2-ASN.mmdb geosite.dat; do
        [ -f "$DATA/$_name" ] && continue
        _src=''
        if [ -f "$PLUGIN_ROOT/$_name" ]; then
            _src="$PLUGIN_ROOT/$_name"
        elif [ -f "$ROOT/$_name" ]; then
            _src="$ROOT/$_name"
        fi
        [ -n "$_src" ] || continue
        _seed="$DATA/$_name.tmp.$$"
        if cp "$_src" "$_seed" 2>/dev/null && mv "$_seed" "$DATA/$_name" 2>/dev/null; then
            echo "meow: 已把 $_src 放进 $DATA" >&2
        else
            rm -f "$_seed"
        fi
    done
    # 这两份 meow 只在文件缺失时去 GitHub 下，而 GEOIP / GEOSITE / IP-ASN 规则一用到就下：
    # 路由器的上游还没通时那一步会一直卡着，插件跟着起不来。放个空文件占位，meow 就当它
    # 存在——用不到就不读；用到了 GEOSITE 只是不命中，IP-ASN 会当场报「库加载失败」而不是
    # 卡在下载上。有真的库就替换掉这两个空文件。
    for _name in GeoLite2-ASN.mmdb geosite.dat; do
        [ -f "$DATA/$_name" ] && continue
        if : > "$DATA/$_name" 2>/dev/null; then
            echo "meow: 已放一个空的 $_name 占位，免得 meow 去 GitHub 下（有真的就替换掉它）" >&2
        fi
    done
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
seed_data_files

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

# set_key <文件> <键> <值>：有就替换那一行，没有就追加到末尾。值已经一样就一个字不动，
# 行尾注释跟着留着（别把 "mixed-port: null   # 网关不需要…" 改成光秃秃一行）。
set_key() {
    _tmp="$1.tmp.$$"
    awk -v key="$2" -v val="$3" '
        function same(s,   v) {
            v = s
            sub("^" key ":[ \t]*", "", v)
            sub(/[ \t]*#.*$/, "", v)
            gsub(/^[ \t]+|[ \t]+$/, "", v)
            gsub(/^["\047]|["\047]$/, "", v)
            return v == val
        }
        BEGIN { done = 0 }
        $0 ~ "^" key ":[ \t]*" {
            if (same($0)) { print; done = 1; next }
            comment = ""
            if (match($0, /#.*/)) comment = " " substr($0, RSTART)
            print key ": " val comment
            done = 1
            next
        }
        { print }
        END { if (!done) print key ": " val }
    ' "$1" > "$_tmp" && mv "$_tmp" "$1"
}

# del_key <文件> <键>：删掉键本身和它下面的缩进行（块也跟着走）。
# 顶格注释是和键平级的，不在块里，原样留着（删 tproxy-port 时别把下面那行
# "# tproxy-sni 已废弃 …" 一起吃掉）。
del_key() {
    _tmp="$1.tmp.$$"
    awk -v key="$2" '
        BEGIN { skip = 0 }
        /^[^ \t]/ {
            skip = ($0 ~ ("^" key ":[ \t]*"))
            if (skip) next
            print; next
        }
        { if (!skip) print }
    ' "$1" > "$_tmp" && mv "$_tmp" "$1"
}

# set_block_key <文件> <块> <键> <值>：块里那一行替换，块里没有就补一行；
# 连块都没有就把整块追加到末尾。
# 块头只认 "<块>:"、"<块>: null"、"<块>: ~"、"<块>: {}" 这几种：后三种是 meow 对「这个段
# 没设过」的序列化，先换成真块头再补键。不这么办，键会补到文件末尾，跟原来的空块头凑成
# 同一个键的第二份，meow 直接报 duplicate entry with key。
set_block_key() {
    _tmp="$1.tmp.$$"
    awk -v block="$2" -v key="$3" -v val="$4" '
        function flush() {
            if (inblock && !done) { print "  " key ": " val; done = 1 }
        }
        function unset_header(s) {
            return s ~ ("^" block ":[ \t]*(null|~|\\{\\})[ \t]*(#.*)?$")
        }
        BEGIN { inblock = 0; seen = 0; done = 0 }
        /^[^ \t#]/ {
            flush()
            inblock = ($0 ~ ("^" block ":[ \t]*$")) || unset_header($0)
            if (inblock) {
                seen = 1
                print ((unset_header($0)) ? block ":" : $0)
                next
            }
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

# 顶层键写两遍，YAML 一个字符都不认：meow 报 duplicate entry with key "tun" 就再也起不来。
# meow 会把没设过的段序列化成 "<键>: null"（添加订阅时它就把 config 重写一遍），老版本又在
# 末尾补过一份真块，同一个键就出现两次。这里只丢 null / ~ / {} 这种纯占位的那一份，真块（以及
# 块里的内容）原样留着；两份都是有内容的块属于没见过的写法，不猜，留给 meow 自己报错。
heal_null_duplicates() {
    _tmp="$1.tmp.$$"
    awk '
        function key_of(s) {
            if (s ~ /^[^ \t#]/ && s ~ /^[A-Za-z0-9_.-]+:/) {
                sub(/:.*/, "", s)
                return s
            }
            return ""
        }
        NR == FNR {
            k = key_of($0)
            if (k != "") count[k]++
            next
        }
        {
            k = key_of($0)
            if (k != "" && count[k] > 1 &&
                $0 ~ /^[A-Za-z0-9_.-]+:[ \t]*(null|~|\{\})[ \t]*(#.*)?$/) next
            print
        }
    ' "$1" "$1" > "$_tmp" || { rm -f "$_tmp"; return 1; }
    if cmp -s "$1" "$_tmp"; then
        rm -f "$_tmp"
        return 0
    fi
    mv "$_tmp" "$1" || { rm -f "$_tmp"; return 1; }
    echo "meow: 配置里重复的 null 占位已清掉，同一个键只留真块" >&2
}

# rule-providers 里 file 型 provider 的 path 是绝对路径时（手搭的那套常写 /etc/sr/meow/…），
# meow 会拒："path ... escapes the provider directory"——它的 provider 路径必须在 -d 里面。
# 把这种文件搬进数据目录，path 行改写成文件名（原文件不动）；找不到文件就只改路径，反正
# 原来也加载不了。
fix_provider_paths() {
    _abs=$(awk '
        /^rule-providers:[ \t]*$/ { inrp = 1; next }
        /^[^ \t#]/ { inrp = 0 }
        inrp && /^[ \t]+path:[ \t]*/ {
            v = $0
            sub(/^[ \t]+path:[ \t]*/, "", v)
            sub(/[ \t]*#.*$/, "", v)
            gsub(/^["\047]|["\047]$/, "", v)
            if (v ~ /^\//) print v
        }
    ' "$CONFIG" 2>/dev/null)
    [ -n "$_abs" ] || return 0
    printf '%s\n' "$_abs" | while IFS= read -r _p; do
        [ -n "$_p" ] || continue
        _b=${_p##*/}
        if [ -f "$_p" ] && [ ! -f "$DATA/$_b" ]; then
            cp "$_p" "$DATA/$_b.tmp.$$" 2>/dev/null && mv "$DATA/$_b.tmp.$$" "$DATA/$_b" 2>/dev/null
        fi
        echo "meow: rule-provider 的 $_p 换成数据目录里的 $_b（meow 只认 -d 里面的路径）" >&2
    done
    _tmp="$CONFIG.tmp.$$"
    awk '
        /^rule-providers:[ \t]*$/ { inrp = 1; print; next }
        /^[^ \t#]/ { inrp = 0; print; next }
        inrp && /^[ \t]+path:[ \t]*\// {
            head = $0
            sub(/path:[ \t]*\/.*$/, "path: ", head)
            v = $0
            sub(/^[ \t]+path:[ \t]*/, "", v)
            sub(/[ \t]*#.*$/, "", v)
            gsub(/^["\047]|["\047]$/, "", v)
            n = split(v, parts, "/")
            print head parts[n]
            next
        }
        { print }
    ' "$CONFIG" > "$_tmp" && mv "$_tmp" "$CONFIG"
    return 0
}

# meow 不认识我们的键（proxy-mode 现在住在 state，另外两个是 router 的可选项）。
strip_our_keys() {
    del_key "$1" proxy-mode
    del_key "$1" lan-interface
    del_key "$1" dns-port
}

# 包里那份 cn_domains.yaml 是 mihomo 的古典写法（DOMAIN-SUFFIX,… / DOMAIN,… /
# DOMAIN-WILDCARD,…），而 1.3.5 之前的模板把它声明成 behavior: domain。domain 型把每条原样
# 当域名塞进 trie，"DOMAIN-SUFFIX,00cdn.com" 于是成了一条永远匹配不到的字面域名——不报错、
# 不给警告，规则集等于空的。旧配置里的 domain 就地改成 classical；别的 provider 一个字不动。
fix_cn_domains_behavior() {
    grep -qE '^rule-providers:' "$1" 2>/dev/null || return 0
    grep -qE '^[[:space:]]+behavior:[[:space:]]*domain[[:space:]]*$' "$1" || return 0
    # 只在数据目录里那份表确实是古典写法时才动。用户自己换成一张纯域名表的话，
    # behavior: domain 反而是对的。
    grep -q 'DOMAIN-' "$DATA/cn_domains.yaml" 2>/dev/null || return 0
    _tmp="$1.tmp.$$"
    awk '
        # 顶层键：rule-providers 开始，别的顶层键结束。
        /^[^ \t#]/ {
            inproviders = ($0 ~ /^rule-providers:[ \t]*$/)
            ours = -1
            print; next
        }
        {
            if (inproviders) {
                line = $0
                sub(/^[ \t]+/, "", line)
                ind = length($0) - length(line)
                if (ind == 0) { print; next }
                # provider 名那一行：记住缩进，它就是我们的那一段的起点。
                if (line ~ /^cn_domains:[ \t]*$/) { ours = ind; print; next }
                # 回到同级或更浅的缩进，说明 cn_domains 这一段结束了。
                if (ours >= 0 && ind <= ours) { ours = -1 }
                if (ours >= 0 && line ~ /^behavior:[ \t]*domain[ \t]*$/) {
                    sub(/domain[ \t]*$/, "classical")
                }
            }
            print
        }
    ' "$1" > "$_tmp" || { rm -f "$_tmp"; return 1; }
    if cmp -s "$1" "$_tmp"; then
        rm -f "$_tmp"
        return 0
    fi
    mv "$_tmp" "$1" || { rm -f "$_tmp"; return 1; }
    echo "meow: rule-providers.cn_domains 的 behavior 改成 classical（那份表是古典写法）" >&2
    return 0
}

# meow 面板上"添加订阅"会用订阅里那份 rules 整体替换本地的：订阅里没带 rules 时，本地的
# 规则就只剩 "rules: []"（实测 3855 条规则一声不响全没），代理按最后那条兜底走。这里只提示、
# 不动文件——meow 每次保存前会把旧配置改名成 config.yaml.bak，想找回来从那儿抄。
warn_rules_wiped() {
    [ -f "$CONFIG.bak" ] || return 0
    grep -q '^subscriptions:' "$CONFIG" 2>/dev/null || return 0
    _items=$(awk '
        /^rules:[ \t]*(\[\])?[ \t]*(#.*)?$/ { inblock = 1; next }
        /^[^ \t#]/ { inblock = 0 }
        inblock && /^[ \t]*-[ \t]/ { n++ }
        END { print n + 0 }
    ' "$CONFIG" 2>/dev/null)
    [ "${_items:-0}" = 0 ] || return 0
    grep -qE '^[ \t]*-[ \t]' "$CONFIG.bak" 2>/dev/null || return 0
    echo "meow: 配置里 rules 是空的（添加订阅时被订阅内容顶掉了），加之前那份在 $CONFIG.bak" >&2
    return 0
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
    heal_null_duplicates "$CONFIG"
    fix_cn_domains_behavior "$CONFIG"
    fix_provider_paths
    warn_rules_wiped
    if [ ! -f "$STATE" ]; then
        write_state "$(mode_of 2>/dev/null || printf off)"
    fi
    return 0
}

# router 形态要一个 tproxy 入口：有就沿用（端口以它为准），没有就往 listeners 块尾补一条。
#   - 块里的条目顶格写（"listeners:" 下面直接 "- name: …"）还是缩进写都行，按它现有的缩进
#     补——混着写 YAML 会报错；
#   - "listeners: null / ~ / []" 是 meow 对「这个段没设过」的序列化，先换成真块头再补；
#   - 连 listeners 都没有才整块追加。
ensure_tproxy_listener() {
    _port=$1
    [ -n "$(tproxy_entry)" ] && return 0
    _tmp="$CONFIG.tmp.$$"
    awk -v port="$_port" '
        function item(   pre_) {
            print pre_ "- name: tproxy"
            print pre_ "  type: tproxy"
            print pre_ "  port: " port
            print pre_ "  listen: 0.0.0.0"
            print pre_ "  firewall: false"
            done = 1
        }
        function indent_of(s) { match(s, /^[ \t]*/); return substr(s, 1, RLENGTH) }
        BEGIN { inblock = 0; seen = 0; done = 0; pre = "  " }
        {
            # 块结束：顶格的、又不是列表条目的行。补在块尾，再打印这一行。
            if (inblock && $0 !~ /^[ \t]*-/ && $0 ~ /^[^ \t]/) {
                if (!done) item(pre)
                inblock = 0
            }
            if (!inblock) {
                if ($0 ~ /^listeners:[ \t]*$/) { inblock = 1; seen = 1; print; next }
                if ($0 ~ /^listeners:[ \t]*(null|~|\[\])[ \t]*(#.*)?$/) {
                    print "listeners:"; inblock = 1; seen = 1; next
                }
            }
            if (inblock && $0 ~ /^[ \t]*-/) pre = indent_of($0)
            print
        }
        END {
            if (inblock && !done) item(pre)
            if (!seen) {
                print ""
                print "listeners:"
                print "  - name: tproxy"
                print "    type: tproxy"
                print "    port: " port
                print "    listen: 0.0.0.0"
                print "    firewall: false"
            }
        }
    ' "$CONFIG" > "$_tmp" && mv "$_tmp" "$CONFIG"
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

# router 形态把入口放在 listeners: 里；切回 pc / off 时那条 tproxy 必须摘掉：顶层
# tproxy-port 和它抢同一个端口，meow 报 "port 7893 already used by listener 'tproxy'"，
# 插件直接起不来（页面上选过 tproxy 再选 tun 就撞这个）。
# 认三种写法：我们补的 "- name: tproxy"、手写的 "- name: tproxy-tcp"、任何自己写着
# "type: tproxy" 的条目（meow 重写配置之后条目是顶格的，名字照抄用户的）。别人的
# listener 一个字不动；条目顶格/缩进都认。
remove_tproxy_listener() {
    grep -qE '^listeners:' "$CONFIG" 2>/dev/null || return 0
    _tmp="$CONFIG.tmp.$$"
    awk '
        function is_item(s) { return s ~ /^[ \t]*-[ \t]/ || s ~ /^[ \t]*-$/ }
        function clean(s) { sub(/[ \t]*#.*$/, "", s); gsub(/^[ \t]+|[ \t]+$/, "", s); return s }
        function field(line, key,   s) {
            s = line
            sub(/^[ \t]*/, "", s)
            sub(/^-[ \t]*/, "", s)
            if (index(s, key ":") != 1) return ""
            s = substr(s, length(key) + 2)
            return clean(s)
        }
        # 条目先攒着：要看它下面的 type 才知道是不是 tproxy 入口。
        function ours() {
            if (name ~ /^tproxy(-[A-Za-z0-9_.-]+)?$/) return 1
            if (type == "tproxy") return 1
            return (raw ~ /^[ \t]*-[ \t]*tproxy[ \t]*$/)
        }
        function flush(   i) {
            if (n == 0) return
            if (!ours()) for (i = 1; i <= n; i++) print buf[i]
            n = 0
        }
        BEGIN { inblock = 0; n = 0; name = ""; type = ""; raw = "" }
        {
            if (is_item($0)) {
                if (!inblock) { print; next }
                flush()
                n = 1; buf[n] = $0; raw = $0
                name = field($0, "name"); type = field($0, "type")
                next
            }
            if ($0 ~ /^[^ \t#]/) {
                flush()
                inblock = ($0 ~ /^listeners:[ \t]*$/)
                print; next
            }
            if (n > 0) {
                n++; buf[n] = $0
                if (name == "") name = field($0, "name")
                if (type == "") type = field($0, "type")
                next
            }
            print
        }
        END { flush() }
    ' "$CONFIG" > "$_tmp" && mv "$_tmp" "$CONFIG"
    return 0
}

# router 形态还会把 dns.listen 改成 0.0.0.0:<dns-port>（劫持 LAN 的 DNS）。切回 pc / off 时
# 还原成模板的 127.0.0.1:5353：本机的解析器不该一直开给整个 LAN。
clear_router_leftovers() {
    remove_tproxy_listener
    _addr=$(dns_listen | awk '{print $1}')
    case "$_addr" in
        0.0.0.0|::) set_block_key "$CONFIG" dns listen 127.0.0.1:5353 ;;
    esac
    return 0
}

apply_settings() {
    _mode=$1
    _port=$(yaml_number mixed-port 7890)
    is_port "$_port" || { echo "meow: mixed-port 不是端口：'$_port'" >&2; return 1; }
    # 网关形态不需要 LAN 侧的 mixed-port：用户写成 null 就让它 null，别硬塞一个 7890 开给
    # 整个 LAN。off / pc 才需要这个口。
    [ "$_mode" = router ] || set_key "$CONFIG" mixed-port "$_port"
    # 订阅里带进来的 tun 块要关掉：三种形态的透明代理都不靠订阅自带的 tun。
    # "tun: null"（meow 序列化出来的空段）本来就是没开，不用改写它。
    if grep -qE '^tun:[ \t]*$' "$CONFIG"; then
        set_block_key "$CONFIG" tun enable false
    fi

    case "$_mode" in
    off)
        # 不接管：只留 mixed-port。tproxy-port 一去掉，meow 就不装 nft 规则。
        clear_router_leftovers
        del_key "$CONFIG" tproxy-port
        del_key "$CONFIG" routing-mark
        ;;
    pc)
        _tproxy=$(tproxy_port)
        is_port "$_tproxy" || { echo "meow: tproxy-port 不是端口：'$_tproxy'" >&2; return 1; }
        clear_router_leftovers
        set_key "$CONFIG" tproxy-port "$_tproxy"
        _mark=$(yaml_number routing-mark 9527)
        case "$_mark" in
            *[!0-9]*) echo "meow: routing-mark 只能是数字：'$_mark'" >&2; return 1 ;;
        esac
        set_key "$CONFIG" routing-mark "$_mark"
        ;;
    router)
        # 入口是 listeners 里的 tproxy 项：顶层 tproxy-port 会让 meow 再开一个入口、和它
        # 抢同一个端口，所以置空（不删行）；routing-mark 必须留着——meow 的出向靠它打
        # SO_MARK，meow-fw.sh 的 "-m mark --mark … -j RETURN" 也认这个值。
        set_key "$CONFIG" tproxy-port null
        # meow 面板上"添加订阅"时，它把整份配置按内存里的 raw 重写一遍：文件里原本没有的
        # 键会被写成 "routing-mark: null"（注释、顺序也一起没了）。meow 没有 mark 就不给
        # 自己的出向打 SO_MARK，而 meow-fw.sh 装的那条放行规则认的是这个值，于是两边对不上。
        # 所以这里把它当受管键写一遍：配置里已有的数字 → state → 9527。
        _mark=$(yaml_number routing-mark "$(our_or routing-mark 9527)")
        case "$_mark" in
            ''|*[!0-9]*) echo "meow: routing-mark 只能是数字：'$_mark'" >&2; return 1 ;;
        esac
        set_key "$CONFIG" routing-mark "$_mark"
        [ "$(dns_enable)" = true ] || set_block_key "$CONFIG" dns enable true
        _dns=$(our_or dns-port 1053)
        is_port "$_dns" || { echo "meow: dns-port 不是端口：'$_dns'" >&2; return 1; }
        # 已经是 0.0.0.0:<dns-port> 就别重写（引号、行尾注释都留着）
        [ "$(dns_listen)" = "0.0.0.0 $_dns" ] ||
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
# 只认块写法（- name: … 换行 port: …），一行流式 map 不认；条目和字段顶格、缩进都认
# ——手写的网关配置常见 "listeners:" 下面直接顶格写 "- name: tproxy-tcp"。
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
    function is_item(s) { return s ~ /^[ \t]*-[ \t]/ || s ~ /^[ \t]*-$/ }
    BEGIN { inblock = 0; done = 0; type = ""; port = ""; fw = ""; listen = "" }
    # 顶格行：新的顶层键结束 listeners 块；顶格写的 "- name: …" 仍是块里的条目。
    /^[^ \t#]/ {
        if (!is_item($0)) { inblock = ($0 ~ /^listeners:[ \t]*$/); next }
        if (!inblock) next
    }
    !inblock { next }
    is_item($0) {
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
    MARK=$(yaml_number routing-mark "$(our_or routing-mark 9527)")
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
    # 0 等于“不设”：iptables 的 "--mark 0" 匹配的是所有没打标的包，那条 RETURN 会把整个
    # 劫持全放过去（表上看规则都在，实际一个包都不进 meow）。
    case "$MARK" in
        '') echo "meow: config 里没有 routing-mark，跳过 SO_MARK 放行规则" >&2 ;;
        0) echo "meow: routing-mark 是 0（等于不设 mark），跳过 SO_MARK 放行规则" >&2
           MARK='' ;;
        *[!0-9]*) router_hint "routing-mark 不是数字：'$MARK'"; return 1 ;;
    esac
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

# 选中的形态没起来就退回 off：宁可只留一个 mixed-port 的普通代理，也别让插件一直起不来
# （页面打不开就什么也改不了）。只写 state，config 的受管键由下一轮 apply_settings 按 off
# 重写；调用方负责 continue。已经是 off 就返回 1，让调用方按原样退出。
fall_back_to_off() {
    [ "$1" = off ] && return 1
    # 只改 proxy-mode 那一行：router 的 lan-interface / dns-port 留着，用户重新选 tproxy
    # 时不用再填一遍。
    set_mode_cache off || return 1
    echo "meow: $1 形态没起来，代理类型已经降成 off（插件照常起；修好之后在页面上重新选 tun / tproxy）" >&2
    return 0
}

# 前台看护：孩子退出后，如果 state 里的 proxy-mode 变了就是页面上的主动切换，按新形态
# 重来一次；没变就是真退出，把 rc 交给宿主（宿主据此重启或报错）。形态起不来时先退回 off。
supervise() {
    while :; do
        MODE=$(mode_of) || return 1
        if ! apply_settings "$MODE"; then
            fall_back_to_off "$MODE" && continue
            return 1
        fi
        if [ "$MODE" = router ]; then
            load_router_params
            if ! validate_router; then
                fall_back_to_off "$MODE" && continue
                return 1
            fi
        else
            # pc/off 形态先做一次配置校验，失败就别起一个秒退进程。别把失败一律算成
            # 配置问题：二进制在宿主机上根本跑不起来（glibc 比构建机老、架构不对）也是
            # 这里失败，所以把 meow 自己的话带出来，一眼能分清是哪一种。
            _check=$("$HERE/meow" -d "$DATA" -f "$CONFIG" -t 2>&1 < /dev/null) || {
                case "$_check" in
                    *GLIBC*|*"not found"*|*"No such file"*|*"cannot execute"*)
                        echo "meow: 这个 meow 二进制在这台机器上跑不起来（不是配置问题）：" >&2
                        echo "meow:   Linux 的 gnu 包要求宿主的 glibc 不比构建机老（本地是 Debian 12 / 2.36，CI 是 Ubuntu 22.04 / 2.35）；" >&2
                        echo "meow:   老一点的发行版（Debian 12 / 很多路由器固件）请用 x86_64-unknown-linux-musl 包。" >&2
                        ;;
                    *)
                        echo "meow: 配置校验没通过（meow -t）：" >&2
                        ;;
                esac
                printf '%s\n' "$_check" | head -n 5 >&2
                # glibc 老、架构不对这种，降成 off 也一样跑不起来：照样退回去再试，
                # 让退回来的那一轮把真正的原因留在日志里。
                fall_back_to_off "$MODE" && continue
                return 1
            }
        fi
        _started=$(date +%s)
        if ! start_child "$MODE"; then
            cleanup_child
            fall_back_to_off "$MODE" && continue
            return 1
        fi
        wait "$mon_pid"
        _rc=$?
        echo "meow exited rc=$_rc (mode=$MODE)" >&2
        cleanup_child
        _next=$(mode_of 2>/dev/null) || _next=''
        if [ -n "$_next" ] && [ "$_next" != "$MODE" ]; then
            echo "meow: proxy-mode $MODE → $_next，按新形态重启" >&2
            continue
        fi
        # 秒退 = 根本没起来（TUN 没权限、端口被占、网卡不存在都在这一刻暴露），退回 off；
        # 跑了一阵才死的算真退出，按原样把 rc 交给宿主，别把跑过的形态悄悄降级。
        if [ "$_rc" -ne 0 ] && [ $(($(date +%s) - _started)) -lt 10 ]; then
            fall_back_to_off "$MODE" && continue
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
