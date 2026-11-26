<#
meow 的配置层（Windows 版）：
  <插件目录>\config  meow 自己的配置（订阅/规则/端口），meow 就是用它启动的
  <插件目录>\state   我们自己的，只缓存代理类型（off / pc / router）

和 tools/meow-config-test.sh 对应：meow.ps1 只能在 PowerShell 里跑，所以只取它的函数定义
（按语法树抽取，不执行脚本最后的 switch），在假布局上调用，再检查配置与状态。

用法：powershell -File tools/meow-config-test.ps1 [-Repo <仓库根>]
#>
param([string]$Repo = (Join-Path $PSScriptRoot '..'))

$ErrorActionPreference = 'Stop'
$Repo = (Resolve-Path $Repo).Path
$Script = Join-Path $Repo 'plugins/meow/platforms/meow.ps1'
$Work = Join-Path ([System.IO.Path]::GetTempPath()) ("meow-config-test-" + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Force -Path $Work | Out-Null

function Fail([string]$Message) {
    Write-Error "FAIL: $Message"
    exit 1
}

# Windows PowerShell 5.1 没有 utf8NoBOM 这个编码名，统一自己写。
function Write-Text([string]$Path, [string]$Text) {
    [System.IO.File]::WriteAllText($Path, $Text, (New-Object System.Text.UTF8Encoding($false)))
}

function Read-Text([string]$Path) {
    return [System.IO.File]::ReadAllText($Path)
}

# 只取函数定义，脚本末尾的 switch 不执行（那会去启动 meow）。
# 返回 scriptblock，由调用方在脚本作用域点源进来——在函数里点源只会定义在函数作用域内。
function Get-FunctionBlocks([string]$Path) {
    $errors = $null
    $ast = [System.Management.Automation.Language.Parser]::ParseFile($Path, [ref]$null, [ref]$errors)
    if ($errors -and $errors.Count) {
        $errors | ForEach-Object { Write-Error "parse error: $($_.Message)" }
        exit 1
    }
    $blocks = @()
    foreach ($function in $ast.FindAll({ param($node) $node -is [System.Management.Automation.Language.FunctionDefinitionAst] }, $true)) {
        $blocks += [scriptblock]::Create($function.Extent.Text)
    }
    return $blocks
}

function Get-MeowConfig {
    @(
        '# 订阅配置（示例）'
        'mixed-port: 7891'
        'proxies:'
        '  - name: node-1'
        '    type: direct'
        'tun:'
        '  enable: false'
        'dns:'
        '  enable: true'
        '  listen: 127.0.0.1:5353'
        'secret: p@ss word'
        'rules:'
        '  - MATCH,DIRECT'
    ) -join "`n"
}

# 假插件目录，并把脚本里那些路径变量指过去。
function New-Fixture([string]$Name) {
    $plugin = Join-Path $Work $Name
    $payload = Join-Path $plugin '1.3.0/payload'
    New-Item -ItemType Directory -Force -Path (Join-Path $payload 'bin') | Out-Null
    New-Item -ItemType Directory -Force -Path (Join-Path $payload 'yacd') | Out-Null
    New-Item -ItemType Directory -Force -Path (Join-Path $payload 'lib') | Out-Null
    Copy-Item (Join-Path $Repo 'plugins/meow/package/payload/config.yaml') (Join-Path $payload 'config.yaml')
    Copy-Item (Join-Path $Repo 'plugins/meow/package/payload/Country.mmdb') (Join-Path $payload 'Country.mmdb')
    Write-Text (Join-Path $plugin 'config') (Get-MeowConfig)

    $script:PLUGIN_ROOT = $plugin
    $script:ROOT = $payload
    $script:DATA = Join-Path $plugin '.data/meow'
    $script:PIDFILE = Join-Path $DATA 'meow.pid'
    $script:CONFIG = Join-Path $plugin 'config'
    $script:LEGACY_CONFIG = Join-Path $plugin 'meow.yaml'
    $script:STATE = Join-Path $plugin 'state'
    $script:TEMPLATE = Join-Path $payload 'config.yaml'
    $script:MEOW = Join-Path $payload 'bin/meow.exe'
    New-Item -ItemType Directory -Force -Path $DATA | Out-Null
    return $plugin
}

function Expect([string]$Path, [string]$Key, [string]$Value, [string]$Label) {
    $actual = Get-ConfigValue $Path $Key
    if ($actual -ne $Value) { Fail "$Label`: $Key 期望 '$Value'，实际 '$actual'" }
}

foreach ($block in (Get-FunctionBlocks $Script)) { . $block }

# ---- 缺省 off：刚装好不接管流量，tun 关掉、tproxy 不开 ----
# 缺省要是 pc（tun），没有订阅、没有可用出口时整机流量会被导进一个转不出去的内核，
# 表现就是断网。
$plugin = New-Fixture 'default'
Prepare-Config
Apply-Settings (Get-Mode) | Out-Null
if ((Get-Mode) -ne 'off') { Fail 'default: state 缺省不是 off' }
if ((Read-Text $STATE) -notmatch '(?m)^proxy-mode: off') { Fail 'default: state 里没写下 off' }
if ((Get-BlockScalar $CONFIG 'tun' 'enable') -ne 'false') { Fail 'default: tun 没关掉' }
if ((Get-BlockScalar $CONFIG 'dns' 'enhanced-mode') -eq 'fake-ip') { Fail 'default: 缺省不该开 fake-ip' }
Expect $CONFIG 'mixed-port' '7891' 'default'
Expect $CONFIG 'tproxy-port' '' 'default'
if ((Read-Text $CONFIG) -notmatch 'node-1') { Fail 'default: 订阅内容被弄丢了' }
if ((Read-Text $CONFIG) -match 'proxy-mode:') { Fail 'default: state 的键写进了配置' }

# ---- pc（页面上的 tun）：tun + fake-ip，用户写的 mixed-port 不动，代理类型落进 state ----
$plugin = New-Fixture 'pc'
Prepare-Config
Set-ModeCache 'pc'
Apply-Settings (Get-Mode) | Out-Null
if ((Get-BlockScalar $CONFIG 'tun' 'enable') -ne 'true') { Fail 'pc: tun.enable 没打开' }
if ((Get-BlockScalar $CONFIG 'dns' 'enhanced-mode') -ne 'fake-ip') { Fail 'pc: dns.enhanced-mode 不是 fake-ip' }
Expect $CONFIG 'mixed-port' '7891' 'pc'
Expect $CONFIG 'tproxy-port' '' 'pc'
if ((Read-Text $CONFIG) -notmatch 'node-1') { Fail 'pc: 订阅内容被弄丢了' }
if ((Get-Mode) -ne 'pc') { Fail 'pc: state 里没有 proxy-mode' }
if ((Read-Text $CONFIG) -match 'proxy-mode:') { Fail 'pc: state 的键写进了配置' }

# ---- 用户改 mixed-port，跟着走；重复跑幂等 ----
Write-Text $CONFIG ((Read-Text $CONFIG) -replace '(?m)^mixed-port: 7891', 'mixed-port: 1080')
Apply-Settings (Get-Mode) | Out-Null
Expect $CONFIG 'mixed-port' '1080' 'pc 改端口'
$before = Read-Text $CONFIG
Apply-Settings (Get-Mode) | Out-Null
if ((Read-Text $CONFIG) -ne $before) { Fail 'pc: 第二次跑改了文件' }

# ---- off：不启动 tun、也不开 tproxy，只剩 mixed-port ----
if (-not (Apply-Settings 'off')) { Fail 'off: Apply-Settings 失败' }
if ((Get-BlockScalar $CONFIG 'tun' 'enable') -ne 'false') { Fail 'off: tun 没关掉' }
Expect $CONFIG 'mixed-port' '1080' 'off'
Expect $CONFIG 'tproxy-port' '' 'off'
if ((Read-Text $CONFIG) -notmatch 'node-1') { Fail 'off: 订阅被弄丢了' }
if (Apply-Settings 'router') { Fail 'off: Windows 上 router 应该被拒' }

# ---- 页面上的代理类型：off / tun / tproxy ↔ off / pc / router ----
$plugin = New-Fixture 'modes'
if ((Get-ModeFromLabel 'tun') -ne 'pc') { Fail 'modes: tun 没映射成 pc' }
if ((Get-ModeFromLabel 'tproxy') -ne 'router') { Fail 'modes: tproxy 没映射成 router' }
if ((Get-ModeFromLabel 'off') -ne 'off') { Fail 'modes: off 没映射成 off' }
if ((Get-ModeFromLabel 'turbo') -ne '') { Fail 'modes: 非法标签应该返回空' }
if ((Get-ModeLabel 'pc') -ne 'tun') { Fail 'modes: pc 没映射回 tun' }

Set-ModeCache 'off'
if ((Get-Mode) -ne 'off') { Fail 'modes: Set-ModeCache 没写进 state' }
$twice = Read-Text $STATE
Set-ModeCache 'off'
if ((Read-Text $STATE) -ne $twice) { Fail 'modes: Set-ModeCache 不幂等' }
if ((Read-Text $STATE) -notmatch '# meow 插件的状态') { Fail 'modes: state 没有说明文字' }
if ((Read-Text $CONFIG) -match 'proxy-mode:') { Fail 'modes: 代理类型写进了 meow 的配置' }

# ---- 能力：读状态（带 secret 给面板用）、拒绝不支持的标签 ----
$json = Get-ModeJson
if ($json -notmatch '"mode":"off"') { Fail "capabilities: Get-ModeJson 没报出代理类型（$json）" }
if ($json -notmatch '"running":false') { Fail "capabilities: running 不对（$json）" }
if ($json -notmatch '"allowed":\["off","tun"\]') { Fail "capabilities: allowed 不对（$json）" }
if ($json -notmatch '"secret":"p@ss word"') { Fail "capabilities: secret 没带给面板（$json）" }

$read = Invoke-Capability 'mode' @{}
if ($read -notmatch '"mode":"off"') { Fail "capabilities: mode 能力读状态不对（$read）" }
$refused = Invoke-Capability 'mode' @{ mode = 'tproxy' }
if (-not ($refused -is [hashtable] -and $refused.Error)) { Fail 'capabilities: Windows 上 tproxy 应该被拒' }
if ((Get-Mode) -ne 'off') { Fail 'capabilities: 被拒的 tproxy 改了 state' }
$bad = Invoke-Capability 'mode' @{ mode = 'turbo' }
if (-not ($bad -is [hashtable] -and $bad.Error)) { Fail 'capabilities: 非法标签应该被拒' }

Write-Output 'meow proxy mode capability (windows): ok'

# ---- 包里带的 Country.mmdb：第一次落地到数据目录，之后不覆盖 ----
$plugin = New-Fixture 'geodata'
Seed-GeoData
$target = Join-Path $DATA 'Country.mmdb'
if (-not (Test-Path -LiteralPath $target)) { Fail 'geodata: 数据目录里没有 Country.mmdb' }
if ((Get-Item -LiteralPath $target).Length -ne (Get-Item -LiteralPath (Join-Path $ROOT 'Country.mmdb')).Length) { Fail 'geodata: 落下去的不是包里那份' }
Write-Text $target 'updated-by-meow'
Seed-GeoData
if ((Read-Text $target) -ne 'updated-by-meow') { Fail 'geodata: 覆盖了已有的库' }

# ---- 老配置里残留的 @DATA@：启动时换成真数据目录（否则 GeoIP 库按字面路径打不开）----
$plugin = New-Fixture 'data-placeholder'
Write-Text $CONFIG (@(
    'mixed-port: 7890'
    'geodata:'
    "  mmdb-path: '@DATA@/Country.mmdb'"
    "  geosite-path: '@DATA@/geosite.dat'"
    'rules:'
    '  - GEOIP,lan,DIRECT,no-resolve'
) -join "`n")
Prepare-Config
if ((Read-Text $CONFIG) -match '@DATA@') { Fail '@DATA@: 启动后配置里还剩占位符' }
$data = (Get-DataPath)
if ((Read-Text $CONFIG) -notmatch [regex]::Escape("mmdb-path: '${data}/Country.mmdb'")) {
    Fail "@DATA@: mmdb-path 没换成数据目录（$((Read-Text $CONFIG) -split "`n" | Where-Object { $_ -match 'mmdb-path' })）"
}
$once = Read-Text $CONFIG
Prepare-Config
if ((Read-Text $CONFIG) -ne $once) { Fail '@DATA@: 第二次跑改了文件' }

# ---- 迁移：1.2/1.3 的 meow.yaml 搬回 config，旧开关搬进 state ----
$plugin = New-Fixture 'migrate'
Write-Text (Join-Path $plugin 'meow.yaml') (Get-MeowConfig)
Write-Text $CONFIG (@(
    '# meow 插件的设置：meow 不读这个文件，也不会改它。'
    'proxy-mode: off'
    'mixed-port: 7890'
    'tproxy-port: 7893'
    'routing-mark: 9527'
    'lan-interface: br-lan'
    'dns-port: 1053'
) -join "`n")
Prepare-Config
if (Test-Path (Join-Path $plugin 'meow.yaml')) { Fail 'migrate: meow.yaml 没搬走' }
if ((Read-Text $CONFIG) -notmatch 'node-1') { Fail 'migrate: 订阅没搬进 config' }
if ((Get-Mode) -ne 'off') { Fail 'migrate: 旧代理类型没搬进 state' }
if ((Get-ConfigValue $STATE 'lan-interface') -ne 'br-lan') { Fail 'migrate: lan-interface 没搬进 state' }
if ((Read-Text $CONFIG) -match 'proxy-mode:') { Fail 'migrate: 配置里还留着我们的键' }

# ---- 迁移：更老的布局（开关和订阅写在一起）----
$plugin = New-Fixture 'migrate-old'
Write-Text $CONFIG ((Get-MeowConfig) -replace '(?m)^mixed-port: 7891', "mixed-port: 7891`nproxy-mode: off`nlan-interface: br-wan")
Prepare-Config
if ((Read-Text $CONFIG) -match 'proxy-mode:') { Fail 'migrate-old: proxy-mode 还留在 config 里' }
if ((Read-Text $CONFIG) -match 'lan-interface:') { Fail 'migrate-old: lan-interface 还留在 config 里' }
if ((Get-ConfigValue $STATE 'lan-interface') -ne 'br-wan') { Fail 'migrate-old: lan-interface 没搬进 state' }

# ---- DNS：meow 把网卡 DNS 改成 127.0.0.1，强杀后没人恢复，stop 要自己修 ----
# 用两个同名函数把 cmdlet 换掉：既能测保存，也能测恢复，而且不碰本机网络。
$script:POLUTED = $false
$script:DNS_CALLS = New-Object System.Collections.Generic.List[string]

function Get-DnsClientServerAddress {
    [CmdletBinding()] param([string]$AddressFamily)
    $servers = if ($script:POLUTED) { @('127.0.0.1') } else { @('192.168.1.1') }
    [pscustomobject]@{ InterfaceAlias = 'eth0'; AddressFamily = $AddressFamily; ServerAddresses = $servers }
}

function Set-DnsClientServerAddress {
    [CmdletBinding()] param([string]$InterfaceAlias, [switch]$ResetServerAddresses, [string[]]$ServerAddresses)
    if ($ResetServerAddresses) {
        $script:DNS_CALLS.Add("reset:$InterfaceAlias")
    } else {
        $script:DNS_CALLS.Add("set:$InterfaceAlias=$($ServerAddresses -join ',')")
    }
}

function Clear-DnsClientCache { [CmdletBinding()] param() }

$plugin = New-Fixture 'dns'
$script:DNS_BACKUP = Join-Path $DATA 'dns-backup.json'

Save-DnsState
if (-not (Test-Path $script:DNS_BACKUP)) { Fail 'dns: 没保存备份' }
if ((Read-Text $script:DNS_BACKUP) -match '127.0.0.1') { Fail 'dns: 备份里不该有 loopback' }

$script:DNS_CALLS.Clear()
Restore-DnsState
if ($script:DNS_CALLS.Count -ne 0) { Fail 'dns: 没被污染时不该动网卡' }

$script:POLUTED = $true
$script:DNS_CALLS.Clear()
Restore-DnsState
$calls = $script:DNS_CALLS -join ' '
if ($calls -notmatch 'reset:eth0') { Fail "dns: 没把 loopback 重置回 DHCP（$calls）" }
if ($calls -notmatch 'set:eth0=192\.168\.1\.1') { Fail "dns: 没按备份恢复（$calls）" }

Write-Output 'meow dns guard (windows): ok'

# ---- pidfile：宿主强杀之后 pidfile 会留下，pid 被复用时不能当成 meow；收尾不杀进程 ----
$plugin = New-Fixture 'pidfile'
$self = (Get-Process -Id $PID).Path
$decoy = Start-Process -FilePath $self -ArgumentList '-NoProfile -Command "Start-Sleep -Seconds 60"' -PassThru
Write-Text $PIDFILE ("{0}`n" -f $decoy.Id)
$script:MEOW = Join-Path $ROOT 'bin/meow.exe'
if ((Get-RunningPid) -ne 0) { Fail 'pidfile: 复用 pid 的无关进程被当成 meow 了' }
$script:MEOW = $self
if ((Get-RunningPid) -ne $decoy.Id) { Fail 'pidfile: 路径对上的进程反而认不出来' }
$script:MEOW = Join-Path $ROOT 'bin/meow.exe'
Stop-Cleanup
if (Test-Path -LiteralPath $PIDFILE) { Fail 'pidfile: 收尾之后 pidfile 还在' }
if (-not (Get-Process -Id $decoy.Id -ErrorAction SilentlyContinue)) { Fail 'pidfile: 收尾把无关进程杀了' }
Stop-Process -Id $decoy.Id -Force -ErrorAction SilentlyContinue

Write-Output 'meow pidfile/stop (windows): ok'

Remove-Item -Recurse -Force $Work
