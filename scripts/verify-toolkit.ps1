param([string]$Executable = '', [switch]$AudioPicker, [switch]$AudioDevices, [switch]$History, [switch]$LibrarySelection)
$ErrorActionPreference = 'Stop'
$rcrRoot = Split-Path -Parent $PSScriptRoot
$rcrExe = Join-Path $rcrRoot 'target/debug/desktop.exe'
if ($Executable) { $rcrExe = if ([System.IO.Path]::IsPathRooted($Executable)) {$Executable} else {Join-Path $rcrRoot $Executable} }
$rcrLogs = Join-Path $rcrRoot '.downloads'
[System.IO.Directory]::CreateDirectory($rcrLogs) | Out-Null
$rcrStamp = [guid]::NewGuid().ToString('N')
$rcrOut = Join-Path $rcrLogs "toolkit-$rcrStamp.stdout.txt"
$rcrErr = Join-Path $rcrLogs "toolkit-$rcrStamp.stderr.txt"
$rcrMode = if ($LibrarySelection) { '--smoke-library-selection' } elseif ($History) { '--smoke-history' } elseif ($AudioDevices) { '--smoke-audio-devices' } elseif ($AudioPicker) { '--smoke-audio-picker' } else { '--smoke-toolkit' }
$rcrExpected = if ($LibrarySelection) { 'Library keyboard smoke: pointer' } elseif ($History) { 'History smoke: 257' } elseif ($AudioDevices) { 'Audio device picker smoke: native' } elseif ($AudioPicker) { 'Audio picker smoke: native' } else { 'Toolkit smoke: ready=true, stages=25' }
# Exercises native GPUI paint and input without desktop automation or a screenshot.
$rcrProcess = Start-Process -FilePath $rcrExe -WorkingDirectory $rcrRoot `
    -ArgumentList $rcrMode -WindowStyle Hidden `
    -RedirectStandardOutput $rcrOut -RedirectStandardError $rcrErr -PassThru
if (-not $rcrProcess.WaitForExit(25000)) {
    $rcrProcess.Kill($true)
    $rcrProcess.WaitForExit()
    throw "Toolkit smoke timed out. Logs: $rcrOut; $rcrErr"
}
$rcrResult = Get-Content -LiteralPath $rcrOut -Raw
if ($rcrProcess.ExitCode -ne 0 -or $rcrResult -notmatch $rcrExpected) {
    Get-Content -LiteralPath $rcrErr
    throw "Toolkit smoke failed with exit $($rcrProcess.ExitCode). Logs: $rcrOut; $rcrErr"
}
$rcrResult.Trim()
