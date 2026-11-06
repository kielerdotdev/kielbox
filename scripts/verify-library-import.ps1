param(
    [string]$Recording = 'recordings/mux-audio-fixture-001.mkv',
    [string]$Executable = 'target/debug/desktop.exe'
)
$ErrorActionPreference = 'Stop'
$rcrRoot = Split-Path -Parent $PSScriptRoot
$rcrSource = (Resolve-Path -LiteralPath $(if ([IO.Path]::IsPathRooted($Recording)) { $Recording } else { Join-Path $rcrRoot $Recording })).Path
$rcrExe = if ([IO.Path]::IsPathRooted($Executable)) { $Executable } else { Join-Path $rcrRoot $Executable }
$rcrLogs = Join-Path $rcrRoot '.downloads'
[IO.Directory]::CreateDirectory($rcrLogs) | Out-Null
$rcrStamp = [guid]::NewGuid().ToString('N')
$rcrOut = Join-Path $rcrLogs "library-import-$rcrStamp.stdout.txt"
$rcrErr = Join-Path $rcrLogs "library-import-$rcrStamp.stderr.txt"
# The executable creates a marked workspace before any preference readers or workers start.
# Keep its evidence directory for inspection; this script never removes user files.
$rcrProcess = Start-Process -FilePath $rcrExe -WorkingDirectory $rcrRoot `
    -ArgumentList @('--smoke-library-import', '--open', ('"' + $rcrSource + '"')) `
    -WindowStyle Hidden -RedirectStandardOutput $rcrOut -RedirectStandardError $rcrErr -PassThru
if (-not $rcrProcess.WaitForExit(90000)) {
    $rcrProcess.Kill($true)
    $rcrProcess.WaitForExit()
    throw "Library import fixture timed out. Logs: $rcrOut; $rcrErr"
}
$rcrResult = Get-Content -LiteralPath $rcrOut -Raw
if ($rcrProcess.ExitCode -ne 0 -or $rcrResult -notmatch 'Library import smoke passed:') {
    Get-Content -LiteralPath $rcrErr
    throw "Library import fixture failed with exit $($rcrProcess.ExitCode). Logs: $rcrOut; $rcrErr"
}
$rcrResult.Trim()
