#!/bin/sh
# meow 的配置层：
#   <插件目录>/config  meow 自己的配置（订阅/规则/端口），meow 就是用它启动的
#   <插件目录>/state   我们自己的，只缓存代理类型（off / pc / router）
#
# 用一个假布局（伪 meow / 伪规则脚本）跑 meow.sh 的配置相关子命令，检查三件事：
# 受管键按形态写进了 meow 的配置、代理类型落在 state、迁移把老布局收拾干净。
# 用法：sh tools/meow-config-test.sh [仓库根]
set -eu

REPO=$(CDPATH= cd -- "${1:-$(dirname -- "$0")/..}" && pwd)
WORK=$(mktemp -d "${TMPDIR:-/tmp}/meow-config-test.XXXXXX")
trap 'rm -rf "$WORK"' EXIT

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

meow_config() {
    cat <<'YAML'
# 订阅配置（示例）
mixed-port: 7891
proxies:
  - name: node-1
    type: direct
tun:
  enable: true
  auto-route: true
dns:
  enable: true
  listen: 127.0.0.1:5353
  nameserver:
    - 223.5.5.5
rules:
  - MATCH,DIRECT
YAML
}

# 假插件目录：<root>/plugin/{config,state} + <root>/plugin/1.3.0/payload/{bin,lib,config.yaml}
fixture() {
    _root=$1
    rm -rf "$_root"
    _plugin="$_root/plugin"
    _bin="$_plugin/1.3.0/payload/bin"
    mkdir -p "$_bin" "$_plugin/1.3.0/payload/yacd" "$_plugin/1.3.0/payload/lib"
    cp "$REPO/plugins/meow/platforms/meow.sh" "$_bin/meow.sh"
    cp "$REPO/plugins/meow/platforms/lib/svcd-plugin.sh" "$_plugin/1.3.0/payload/lib/svcd-plugin.sh"
    cp "$REPO/plugins/meow/package/payload/config.yaml" "$_plugin/1.3.0/payload/config.yaml"
    cp "$REPO/plugins/meow/package/payload/Country.mmdb" "$_plugin/1.3.0/payload/Country.mmdb"
    cp "$REPO/plugins/meow/package/payload/cn_domains.yaml" "$_plugin/1.3.0/payload/cn_domains.yaml"
    meow_config > "$_plugin/config"
    # 伪 meow：-t 时按真 meow 的脾气来——顶层键写两遍就直接报 duplicate entry with key "x"，
    # 整份配置一个字符都不认；其余情况（真正启动）赖着不退，好让看护循环有东西可等。
    cat > "$_bin/meow" <<'FAKE'
#!/bin/sh
cfg=''
dir=''
check=0
while [ $# -gt 0 ]; do
    case "$1" in
        -d) dir=$2; shift 2 ;;
        -f) cfg=$2; shift 2 ;;
        -t) check=1; shift ;;
        *) shift ;;
    esac
done
if [ "$check" = 1 ]; then
    dup=$(sed -n 's/^\([A-Za-z0-9_.-][A-Za-z0-9_.-]*\):.*/\1/p' "$cfg" 2>/dev/null |
        sort | uniq -d | head -n 1)
    [ -z "$dup" ] || { echo "Error: duplicate entry with key \"$dup\""; exit 1; }
    # 模拟「这台机器上 pc 形态起不来」（TUN 没权限、nft 不存在之类）：只看受管键在不在，
    # 降成 off 之后 tproxy-port 会被删掉，校验自然就过了。
    if [ -f "$dir/fail-pc" ] && grep -q '^tproxy-port:' "$cfg" 2>/dev/null; then
        echo "Error: tproxy: operation not permitted"
        exit 1
    fi
    exit 0
fi
while :; do sleep 1; done
FAKE
    printf '#!/bin/sh\necho "fw-stub: $*"\n' > "$_bin/meow-fw.sh"
    chmod +x "$_bin/meow" "$_bin/meow.sh" "$_bin/meow-fw.sh"
}

run_meow() {
    SVCD_PLUGIN_DIR="$_plugin" sh "$_plugin/1.3.0/payload/bin/meow.sh" "$@" 2>&1
}

state_of() {
    sed -n 's/^proxy-mode:[[:space:]]*//p' "$_plugin/state" 2>/dev/null | head -n 1
}

# ---- 缺省 off：刚装好不接管流量，只留 mixed-port（tun/tproxy 都不该出现） ----
# 默认要是 pc（tun）就会把整机流量导进一个没有可用出口的内核，表现就是断网。
fixture "$WORK/default"
cp "$_plugin/config" "$WORK/config.before"
run_meow mode >/dev/null
grep -qx 'mixed-port: 7891' "$_plugin/config" || fail 'default: 用户写的 mixed-port 被改掉了'
grep -qx 'tproxy-port:.*' "$_plugin/config" && fail 'default: 缺省不该有 tproxy-port'
grep -qx 'routing-mark:.*' "$_plugin/config" && fail 'default: 缺省不该有 routing-mark'
grep -A1 -x 'tun:' "$_plugin/config" | grep -qx '  enable: false' || fail 'default: tun 没关掉'
grep -q 'node-1' "$_plugin/config" || fail 'default: 订阅内容被弄丢了'
grep -qx 'proxy-mode: off' "$_plugin/state" || fail 'default: state 缺省不是 off'
cmp -s "$WORK/config.before" "$_plugin/state" && fail 'default: state 跟配置混在一起了'

# ---- pc（页面上的 tun）：tproxy 端口和 mark 写进去，tun 关掉，代理类型落进 state ----
fixture "$WORK/pc"
cp "$_plugin/config" "$WORK/config.before"
run_meow set-mode tun >/dev/null
run_meow mode >/dev/null
grep -qx 'mixed-port: 7891' "$_plugin/config" || fail 'pc: 用户写的 mixed-port 被改掉了'
grep -qx 'tproxy-port: 7893' "$_plugin/config" || fail 'pc: tproxy-port 没写进去'
grep -qx 'routing-mark: 9527' "$_plugin/config" || fail 'pc: routing-mark 没写进去'
grep -A1 -x 'tun:' "$_plugin/config" | grep -qx '  enable: false' || fail 'pc: tun 没关掉'
grep -q 'node-1' "$_plugin/config" || fail 'pc: 订阅内容被弄丢了'
grep -qx 'proxy-mode: pc' "$_plugin/state" || fail 'pc: state 里没有 proxy-mode'
cmp -s "$WORK/config.before" "$_plugin/state" && fail 'pc: state 跟配置混在一起了'

# 用户改 mixed-port，跟着走（配置是 meow 的，端口以它为准）
sed -i 's/^mixed-port: 7891/mixed-port: 1080/' "$_plugin/config"
run_meow mode >/dev/null
grep -qx 'mixed-port: 1080' "$_plugin/config" || fail 'pc: 改 config 的 mixed-port 没生效'

# ---- off：从 pc 切回来，只留 mixed-port，tproxy/mark/tun 全撤 ----
fixture "$WORK/off"
run_meow set-mode tun >/dev/null
run_meow mode >/dev/null
run_meow set-mode off >/dev/null
grep -qx 'proxy-mode: off' "$_plugin/state" || fail 'off: state 里的代理类型没改成 off'
run_meow mode >/dev/null
grep -qx 'mixed-port: 7891' "$_plugin/config" || fail 'off: mixed-port 被动过'
grep -qx 'tproxy-port:.*' "$_plugin/config" && fail 'off: 不该有 tproxy-port'
grep -qx 'routing-mark:.*' "$_plugin/config" && fail 'off: 不该有 routing-mark'
grep -A1 -x 'tun:' "$_plugin/config" | grep -qx '  enable: false' || fail 'off: tun 没关掉'

# ---- meow 序列化出来的 "tun: null" 和我们补的真块：同一个键两份，meow 直接拒 ----
# 订阅一更新，meow 就把 config 重写一遍，没设过的段写成 "tun: null"；老版本又在末尾补过
# 一份 "tun:"。两份凑到一起就是 duplicate entry with key "tun"，插件再也起不来。启动时
# 要丢掉 null 那份、只留真块，而且真块里的内容一个字不能动。
fixture "$WORK/duplicate-null"
cat > "$_plugin/config" <<'DUP'
mixed-port: 7891
proxies:
  - name: node-1
    type: direct
tun: null
rules:
  - MATCH,DIRECT

tun:
  enable: false
  stack: gvisor
DUP
run_meow mode >/dev/null
[ "$(grep -c '^tun:' "$_plugin/config")" = 1 ] ||
    fail "duplicate-null: tun 还有 $(grep -c '^tun:' "$_plugin/config") 份"
grep -A2 -x 'tun:' "$_plugin/config" | grep -qx '  enable: false' ||
    fail 'duplicate-null: tun 没关掉'
grep -qx '  stack: gvisor' "$_plugin/config" || fail 'duplicate-null: 真块里的 stack 被删了'
grep -q 'node-1' "$_plugin/config" || fail 'duplicate-null: 订阅内容被弄丢了'
grep -q 'MATCH,DIRECT' "$_plugin/config" || fail 'duplicate-null: 规则被弄丢了'
cp "$_plugin/config" "$WORK/duplicate-null.once"
run_meow mode >/dev/null
cmp -s "$WORK/duplicate-null.once" "$_plugin/config" || fail 'duplicate-null: 第二次跑改了文件'

# ---- 只有 "tun: null"：本来就是没开，一个字都不用改 ----
# 只认真块头（"tun:"），null / ~ 这种空段是 meow 序列化出来的，别去动它——改了反而多出
# 几行噪音，下次它重写又变回 null。
fixture "$WORK/null-only"
cat > "$_plugin/config" <<'NUL'
mixed-port: 7891
tun: null
NUL
cp "$_plugin/config" "$WORK/null-only.before"
run_meow mode >/dev/null
cmp -s "$WORK/null-only.before" "$_plugin/config" || fail 'null-only: 配置被改了'

# ---- router 形态碰到 "dns: null"：同样换成真块头，不能补出第二个 dns ----
fixture "$WORK/dns-null"
cat > "$_plugin/config" <<'DN'
mixed-port: 7891
dns: null
tun:
  enable: false
DN
printf 'proxy-mode: router\n' > "$_plugin/state"
run_meow mode >/dev/null
[ "$(grep -c '^dns:' "$_plugin/config")" = 1 ] || fail 'dns-null: dns 写成了两份'
grep -A1 -x 'dns:' "$_plugin/config" | grep -qx '  enable: true' || fail 'dns-null: dns.enable 没写进去'

# ---- 页面上的三个值 ↔ 形态：tun=pc / tproxy=router / off=off ----
fixture "$WORK/labels"
run_meow set-mode tun >/dev/null
[ "$(state_of)" = pc ] || fail 'labels: tun 没落成 pc'
if [ "$(uname -s)" = Linux ]; then
    run_meow set-mode tproxy >/dev/null
    [ "$(state_of)" = router ] || fail 'labels: tproxy 没落成 router'
else
    # 网关形态只有 Linux 有；别的平台要明确拒绝，而不是写进 state。
    if run_meow set-mode tproxy >/dev/null 2>&1; then fail 'labels: 非 Linux 上 tproxy 应该被拒'; fi
    [ "$(state_of)" = pc ] || fail 'labels: 被拒的 tproxy 改了 state'
fi
run_meow set-mode off >/dev/null
[ "$(state_of)" = off ] || fail 'labels: off 没落成 off'
if run_meow set-mode turbo >/dev/null 2>&1; then fail 'labels: 非法值应该被拒'; fi
[ "$(state_of)" = off ] || fail 'labels: 非法值把 state 改了'
run_meow status | grep -q '"proxy":"off"' || fail 'labels: status 没报出代理类型'

# ---- router：删掉顶层 tproxy-port，DNS 绑 0.0.0.0，自带一条 tproxy 入口 ----
fixture "$WORK/router"
# 直接写缓存：网关形态的平台检查不走这里，这里只测受管键怎么写。
printf 'proxy-mode: router\n' > "$_plugin/state"
run_meow fw status >/dev/null
# 顶层 tproxy-port 会和 listeners 里的入口抢端口：置空（不删行，用户的注释留着）
grep -qE '^tproxy-port:[ \t]*[0-9]' "$_plugin/config" &&
    fail 'router: 顶层 tproxy-port 应该置空（会和 listeners 里的入口抢端口）'
grep -qx '  listen: 0.0.0.0:1053' "$_plugin/config" || fail 'router: dns.listen 没改成 0.0.0.0:1053'
grep -qx '    type: tproxy' "$_plugin/config" || fail 'router: 没补 tproxy 入口'
grep -qx '    firewall: false' "$_plugin/config" || fail 'router: tproxy 入口没写 firewall: false'
grep -qx '  enable: false' "$_plugin/config" || fail 'router: tun 没关掉'

# ---- 重复跑要幂等 ----
cp "$_plugin/config" "$WORK/once.yaml"
run_meow fw status >/dev/null
cmp -s "$WORK/once.yaml" "$_plugin/config" || fail 'router: 第二次跑改了文件'

# ---- router 切回 pc / off：listeners 里那条 tproxy 要摘掉，dns.listen 要还原 ----
# 留着那条 tproxy，pc 形态的顶层 tproxy-port 就和它抢同一个端口：meow 报
# "port 7893 already used by listener 'tproxy'"，插件直接起不来（页面上先选 tproxy 再选
# tun 就撞这个）。dns.listen 也一样：0.0.0.0:1053 是给 LAN 用的，切走之后本机的解析器
# 不该一直开给整个 LAN。
fixture "$WORK/router-leave"
printf 'proxy-mode: router\n' > "$_plugin/state"
run_meow mode >/dev/null
grep -qx '    type: tproxy' "$_plugin/config" || fail 'router-leave: 没补上 tproxy 入口'
grep -qx '  listen: 0.0.0.0:1053' "$_plugin/config" || fail 'router-leave: dns.listen 没改成 0.0.0.0'
printf 'proxy-mode: pc\n' > "$_plugin/state"
run_meow mode >/dev/null
grep -q 'type: tproxy' "$_plugin/config" && fail 'router-leave: 切回 pc 之后 tproxy 入口还在'
grep -qx '  listen: 127.0.0.1:5353' "$_plugin/config" || fail 'router-leave: 切回 pc 之后 dns.listen 没还原'
grep -qx 'tproxy-port: 7893' "$_plugin/config" || fail 'router-leave: pc 的 tproxy-port 没写进去'
printf 'proxy-mode: router\n' > "$_plugin/state"
run_meow mode >/dev/null
printf 'proxy-mode: off\n' > "$_plugin/state"
run_meow mode >/dev/null
grep -q 'type: tproxy' "$_plugin/config" && fail 'router-leave: 切到 off 之后 tproxy 入口还在'
grep -qx '  listen: 127.0.0.1:5353' "$_plugin/config" || fail 'router-leave: 切到 off 之后 dns.listen 没还原'
grep -qx 'tproxy-port:.*' "$_plugin/config" && fail 'router-leave: 切到 off 之后 tproxy-port 还在'

# ---- 手写的网关配置：格式原样保留，只改该改的那两行 ----
# 路由器上那份是顶格写 listeners、带行尾注释、routing-mark / tun: null 都在的写法。
# 插件只该动 behavior 和 provider 的绝对路径，别重排缩进、别补第二条入口、别删 mark。
fixture "$WORK/surgical"
cat > "$_plugin/config" <<'SG'
mixed-port: null                                   # 网关不需要 LAN 侧 HTTP/SOCKS
mode: rule
log-level: warning
dns:
  enable: true
  listen: "0.0.0.0:1053"                            # 必须配 meow-fw.sh 的 DNS_PORT
geodata:
  mmdb-path: '@DATA@/Country.mmdb'
rule-providers:
  cn_domains:
    type: file
    behavior: domain
    path: /etc/sr/meow/cn_domains.yaml
strict: null
tproxy-port: null
# tproxy-sni 已废弃，改用上面的 sniffer
routing-mark: 9527
listeners:
- name: tproxy-tcp
  type: tproxy
  port: 7893
  listen: "0.0.0.0"
  firewall: false
  max-connections: 4096
tun: null
max-connections: 4096
rules:
  - MATCH,DIRECT
SG
printf 'proxy-mode: router\n' > "$_plugin/state"
run_meow mode >/dev/null
[ "$(grep -c 'type: tproxy' "$_plugin/config")" = 1 ] || fail 'surgical: 又补了一条 tproxy 入口'
grep -qx 'routing-mark: 9527' "$_plugin/config" || fail 'surgical: routing-mark 被删了'
grep -qx '# tproxy-sni 已废弃，改用上面的 sniffer' "$_plugin/config" ||
    fail 'surgical: 顶格注释被吞了'
grep -qx '  max-connections: 4096' "$_plugin/config" || fail 'surgical: 用户的 max-connections 被动过'
grep -qx '    behavior: classical' "$_plugin/config" || fail 'surgical: behavior 没改成 classical'
grep -qx '    path: cn_domains.yaml' "$_plugin/config" || fail 'surgical: provider 的绝对路径没换'
[ "$(grep -c '^mixed-port: 7890' "$_plugin/config")" = 0 ] || fail 'surgical: 网关形态不该塞 mixed-port'
grep -q '^  listen: "0.0.0.0:1053"' "$_plugin/config" || fail 'surgical: dns.listen 被重写了'
grep -qx 'tun: null' "$_plugin/config" || fail 'surgical: tun: null 被改写了'

# ---- "listeners: null" + router：补 tproxy 入口不能补出第二份 listeners ----
# meow 重写 config 时把没设过的段写成 "listeners: null"；只认 "listeners:" 空块头的代码会
# 整块追加，同一个键两份，meow 报 duplicate entry with key "listeners" 直接起不来。
fixture "$WORK/listeners-null"
cat > "$_plugin/config" <<'LN'
mixed-port: 7890
dns:
  enable: true
  listen: 127.0.0.1:5353
listeners: null
rules:
  - MATCH,DIRECT
LN
printf 'proxy-mode: router\n' > "$_plugin/state"
run_meow mode >/dev/null
[ "$(grep -c '^listeners:' "$_plugin/config")" = 1 ] ||
    fail "listeners-null: listeners 有 $(grep -c '^listeners:' "$_plugin/config") 份"
grep -qx '    type: tproxy' "$_plugin/config" || fail 'listeners-null: 没补上 tproxy 入口'
grep -q 'listeners: null' "$_plugin/config" && fail 'listeners-null: null 还留着'
cp "$_plugin/config" "$WORK/listeners-null.once"
run_meow mode >/dev/null
cmp -s "$WORK/listeners-null.once" "$_plugin/config" || fail 'listeners-null: 第二次跑改了文件'

# ---- 已经被写成两份 listeners 的旧配置：启动时把 null 那份清掉 ----
fixture "$WORK/listeners-dup"
cat > "$_plugin/config" <<'LD'
mixed-port: 7890
listeners: null
listeners:
  - name: tproxy
    type: tproxy
    port: 7893
    listen: 0.0.0.0
    firewall: false
rules:
  - MATCH,DIRECT
LD
printf 'proxy-mode: router\n' > "$_plugin/state"
run_meow mode >/dev/null
[ "$(grep -c '^listeners:' "$_plugin/config")" = 1 ] ||
    fail "listeners-dup: 修完还是有 $(grep -c '^listeners:' "$_plugin/config") 份 listeners"
grep -qx '    type: tproxy' "$_plugin/config" || fail 'listeners-dup: tproxy 入口被删了'

# ---- meow 自己重写过的配置：加完订阅那份，routing-mark 是 null ----
# meow 面板上"添加订阅"会把整份配置按它内存里的 raw 重写一遍：注释没了、顺序按结构体排、
# 没设过的键全部摊成 "x: null"，listeners 的条目落到顶格。启动时文件里没有 routing-mark
# 的话，它就这么写回 null —— 而 meow-fw.sh 的 "-m mark --mark 9527 -j RETURN" 认的是这个
# 值，两边对不上。插件把受管键再写一遍，其余一个字不动。
fixture "$WORK/meow-rewrite"
cat > "$_plugin/config" <<'MR'
port: null
mixed-port: null
mode: null
log-level: warning
dns:
  enable: true
  listen: 0.0.0.0:1053
  enhanced-mode: fake-ip
  fake-ip-range: 198.18.0.1/16
  nameserver:
  - 223.5.5.5
proxies:
- type: direct
  name: n1
proxy-groups: []
rules:
- MATCH,DIRECT
rule-providers: null
subscriptions:
- name: s1
  url: http://127.0.0.1:1/sub.yaml
  interval: null
  last-updated: 1
tproxy-port: null
routing-mark: null
listeners:
- name: tproxy-tcp
  type: tproxy
  port: 7893
  listen: 0.0.0.0
  firewall: false
  max-connections: 4096
tun: null
max-connections: 4096
MR
printf 'proxy-mode: router\n' > "$_plugin/state"
run_meow mode >/dev/null
grep -qx 'routing-mark: 9527' "$_plugin/config" ||
    fail 'meow-rewrite: routing-mark: null 没补成 9527'
[ "$(grep -c '^listeners:' "$_plugin/config")" = 1 ] ||
    fail "meow-rewrite: listeners 变成了 $(grep -c '^listeners:' "$_plugin/config") 份"
[ "$(grep -c 'type: tproxy' "$_plugin/config")" = 1 ] ||
    fail 'meow-rewrite: tproxy 入口变成了两份'
grep -qx '  listen: 0.0.0.0:1053' "$_plugin/config" ||
    fail 'meow-rewrite: dns.listen 被改掉了'
grep -qE '^tproxy-port:[ \t]*[0-9]' "$_plugin/config" &&
    fail 'meow-rewrite: 顶层 tproxy-port 没置空'
grep -q 'name: s1' "$_plugin/config" || fail 'meow-rewrite: 订阅被弄丢了'
cp "$_plugin/config" "$WORK/meow-rewrite.once"
run_meow mode >/dev/null
cmp -s "$WORK/meow-rewrite.once" "$_plugin/config" ||
    fail 'meow-rewrite: 第二次跑改了文件'

# 用户自己定的 mark 要跟着走，别一律写成 9527。
fixture "$WORK/mark-keep"
printf 'mixed-port: 7890\nrouting-mark: 1234\nrules:\n  - MATCH,DIRECT\n' > "$_plugin/config"
printf 'proxy-mode: router\n' > "$_plugin/state"
run_meow mode >/dev/null
grep -qx 'routing-mark: 1234' "$_plugin/config" || fail 'mark-keep: 用户的 routing-mark 被改成 9527 了'

# pc 形态同理：null 也是没设，补回 9527；0 不算 mark，别往 iptables 里塞 --mark 0。
fixture "$WORK/pc-nullmark"
printf 'mixed-port: 7890\nrouting-mark: null\nrules:\n  - MATCH,DIRECT\n' > "$_plugin/config"
run_meow set-mode tun >/dev/null
run_meow mode >/dev/null
grep -qx 'routing-mark: 9527' "$_plugin/config" || fail 'pc-nullmark: null 没补成 9527'

# ---- 加订阅把本地 rules 顶掉了：启动时提示一句，但别猜着恢复 ----
# meow 保存订阅时用订阅里那份 rules 整体替换本地的；订阅没带 rules，本地规则就只剩
# "rules: []"，代理按兜底规则走。提示指向 meow 自己留的 config.yaml.bak。
fixture "$WORK/rules-wiped"
printf 'mixed-port: 7890\nsubscriptions:\n- name: s1\n  url: http://127.0.0.1:1/x.yaml\nrules: []\nrouting-mark: null\n' > "$_plugin/config"
printf 'mixed-port: 7890\nrules:\n  - RULE-SET,cn_domains,DIRECT\n  - MATCH,DIRECT\n' > "$_plugin/config.bak"
_out=$(run_meow mode 2>&1)
case "$_out" in
    *'rules 是空的'*) ;;
    *) fail 'rules-wiped: 规则被订阅顶掉了没提示' ;;
esac
grep -q 'rules: \[\]' "$_plugin/config" || fail 'rules-wiped: 提示归提示，不该动配置'
grep -q 'RULE-SET,cn_domains' "$_plugin/config" && fail 'rules-wiped: 不该自动拿备份恢复（猜的）'

# 没有订阅（或者备份里也没规则）就别乱叫。
fixture "$WORK/rules-empty-ok"
printf 'mixed-port: 7890\nrules: []\n' > "$_plugin/config"
_out=$(run_meow mode 2>&1)
case "$_out" in
    *'rules 是空的'*) fail 'rules-empty-ok: 没订阅也提示了' ;;
esac

# ---- cn_domains 的表是古典写法：behavior 不能是 domain ----
# domain 型把 "DOMAIN-SUFFIX,00cdn.com" 当成一个域名塞进 trie，永远匹配不到，日志还一声
# 不响；1.3.5 之前的模板就是这么写的，启动时就地改过来。别的 provider 一个字不动。
fixture "$WORK/cn-behavior"
cat > "$_plugin/config" <<'CB'
mixed-port: 7890
rule-providers:
  cn_domains:
    type: file
    behavior: domain
    path: cn_domains.yaml
  other:
    type: file
    behavior: domain
    path: other.yaml
rules:
  - RULE-SET,cn_domains,DIRECT
CB
run_meow mode >/dev/null
grep -qx '    behavior: classical' "$_plugin/config" ||
    fail 'cn-behavior: cn_domains 的 behavior 没改成 classical'
[ "$(grep -c 'behavior: domain' "$_plugin/config")" = 1 ] ||
    fail 'cn-behavior: 别人的 behavior 也被改了'
grep -q 'DOMAIN-SUFFIX,' "$_plugin/data/cn_domains.yaml" ||
    fail 'cn-behavior: 落下去的表不是古典写法'
grep -q '^    behavior: classical' "$REPO/plugins/meow/package/payload/config.yaml" ||
    fail 'cn-behavior: 模板里的 behavior 不是 classical'

# ---- 包里带的数据文件：第一次落地到数据目录，之后不覆盖 ----
# Country.mmdb 是 GEOIP 规则要用的库；cn_domains.yaml 是 file 型 rule-provider 的表，而
# file 型的 path 相对数据目录解析——留在安装目录里等于没带，meow 每次启动都报
# "rule-provider 'cn_domains': payload prefetch failed"，那条 RULE-SET 一直是空集。
fixture "$WORK/geodata"
run_meow mode >/dev/null
for _f in Country.mmdb cn_domains.yaml; do
    [ -f "$_plugin/data/$_f" ] || fail "geodata: 数据目录里没有 $_f"
    cmp -s "$_plugin/1.3.0/payload/$_f" "$_plugin/data/$_f" ||
        fail "geodata: 落下去的不是包里那份 $_f"
done
# meow 缺这两份库时会自己去 GitHub 下（GEOIP / GEOSITE / IP-ASN 规则一用到就下），设备
# 上游还没通时那一步会一直卡着：先放空占位，别让启动挂在下载上。
for _f in GeoLite2-ASN.mmdb geosite.dat; do
    [ -f "$_plugin/data/$_f" ] || fail "geodata: 没有 $_f 的空占位"
    [ -s "$_plugin/data/$_f" ] && fail "geodata: $_f 的占位不是空的"
done
echo 'updated-by-meow' > "$_plugin/data/Country.mmdb"
echo 'updated-by-meow' > "$_plugin/data/cn_domains.yaml"
echo 'real-asn' > "$_plugin/data/GeoLite2-ASN.mmdb"
run_meow mode >/dev/null
grep -qx 'updated-by-meow' "$_plugin/data/Country.mmdb" ||
    fail 'geodata: 覆盖了数据目录里已有的库'
grep -qx 'real-asn' "$_plugin/data/GeoLite2-ASN.mmdb" ||
    fail 'geodata: 覆盖了数据目录里已有的 ASN 库'

# ---- 放在 config 旁边（<插件目录>/<名字>）的库也算数：搬进数据目录 ----
# 手搭的那套把 config.yaml 和 geo 库、域名表放在同一个目录里，meow 在那儿自动找得到；
# 插件这边 meow 的家目录是 <插件目录>/data，所以启动时把它们搬进去。
fixture "$WORK/geodata-beside"
printf 'real-geosite\n' > "$_plugin/geosite.dat"
printf 'real-asn\n' > "$_plugin/GeoLite2-ASN.mmdb"
run_meow mode >/dev/null
grep -qx 'real-geosite' "$_plugin/data/geosite.dat" ||
    fail 'geodata: config 旁边的 geosite.dat 没搬进数据目录'
grep -qx 'real-asn' "$_plugin/data/GeoLite2-ASN.mmdb" ||
    fail 'geodata: config 旁边的 GeoLite2-ASN.mmdb 没搬进数据目录'
grep -q 'DOMAIN-SUFFIX,' "$_plugin/data/cn_domains.yaml" ||
    fail 'geodata: 包里的域名表没落地'

# ---- 老配置里残留的 @DATA@：启动时换成真数据目录（否则 GeoIP 库按字面路径打不开）----
fixture "$WORK/data-placeholder"
cat > "$_plugin/config" <<'OLD'
mixed-port: 7890
geodata:
  mmdb-path: '@DATA@/Country.mmdb'
  geosite-path: '@DATA@/geosite.dat'
rules:
  - GEOIP,lan,DIRECT,no-resolve
OLD
run_meow mode >/dev/null
grep -q '@DATA@' "$_plugin/config" && fail '@DATA@: 启动后配置里还剩占位符'
grep -qx "  mmdb-path: '$_plugin/data/Country.mmdb'" "$_plugin/config" ||
    fail "@DATA@: mmdb-path 没换成数据目录（$(grep mmdb-path "$_plugin/config")）"
grep -qx "  geosite-path: '$_plugin/data/geosite.dat'" "$_plugin/config" ||
    fail '@DATA@: geosite-path 没换成数据目录'
# 幂等：再跑一次不该再改文件。
cp "$_plugin/config" "$WORK/data-placeholder.once"
run_meow mode >/dev/null
cmp -s "$WORK/data-placeholder.once" "$_plugin/config" || fail '@DATA@: 第二次跑改了文件'

# ---- 迁移：1.2/1.3 的 meow.yaml 搬回 config，旧开关搬进 state ----
fixture "$WORK/migrate"
meow_config > "$_plugin/meow.yaml"
cat > "$_plugin/config" <<'OLD'
# meow 插件的设置：meow 不读这个文件，也不会改它。
proxy-mode: off
mixed-port: 7890
tproxy-port: 7893
routing-mark: 9527
lan-interface: br-lan
dns-port: 1053
OLD
run_meow mode >/dev/null
[ -f "$_plugin/meow.yaml" ] && fail 'migrate: meow.yaml 没搬走'
grep -q 'node-1' "$_plugin/config" || fail 'migrate: 订阅没搬进 config'
grep -qx 'proxy-mode: off' "$_plugin/state" || fail 'migrate: 旧开关没搬进 state'
grep -qx 'lan-interface: br-lan' "$_plugin/state" || fail 'migrate: lan-interface 没搬进 state'
grep -qx 'proxy-mode:.*' "$_plugin/config" && fail 'migrate: config 里还留着我们的键'

# ---- 迁移：更老的布局（开关和订阅写在一起）——把我们的键摘出去 ----
fixture "$WORK/migrate-old"
meow_config | sed -e 's/^mixed-port: 7891/mixed-port: 7891\nproxy-mode: pc\nlan-interface: br-wan/' > "$_plugin/config"
run_meow mode >/dev/null
grep -qx 'proxy-mode:.*' "$_plugin/config" && fail 'migrate-old: proxy-mode 还留在 config 里'
grep -qx 'lan-interface:.*' "$_plugin/config" && fail 'migrate-old: lan-interface 还留在 config 里'
grep -qx 'lan-interface: br-wan' "$_plugin/state" || fail 'migrate-old: lan-interface 没搬进 state'

# ---- 能力钩子：页面调 mode 读状态、切代理类型 ----
# 钩子就是一次普通执行（#342）：arguments 从 stdin 进，stdout 就是这次调用的结果。
# 这一半只用 shell，两个平台都跑。
fixture "$WORK/hook"
MEOW_SH="$_plugin/1.3.0/payload/bin/meow.sh"
hook_mode() {
    printf '%s\n' "$1" | SVCD_PLUGIN_DIR="$_plugin" sh "$MEOW_SH" hook mode
}

# 刚装好（还没写过 state）第一次读就该是缺省的 off。
if ! hook_mode '{}' > "$WORK/hook-read.out" 2> "$WORK/hook-read.err"; then
    fail "hook: 读状态退出非零（$(cat "$WORK/hook-read.err")）"
fi
grep -qF '"mode":"off"' "$WORK/hook-read.out" ||
    fail "hook: 读状态没回缺省的 off（$(cat "$WORK/hook-read.out")）"

if ! hook_mode '{"mode":"tun"}' > "$WORK/hook-set.out" 2> "$WORK/hook-set.err"; then
    fail "hook: 切代理类型退出非零（$(cat "$WORK/hook-set.err")）"
fi
grep -qF '"changing":true' "$WORK/hook-set.out" || fail 'hook: 切代理类型没回执'
[ "$(state_of)" = pc ] || fail 'hook: 切完没落到 state'

# 参数不认识就报错退出，state 一个字都不许动。
if hook_mode '{"mode":"bogus"}' >/dev/null 2>&1; then
    fail 'hook: 非法代理类型没被拒'
fi
grep -qx 'proxy-mode: pc' "$_plugin/state" || fail 'hook: 被拒的调用改动了 state'

# ---- 钩子与看护循环的交接：钩子只写 state，重启由前台看护循环做 ----
# 只在 Linux 上跑：MSYS/Cygwin 的 fork + 信号模型撑不起「后台看护 + 前台 wait」，
# 测出来的是仿真层的脾气，不是脚本的行为。CI 与 WSL 都走 Linux 这一支。
if [ "$(uname -s)" != Linux ]; then
    echo "skip: 看护循环的交接只在 Linux 上跑（当前 $(uname -s)）" >&2
else
fixture "$WORK/handoff"
MEOW_SH="$_plugin/1.3.0/payload/bin/meow.sh"
SVCD_PLUGIN_DIR="$_plugin" sh "$MEOW_SH" start > "$WORK/handoff.out" 2> "$WORK/handoff.err" &
SUPERVISOR=$!
n=0
while [ "$n" -lt 100 ] && [ ! -s "$_plugin/data/meow.pid" ]; do
    sleep 0.1
    n=$((n + 1))
done
[ -s "$_plugin/data/meow.pid" ] || fail 'handoff: 看护进程没把孩子拉起来'

hook_mode '{"mode":"tun"}' >/dev/null 2>&1 || fail 'handoff: 钩子切换失败'
n=0
while [ "$n" -lt 100 ] && ! grep -q 'proxy-mode off → pc' "$WORK/handoff.err"; do
    sleep 0.1
    n=$((n + 1))
done
grep -q 'proxy-mode off → pc' "$WORK/handoff.err" ||
    fail "handoff: 看护循环没按新形态重启（$(cat "$WORK/handoff.err")）"

kill -TERM "$SUPERVISOR" 2>/dev/null || true
wait "$SUPERVISOR" 2>/dev/null || true
[ -f "$_plugin/data/meow.pid" ] && fail 'handoff: 退出后还留着 pidfile'

# ---- 选中的形态起不来：自动降成 off，别让插件一直起不来 ----
# pc 形态（Linux 上是 tproxy-port + nft）在这台机器上起不来时，宿主重启撞的还是同一堵墙：
# 插件一直起不来，页面也就永远打不开。所以先把代理类型降成 off，起一个普通的 mixed-port，
# 修好之后在页面上重新选 tun。
fixture "$WORK/fallback"
MEOW_SH="$_plugin/1.3.0/payload/bin/meow.sh"
mkdir -p "$_plugin/data"
: > "$_plugin/data/fail-pc"
printf 'proxy-mode: pc\n' > "$_plugin/state"
SVCD_PLUGIN_DIR="$_plugin" sh "$MEOW_SH" start > "$WORK/fallback.out" 2> "$WORK/fallback.err" &
SUPERVISOR=$!
n=0
while [ "$n" -lt 100 ] && ! grep -q '降成 off' "$WORK/fallback.err"; do
    sleep 0.1
    n=$((n + 1))
done
kill -TERM "$SUPERVISOR" 2>/dev/null || true
wait "$SUPERVISOR" 2>/dev/null || true
grep -q '降成 off' "$WORK/fallback.err" ||
    fail "fallback: 降级没写进日志（$(cat "$WORK/fallback.err")）"
[ "$(state_of)" = off ] || fail "fallback: state 没降成 off（$(state_of)）"
grep -qx 'tproxy-port:.*' "$_plugin/config" && fail 'fallback: 降成 off 之后 tproxy-port 还在'
[ -f "$_plugin/data/meow.pid" ] && fail 'fallback: 退出后还留着 pidfile'
fi

# ---- 包里的 YACD 外壳：service worker 把它预缓存住了，改了文件就得改 revision ----
# sw.js 用 navigate 回退把 /ui/ 的导航交给预缓存的 index.html；revision 不变，老用户
# 浏览器里永远是旧外壳——比如那份没有 theme-bridge.js，顶部栏就不会跟着面板主题变。
yacd="$REPO/plugins/meow/package/payload/yacd"
revision=$(grep -o '"revision":"[0-9a-f]*","url":"index.html"' "$yacd/sw.js" | head -n 1 |
    sed -e 's/.*"revision":"\([0-9a-f]*\)".*/\1/')
[ -n "$revision" ] || fail 'yacd: sw.js 里找不到预缓存的 index.html'
if command -v md5sum >/dev/null 2>&1; then
    actual=$(md5sum "$yacd/index.html" | cut -d' ' -f1)
else
    actual=$(md5 -q "$yacd/index.html")   # macOS
fi
[ "$revision" = "$actual" ] ||
    fail "yacd: index.html 改过了，sw.js 的 revision 还是 $revision（应该是 $actual）"

echo "meow config layer: ok"
