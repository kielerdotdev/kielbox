param(
    [Parameter(Mandatory)][string]$Recording,
    [string]$Executable = 'target/debug/rcr-desktop.exe',
    [string]$ReuseSeekCache = '',
    [ValidateSet('1440x900','2560x1440')][string]$WindowSize = '2560x1440'
)
$ErrorActionPreference = 'Stop'
$rcrMemoryRoot = Split-Path -Parent $PSScriptRoot
$rcrMemorySource = (Resolve-Path -LiteralPath $(if ([IO.Path]::IsPathRooted($Recording)) { $Recording } else { Join-Path $rcrMemoryRoot $Recording })).Path
$rcrMemoryExe = if ([IO.Path]::IsPathRooted($Executable)) { $Executable } else { Join-Path $rcrMemoryRoot $Executable }
$rcrMemoryOwned = Join-Path $rcrMemoryRoot ('.downloads/editor-memory-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $rcrMemoryOwned | Out-Null
Set-Content -LiteralPath (Join-Path $rcrMemoryOwned '.rcr-memory-proof') -Value 'rcr-owned-editor-memory-v1'
$rcrMemoryCache = Join-Path $rcrMemoryOwned 'seek-cache'
if ($ReuseSeekCache) {
    $rcrMemoryCache = (Resolve-Path -LiteralPath $ReuseSeekCache).Path
    $rcrMemoryParent = Split-Path -Parent $rcrMemoryCache
    $rcrDownloads = (Resolve-Path -LiteralPath (Join-Path $rcrMemoryRoot '.downloads')).Path.TrimEnd('\')
    if (-not $rcrMemoryCache.StartsWith($rcrDownloads + '\scrub-pressure-', [StringComparison]::OrdinalIgnoreCase) -or
        (Split-Path -Leaf $rcrMemoryCache) -ne 'seek-cache' -or
        (Get-Content -LiteralPath (Join-Path $rcrMemoryParent '.rcr-seek-smoke') -Raw).Trim() -ne 'rcr-owned-sustained-pressure-v1') { throw 'Cache reuse requires an owned pressure fixture cache.' }
    foreach ($rcrMemoryPath in @($rcrMemoryCache,$rcrMemoryParent)) {
        if ((Get-Item -LiteralPath $rcrMemoryPath).Attributes -band [IO.FileAttributes]::ReparsePoint) { throw 'Owned cache cannot be a reparse point.' }
    }
}
$rcrMemoryBefore = (Get-FileHash -LiteralPath $rcrMemorySource -Algorithm SHA256).Hash
$rcrMemoryOldTrace = $env:RCR_TRACE_MEMORY
$rcrMemoryOldCache = $env:RCR_SEEK_CACHE_DIR
$rcrMemoryOut = Join-Path $rcrMemoryOwned 'stdout.txt'
$rcrMemoryErr = Join-Path $rcrMemoryOwned 'stderr.txt'
try {
    $env:RCR_TRACE_MEMORY = '1'
    $env:RCR_SEEK_CACHE_DIR = $rcrMemoryCache
    $rcrMemoryChild = Start-Process -FilePath $rcrMemoryExe -WorkingDirectory $rcrMemoryOwned -ArgumentList @('--open',('"' + $rcrMemorySource + '"'),'--smoke-editor-resources','--snapshot-size',$WindowSize) -WindowStyle Hidden -RedirectStandardOutput $rcrMemoryOut -RedirectStandardError $rcrMemoryErr -PassThru
    $rcrMemoryTimedOut = -not $rcrMemoryChild.WaitForExit(45000)
    if ($rcrMemoryTimedOut) { $rcrMemoryChild.Kill($true); $rcrMemoryChild.WaitForExit() }
    $rcrMemoryText = Get-Content -LiteralPath $rcrMemoryOut -Raw
    Write-Output $rcrMemoryText
    $rcrMemoryReleased = @([IO.File]::ReadLines($rcrMemoryOut) | Where-Object { $_.StartsWith('Editor resources released: ') })
    if ($rcrMemoryTimedOut -or $rcrMemoryChild.ExitCode -ne 0 -or $rcrMemoryReleased.Count -ne 4 -or $rcrMemoryText -notmatch 'Editor open smoke: ready=true') {
        Get-Content -LiteralPath $rcrMemoryErr -Tail 8
        throw "Editor memory lifecycle proof failed: $rcrMemoryOwned"
    }
} finally {
    $env:RCR_TRACE_MEMORY = $rcrMemoryOldTrace
    $env:RCR_SEEK_CACHE_DIR = $rcrMemoryOldCache
    if ((Get-FileHash -LiteralPath $rcrMemorySource -Algorithm SHA256).Hash -ne $rcrMemoryBefore) { throw 'Original media SHA-256 changed.' }
    Write-Output "Original media unchanged; owned memory proof: $rcrMemoryOwned"
}
