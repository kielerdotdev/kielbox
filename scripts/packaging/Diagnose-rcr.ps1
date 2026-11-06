param([Parameter(Mandatory = $true)][string]$WorkingDirectory)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$rcrLaunch = & (Join-Path $PSScriptRoot 'Start-rcr.ps1') -WorkingDirectory $WorkingDirectory -ValidateOnly
Push-Location -LiteralPath $rcrLaunch.working_directory
try {
    & $rcrLaunch.executable --diagnose
    if ($LASTEXITCODE -ne 0) { throw 'Setup diagnostics reported missing requirements; inspect the JSON above.' }
} finally { Pop-Location }
