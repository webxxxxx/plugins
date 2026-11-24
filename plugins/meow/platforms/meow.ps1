<#
meow：Windows 上的生命周期包装（Linux/macOS 用同目录的 meow.sh）。

由清单指过来（platforms/windows.json 把 exec/lifecycle 换成这个脚本、interpreter 换成
powershell；platforms/unix.json 给 6 个 unix 三元组，指向同目录的 meow.sh + sh）：
  powershell -NoProfile -ExecutionPolicy Bypass -File meow.ps1 start|stop|status|mode

和 meow.sh 的分工保持一致：这里只做「找路径、校验配置、起停进程、报状态」。
Windows 上没有 nftables / pf，本机透明代理只有 TUN（Wintun）一条路：配置里
写 tun: enable: true + dns.enhanced-mode: fake-ip，别写 tproxy-port。
网关形态（proxy-mode: router）依赖 iptables，Windows 不支持。

日志：stderr 由宿主收集；status 往 stdout 打一行 JSON。
#>
param(
    [Parameter(Position = 0)][string]$Action = 'status'
)

$ErrorActionPreference = 'Stop'

$HERE = Split-Path -Parent $MyInvocation.MyCommand.Path          # payload\bin
$ROOT = Split-Path -Parent $HERE                                 # payload\
$PLUGIN_ROOT = Split-Path -Parent (Split-Path -Parent $ROOT)     # <插件目录>\
if ($env:SVCD_DATA_DIR) { $BASE = $env:SVCD_DATA_DIR } else { $BASE = Join-Path $PLUGIN_ROOT '.data' }
$DATA = Join-Path $BASE 'meow'
$PIDFILE = Join-Path $DATA 'meow.pid'
$CONFIG = Join-Path $PLUGIN_ROOT 'config'
$DATA_CONFIG = Join-Path $DATA 'config.yaml'
$MEOW = Join-Path $HERE 'meow.exe'
if (-not (Test-Path -LiteralPath $MEOW)) { $MEOW = Join-Path $HERE 'meow' }

New-Item -ItemType Directory -Force -Path $DATA | Out-Null

function Write-Note([string]$Text) {
    [Console]::Error.WriteLine("meow: $Text")
}

function Write-Utf8([string]$Path, [string]$Text) {
    $utf8 = New-Object System.Text.UTF8Encoding($false)
    [System.IO.File]::WriteAllText($Path, $Text, $utf8)
}

# @DATA@ 是写进 YAML 里的：Windows 路径的反斜杠在双引号标量里会被当成转义
# （\P 直接让配置解析失败）。正斜杠两边都认，也不会让 YAML 翻脸。
function Get-DataPath {
    return ($DATA -replace '\\', '/')
}

# 和 meow.sh 的 prepare_config 等价：活动配置优先；源码 checkout 用模板生成副本。
function Prepare-Config {
    if (-not (Test-Path -LiteralPath $CONFIG -PathType Leaf)) {
        $template = Join-Path $PLUGIN_ROOT 'package\config\config'
        if (-not (Test-Path -LiteralPath $template -PathType Leaf)) {
            Write-Note 'config.yaml missing'
            exit 1
        }
        if (-not (Test-Path -LiteralPath $DATA_CONFIG -PathType Leaf)) {
            $text = [System.IO.File]::ReadAllText($template)
            Write-Utf8 $DATA_CONFIG $text.Replace('@DATA@', (Get-DataPath))
        }
        return $DATA_CONFIG
    }
    $text = [System.IO.File]::ReadAllText($CONFIG)
    if ($text.Contains('@DATA@')) {
        Write-Utf8 $CONFIG $text.Replace('@DATA@', (Get-DataPath))
    }
    return $CONFIG
}

# 顶层标量（行首 key: value），去掉行尾注释和引号。
function Get-ConfigValue([string]$Path, [string]$Key) {
    if (-not (Test-Path -LiteralPath $Path)) { return '' }
    $pattern = '^\s*' + [regex]::Escape($Key) + '\s*:\s*(.*)$'
    foreach ($line in [System.IO.File]::ReadAllLines($Path)) {
        if ($line -match $pattern) {
            $value = $Matches[1] -replace '\s*#.*$', ''
            return $value.Trim().Trim('"').Trim("'")
        }
    }
    return ''
}

# tun 块里的 enable:；没有 tun 块就是 false。
function Get-TunEnabled([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path)) { return $false }
    $inTun = $false
    foreach ($line in [System.IO.File]::ReadAllLines($Path)) {
        if ($line -match '^\S') {
            $inTun = ($line -match '^tun:\s*$')
            continue
        }
        if ($inTun -and $line -match '^\s+enable:\s*(\S+)') {
            return (($Matches[1] -replace '["'']', '') -eq 'true')
        }
    }
    return $false
}

function Get-Mode([string]$Path) {
    $mode = Get-ConfigValue $Path 'proxy-mode'
    if (-not $mode) { return 'pc' }
    return $mode
}

function Get-RunningPid {
    if (-not (Test-Path -LiteralPath $PIDFILE)) { return 0 }
    $raw = (Get-Content -LiteralPath $PIDFILE -ErrorAction SilentlyContinue | Select-Object -First 1)
    if (-not $raw) { return 0 }
    $parsed = 0
    if (-not [int]::TryParse($raw.Trim(), [ref]$parsed)) { return 0 }
    if (Get-Process -Id $parsed -ErrorAction SilentlyContinue) { return $parsed }
    return 0
}

function Test-Elevated {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = New-Object Security.Principal.WindowsPrincipal($identity)
    return $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}

function Quote-Arg([string]$Value) {
    if ($Value -match '[\s"]') {
        return '"' + ($Value -replace '"', '\"') + '"'
    }
    return $Value
}

function Start-Meow([string]$Config) {
    if (-not (Test-Path -LiteralPath $MEOW -PathType Leaf)) {
        Write-Note "missing $MEOW"
        return 1
    }
    $mode = Get-Mode $Config
    if ($mode -eq 'router') {
        Write-Note 'Windows 上不支持 proxy-mode: router —— 网关形态要 iptables，只有 Linux 有'
        return 1
    }
    if ($mode -ne 'pc') {
        Write-Note "proxy-mode 只能是 pc 或 router（现在是 '$mode'）"
        return 1
    }
    if (-not (Test-Elevated)) {
        Write-Note '需要管理员权限：TUN/Wintun 适配器要提权，否则 meow 会起不来'
    }
    $tproxy = Get-ConfigValue $Config 'tproxy-port'
    if ($tproxy -and $tproxy -ne 'null') {
        Write-Note 'Windows 不支持 tproxy-port（那是 Linux nftables / macOS pf 的路）；本机透明代理请用 tun:'
    }
    if (-not (Get-TunEnabled $Config)) {
        Write-Note '没有开启 tun: enable: true —— Windows 上本机透明代理只能走 TUN（Wintun），否则只有 mixed 代理'
    }

    $validate = & $MEOW -d $ROOT -f $Config -t 2>&1
    if ($LASTEXITCODE -ne 0) {
        Write-Note '配置校验失败（meow -t）：'
        foreach ($line in $validate) { [Console]::Error.WriteLine("  $line") }
        return 1
    }

    $meowArgs = @('-d', $ROOT, '-f', $Config, '--ext-ui', (Join-Path $ROOT 'yacd'))
    $quoted = ($meowArgs | ForEach-Object { Quote-Arg $_ }) -join ' '
    Write-Note "meow starting mode=pc (data=$ROOT, config=$Config)"
    # Start-Process -Wait 拿不到启动瞬间的 PID（而那正是 stop 要的），所以用
    # ProcessStartInfo：先拿 Id 写 pidfile，再阻塞等退出。
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = $MEOW
    $psi.Arguments = $quoted
    $psi.UseShellExecute = $false
    $psi.WorkingDirectory = $ROOT
    $proc = [System.Diagnostics.Process]::Start($psi)
    Write-Utf8 $PIDFILE ("{0}`n" -f $proc.Id)
    $proc.WaitForExit()
    $code = $proc.ExitCode
    # 被 stop 强杀时 Windows 给的是 0xFFFFFFFF（-1）；归一成 1，宿主日志好读。
    if ($code -lt 0) { $code = 1 }
    Remove-Item -LiteralPath $PIDFILE -ErrorAction SilentlyContinue
    Write-Note "meow exited rc=$code"
    return $code
}

function Stop-Meow {
    $procId = Get-RunningPid
    if (-not $procId) {
        Remove-Item -LiteralPath $PIDFILE -ErrorAction SilentlyContinue
        return
    }
    try { Stop-Process -Id $procId -Force -ErrorAction Stop } catch { }
    for ($i = 0; $i -lt 10; $i++) {
        if (-not (Get-Process -Id $procId -ErrorAction SilentlyContinue)) { break }
        Start-Sleep -Milliseconds 300
    }
    Remove-Item -LiteralPath $PIDFILE -ErrorAction SilentlyContinue
    Write-Output "stopped (pid $procId)"
}

# 活动配置优先，其次数据目录副本；都没有就是 $null（只读，不迁移）。
function Resolve-Config {
    if (Test-Path -LiteralPath $CONFIG -PathType Leaf) { return $CONFIG }
    if (Test-Path -LiteralPath $DATA_CONFIG -PathType Leaf) { return $DATA_CONFIG }
    return $null
}

function Show-Status {
    $cfg = Resolve-Config

    $mode = 'pc'
    $tun = $false
    $mixed = ''
    if ($cfg) {
        $mode = Get-Mode $cfg
        $tun = Get-TunEnabled $cfg
        $mixed = Get-ConfigValue $cfg 'mixed-port'
        if ($mixed -eq 'null') { $mixed = '' }
    }
    $pidValue = Get-RunningPid
    $tunJson = if ($tun) { 'true' } else { 'false' }
    $mixedJson = ''
    if ($mixed -match '^\d+$') { $mixedJson = ',"mixed-port":' + $mixed }
    # 两个分支只差前半句，信封只写一次。
    $head = if ($pidValue) { '"running":true,"pid":' + $pidValue } else { '"running":false' }
    Write-Output ('{' + $head + ',"mode":"' + $mode + '","controller":"127.0.0.1:9090","tun":' + $tunJson + $mixedJson + '}')
}

function Show-Mode {
    $cfg = Resolve-Config
    Write-Output "config: $cfg"
    if ($cfg) {
        Write-Output ("mode  : " + (Get-Mode $cfg))
        Write-Output ("tun   : " + (Get-TunEnabled $cfg))
    } else {
        Write-Output 'mode  : pc'
    }
    Write-Output "meow  : $MEOW"
    Write-Output "data  : $DATA"
}

switch ($Action) {
    'start' {
        $cfg = Prepare-Config
        $running = Get-RunningPid
        if ($running) {
            Write-Note "already running (pid $running)"
            exit 1
        }
        exit (Start-Meow $cfg)
    }
    'stop' {
        Stop-Meow
        exit 0
    }
    'status' {
        Show-Status
        exit 0
    }
    'mode' {
        Show-Mode
        exit 0
    }
    default {
        Write-Note "usage: meow.ps1 start|stop|status|mode"
        exit 2
    }
}
