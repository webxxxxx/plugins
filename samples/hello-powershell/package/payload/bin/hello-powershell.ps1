# hello-powershell - the reference plugin, in PowerShell.
#
# Same five capabilities as `samples/hello` and `samples/hello-c`: echo, stats,
# summarize (blob in), inspect_file (file package in), report (blob out).
# The protocol shell lives in ../lib/SvcdPlugin.ps1.
. "$PSScriptRoot/../lib/SvcdPlugin.ps1"

$Capabilities = @('echo', 'get_stats', 'count_text', 'get_file_info', 'create_report')
$Context = Get-SvcdContext
$Config = Get-SvcdConfig -Context $Context
$Greeting = if ($Config -and $Config.greeting) { [string]$Config.greeting } else { 'hello' }
$Started = Get-Date
$Calls = 0

function Invoke-SvcdCapability {
    param(
        [Parameter(Mandatory = $true)][string]$Name,
        $Arguments
    )
    $Script:Calls++
    $outcome = Invoke-Dispatch -Name $Name -Arguments $Arguments

    # Every call is visible twice: a log line (stderr → svcd's log) and a
    # notification (→ SSE / WebSocket subscribers).
    Send-SvcdNotification -Method 'notifications/message' -Params @{
        type  = 'call'
        name  = $Name
        count = $Calls
    }
    Write-SvcdLog 'info' "call #$Calls $Name"
    return $outcome
}

function Invoke-Dispatch {
    param(
        [Parameter(Mandatory = $true)][string]$Name,
        $Arguments
    )

    switch ($Name) {
        'echo' {
            $message = $Arguments.message
            if ([string]::IsNullOrEmpty($message)) {
                return @{ Text = 'echo: "message" is required'; Error = $true }
            }
            return @{ Text = "$Greeting $message"; Error = $false }
        }

        'get_stats' {
            $body = [ordered]@{
                greeting    = $Greeting
                calls       = $Calls
                uptime_secs = [int](((Get-Date) - $Started).TotalSeconds)
                pid         = $PID
            }
            return @{ Text = ($body | ConvertTo-Json -Compress); Error = $false }
        }

        'count_text' {
            # The host replaced {"$blob": id} with {path, size, …}.
            $input_ = $Arguments.input
            if (-not $input_ -or -not $input_.path) {
                return @{ Text = 'count_text: "input" must be a blob argument ({$blob: …})'; Error = $true }
            }
            $bytes = (Get-Item -LiteralPath $input_.path).Length
            $lines = 0
            $words = 0
            $reader = [IO.File]::OpenText($input_.path)
            try {
                while ($null -ne ($line = $reader.ReadLine())) {
                    $lines++
                    $words += @($line -split '\s+' | Where-Object { $_ -ne '' }).Count
                }
            } finally {
                $reader.Close()
            }
            return @{ Text = "$bytes bytes, $lines lines, $words words"; Error = $false }
        }

        'get_file_info' {
            $file = $Arguments.input
            if (-not $file -or -not $file.manifest) {
                return @{ Text = 'get_file_info: "input" must be a file argument ({$file: …})'; Error = $true }
            }
            $total = 0
            $count = 0
            foreach ($part in Get-SvcdFileParts -File $file) {
                $total += (Get-Item -LiteralPath $part).Length
                $count++
            }
            $label = if ($file.name) { [string]$file.name } else { 'file' }
            return @{ Text = "${label}: $total bytes in $count part(s)"; Error = $false }
        }

        'create_report' {
            $path = New-SvcdIncomingPath -Name 'report.txt'
            $body = @(
                'hello-powershell report'
                "greeting: $Greeting"
                "calls: $Calls"
                "uptime_secs: $([int](((Get-Date) - $Started).TotalSeconds))"
            ) -join "`n"
            Set-Content -LiteralPath $path -Value $body -NoNewline
            return @{ BlobPath = $path; Name = 'report.txt'; MimeType = 'text/plain; charset=utf-8' }
        }

        default {
            return @{ Text = "unknown tool: $Name"; Error = $true }
        }
    }
}

Write-SvcdLog 'info' "starting (temp: $($Context.TempDir))"
Invoke-SvcdPlugin -Handler ${function:Invoke-SvcdCapability} -Capabilities $Capabilities
