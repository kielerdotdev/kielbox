param(
    [Parameter(Mandatory = $true)][string]$Player,
    [Parameter(Mandatory = $true)][string]$ReadyFile,
    [ValidateRange(1, 120)][int]$Seconds = 12
)
$ErrorActionPreference = 'Stop'
$rcrChild = $null
try {
    $rcrChild = Start-Process -FilePath $Player `
        -ArgumentList @('-loglevel','error','-nodisp','-autoexit','-volume','5','-f','lavfi','-i',"sine=frequency=440:sample_rate=48000:duration=$Seconds") `
        -WindowStyle Hidden -PassThru
    [IO.File]::WriteAllText($ReadyFile, 'owned generated tone child started')
    $rcrChild.WaitForExit()
    if ($rcrChild.ExitCode -ne 0) { throw "Owned tone child exited with $($rcrChild.ExitCode)" }
}
finally {
    if ($null -ne $rcrChild) {
        $rcrChild.Refresh()
        if (-not $rcrChild.HasExited) { $rcrChild.Kill(); $rcrChild.WaitForExit() }
        $rcrChild.Dispose()
    }
}
