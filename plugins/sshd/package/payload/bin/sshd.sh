#!/bin/sh
# sshd：dropbear + sftp-server 的生命周期包装。
#
# 宿主监督的就是这个脚本（exec.args = start），所以 start 必须 exec 到 dropbear，
# 不能像旧版那样 start 完就 exit（那样宿主只看到一个已经退出的进程）。
# stop 在进程退出之后跑，只做收尾（清 pidfile、确认端口释放）；status 按需探测。
set -u

HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
# 宿主给插件的三个目录之一是 SVCD_PLUGIN_DIR（安装目录）。手跑时没有它，就按脚本
# 自己的位置往上三层推：<插件>/<版本>/payload/bin/sshd.sh。
PLUGIN_ROOT=$(CDPATH= cd -- "${SVCD_PLUGIN_DIR:-$HERE/../../..}" && pwd)
# 插件的数据只能落在插件目录里：<插件目录>/data。宿主升级、卸载（不带 --purge）都
# 保留它，dropbear 的主机密钥因此不会变。
DATA="$PLUGIN_ROOT/data"

MULTI="$HERE/sshd"
SFTP_BIN="$HERE/sftp-server"
# dropbear 编译时的 sftp 子系统默认路径（build.sh 里的 localoptions.h）就是这条，
# 启动时把它指到包里真正的 sftp-server。放在 /etc 下是因为有的路由器只有 /etc 可写，
# /usr/libexec 根本不存在；两端用同一个路径，谁也不会找不到。
SFTP_LINK=/etc/sshd/sftp-server

PORT=2222
CONFIG="$PLUGIN_ROOT/config"
if [ -f "$CONFIG" ]; then
    configured=$(sed -n 's/^[[:space:]]*port[[:space:]]*[:=][[:space:]]*\([0-9][0-9]*\).*/\1/p' "$CONFIG" | head -n 1)
    [ -n "$configured" ] && PORT=$configured
fi

PIDFILE="$DATA/dropbear.pid"
KEY="$DATA/dropbear_host_key"

fail() { echo "sshd: $*" >&2; return 1; }

# 1.0 把数据放在插件目录之外，两种老位置：默认根布局下是 <data>/sshd（也就是插件
# 目录的上上级同名目录），手跑时是 <插件目录>/.data/sshd。第一次 start 搬进来。
migrate_legacy() {
    [ -d "$DATA" ] && return 0
    for old in "$PLUGIN_ROOT/../../sshd" "$PLUGIN_ROOT/.data/sshd"; do
        [ -d "$old" ] || continue
        if mv "$old" "$DATA" 2>/dev/null; then
            echo "sshd: 旧数据 $old 已搬到 $DATA" >&2
        fi
        return 0
    done
    return 0
}

link_applets() {
    mkdir -p "$DATA" || return 1
    ln -sf "$MULTI" "$DATA/dropbear" 2>/dev/null || return 1
    ln -sf "$MULTI" "$DATA/dropbearkey" 2>/dev/null || return 1
}

# sftp 子系统：把编译时那条路径指到包里的 sftp-server。sftp-server 要以登录用户身份
# 执行，所以顺便把权限放开到 0755（安装包里的可执行位可能只有 root 能读）。
# /etc 不可写（只读根、非 root）时只告警：下面还会导出 SVCD_SFTP_SERVER，
# dropbear 优先读它，功能不受影响。
ensure_sftp_link() {
    chmod 755 "$SFTP_BIN" 2>/dev/null || true
    mkdir -p "${SFTP_LINK%/*}" 2>/dev/null || true
    ln -sf "$SFTP_BIN" "$SFTP_LINK" 2>/dev/null || true
    [ -x "$SFTP_LINK" ] && return 0
    echo "sshd: $SFTP_LINK 指不过去（/etc 可写吗？），改用 SVCD_SFTP_SERVER" >&2
    return 0
}

# pidfile 里的 pid 是不是我们的 dropbear。宿主负责终止：强杀（SIGKILL）或断电之后
# pidfile 会留下来，而那个 pid 很可能已经被系统复用给别的进程——只看 kill -0 会把它
# 当成自己（status 谎报 running、start 说 already running）。
running() {
    _pid=$(cat "$PIDFILE" 2>/dev/null) || return 1
    [ -n "$_pid" ] || return 1
    kill -0 "$_pid" 2>/dev/null || return 1
    # 有 /proc 就按命令行认（NUL 分隔，grep -F 直接当字节串搜）。
    if [ -r "/proc/$_pid/cmdline" ]; then
        grep -qF -- "$DATA/dropbear" "/proc/$_pid/cmdline" 2>/dev/null
        return
    fi
    # macOS 没有 /proc：退回 ps。ps 也没有就只能信 kill -0，和以前一样。
    _argv=$(ps -o args= -p "$_pid" 2>/dev/null) || return 0
    case "$_argv" in
    *"$DATA/dropbear"*) return 0 ;;
    esac
    return 1
}

# /proc/net/tcp 里本地端口是十六进制，状态 0A = LISTEN（busybox 不一定有 netstat）。
port_listening() {
    _hex=$(printf '%04X' "$1" 2>/dev/null) || return 1
    grep -qE "^ *[0-9]+: [0-9A-F]+:$_hex [0-9A-F]+:[0-9A-F]+ 0A" /proc/net/tcp 2>/dev/null
}

do_start() {
    migrate_legacy
    link_applets || fail "不能创建 dropbear/dropbearkey 符号链接"
    [ -x "$SFTP_BIN" ] || fail "缺少 $SFTP_BIN"
    ensure_sftp_link
    if running; then
        echo "already running (pid $(cat "$PIDFILE"), port $PORT)"
        # 宿主监督的就是这个脚本；已经有人占着端口时不要假装成功。
        return 1
    fi
    if [ ! -f "$KEY" ]; then
        "$DATA/dropbearkey" -t ed25519 -f "$KEY" >/dev/null 2>&1 || fail "生成主机密钥失败"
    fi
    echo $$ > "$PIDFILE"
    echo "sshd starting on port $PORT" >&2
    # dropbear 的 sftp 子系统路径由补丁优先读这个环境变量；值来自包内相对路径。
    SVCD_SFTP_SERVER="$SFTP_BIN"
    export SVCD_SFTP_SERVER
    # -F 前台运行，-P 写 pid（dropbear 退出时会清理），-r 主机密钥。
    exec "$DATA/dropbear" -F -p "$PORT" -r "$KEY" -P "$PIDFILE"
}

# 宿主负责终止（Unix 进程组 SIGTERM → 10 秒 → SIGKILL），stop 跑到这里时 dropbear 已经
# 不在了：它只收尾，不杀进程。pidfile 可能是强杀留下的，那个 pid 甚至已经被复用给别的
# 进程，所以只删不按它发信号；端口还听着说明不是我们留的进程，报一声。
do_stop() {
    rm -f "$PIDFILE"
    if port_listening "$PORT"; then
        echo "sshd: 端口 $PORT 还在监听，但不是这个插件留下的进程" >&2
    fi
    echo "stopped (port $PORT)"
}

do_status() {
    if running; then
        printf '{"running":true,"pid":%s,"port":%s}\n' "$(cat "$PIDFILE")" "$PORT"
    else
        printf '{"running":false,"port":%s}\n' "$PORT"
    fi
}

case "${1:-}" in
start)  do_start ;;
stop)   do_stop ;;
status) do_status ;;
*)      echo "usage: $0 start|stop|status" >&2; exit 2 ;;
esac
