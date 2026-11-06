param(
    [string]$Recording = '.downloads/gpu-transport-long-bt709-20261024.mkv',
    [string]$FfmpegLibDirectory,
    [switch]$Automatic,
    [switch]$UntilEof,
    [string]$Executable = 'target/debug/desktop.exe'
)
$ErrorActionPreference = 'Stop'
$rcrRoot = Split-Path -Parent $PSScriptRoot
$rcrSource = (Resolve-Path -LiteralPath $(if ([IO.Path]::IsPathRooted($Recording)) { $Recording } else { Join-Path $rcrRoot $Recording })).Path
$rcrExe = if ([IO.Path]::IsPathRooted($Executable)) { $Executable } else { Join-Path $rcrRoot $Executable }
if (-not $Automatic -and -not $FfmpegLibDirectory) { throw 'Choose -Automatic or supply -FfmpegLibDirectory.' }
if ($Automatic -and $FfmpegLibDirectory) { throw 'Automatic discovery does not take an explicit DLL directory.' }
$rcrDlls = if ($FfmpegLibDirectory) { (Resolve-Path -LiteralPath $FfmpegLibDirectory).Path } else { $null }
$rcrLogs = Join-Path $rcrRoot '.downloads'
[IO.Directory]::CreateDirectory($rcrLogs) | Out-Null
$rcrStamp = [guid]::NewGuid().ToString('N')
$rcrOut = Join-Path $rcrLogs "gpu-playback-$rcrStamp.stdout.txt"
$rcrErr = Join-Path $rcrLogs "gpu-playback-$rcrStamp.stderr.txt"
$rcrOldGpu = $env:RCR_GPU_PLAYBACK
$rcrOldDlls = $env:RCR_FFMPEG_LIB_DIR
$rcrMode = if ($UntilEof) { '--smoke-gpu-playback-eof' } elseif ($Automatic) { '--smoke-gpu-playback-automatic' } else { '--smoke-gpu-playback' }
$rcrSourceHash = $null
if ($UntilEof) {
    if ((Get-Item -LiteralPath $rcrSource).Length -gt 64MB) { throw 'The EOF diagnostic expects an owned short fixture no larger than 64 MiB.' }
    $rcrSourceHash = (Get-FileHash -LiteralPath $rcrSource -Algorithm SHA256).Hash
}
try {
    # Only this child opts in; no preference, registry or system PATH changes.
    $env:RCR_GPU_PLAYBACK = if ($Automatic) { $null } else { '1' }
    $env:RCR_FFMPEG_LIB_DIR = $rcrDlls
    $rcrArguments = @('--open', ('"' + $rcrSource + '"'), $rcrMode)
    if ($Automatic -and $UntilEof) { $rcrArguments += '--smoke-gpu-playback-automatic' }
    $rcrProcess = Start-Process -FilePath $rcrExe -WorkingDirectory $rcrRoot `
        -ArgumentList $rcrArguments `
        -WindowStyle Hidden -RedirectStandardOutput $rcrOut -RedirectStandardError $rcrErr -PassThru
    if (-not $rcrProcess.WaitForExit(35000)) {
        $rcrProcess.Kill($true)
        $rcrProcess.WaitForExit()
        throw "Owned GPU playback fixture timed out. Logs: $rcrOut; $rcrErr"
    }
    $rcrResult = Get-Content -LiteralPath $rcrOut -Raw
    if ($rcrProcess.ExitCode -ne 0 -or $rcrResult -notmatch 'GPU playback smoke passed:' -or $rcrResult -notmatch 'Editor open smoke: ready=true') {
        Get-Content -LiteralPath $rcrErr
        throw "GPU playback fixture failed with exit $($rcrProcess.ExitCode). Logs: $rcrOut; $rcrErr"
    }
    $rcrResult.Trim()
    if ($UntilEof) {
        if ($rcrResult -notmatch '"mode":"until_eof"') { throw 'The EOF diagnostic did not report its completed mode.' }
        if ((Get-FileHash -LiteralPath $rcrSource -Algorithm SHA256).Hash -ne $rcrSourceHash) { throw 'The owned source fixture changed during the diagnostic.' }
        Write-Output 'Owned GPU fixture source SHA-256 unchanged.'
    }
} finally {
    $env:RCR_GPU_PLAYBACK = $rcrOldGpu
    $env:RCR_FFMPEG_LIB_DIR = $rcrOldDlls
}
