# __NAME__ - the Windows side of a script plugin.
#
# Only the capability bodies below are yours: the protocol shell next door owns
# the readiness probe, the shutdown request, framing and the MCP result shape.
# `plugin-tools build` copies this file into payload\bin\ for Windows packages
# only; the Unix side lives in __NAME__.sh next door.
. "$PSScriptRoot/../lib/SvcdPlugin.ps1"

# Must equal `capabilities[].name` in package/manifest.json - the same list the
# Unix side implements.
$Capabilities = @('uname', 'date')

function Invoke-SvcdCapability {
    param(
        [Parameter(Mandatory = $true)][string]$Name,
        $Arguments
    )

    switch ($Name) {
        'uname' {
            $os = [System.Runtime.InteropServices.RuntimeInformation]::OSDescription
            return @{ Text = [string]$os; Error = $false }
        }
        'date' {
            $utc = (Get-Date).ToUniversalTime().ToString('yyyy-MM-ddTHH:mm:ssZ')
            return @{ Text = ('{"utc":"' + $utc + '"}'); Error = $false }
        }
        default {
            return @{ Text = "unknown tool: $Name"; Error = $true }
        }
    }
}

Write-SvcdLog 'info' 'starting'
Invoke-SvcdPlugin -Handler ${function:Invoke-SvcdCapability} -Capabilities $Capabilities
