param(
    [string]$FFplay = 'ffplay',
    [string]$Executable = 'target/debug/examples/process-audio-proof.exe',
    [ValidateRange(0.1, 10.0)][double]$Seconds = 3,
    [switch]$IncludeChildren,
    [switch]$ExpectExit
)
$ErrorActionPreference = 'Stop'
$rcrRoot = Split-Path -Parent $PSScriptRoot
$rcrPlayer = (Get-Command $FFplay -ErrorAction Stop).Source
$rcrProof = if ([IO.Path]::IsPathRooted($Executable)) { $Executable } else { Join-Path $rcrRoot $Executable }
$rcrLogs = Join-Path $rcrRoot '.downloads'
[IO.Directory]::CreateDirectory($rcrLogs) | Out-Null
$rcrStamp = [guid]::NewGuid().ToString('N')
$rcrTarget = $null
$rcrOther = $null
try {
    # Two quiet generated tones; capture only the selected process, never the
    # microphone or desktop. Keep both alive through the complete measurement.
    $rcrDuration = [Math]::Ceiling($Seconds) + 8
    if ($ExpectExit -and $Seconds -lt 3) { throw 'Exit verification needs at least three seconds' }
    $rcrTargetDuration = if ($ExpectExit) { 2 } else { $rcrDuration }
    $rcrCommon = @('-loglevel','error','-nodisp','-autoexit','-volume','5','-f','lavfi','-i')
    if ($IncludeChildren) {
        $rcrHostExe = (Get-Process -Id $PID).Path
        $rcrHelper = Join-Path $PSScriptRoot 'process-audio-tone-parent.ps1'
        $rcrReady = Join-Path $rcrLogs "process-tone-$rcrStamp.ready"
        $rcrTarget = Start-Process -FilePath $rcrHostExe `
            -ArgumentList @('-NoProfile','-NonInteractive','-File',('"' + $rcrHelper + '"'),'-Player',('"' + $rcrPlayer + '"'),'-ReadyFile',('"' + $rcrReady + '"'),'-Seconds',$rcrTargetDuration) `
            -WindowStyle Hidden -RedirectStandardError (Join-Path $rcrLogs "process-tone-$rcrStamp-target.txt") -PassThru
        $rcrReadyDeadline = [DateTime]::UtcNow.AddSeconds(5)
        while (-not (Test-Path -LiteralPath $rcrReady)) {
            if ($rcrTarget.HasExited -or [DateTime]::UtcNow -ge $rcrReadyDeadline) { throw 'Owned child tone did not start' }
            Start-Sleep -Milliseconds 50
        }
    } else {
        $rcrTarget = Start-Process -FilePath $rcrPlayer -ArgumentList ($rcrCommon + "sine=frequency=440:sample_rate=48000:duration=$rcrTargetDuration") `
            -WindowStyle Hidden -RedirectStandardError (Join-Path $rcrLogs "process-tone-$rcrStamp-target.txt") -PassThru
    }
    $rcrOther = Start-Process -FilePath $rcrPlayer -ArgumentList ($rcrCommon + "sine=frequency=880:sample_rate=48000:duration=$rcrDuration") `
        -WindowStyle Hidden -RedirectStandardError (Join-Path $rcrLogs "process-tone-$rcrStamp-other.txt") -PassThru
    Start-Sleep -Milliseconds 800
    if ($rcrTarget.HasExited -or $rcrOther.HasExited) { throw 'Owned tone generator exited before capture' }
    $rcrProofArguments = @($rcrTarget.Id, $Seconds.ToString([Globalization.CultureInfo]::InvariantCulture), 440, 880, 10)
    if ($ExpectExit) { $rcrProofArguments += '--expect-exit' }
    & $rcrProof @rcrProofArguments |
        Tee-Object -FilePath (Join-Path $rcrLogs "process-audio-$rcrStamp.json")
    if ($LASTEXITCODE -ne 0) { throw 'Process audio isolation proof failed' }
}
finally {
    foreach ($rcrOwned in @($rcrTarget, $rcrOther)) {
        if ($null -ne $rcrOwned) {
            $rcrOwned.Refresh()
            if (-not $rcrOwned.HasExited) {
                if ($IncludeChildren -and $rcrOwned.Id -eq $rcrTarget.Id) { $rcrOwned.Kill($true) } else { $rcrOwned.Kill() }
                $rcrOwned.WaitForExit()
            }
            $rcrOwned.Dispose()
        }
    }
}
