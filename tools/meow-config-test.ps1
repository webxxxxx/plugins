<#
meow 的配置层（Windows 版）：pc 形态自动写 tun + fake-ip + mixed-port。

和 tools/meow-config-test.sh 对应，但 meow.ps1 只能在 PowerShell 里跑，所以只取它的
函数定义（按语法树抽取，不执行脚本最后的 switch），在假布局上调用，再检查 meow.yaml。

用法：pwsh tools/meow-config-test.ps1 [-Repo <仓库根>]
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

# 假插件目录，并把脚本里那些路径变量指过去。
function New-Fixture([string]$Name) {
    $plugin = Join-Path $Work $Name
    $payload = Join-Path $plugin '1.2.0/payload'
    New-Item -ItemType Directory -Force -Path (Join-Path $payload 'bin') | Out-Null
    New-Item -ItemType Directory -Force -Path (Join-Path $payload 'yacd') | Out-Null
    Copy-Item (Join-Path $Repo 'plugins/meow/package/payload/config.yaml') (Join-Path $payload 'config.yaml')
    Copy-Item (Join-Path $Repo 'plugins/meow/package/config/config') (Join-Path $plugin 'config')

    $yaml = @(
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
        'rules:'
        '  - MATCH,DIRECT'
    ) -join "`n"
    Write-Text (Join-Path $plugin 'meow.yaml') $yaml

    $script:PLUGIN_ROOT = $plugin
    $script:ROOT = $payload
    $script:DATA = Join-Path $plugin '.data/meow'
    $script:PIDFILE = Join-Path $DATA 'meow.pid'
    $script:CONFIG = Join-Path $plugin 'config'
    $script:MEOW_CONFIG = Join-Path $plugin 'meow.yaml'
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

# ---- pc：tun + fake-ip + mixed-port 自动写进去，tproxy-port 删掉 ----
$plugin = New-Fixture 'pc'
$configCopy = Read-Text (Join-Path $plugin 'config')
Prepare-Config
Apply-Settings (Get-Mode) | Out-Null
if ((Get-BlockScalar $MEOW_CONFIG 'tun' 'enable') -ne 'true') { Fail 'pc: tun.enable 没打开' }
if ((Get-BlockScalar $MEOW_CONFIG 'dns' 'enhanced-mode') -ne 'fake-ip') { Fail 'pc: dns.enhanced-mode 不是 fake-ip' }
Expect $MEOW_CONFIG 'mixed-port' '7890' 'pc'
Expect $MEOW_CONFIG 'tproxy-port' '' 'pc'
if ((Read-Text $MEOW_CONFIG) -notmatch 'node-1') { Fail 'pc: 订阅内容被弄丢了' }
if ((Read-Text (Join-Path $plugin 'config')) -ne $configCopy) { Fail 'pc: config 被改了' }

# ---- 用户改 mixed-port，跟着走；重复跑幂等 ----
Write-Text (Join-Path $plugin 'config') ((Read-Text (Join-Path $plugin 'config')) -replace '(?m)^mixed-port: 7890', 'mixed-port: 1080')
Apply-Settings (Get-Mode) | Out-Null
Expect $MEOW_CONFIG 'mixed-port' '1080' 'pc 改端口'
$before = Read-Text $MEOW_CONFIG
Apply-Settings (Get-Mode) | Out-Null
if ((Read-Text $MEOW_CONFIG) -ne $before) { Fail 'pc: 第二次跑改了文件' }

# ---- 旧布局：订阅写在 config 里，第一次启动搬过去 ----
$plugin = New-Fixture 'migrate'
Copy-Item (Join-Path $plugin 'meow.yaml') (Join-Path $plugin 'config') -Force
Remove-Item (Join-Path $plugin 'meow.yaml')
Prepare-Config
if (-not (Test-Path (Join-Path $plugin 'meow.yaml'))) { Fail 'migrate: meow.yaml 没生成' }
if ((Read-Text (Join-Path $plugin 'meow.yaml')) -notmatch 'node-1') { Fail 'migrate: 订阅没迁过去' }
Expect (Join-Path $plugin 'config') 'proxy-mode' 'pc' 'migrate'

Remove-Item -Recurse -Force $Work
Write-Output 'meow config layer (windows): ok'

# ---- 页面上的代理类型：off / tun / tproxy ↔ off / pc / router ----
# 页面只认这三个标签；落盘的永远是 proxy-mode，所以配置永远能被 meow.sh 读。
$plugin = New-Fixture 'modes'
if ((Get-ModeFromLabel 'tun') -ne 'pc') { Fail 'modes: tun 没映射成 pc' }
if ((Get-ModeFromLabel 'tproxy') -ne 'router') { Fail 'modes: tproxy 没映射成 router' }
if ((Get-ModeFromLabel 'off') -ne 'off') { Fail 'modes: off 没映射成 off' }
if ((Get-ModeFromLabel 'turbo') -ne '') { Fail 'modes: 非法标签应该返回空' }
if ((Get-ModeLabel 'pc') -ne 'tun') { Fail 'modes: pc 没映射回 tun' }

$before = Read-Text $CONFIG
Set-OurValue 'proxy-mode' 'off'
if ((Get-Mode) -ne 'off') { Fail 'modes: Set-OurValue 没写进 proxy-mode' }
if ((Read-Text $CONFIG) -notmatch '(?m)^mixed-port: 7890') { Fail 'modes: Set-OurValue 弄丢了别的键' }
if ((Read-Text $CONFIG) -notmatch '# meow 插件的设置') { Fail 'modes: Set-OurValue 弄丢了注释' }
$twice = Read-Text $CONFIG
Set-OurValue 'proxy-mode' 'off'
if ((Read-Text $CONFIG) -ne $twice) { Fail 'modes: Set-OurValue 不幂等' }

# off：不启动 tun，也不开 tproxy —— 只剩 mixed-port。
if (-not (Apply-Settings 'off')) { Fail 'modes: Apply-Settings off 失败' }
if ((Get-BlockScalar $MEOW_CONFIG 'tun' 'enable') -ne 'false') { Fail 'modes: off 没把 tun 关掉' }
Expect $MEOW_CONFIG 'mixed-port' '7890' 'modes'
Expect $MEOW_CONFIG 'tproxy-port' '' 'modes'
if ((Read-Text $MEOW_CONFIG) -notmatch 'node-1') { Fail 'modes: off 把订阅弄丢了' }
if (Apply-Settings 'router') { Fail 'modes: Windows 上 router 应该被拒' }

$json = Get-ModeJson
if ($json -notmatch '"mode":"off"') { Fail "modes: Get-ModeJson 没报出代理类型（$json）" }
if ($json -notmatch '"running":false') { Fail "modes: Get-ModeJson running 不对（$json）" }
if ($json -notmatch '"allowed":\["off","tun"\]') { Fail "modes: Get-ModeJson allowed 不对（$json）" }

# 页面走的能力入口：不带 mode 是读，带了非法/不支持的标签要报错且不动配置。
$read = Invoke-Capability 'mode' @{}
if ($read -notmatch '"mode":"off"') { Fail "modes: mode 能力读状态不对（$read）" }
$refused = Invoke-Capability 'mode' @{ mode = 'tproxy' }
if (-not ($refused -is [hashtable] -and $refused.Error)) { Fail 'modes: Windows 上 tproxy 应该被拒' }
if ((Get-Mode) -ne 'off') { Fail 'modes: 被拒的 tproxy 改了配置' }
$bad = Invoke-Capability 'mode' @{ mode = 'turbo' }
if (-not ($bad -is [hashtable] -and $bad.Error)) { Fail 'modes: 非法标签应该被拒' }

Write-Output 'meow proxy mode capability (windows): ok'

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
