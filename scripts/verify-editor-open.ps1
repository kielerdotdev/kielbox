param(
    [string]$Recording = 'recordings/mux-audio-fixture-001.mkv',
    [switch]$Trim,
    [switch]$Scrub,
    [switch]$Pressure,
    [switch]$PressureCold,
    [switch]$Random,
    [switch]$Zoom,
    [switch]$Recovery,
    [switch]$Keyboard,
    [switch]$Autosave,
    [string]$Executable = ''
)
$ErrorActionPreference = 'Stop'
$rcrRoot = Split-Path -Parent $PSScriptRoot
$rcrExe = Join-Path $rcrRoot 'target/debug/rcr-desktop.exe'
if ($Executable) { $rcrExe = if ([System.IO.Path]::IsPathRooted($Executable)) {$Executable} else {Join-Path $rcrRoot $Executable} }
if (-not [System.IO.Path]::IsPathRooted($Recording)) {
    $Recording = Join-Path $rcrRoot $Recording
}
$rcrSource = (Resolve-Path -LiteralPath $Recording).Path
$rcrPressureRequested = $Pressure -or $PressureCold
$rcrSourceHash = if ($rcrPressureRequested) { (Get-FileHash -LiteralPath $rcrSource -Algorithm SHA256).Hash } else { $null }
$rcrLogs = Join-Path $rcrRoot '.downloads'
[System.IO.Directory]::CreateDirectory($rcrLogs) | Out-Null
$rcrStamp = [guid]::NewGuid().ToString('N')
$rcrOut = Join-Path $rcrLogs "editor-open-$rcrStamp.stdout.txt"
$rcrErr = Join-Path $rcrLogs "editor-open-$rcrStamp.stderr.txt"
# Exercise native layout/paint in a hidden window, without desktop automation.
$rcrMode = if ($Autosave) {'--smoke-autosave'} elseif ($Recovery) {'--smoke-recovery'} elseif ($Keyboard) {'--smoke-editor-keyboard'} elseif ($Zoom) {'--smoke-zoom'} elseif ($Random) {'--smoke-random'} elseif ($PressureCold) {'--smoke-scrub-pressure-cold'} elseif ($Pressure) {'--smoke-scrub-pressure'} elseif ($Scrub) {'--smoke-scrub'} elseif ($Trim) {'--smoke-trim'} else {'--smoke-open'}
$rcrProcess = Start-Process -FilePath $rcrExe -WorkingDirectory $rcrRoot `
    -ArgumentList @('--open', ('"' + $rcrSource + '"'), $rcrMode) `
    -WindowStyle Hidden -RedirectStandardOutput $rcrOut -RedirectStandardError $rcrErr -PassThru
$rcrTimedOut = -not $rcrProcess.WaitForExit($(if ($Random) {110000} else {25000}))
if ($rcrTimedOut) {
    $rcrProcess.Kill($true)
    $rcrProcess.WaitForExit()
}
$rcrResult = Get-Content -LiteralPath $rcrOut -Raw
if ($rcrPressureRequested -and (Get-FileHash -LiteralPath $rcrSource -Algorithm SHA256).Hash -ne $rcrSourceHash) {
    throw 'Pressure fixture source SHA-256 changed.'
}
if ($rcrTimedOut) { throw "Editor smoke timed out. Logs: $rcrOut; $rcrErr" }
if ($rcrProcess.ExitCode -ne 0 -or $rcrResult -notmatch 'Editor open smoke: ready=true') {
    Get-Content -LiteralPath $rcrErr
    throw "Editor smoke failed with exit $($rcrProcess.ExitCode). Logs: $rcrOut; $rcrErr"
}
if (($rcrPressureRequested -and $rcrResult -notmatch 'Sustained scrub pressure:') -or
    ($Autosave -and $rcrResult -notmatch 'Autosave smoke passed:') -or
    ($Recovery -and $rcrResult -notmatch 'Recovery smoke passed:') -or
    ($Keyboard -and $rcrResult -notmatch 'Editor keyboard smoke passed:')) {
    throw "Requested editor fixture did not report its completed checks. Logs: $rcrOut; $rcrErr"
}
$rcrResult.Trim()
