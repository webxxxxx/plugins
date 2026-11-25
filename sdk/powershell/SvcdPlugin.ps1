# SvcdPlugin.ps1 - the protocol shell for PowerShell plugins.
#
# The control channel is newline-delimited JSON-RPC 2.0 on stdout/stdin, with no
# handshake. This file owns framing, the readiness probe, the shutdown request
# and the MCP result shape. A plugin author writes one handler:
#
#   . "$PSScriptRoot/../lib/SvcdPlugin.ps1"
#   Invoke-SvcdPlugin -Handler ${function:Invoke-Capability} -Capabilities @('echo')
#
# stdout is the protocol. Log with Write-SvcdLog, which writes to stderr and is
# captured by svcd into <logs>/<id>.log.

$ErrorActionPreference = 'Stop'

function Get-SvcdContext {
    # A plugin child gets three directories: its install directory, the log
    # directory and the transient root. Its own state goes to <plugin>\data.
    $pluginDir = $env:SVCD_PLUGIN_DIR
    [pscustomobject]@{
        Id        = if ($env:SVCD_PLUGIN_ID) { $env:SVCD_PLUGIN_ID } else { 'plugin' }
        Version   = if ($env:SVCD_PLUGIN_VERSION) { $env:SVCD_PLUGIN_VERSION } else { '0.0.0' }
        Config    = $env:SVCD_CONFIG
        PluginDir = $pluginDir
        DataDir   = if ($pluginDir) { Join-Path $pluginDir 'data' } else { $null }
        LogDir    = $env:SVCD_LOG_DIR
        TempDir   = $env:SVCD_TEMP_DIR
    }
}

function Write-SvcdLog {
    param(
        [Parameter(Mandatory = $true)][string]$Level,
        [Parameter(Mandatory = $true)][string]$Message
    )
    [Console]::Error.WriteLine("[$Level] $Message")
}

function Send-SvcdMessage {
    param([Parameter(Mandatory = $true)]$Message)
    $json = $Message | ConvertTo-Json -Compress -Depth 32
    [Console]::Out.WriteLine($json)
    [Console]::Out.Flush()
}

function Send-SvcdResult {
    param($Id, [AllowEmptyString()][string]$Text, [bool]$IsError)
    Send-SvcdMessage @{
        jsonrpc = '2.0'
        id      = $Id
        result  = @{
            content = @(@{ type = 'text'; text = $Text })
            isError = $IsError
        }
    }
}

function Send-SvcdError {
    param($Id, [int]$Code, [string]$Message)
    Send-SvcdMessage @{
        jsonrpc = '2.0'
        id      = $Id
        error   = @{ code = $Code; message = $Message }
    }
}

# Push a notification to the host; it mirrors it to UI subscribers.
function Send-SvcdNotification {
    param([Parameter(Mandatory = $true)][string]$Method, $Params)
    Send-SvcdMessage @{ jsonrpc = '2.0'; method = $Method; params = $Params }
}

# Serve the protocol until stdin closes or the host asks us to stop.
function Invoke-SvcdPlugin {
    param(
        [Parameter(Mandatory = $true)][scriptblock]$Handler,
        [string[]]$Capabilities = @()
    )

    try {
        [Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
        [Console]::InputEncoding = New-Object System.Text.UTF8Encoding($false)
    } catch {
        # Older hosts: leave the console encoding alone.
    }

    while ($null -ne ($line = [Console]::In.ReadLine())) {
        if ([string]::IsNullOrWhiteSpace($line)) { continue }

        try {
            $request = $line | ConvertFrom-Json
        } catch {
            Send-SvcdError $null -32700 'parse error'
            continue
        }
        if ($null -eq $request) { continue }

        $hasId = $null -ne $request.PSObject.Properties['id']
        $id = if ($hasId) { $request.id } else { $null }
        $method = [string]$request.method

        switch ($method) {
            'svcd.ping' {
                Send-SvcdMessage @{ jsonrpc = '2.0'; id = $id; result = @{ ok = $true } }
            }
            'svcd.status' {
                Send-SvcdMessage @{
                    jsonrpc = '2.0'
                    id      = $id
                    result  = @{ summary = 'running'; details = @{} }
                }
            }
            'svcd.shutdown' {
                if ($hasId) { Send-SvcdMessage @{ jsonrpc = '2.0'; id = $id; result = $null } }
                return
            }
            'tools/call' {
                $name = if ($request.params) { [string]$request.params.name } else { '' }
                if ($Capabilities -notcontains $name) {
                    Send-SvcdError $id -32602 "unknown tool: $name"
                    continue
                }
                $arguments = if ($request.params) { $request.params.arguments } else { $null }
                if ($null -eq $arguments) { $arguments = @{} }

                $outcome = & $Handler $name $arguments
                if ($outcome -is [hashtable] -and $outcome.ContainsKey('BlobPath')) {
                    # Large result: the host imports the file and returns a
                    # resource_link. See plugin-manager/docs/plugin-blob.md.
                    Send-SvcdMessage @{
                        jsonrpc = '2.0'
                        id      = $id
                        result  = @{
                            '$blobPath' = [string]$outcome.BlobPath
                            name        = if ($outcome.Name) { [string]$outcome.Name } else { 'blob' }
                            mimeType    = if ($outcome.MimeType) { [string]$outcome.MimeType } else { 'application/octet-stream' }
                        }
                    }
                } elseif ($outcome -is [hashtable] -and $outcome.ContainsKey('Error')) {
                    Send-SvcdResult $id ([string]$outcome.Text) ([bool]$outcome.Error)
                } else {
                    Send-SvcdResult $id ([string]$outcome) $false
                }
            }
            default {
                if ($hasId) { Send-SvcdError $id -32601 "method not found: $method" }
            }
        }
    }
}

# --- helpers a capability usually wants --------------------------------

# Parse the user-editable config copy; missing file means "defaults still apply".
function Get-SvcdConfig {
    param($Context)
    if (-not $Context -or -not $Context.Config) { return $null }
    if (-not (Test-Path -LiteralPath $Context.Config)) { return $null }
    try { return Get-Content -LiteralPath $Context.Config -Raw | ConvertFrom-Json } catch { return $null }
}

# Build a unique path under $SVCD_TEMP_DIR/incoming for a blob result.
function New-SvcdIncomingPath {
    param([string]$Name = 'blob')
    if (-not $env:SVCD_TEMP_DIR) { throw 'SVCD_TEMP_DIR is not set' }
    $dir = Join-Path $env:SVCD_TEMP_DIR 'incoming'
    New-Item -ItemType Directory -Force -Path $dir | Out-Null
    $safe = ($Name -replace '[^A-Za-z0-9._-]', '_')
    return Join-Path $dir ("{0}-{1}-{2}" -f $safe, $PID, (Get-Random))
}

# Parts of a `$file` package, in manifest order (host names them part-00001 …).
function Get-SvcdFileParts {
    param($File)
    $manifest = Get-Content -LiteralPath $File.manifest -Raw | ConvertFrom-Json
    foreach ($part in $manifest.parts) {
        Join-Path $File.dir ('part-{0:D5}' -f [int]$part.partNumber)
    }
}
