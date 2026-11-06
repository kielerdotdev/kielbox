param(
    [Parameter(Mandatory = $true)][string]$WorkingDirectory,
    [switch]$ValidateOnly
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$rcrWorkingFolder = (Resolve-Path -LiteralPath $WorkingDirectory).ProviderPath
if (-not (Test-Path -LiteralPath $rcrWorkingFolder -PathType Container)) { throw 'Choose an existing working/data folder.' }
$rcrLauncherExe = Join-Path $PSScriptRoot 'rcr.exe'
if (-not (Test-Path -LiteralPath $rcrLauncherExe -PathType Leaf)) { throw 'rcr.exe is missing; extract the whole package.' }
if ($ValidateOnly) { [pscustomobject]@{ executable = $rcrLauncherExe; working_directory = $rcrWorkingFolder }; return }
# This is an explicitly requested interactive application launch.
Start-Process -FilePath $rcrLauncherExe -WorkingDirectory $rcrWorkingFolder
