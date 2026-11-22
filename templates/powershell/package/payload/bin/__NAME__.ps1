# __NAME__ - a svcd plugin in PowerShell.
#
# Only the capability bodies below are yours: the protocol shell next door owns
# the readiness probe, the shutdown request, framing and the MCP result shape.
. "$PSScriptRoot/../lib/SvcdPlugin.ps1"

# Must equal `capabilities[].name` in package/manifest.json.
$Capabilities = @('echo', 'info')

function Invoke-SvcdCapability {
    param(
        [Parameter(Mandatory = $true)][string]$Name,
        $Arguments
    )

    switch ($Name) {
        'echo' {
            $message = $Arguments.message
            if ([string]::IsNullOrEmpty($message)) {
                return @{ Text = 'echo: missing "message"'; Error = $true }
            }
            return @{ Text = [string]$message; Error = $false }
        }
        'info' {
            $edition = if ($PSVersionTable.PSEdition) { $PSVersionTable.PSEdition } else { 'Desktop' }
            # JSON, so `info.outputSchema` in the manifest has something to describe.
            $body = [ordered]@{
                edition = [string]$edition
                version = [string]$PSVersionTable.PSVersion
            }
            return @{ Text = ($body | ConvertTo-Json -Compress); Error = $false }
        }
        default {
            return @{ Text = "unknown tool: $Name"; Error = $true }
        }
    }
}

Write-SvcdLog 'info' 'starting'
Invoke-SvcdPlugin -Handler ${function:Invoke-SvcdCapability} -Capabilities $Capabilities
