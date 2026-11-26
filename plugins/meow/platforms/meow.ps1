<#
meow：Windows 上的生命周期包装（Linux/macOS 用同目录的 meow.sh）。

由清单指过来（platforms/windows.json 把 exec/lifecycle 换成这个脚本、interpreter 换成
powershell；platforms/unix.json 给 unix 三元组，指向同目录的 meow.sh + sh）：
  powershell -NoProfile -ExecutionPolicy Bypass -File meow.ps1 start|stop|status|mode

两个文件，各管各的：
  $PLUGIN_ROOT\state    我们自己的：只缓存代理类型（页面上的 off / tun / tproxy）。
  $PLUGIN_ROOT\data     运行数据：meow 的 geo 库、pid、-d 指过来的缓存。宿主升级时保留；
                        升级前的老位置在 <data>\meow，启动时搬进来。
  $PLUGIN_ROOT\config    meow 自己的配置（订阅、规则、代理组），meow 用它启动。启动时只改受管键
                          （mixed-port / tun / dns），其余原样保留。

Windows 上没有 nftables / pf，本机透明代理只有 TUN（Wintun）一条路：pc 形态会自动把
tun 和 dns.fake-ip 打开（这两样必须成对，缺一个 meow 就一条路由都不装）。网关形态
（proxy-mode: router）依赖 iptables，Windows 不支持。

三种形态就是页面 A 顶部的代理类型选择器（state 里没有 proxy-mode 时缺省 off）：
  off    不启动 tun，也不开 tproxy：meow 只当普通 HTTP/SOCKS 代理（mixed-port）。
         刚装好的缺省值：既没有订阅也没有可用的出口，这时要是在 tun 形态下起来，
         整机流量会被导进一个转不出去的内核，表现就是断网。
  tun    就是 proxy-mode: pc —— tun + fake-ip。
  tproxy 就是 proxy-mode: router —— 只有 Linux 有，Windows 明确拒绝。

start 是常驻的看护进程：meow 是自己的子进程，通道能力（页面调它）在同一进程里；
孩子意外退出时看门狗结束本进程，宿主会跑 stop 收尾再按策略重启或报错。

清单里 exec.ready 是 notify：宿主先发一条 svcd.ping，收到回复才算启动成功，之后这条路
才是能力通道（ready: exec 的 stdout 只进日志，页面调不到任何能力）。

日志：stderr 由宿主收集；status 往 stdout 打一行 JSON。
#>
param(
    [Parameter(Position = 0)][string]$Action = 'status'
)

$ErrorActionPreference = 'Stop'

$HERE = Split-Path -Parent $MyInvocation.MyCommand.Path          # payload\bin
$ROOT = Split-Path -Parent $HERE                                 # payload\
# 宿主给插件的三个目录之一是 SVCD_PLUGIN_DIR（安装目录）；手跑时没有它，就按脚本自己
# 的位置往上三层推（<插件>\<版本>\payload\bin\meow.ps1）。
$PLUGIN_ROOT = if ($env:SVCD_PLUGIN_DIR) { $env:SVCD_PLUGIN_DIR } else { Split-Path -Parent (Split-Path -Parent $ROOT) }
# 插件的数据只能落在插件目录里：<插件目录>\data。宿主升级、卸载（不带 --purge）都保留
# 它，meow 的 geo 库、pid、日志因此不会丢。
$DATA = Join-Path $PLUGIN_ROOT 'data'
$PIDFILE = Join-Path $DATA 'meow.pid'
$CONFIG = Join-Path $PLUGIN_ROOT 'config'            # meow 的配置：meow 用它启动，宿主的 editconfig 改它
$LEGACY_CONFIG = Join-Path $PLUGIN_ROOT 'meow.yaml'  # 1.2/1.3 的布局：meow 的配置曾放这里
$TEMPLATE = Join-Path $ROOT 'config.yaml'            # 包里的出厂模板
$STATE = Join-Path $PLUGIN_ROOT 'state'              # 我们自己的：只缓存代理类型
$MEOW = Join-Path $HERE 'meow.exe'
if (-not (Test-Path -LiteralPath $MEOW)) { $MEOW = Join-Path $HERE 'meow' }
$SDK = Join-Path (Join-Path $ROOT 'lib') 'SvcdPlugin.ps1'

# 1.2/1.3 把数据放在插件目录之外，两种老位置：默认根布局下是 <data>\meow（插件目录的
# 上上级同名目录），手跑时是 <插件目录>\.data\meow。第一次启动搬进来：geo 库跟着走。
function Move-LegacyData {
    if (Test-Path -LiteralPath $DATA) { return }
    $old = @(Join-Path (Split-Path -Parent (Split-Path -Parent $PLUGIN_ROOT)) 'meow')
    $old += (Join-Path $PLUGIN_ROOT '.data\meow')
    foreach ($path in $old) {
        if (-not (Test-Path -LiteralPath $path)) { continue }
        try {
            Move-Item -LiteralPath $path -Destination $DATA
            Write-Note "旧数据 $path 已搬到 $DATA"
        } catch {
            Write-Note "旧数据 $path 搬不动（$($_.Exception.Message)），就地不动"
        }
        return
    }
}

# 通道能力（页面 A 的代理类型选择器）用 SDK；sync 会把 sdk/powershell 的副本刷到 payload/lib。
if (Test-Path -LiteralPath $SDK) { . $SDK }

function Write-Note([string]$Text) {
    [Console]::Error.WriteLine("meow: $Text")
}

# 包里带了一份精简的 Country.mmdb（130 KB）。数据目录里还没有就放进去：很多路由器
# 直连不通 GitHub，meow 自己下 7 MB 的库会失败，而 GEOIP 规则没有库整份配置都起不来。
# 已经有了就不动——那可能是 meow 自己更新过的。
function Seed-GeoData {
    $source = Join-Path $ROOT 'Country.mmdb'
    if (-not (Test-Path -LiteralPath $source -PathType Leaf)) { return }
    $target = Join-Path $DATA 'Country.mmdb'
    if (Test-Path -LiteralPath $target -PathType Leaf) { return }
    try {
        Copy-Item -LiteralPath $source -Destination $target -Force
        Write-Note "已把包里的 Country.mmdb 放进 $DATA"
    } catch {
        Write-Note "Country.mmdb 放不进 $DATA（$($_.Exception.Message)）"
    }
}

# 迁移要打日志，所以放在 Write-Note 之后；之后所有路径都指进 <插件目录>\data。
Move-LegacyData
New-Item -ItemType Directory -Force -Path $DATA | Out-Null
Seed-GeoData

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

function Get-Mode {
    # 缺省 off：刚装好不接管流量。不能缺省 pc（tun）：没有订阅、没有可用出口时
    # TUN + fake-ip 会把整机流量导进一个转不出去的内核，表现就是断网。
    $mode = Get-ConfigValue $STATE 'proxy-mode'
    if (-not $mode) { return 'off' }
    if ($mode -notin @('off', 'pc', 'router')) {
        Write-Note "缓存里 proxy-mode 只能是 off、pc 或 router（现在是 '$mode'），按 off 处理"
        return 'off'
    }
    return $mode
}

# 页面上的三个选项 ↔ 实际形态：tun ↔ pc，tproxy ↔ router。
function Get-ModeLabel([string]$Mode) {
    switch ($Mode) {
        'pc' { return 'tun' }
        'router' { return 'tproxy' }
        default { return 'off' }
    }
}

function Get-ModeFromLabel([string]$Label) {
    switch ($Label) {
        'off' { return 'off' }
        'tun' { return 'pc' }
        'tproxy' { return 'router' }
        default { return '' }
    }
}

# 只改我们自己的 config：替换同名顶层键，别的行（注释、其它开关）原样保留。
function Set-StateValue([string]$Key, [string]$Value) {
    $lines = @()
    if (Test-Path -LiteralPath $STATE) { $lines = @([System.IO.File]::ReadAllLines($STATE)) }
    $pattern = '^' + [regex]::Escape($Key) + ':\s*'
    $out = @()
    $done = $false
    foreach ($line in $lines) {
        if ($line -match $pattern) {
            $out += ($Key + ': ' + $Value)
            $done = $true
        } else {
            $out += $line
        }
    }
    if (-not $done) { $out += ($Key + ': ' + $Value) }
    Write-Lines $STATE $out
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

# ---- meow 配置的小编辑 ----
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
# 我们的全部状态就这一份：代理类型。配置在 <插件目录>/config（meow 自己的那份）。
function Write-State([string]$Mode) {
    $text = @(
        '# meow 插件的状态：只记代理类型，页面上的 off / tun / tproxy 就是它。',
        '# meow 的配置在 <插件目录>/config（订阅、代理组、规则、端口都在那里）。',
        "proxy-mode: $Mode",
        ''
    ) -join "`r`n"
    Write-Utf8 $STATE $text
}

function Set-ModeCache([string]$Mode) {
    if (-not (Test-Path -LiteralPath $STATE -PathType Leaf)) { Write-State $Mode; return }
    Set-StateValue 'proxy-mode' $Mode
}

# 老布局里我们的键混在 meow 的配置里：搬进 state，再从配置里删掉。
function Migrate-State([string]$Path) {
    foreach ($key in 'proxy-mode', 'lan-interface', 'dns-port') {
        $value = Get-ConfigValue $Path $key
        if (-not $value -or $value -eq 'null') { continue }
        if ($key -eq 'proxy-mode') {
            if ($value -in @('off', 'pc', 'router')) { Set-ModeCache $value }
            else { Write-Note "迁移时忽略未知的 proxy-mode '$value'" }
        } else {
            Set-StateValue $key $value
        }
    }
}

# meow 不认识我们的键（proxy-mode 现在住在 state，另外两个是 router 的可选项）。
function Remove-OurKeys([string]$Path) {
    Remove-TopKey $Path 'proxy-mode'
    Remove-TopKey $Path 'lan-interface'
    Remove-TopKey $Path 'dns-port'
}

# 保证 <插件目录>/config 是 meow 的配置且干净（迁移同 meow.sh）。
function Prepare-Config {
    if (Test-Path -LiteralPath $LEGACY_CONFIG -PathType Leaf) {
        if (Test-Path -LiteralPath $CONFIG -PathType Leaf) { Migrate-State $CONFIG }
        Move-Item -LiteralPath $LEGACY_CONFIG -Destination $CONFIG -Force
        Write-Note "已把 meow.yaml 搬回 config（代理类型记在 $STATE）"
    }
    if (-not (Test-Path -LiteralPath $CONFIG -PathType Leaf)) {
        if (-not (Test-Path -LiteralPath $TEMPLATE -PathType Leaf)) {
            Write-Note 'payload/config.yaml missing'
            exit 1
        }
        $text = [System.IO.File]::ReadAllText($TEMPLATE)
        Write-Utf8 $CONFIG $text.Replace('@DATA@', (Get-DataPath))
    }
    # 老版本生成的配置里可能还留着 @DATA@：meow 会按字面路径去开 GeoIP 库，加载失败后
    # 整份配置都起不来（添加订阅的接口跟着报 400）。每次启动补一遍，幂等。
    $text = [System.IO.File]::ReadAllText($CONFIG)
    if ($text.Contains('@DATA@')) {
        Write-Utf8 $CONFIG $text.Replace('@DATA@', (Get-DataPath))
        Write-Note "配置里的 @DATA@ 已替换成 $(Get-DataPath)"
    }
    # 更老的布局把我们的键和订阅写在一起：先搬进 state，再从配置里摘掉。
    Migrate-State $CONFIG
    Remove-OurKeys $CONFIG
    if (-not (Test-Path -LiteralPath $STATE -PathType Leaf)) { Write-State (Get-Mode) }
}

# 按形态把受管键写进 meow 的配置：pc 形态要 tun + fake-ip（成对，缺一个就没有路由），
# off 形态把 tun 关掉、只留 mixed-port。
function Apply-Settings([string]$Mode) {
    # 端口以 meow 的配置为准（用户改成别的就跟着走），没有才用默认值。
    $mixed = Get-ConfigValue $CONFIG 'mixed-port'
    if (-not $mixed -or $mixed -eq 'null') { $mixed = '7890' }
    if ($mixed -notmatch '^\d+$') {
        Write-Note "mixed-port 不是端口：'$mixed'"
        return $false
    }
    Set-TopKey $CONFIG 'mixed-port' $mixed

    # 订阅里带进来的 tproxy 端口在 Windows 上没有意义。
    Remove-TopKey $CONFIG 'tproxy-port'
    Remove-TopKey $CONFIG 'routing-mark'

    switch ($Mode) {
        'off' {
            # 不接管：不启动 tun，也不开 tproxy，meow 只当普通 HTTP/SOCKS 代理。
            Set-BlockKey $CONFIG 'tun' 'enable' 'false'
            return $true
        }
        'pc' {
            Set-BlockKey $CONFIG 'tun' 'enable' 'true'
            Set-BlockKey $CONFIG 'tun' 'auto-route' 'true'
            Set-BlockKey $CONFIG 'tun' 'dns-hijack' '[any:53]'
            Set-BlockKey $CONFIG 'dns' 'enable' 'true'
            Set-BlockKey $CONFIG 'dns' 'enhanced-mode' 'fake-ip'
            Set-BlockKey $CONFIG 'dns' 'fake-ip-range' '198.18.0.1/16'
            return $true
        }
        default {
            Write-Note 'Windows 上不支持 tproxy（proxy-mode: router）—— 网关形态要 iptables，只有 Linux 有'
            return $false
        }
    }
}

# ---- 进程与网络 ----

function Get-RunningPid {
    if (-not (Test-Path -LiteralPath $PIDFILE)) { return 0 }
    $raw = (Get-Content -LiteralPath $PIDFILE -ErrorAction SilentlyContinue | Select-Object -First 1)
    if (-not $raw) { return 0 }
    $parsed = 0
    if (-not [int]::TryParse($raw.Trim(), [ref]$parsed)) { return 0 }
    $proc = Get-Process -Id $parsed -ErrorAction SilentlyContinue
    if (-not $proc) { return 0 }
    # 宿主负责终止：Windows 直接强杀整棵进程树，pidfile 会留下来，而那个 pid 可能已经
    # 被复用给别的进程。只看 Get-Process 会把别人的进程当成 meow（status 谎报 running、
    # 切代理类型时还会杀错人）。自己的子进程一定能读到 Path，读不到就当作不是我们的。
    $path = ''
    try { $path = $proc.Path } catch { }
    if ($path) {
        if ($path -ne $MEOW) { return 0 }
    } elseif ($proc.ProcessName -ne [System.IO.Path]::GetFileNameWithoutExtension($MEOW)) {
        return 0
    }
    return $parsed
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

# meow 在 Windows 上为了 TUN 的 DNS 劫持，会把**所有网卡的 DNS 改成 127.0.0.1 / ::1**，
# 只在它自己正常退出时（Drop）恢复。我们 stop 是强杀、关机更是直接终止进程，恢复不会
# 发生：网卡 DNS 停在 loopback，整机解析不了域名，重启也不会好（网卡 DNS 是持久设置），
# 只有“网络重置”能救。所以启动前记一份真实 DNS，停止后（和每次启动前）自己恢复。
$DNS_BACKUP = Join-Path $DATA 'dns-backup.json'

function Get-LoopbackDnsAdapters {
    $found = @()
    foreach ($family in 'IPv4', 'IPv6') {
        $found += Get-DnsClientServerAddress -AddressFamily $family -ErrorAction SilentlyContinue |
            Where-Object {
                $_.ServerAddresses -and
                (($_.ServerAddresses -contains '127.0.0.1') -or ($_.ServerAddresses -contains '::1'))
            }
    }
    return $found
}

# 启动前把各网卡真实的 DNS 存下来（跳过 loopback，那说明上一次没恢复干净）。
function Save-DnsState {
    $entries = @()
    foreach ($family in 'IPv4', 'IPv6') {
        $entries += Get-DnsClientServerAddress -AddressFamily $family -ErrorAction SilentlyContinue |
            Where-Object {
                $_.ServerAddresses -and
                -not ($_.ServerAddresses -contains '127.0.0.1') -and
                -not ($_.ServerAddresses -contains '::1')
            } |
            ForEach-Object {
                [pscustomobject]@{
                    Alias   = $_.InterfaceAlias
                    Family  = $family
                    Servers = @($_.ServerAddresses)
                }
            }
    }
    if ($entries.Count -eq 0) { return }
    $json = ConvertTo-Json -InputObject @($entries) -Depth 4
    Write-Utf8 $DNS_BACKUP $json
}

# 只处理被 meow 改成 loopback 的网卡：先重置回 DHCP（和 meow 自己的安全网一致），
# 再用我们存的那份精确恢复。没有 loopback 残留时什么都不做。
function Restore-DnsState {
    $stale = @(Get-LoopbackDnsAdapters)
    if ($stale.Count -eq 0) { return }
    foreach ($entry in $stale) {
        Set-DnsClientServerAddress -InterfaceAlias $entry.InterfaceAlias -ResetServerAddresses -ErrorAction SilentlyContinue
    }
    if (Test-Path -LiteralPath $DNS_BACKUP) {
        try {
            $backup = Get-Content -LiteralPath $DNS_BACKUP -Raw | ConvertFrom-Json
            foreach ($entry in @($backup)) {
                Set-DnsClientServerAddress -InterfaceAlias $entry.Alias -ServerAddresses @($entry.Servers) -ErrorAction SilentlyContinue
            }
        } catch { }
    }
    Clear-DnsClientCache -ErrorAction SilentlyContinue
    Write-Note '已把 meow 改过的网卡 DNS 恢复回去'
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

function Start-MeowChild {
    if (-not (Test-Path -LiteralPath $MEOW -PathType Leaf)) {
        Write-Note "missing $MEOW"
        return 0
    }
    if (-not (Test-Elevated)) {
        Write-Note '需要管理员权限：TUN/Wintun 适配器要提权，否则 meow 会起不来'
    }

    $validate = & $MEOW -d $DATA -f $CONFIG -t 2>&1
    if ($LASTEXITCODE -ne 0) {
        Write-Note '配置校验失败（meow -t）：'
        foreach ($line in $validate) { [Console]::Error.WriteLine("  $line") }
        return 0
    }

    $mode = Get-Mode
    if ($mode -eq 'off') {
        Write-Note '代理类型是 off —— 只开 mixed-port，不接管系统流量（页面上选 tun 才接管）'
    }
    # -d 是 meow 的家目录：缓存落在这里，指到插件自己的 data\，不要指 payload（版本目录）。
    $meowArgs = @('-d', $DATA, '-f', $CONFIG, '--ext-ui', (Join-Path $ROOT 'yacd'))
    $quoted = ($meowArgs | ForEach-Object { Quote-Arg $_ }) -join ' '
    Write-Note "meow starting mode=$mode (data=$DATA, config=$CONFIG)"
    # meow 的日志默认走 stdout，而 stdout 是我们的协议口：不能让它混进去，
    # 也不能让它继承通道的 stdin（那会跟宿主抢帧）。日志落到数据目录里的一份文件。
    $meowLog = Join-Path $DATA 'meow.out.log'
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = $MEOW
    $psi.Arguments = $quoted
    $psi.UseShellExecute = $false
    $psi.WorkingDirectory = $DATA
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardInput = $true
    $proc = [System.Diagnostics.Process]::Start($psi)
    if (-not $proc) {
        Write-Note '起 meow 失败'
        return 0
    }
    Write-Utf8 $PIDFILE ("{0}`n" -f $proc.Id)
    # 用一个 runspace 把它的 stdout 抄进日志文件：主线程要留在通道读循环里。
    $pump = [PowerShell]::Create()
    $null = $pump.AddScript({
        param($Reader, $Log)
        $writer = $null
        try {
            $writer = New-Object System.IO.StreamWriter($Log, $true, (New-Object System.Text.UTF8Encoding($false)))
            $writer.AutoFlush = $true
            while ($null -ne ($line = $Reader.ReadLine())) { $writer.WriteLine($line) }
        } catch { } finally { if ($writer) { $writer.Dispose() } }
    }).AddArgument($proc.StandardOutput).AddArgument($meowLog)
    $null = $pump.BeginInvoke()
    return $proc.Id
}

# 孩子自己死了（不是我们在切换）就结束本进程：宿主看到 exit，会先跑 stop 收尾、再按策略
# 重启或报错。PowerShell 没有 fork，主线程又阻塞在通道读上，所以用一个 runspace 盯 pidfile；
# 主动切换期间挂 hold 文件，看门狗就不把「孩子被我们杀掉」当成崩溃。
function Start-Watchdog {
    $hold = Join-Path $DATA 'switching'
    $script:WATCHER = [PowerShell]::Create()
    $null = $script:WATCHER.AddScript({
        param($PidFile, $HoldFile)
        Start-Sleep -Seconds 3
        while ($true) {
            Start-Sleep -Seconds 2
            if (Test-Path -LiteralPath $HoldFile) { continue }
            $text = ''
            try { $text = [System.IO.File]::ReadAllText($PidFile) } catch { }
            $child = 0
            if (-not [int]::TryParse($text.Trim(), [ref]$child)) { [Environment]::Exit(1) }
            if (-not (Get-Process -Id $child -ErrorAction SilentlyContinue)) { [Environment]::Exit(1) }
        }
    }).AddArgument($PIDFILE).AddArgument($hold)
    $null = $script:WATCHER.BeginInvoke()
}

# ---- 能力：代理类型（页面 A 的顶部选择器） ----

# 读：{"mode":"off|tun|tproxy","proxy-mode":"off|pc|router","running":bool,"secret":...}
# secret 是 meow 面板要的密钥（订阅里常常自带），页面把它传给 iframe，面板就不用每次手输。
function Get-ModeJson {
    $mode = Get-Mode
    $running = if (Get-RunningPid) { 'true' } else { 'false' }
    $mixed = ''
    $secret = ''
    if (Test-Path -LiteralPath $CONFIG -PathType Leaf) {
        $value = Get-ConfigValue $CONFIG 'mixed-port'
        if ($value -match '^\d+$') { $mixed = ',"mixed-port":' + $value }
        $secret = Get-ConfigValue $CONFIG 'secret'
    }
    $secret = $secret.Replace('\', '\\').Replace('"', '\"')
    return '{"mode":"' + (Get-ModeLabel $mode) + '","proxy-mode":"' + $mode +
        '","running":' + $running + ',"allowed":["off","tun"]' + ',"secret":"' + $secret + '"' + $mixed + '}'
}

# 写：把页面上的代理类型落成 proxy-mode，重启孩子。成功返回 $true。
function Switch-Mode([string]$Label) {
    $target = Get-ModeFromLabel $Label
    if (-not $target) {
        Write-Note "代理类型只能是 off / tun / tproxy（收到 '$Label'）"
        return $false
    }
    if ($target -eq 'router') {
        Write-Note 'Windows 上没有 tproxy：网关形态要 iptables，只有 Linux 有'
        return $false
    }
    if ((Get-Mode) -eq $target -and (Get-RunningPid)) { return $true }

    $hold = Join-Path $DATA 'switching'
    New-Item -ItemType File -Force -Path $hold | Out-Null
    try {
        Set-ModeCache $target
        $null = Stop-Meow
        if (-not (Apply-Settings (Get-Mode))) { return $false }
        $null = Clear-TunRoutes
        $null = Restore-DnsState
        $null = Save-DnsState
        return [bool](Start-MeowChild)
    } finally {
        Remove-Item -LiteralPath $hold -ErrorAction SilentlyContinue
    }
}

function Invoke-Capability($Name, $Arguments) {
    if ($Name -ne 'mode') { return @{ Text = "unknown tool: $Name"; Error = $true } }
    $label = ''
    # 宿主给的是 ConvertFrom-Json 的 PSCustomObject，测试里给的是 hashtable，两种都认。
    if ($Arguments) {
        if ($Arguments -is [System.Collections.IDictionary]) {
            if ($Arguments.Contains('mode')) { $label = [string]$Arguments['mode'] }
        } elseif ($Arguments.PSObject.Properties['mode']) {
            $label = [string]$Arguments.mode
        }
    }
    if (-not $label) { return (Get-ModeJson) }
    if (-not (Switch-Mode $label)) { return @{ Text = '切换代理类型失败（原因见日志）'; Error = $true } }
    return (Get-ModeJson)
}

# 收尾：进程已经不在时要做的事。强杀和断电都会留下这些东西（pidfile、auto-route 装的
# 路由、被 meow 改成 loopback 的网卡 DNS），所以 stop 钩子和每次 start 之前都跑一遍。
# 这里不发任何信号：宿主负责终止，pidfile 里的 pid 可能早就被复用了。
function Stop-Cleanup {
    Remove-Item -LiteralPath $PIDFILE -ErrorAction SilentlyContinue
    # 不留半套路由：宁可直连，也别把整机流量导进死网卡。
    Clear-TunRoutes
    # 强杀时 meow 的 DnsGuard 来不及恢复，网卡 DNS 会停在 loopback，整机就没法解析域名了。
    Restore-DnsState
}

# 结束孩子进程：只给「我们自己要重启它」的路径用（切代理类型、通道结束）。宿主停止
# 插件时不走这里——它先杀完整棵树，然后 stop 钩子跑 Stop-Cleanup。
function Stop-Meow {
    $procId = Get-RunningPid
    if (-not $procId) {
        Stop-Cleanup
        Write-Output 'not running'
        return
    }
    try { Stop-Process -Id $procId -Force -ErrorAction Stop } catch { }
    for ($i = 0; $i -lt 10; $i++) {
        if (-not (Get-Process -Id $procId -ErrorAction SilentlyContinue)) { break }
        Start-Sleep -Milliseconds 300
    }
    Stop-Cleanup
    Write-Output "stopped (pid $procId)"
}

function Show-Status {
    $mode = 'off'
    if (Test-Path -LiteralPath $STATE -PathType Leaf) { $mode = Get-Mode }

    $tun = $false
    $mixed = ''
    if (Test-Path -LiteralPath $CONFIG -PathType Leaf) {
        $tun = ((Get-BlockScalar $CONFIG 'tun' 'enable') -eq 'true')
        $mixed = Get-ConfigValue $CONFIG 'mixed-port'
        if ($mixed -eq 'null') { $mixed = '' }
    }
    $pidValue = Get-RunningPid
    $tunJson = if ($tun) { 'true' } else { 'false' }
    $mixedJson = ''
    if ($mixed -match '^\d+$') { $mixedJson = ',"mixed-port":' + $mixed }
    $proxyJson = ',"proxy":"' + (Get-ModeLabel $mode) + '"'
    # 两个分支只差前半句，信封只写一次。
    $head = if ($pidValue) { '"running":true,"pid":' + $pidValue } else { '"running":false' }
    Write-Output ('{' + $head + ',"mode":"' + $mode + '"' + $proxyJson + ',"controller":"127.0.0.1:9090","tun":' + $tunJson + $mixedJson + '}')
}

function Show-Mode {
    Write-Output "config: $STATE"
    Write-Output "meow  : $CONFIG"
    Write-Output ("mode  : " + (Get-Mode))
    if (Test-Path -LiteralPath $CONFIG -PathType Leaf) {
        Write-Output ("tun   : " + (Get-BlockScalar $CONFIG 'tun' 'enable'))
        Write-Output ("dns   : " + (Get-BlockScalar $CONFIG 'dns' 'enhanced-mode'))
        Write-Output ("mixed : " + (Get-ConfigValue $CONFIG 'mixed-port'))
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
        if (-not (Test-Path -LiteralPath $SDK)) {
            Write-Note "缺少通道 SDK（$SDK），页面上的代理类型开关不可用"
            exit 1
        }
        # 上一次被强杀留下的路由和 loopback DNS 先清掉，否则新的 tun 起来也接管不了，
        # 而且残留的 DNS 会让这台机器看起来“彻底断网”。
        $null = Clear-TunRoutes
        $null = Restore-DnsState
        # 记下启动前真实的 DNS，停止（或崩溃）后用它恢复。必须在 meow 改 DNS 之前。
        $null = Save-DnsState
        Remove-Item -LiteralPath (Join-Path $DATA 'switching') -ErrorAction SilentlyContinue
        if (-not (Start-MeowChild)) { exit 1 }
        Start-Watchdog
        # 常驻：从 stdin 收帧、往 stdout 回结果，直到宿主 shutdown（或看门狗结束本进程）。
        try {
            Invoke-SvcdPlugin -Handler ${function:Invoke-Capability} -Capabilities @('mode')
        } finally {
            $null = Stop-Meow
        }
        exit 0
    }
    'stop' {
        # 宿主负责终止（Windows 直接强杀整棵树），到这里时看护进程和 meow 都已经不在：
        # 只收尾，不杀进程。
        Stop-Cleanup
        Write-Output 'stopped'
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
