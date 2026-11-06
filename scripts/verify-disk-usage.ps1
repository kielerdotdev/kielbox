param([string]$Executable = 'target/debug/desktop.exe')
$ErrorActionPreference = 'Stop'
$rcrDiskRoot = Split-Path -Parent $PSScriptRoot
$rcrDiskExe = (Resolve-Path -LiteralPath $(if ([IO.Path]::IsPathRooted($Executable)) { $Executable } else { Join-Path $rcrDiskRoot $Executable })).Path
$rcrDiskWork = Join-Path $rcrDiskRoot ('.downloads/disk-usage-' + [guid]::NewGuid().ToString('N'))
$rcrDiskInput = Join-Path $rcrDiskWork 'fixture'
[IO.Directory]::CreateDirectory((Join-Path $rcrDiskInput 'Games')) | Out-Null
[IO.Directory]::CreateDirectory((Join-Path $rcrDiskInput 'Captures')) | Out-Null
[IO.Directory]::CreateDirectory((Join-Path $rcrDiskInput 'Empty')) | Out-Null
foreach ($rcrDiskFile in @(@('Games/game.data', 8MB), @('Captures/replay.mkv', 4MB), @('Captures/screenshot.png', 2MB), @('notes.txt', 1MB))) {
    $rcrDiskStream = [IO.File]::Create((Join-Path $rcrDiskInput $rcrDiskFile[0]))
    try { $rcrDiskStream.SetLength($rcrDiskFile[1]) } finally { $rcrDiskStream.Dispose() }
}
for ($rcrDiskIndex = 0; $rcrDiskIndex -lt 80; $rcrDiskIndex++) {
    $rcrDiskSuffix = @('log', 'png', 'zip')[$rcrDiskIndex % 3]
    $rcrDiskStream = [IO.File]::Create((Join-Path $rcrDiskInput ('sample-{0:D2}.{1}' -f $rcrDiskIndex, $rcrDiskSuffix)))
    try { $rcrDiskStream.SetLength(64KB + $rcrDiskIndex * 2KB) } finally { $rcrDiskStream.Dispose() }
}
foreach ($rcrDiskCase in @(@('1440x900', 'tools-disk-usage'), @('1440x900', 'tools-disk-usage-files'), @('900x600', 'tools-disk-usage'), @('1440x900', 'tools-disk-usage-explore'), @('900x600', 'tools-disk-usage-explore'), @('1440x900', 'tools-disk-usage-explore-boxes'))) {
    $rcrDiskSize = $rcrDiskCase[0]
    $rcrDiskRoute = $rcrDiskCase[1]
    $rcrDiskOutput = Join-Path $rcrDiskWork "$rcrDiskSize-$rcrDiskRoute.png"
    $rcrDiskOut = Join-Path $rcrDiskWork "$rcrDiskSize-$rcrDiskRoute.stdout.txt"
    $rcrDiskErr = Join-Path $rcrDiskWork "$rcrDiskSize-$rcrDiskRoute.stderr.txt"
    # The app renders its own hidden GPUI window; no desktop capture or OS input.
    $rcrDiskProcess = Start-Process -FilePath $rcrDiskExe -WorkingDirectory $rcrDiskWork -WindowStyle Hidden -PassThru `
        -ArgumentList @('--ui-snapshot', $rcrDiskRoute, '--analyze-folder', ('"' + $rcrDiskInput + '"'), '--snapshot-output', ('"' + $rcrDiskOutput + '"'), '--snapshot-size', $rcrDiskSize) `
        -RedirectStandardOutput $rcrDiskOut -RedirectStandardError $rcrDiskErr
    if (-not $rcrDiskProcess.WaitForExit(45000)) {
        $rcrDiskProcess.Kill($true)
        $rcrDiskProcess.WaitForExit()
        throw "Disk usage snapshot timed out. Logs: $rcrDiskWork"
    }
    if ($rcrDiskProcess.ExitCode -ne 0 -or -not (Test-Path -LiteralPath $rcrDiskOutput)) {
        Get-Content -LiteralPath $rcrDiskErr
        throw "Disk usage snapshot failed. Logs: $rcrDiskWork"
    }
    Write-Output $rcrDiskOutput
}
Write-Output 'Expected: 26.2 MiB (27443200 bytes), 84 files, 3 folders. Inspect all snapshots for layout; scanner unit tests verify totals and cancellation.'
