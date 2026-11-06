param([switch]$RequireReuse, [switch]$VideoPreview, [string]$Executable = 'target/debug/desktop.exe')
$ErrorActionPreference = 'Stop'
$rcrRoot = Split-Path -Parent $PSScriptRoot
$rcrExe = if ([IO.Path]::IsPathRooted($Executable)) { $Executable } else { Join-Path $rcrRoot $Executable }
$rcrStamp = [guid]::NewGuid().ToString('N')
$rcrLogs = Join-Path $rcrRoot '.downloads'
[IO.Directory]::CreateDirectory($rcrLogs) | Out-Null
$rcrOut = Join-Path $rcrLogs "atlas-churn-$rcrStamp.stdout.txt"
$rcrErr = Join-Path $rcrLogs "atlas-churn-$rcrStamp.stderr.txt"
$rcrArguments = @('--smoke-atlas-churn')
if ($RequireReuse) { $rcrArguments += '--require-atlas-reuse' }
if ($VideoPreview) { $rcrArguments += '--atlas-video-preview' }
# Own hidden GPUI target, synthetic pixels, and one bounded child only.
$rcrProcess = Start-Process -FilePath $rcrExe -WorkingDirectory $rcrRoot `
    -ArgumentList $rcrArguments -WindowStyle Hidden -PassThru `
    -RedirectStandardOutput $rcrOut -RedirectStandardError $rcrErr
if (-not $rcrProcess.WaitForExit(25000)) {
    $rcrProcess.Kill($true)
    $rcrProcess.WaitForExit()
    throw "Owned atlas fixture timed out. Logs: $rcrOut; $rcrErr"
}
$rcrResult = Get-Content -LiteralPath $rcrOut -Raw
if ($rcrProcess.ExitCode -ne 0 -or $rcrResult -notmatch 'Atlas churn proof passed:') {
    Get-Content -LiteralPath $rcrErr
    throw "Atlas fixture failed with exit $($rcrProcess.ExitCode). Logs: $rcrOut; $rcrErr"
}
$rcrResult.Trim()
