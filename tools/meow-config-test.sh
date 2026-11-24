#!/bin/sh
# meow 的配置层：proxy-mode 决定往 meow.yaml 里写什么。
#
# 只用一个假布局（伪 meow / 伪规则脚本）跑 meow.sh 的配置相关子命令，然后检查
# 生成出来的 meow.yaml：受管键对不对、订阅和注释有没有被动过、config 有没有被写。
# 用法：sh tools/meow-config-test.sh [仓库根]
set -eu

REPO=$(CDPATH= cd -- "${1:-$(dirname -- "$0")/..}" && pwd)
WORK=$(mktemp -d "${TMPDIR:-/tmp}/meow-config-test.XXXXXX")
trap 'rm -rf "$WORK"' EXIT

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

# 假插件目录：<root>/plugin/{config,meow.yaml} + <root>/plugin/1.2.0/payload/{bin,config.yaml}
fixture() {
    _root=$1
    rm -rf "$_root"
    _plugin="$_root/plugin"
    _bin="$_plugin/1.2.0/payload/bin"
    mkdir -p "$_bin" "$_plugin/1.2.0/payload/yacd"
    cp "$REPO/plugins/meow/platforms/meow.sh" "$_bin/meow.sh"
    cp "$REPO/plugins/meow/package/payload/config.yaml" "$_plugin/1.2.0/payload/config.yaml"
    cp "$REPO/plugins/meow/package/config/config" "$_plugin/config"
    printf '#!/bin/sh\nexit 0\n' > "$_bin/meow"
    printf '#!/bin/sh\necho "fw-stub: $*"\n' > "$_bin/meow-fw.sh"
    chmod +x "$_bin/meow" "$_bin/meow.sh" "$_bin/meow-fw.sh"
    cat > "$_plugin/meow.yaml" <<'YAML'
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

run_meow() {
    SVCD_DATA_DIR="$_plugin/.data" sh "$_plugin/1.2.0/payload/bin/meow.sh" "$@" 2>&1
}

# ---- pc：tproxy 端口和 mark 写进去，tun 关掉 ----
fixture "$WORK/pc"
cp "$_plugin/config" "$WORK/config.before"
run_meow mode >/dev/null
grep -qx 'mixed-port: 7890' "$_plugin/meow.yaml" || fail 'pc: mixed-port 没写进去'
grep -qx 'tproxy-port: 7893' "$_plugin/meow.yaml" || fail 'pc: tproxy-port 没写进去'
grep -qx 'routing-mark: 9527' "$_plugin/meow.yaml" || fail 'pc: routing-mark 没写进去'
grep -A1 -x 'tun:' "$_plugin/meow.yaml" | grep -qx '  enable: false' || fail 'pc: tun 没关掉'
grep -q 'node-1' "$_plugin/meow.yaml" || fail 'pc: 订阅内容被弄丢了'
cmp -s "$WORK/config.before" "$_plugin/config" || fail 'pc: config 被改了'

# 用户改 mixed-port，跟着走
sed -i 's/^mixed-port: 7890/mixed-port: 1080/' "$_plugin/config"
run_meow mode >/dev/null
grep -qx 'mixed-port: 1080' "$_plugin/meow.yaml" || fail 'pc: 改 config 的 mixed-port 没生效'

# ---- router：删掉顶层 tproxy-port，DNS 绑 0.0.0.0，自带一条 tproxy 入口 ----
fixture "$WORK/router"
sed -i 's/^proxy-mode: pc/proxy-mode: router/' "$_plugin/config"
run_meow fw status >/dev/null
grep -qx 'tproxy-port:.*' "$_plugin/meow.yaml" && fail 'router: 顶层 tproxy-port 应该被删掉'
grep -qx '  listen: 0.0.0.0:1053' "$_plugin/meow.yaml" || fail 'router: dns.listen 没改成 0.0.0.0:1053'
grep -qx '    type: tproxy' "$_plugin/meow.yaml" || fail 'router: 没补 tproxy 入口'
grep -qx '    firewall: false' "$_plugin/meow.yaml" || fail 'router: tproxy 入口没写 firewall: false'
grep -qx '  enable: false' "$_plugin/meow.yaml" || fail 'router: tun 没关掉'

# ---- 重复跑要幂等 ----
cp "$_plugin/meow.yaml" "$WORK/once.yaml"
run_meow fw status >/dev/null
cmp -s "$WORK/once.yaml" "$_plugin/meow.yaml" || fail 'router: 第二次跑改了文件'

# ---- 旧布局：订阅写在 config 里，第一次启动搬过去 ----
fixture "$WORK/migrate"
cp "$_plugin/meow.yaml" "$_plugin/config"
rm "$_plugin/meow.yaml"
run_meow mode >/dev/null
[ -f "$_plugin/meow.yaml" ] || fail 'migrate: meow.yaml 没生成'
grep -q 'node-1' "$_plugin/meow.yaml" || fail 'migrate: 订阅没迁过去'
grep -qx 'proxy-mode: pc' "$_plugin/config" || fail 'migrate: config 没恢复成开关文件'

echo "meow config layer: ok"
