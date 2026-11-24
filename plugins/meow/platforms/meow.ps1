<#
meow：Windows 上的生命周期包装（Linux/macOS 用同目录的 meow.sh）。

由清单指过来（platforms/windows.json 把 exec/lifecycle 换成这个脚本、interpreter 换成
powershell；platforms/unix.json 给 unix 三元组，指向同目录的 meow.sh + sh）：
  powershell -NoProfile -ExecutionPolicy Bypass -File meow.ps1 start|stop|status|mode

两个文件，各管各的：
  $PLUGIN_ROOT\config     我们的设置（manifest 的 config 指向它）。meow 不读也不改。
  $PLUGIN_ROOT\meow.yaml  meow 自己的配置（订阅、规则、代理组）。启动时只改受管键
                          （mixed-port / tun / dns），其余原样保留。

Windows 上没有 nftables / pf，本机透明代理只有 TUN（Wintun）一条路：pc 形态会自动把
tun 和 dns.fake-ip 打开（这两样必须成对，缺一个 meow 就一条路由都不装）。网关形态
（proxy-mode: router）依赖 iptables，Windows 不支持。

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
$CONFIG = Join-Path $PLUGIN_ROOT 'config'                        # 我们的设置
$MEOW_CONFIG = Join-Path $PLUGIN_ROOT 'meow.yaml'                # meow 的配置
$TEMPLATE = Join-Path $ROOT 'config.yaml'                        # 包里的模板
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

function Write-Lines([string]$Path, $Lines) {
    $utf8 = New-Object System.Text.UTF8Encoding($false)
    [System.IO.File]::WriteAllLines($Path, $Lines, $utf8)
}

# ---- 我们的设置 ----

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

function Get-OurValue([string]$Key, [string]$Default) {
    $value = Get-ConfigValue $CONFIG $Key
    if (-not $value -or $value -eq 'null') { return $Default }
    return $value
}

function Get-Mode {
    $mode = Get-ConfigValue $CONFIG 'proxy-mode'
    if (-not $mode) { return 'pc' }
    return $mode
}

# 读某个顶层块下的标量（缩进的那一行），去掉引号；没有就返回空串。
function Get-BlockScalar([string]$Path, [string]$Block, [string]$Key) {
    if (-not (Test-Path -LiteralPath $Path)) { return '' }
    $inBlock = $false
    $keyPattern = '^\s+' + [regex]::Escape($Key) + ':\s*(\S+)'
    $blockPattern = '^' + [regex]::Escape($Block) + ':\s*$'
    foreach ($line in [System.IO.File]::ReadAllLines($Path)) {
        if ($line -match '^[^\s#]') {
            $inBlock = ($line -match $blockPattern)
            continue
        }
        if ($inBlock -and $line -match $keyPattern) {
            return ($Matches[1] -replace '["'']', '').Trim()
        }
    }
    return ''
}

# ---- meow.yaml 的小编辑 ----
# 只做三件事：改顶层标量、删顶层键、改块内标量。注释、顺序、缩进都原样保留——
# 这份文件里有订阅和规则，不能被重新序列化。

function Set-TopKey([string]$Path, [string]$Key, [string]$Value) {
    $lines = New-Object System.Collections.Generic.List[string]
    $lines.AddRange([System.IO.File]::ReadAllLines($Path))
    $pattern = '^' + [regex]::Escape($Key) + ':\s*'
    $done = $false
    for ($i = 0; $i -lt $lines.Count; $i++) {
        if (-not $done -and $lines[$i] -match $pattern) {
            $lines[$i] = "${Key}: $Value"
            $done = $true
        }
    }
    if (-not $done) { $lines.Add("${Key}: $Value") }
    Write-Lines $Path $lines
}

# 删掉顶层键和它下面的缩进行（整块跟着走）。
function Remove-TopKey([string]$Path, [string]$Key) {
    $lines = New-Object System.Collections.Generic.List[string]
    $lines.AddRange([System.IO.File]::ReadAllLines($Path))
    $pattern = '^' + [regex]::Escape($Key) + ':\s*'
    $out = New-Object System.Collections.Generic.List[string]
    $skip = $false
    foreach ($line in $lines) {
        if ($line -match '^[^\s#]') {
            $skip = ($line -match $pattern)
            if (-not $skip) { $out.Add($line) }
            continue
        }
        if (-not $skip) { $out.Add($line) }
    }
    Write-Lines $Path $out
}

# 块里的键：有就替换，没有就补在块尾；连块都没有就整块追加。
function Set-BlockKey([string]$Path, [string]$Block, [string]$Key, [string]$Value) {
    $lines = New-Object System.Collections.Generic.List[string]
    $lines.AddRange([System.IO.File]::ReadAllLines($Path))
    $blockPattern = '^' + [regex]::Escape($Block) + ':\s*$'
    $keyPattern = '^[ \t]+' + [regex]::Escape($Key) + ':\s*'
    $inBlock = $false
    $seen = $false
    $done = $false
    for ($i = 0; $i -lt $lines.Count; $i++) {
        $line = $lines[$i]
        if ($line -match '^[^\s#]') {
            if ($inBlock -and -not $done) {
                $lines.Insert($i, "  ${Key}: $Value")
                $i++
                $done = $true
            }
            $inBlock = ($line -match $blockPattern)
            if ($inBlock) { $seen = $true }
            continue
        }
        if ($inBlock -and -not $done -and $line -match $keyPattern) {
            $lines[$i] = "  ${Key}: $Value"
            $done = $true
        }
    }
    if ($inBlock -and -not $done) { $lines.Add("  ${Key}: $Value"); $done = $true }
    if (-not $seen) {
        $lines.Add('')
        $lines.Add("${Block}:")
        $lines.Add("  ${Key}: $Value")
    }
    Write-Lines $Path $lines
}

# @DATA@ 是写进 YAML 里的：Windows 路径的反斜杠在双引号标量里会被当成转义
# （\P 直接让配置解析失败）。正斜杠两边都认，也不会让 YAML 翻脸。
function Get-DataPath {
    return ($DATA -replace '\\', '/')
}

# 这份文件看起来是不是 meow 的配置（旧布局把两者放在同一个文件里）。
function Test-MeowConfig([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path)) { return $false }
    foreach ($line in [System.IO.File]::ReadAllLines($Path)) {
        if ($line -match '^(proxies|proxy-groups|rules|mixed-port|tun|listeners):') { return $true }
    }
    return $false
}

function Write-OurConfig([string]$Mode) {
    $text = @(
        '# meow 插件的设置：meow 自己不读这个文件，也不会改它（订阅和规则在 meow.yaml 里）。',
        '# 改完 plugin restart meow 生效。',
        "proxy-mode: $Mode",
        'mixed-port: 7890',
        '',
        '# 本机 mixed 代理端口之外，还有透明代理端口（Windows 走 TUN，不看这些）。',
        'tproxy-port: 7893',
        'routing-mark: 9527',
        ''
    ) -join "`r`n"
    Write-Utf8 $CONFIG $text
}

# 保证 meow.yaml 存在：旧布局（订阅写在 config 里）就迁过来，否则用包里的模板。
function Prepare-Config {
    if (Test-Path -LiteralPath $MEOW_CONFIG -PathType Leaf) { return }
    if (Test-MeowConfig $CONFIG) {
        $mode = Get-Mode
        Copy-Item -LiteralPath $CONFIG -Destination $MEOW_CONFIG -Force
        Write-OurConfig $mode
        Write-Note "已把旧配置迁到 $MEOW_CONFIG，设置留在 $CONFIG"
        return
    }
    if (-not (Test-Path -LiteralPath $TEMPLATE -PathType Leaf)) {
        Write-Note 'payload/config.yaml missing'
        exit 1
    }
    $text = [System.IO.File]::ReadAllText($TEMPLATE)
    Write-Utf8 $MEOW_CONFIG $text.Replace('@DATA@', (Get-DataPath))
}

# 按形态把受管键写进 meow.yaml：pc 形态要 tun + fake-ip（成对，缺一个就没有路由）。
function Apply-Settings([string]$Mode) {
    $mixed = Get-OurValue 'mixed-port' '7890'
    Set-TopKey $MEOW_CONFIG 'mixed-port' $mixed

    if ($Mode -ne 'pc') {
        Write-Note 'Windows 上不支持 proxy-mode: router —— 网关形态要 iptables，只有 Linux 有'
        return $false
    }

    # 订阅里带进来的 tproxy-port 在 Windows 上没有意义。
    Remove-TopKey $MEOW_CONFIG 'tproxy-port'
    Remove-TopKey $MEOW_CONFIG 'routing-mark'

    Set-BlockKey $MEOW_CONFIG 'tun' 'enable' 'true'
    Set-BlockKey $MEOW_CONFIG 'tun' 'auto-route' 'true'
    Set-BlockKey $MEOW_CONFIG 'tun' 'dns-hijack' '[any:53]'
    Set-BlockKey $MEOW_CONFIG 'dns' 'enable' 'true'
    Set-BlockKey $MEOW_CONFIG 'dns' 'enhanced-mode' 'fake-ip'
    Set-BlockKey $MEOW_CONFIG 'dns' 'fake-ip-range' '198.18.0.1/16'
    return $true
}

# ---- 进程与网络 ----

function Get-RunningPid {
    if (-not (Test-Path -LiteralPath $PIDFILE)) { return 0 }
    $raw = (Get-Content -LiteralPath $PIDFILE -ErrorAction SilentlyContinue | Select-Object -First 1)
    if (-not $raw) { return 0 }
    $parsed = 0
    if (-not [int]::TryParse($raw.Trim(), [ref]$parsed)) { return 0 }
    if (Get-Process -Id $parsed -ErrorAction SilentlyContinue) { return $parsed }
    return 0
}

# 停掉 meow 时它是被强杀的，来不及撤 auto-route 装的路由；TUN 适配器是持久的，
# 这些路由会留下并把整机流量导进一个没人读的网卡（表现就是“停止后上不了网”）。
# 所以每次 start 之前和 stop 之后都清一遍。
function Clear-TunRoutes {
    $adapters = Get-NetAdapter -Name 'meow-tun*' -ErrorAction SilentlyContinue
    foreach ($adapter in $adapters) {
        Get-NetRoute -InterfaceIndex $adapter.ifIndex -ErrorAction SilentlyContinue |
            Remove-NetRoute -Confirm:$false -ErrorAction SilentlyContinue
    }
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

function Start-Meow {
    if (-not (Test-Path -LiteralPath $MEOW -PathType Leaf)) {
        Write-Note "missing $MEOW"
        return 1
    }
    if (-not (Test-Elevated)) {
        Write-Note '需要管理员权限：TUN/Wintun 适配器要提权，否则 meow 会起不来'
    }

    $validate = & $MEOW -d $ROOT -f $MEOW_CONFIG -t 2>&1
    if ($LASTEXITCODE -ne 0) {
        Write-Note '配置校验失败（meow -t）：'
        foreach ($line in $validate) { [Console]::Error.WriteLine("  $line") }
        return 1
    }

    $meowArgs = @('-d', $ROOT, '-f', $MEOW_CONFIG, '--ext-ui', (Join-Path $ROOT 'yacd'))
    $quoted = ($meowArgs | ForEach-Object { Quote-Arg $_ }) -join ' '
    Write-Note "meow starting mode=pc (data=$ROOT, config=$MEOW_CONFIG)"
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
    Clear-TunRoutes
    Write-Note "meow exited rc=$code"
    return $code
}

function Stop-Meow {
    $procId = Get-RunningPid
    if (-not $procId) {
        Remove-Item -LiteralPath $PIDFILE -ErrorAction SilentlyContinue
        Clear-TunRoutes
        Write-Output 'not running'
        return
    }
    try { Stop-Process -Id $procId -Force -ErrorAction Stop } catch { }
    for ($i = 0; $i -lt 10; $i++) {
        if (-not (Get-Process -Id $procId -ErrorAction SilentlyContinue)) { break }
        Start-Sleep -Milliseconds 300
    }
    Remove-Item -LiteralPath $PIDFILE -ErrorAction SilentlyContinue
    # 不留半套路由：宁可直连，也别把整机流量导进死网卡。
    Clear-TunRoutes
    Write-Output "stopped (pid $procId)"
}

function Show-Status {
    $mode = 'pc'
    if (Test-Path -LiteralPath $CONFIG -PathType Leaf) { $mode = Get-Mode }

    $tun = $false
    $mixed = ''
    if (Test-Path -LiteralPath $MEOW_CONFIG -PathType Leaf) {
        $tun = ((Get-BlockScalar $MEOW_CONFIG 'tun' 'enable') -eq 'true')
        $mixed = Get-ConfigValue $MEOW_CONFIG 'mixed-port'
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
    Write-Output "config: $CONFIG"
    Write-Output "meow  : $MEOW_CONFIG"
    Write-Output ("mode  : " + (Get-Mode))
    if (Test-Path -LiteralPath $MEOW_CONFIG -PathType Leaf) {
        Write-Output ("tun   : " + (Get-BlockScalar $MEOW_CONFIG 'tun' 'enable'))
        Write-Output ("dns   : " + (Get-BlockScalar $MEOW_CONFIG 'dns' 'enhanced-mode'))
        Write-Output ("mixed : " + (Get-ConfigValue $MEOW_CONFIG 'mixed-port'))
    }
    Write-Output "meow  : $MEOW"
    Write-Output "data  : $DATA"
}

switch ($Action) {
    'start' {
        Prepare-Config
        $mode = Get-Mode
        if (-not (Apply-Settings $mode)) { exit 1 }
        $running = Get-RunningPid
        if ($running) {
            Write-Note "already running (pid $running)"
            exit 1
        }
        # 上一次被强杀留下的路由先清掉，否则新的 tun 起来也接管不了。
        Clear-TunRoutes
        exit (Start-Meow)
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
        Prepare-Config
        # 先把受管键写一遍（和 start 一样），打印出来的就是 meow 实际要用的配置。
        $mode = Get-Mode
        if (-not (Apply-Settings $mode)) { exit 1 }
        Show-Mode
        exit 0
    }
    default {
        Write-Note "usage: meow.ps1 start|stop|status|mode"
        exit 2
    }
}
