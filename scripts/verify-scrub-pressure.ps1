param(
    [string]$Recording = '.downloads/gpu-transport-long-bt709-20261024.mkv',
    [switch]$Cold,
    [switch]$Trace,
    [switch]$Software,
    [switch]$NativeAv1,
    [switch]$NativeMapped,
    [switch]$InteractiveLanding,
    [switch]$DisableProxy,
    [ValidateRange(-1, 2147483647)][int]$FinalFrame = -1,
    [ValidateSet('1440x900', '2048x1152', '2560x1440')][string]$WindowSize = '1440x900',
    [string]$ReuseSeekCache = '',
    [ValidateSet(1, 2)][int]$ProxyThreads = 1,
    [ValidateSet('frame', 'slice')][string]$ProxyThreadType = 'frame',
    [string]$Executable = 'target/debug/rcr-desktop.exe'
)
$ErrorActionPreference = 'Stop'
if ($DisableProxy -and -not $Cold) { throw 'Use -Cold when disabling proxy preparation.' }
if ($NativeMapped -and (-not $NativeAv1 -or -not $Trace -or -not $Cold -or -not $DisableProxy)) {
    throw 'Mapped decoder diagnostics require -NativeAv1 -Trace -Cold -DisableProxy to establish actual backend use without proxy substitution.'
}
$rcrRoot = Split-Path -Parent $PSScriptRoot
$rcrSource = (Resolve-Path -LiteralPath $(if ([IO.Path]::IsPathRooted($Recording)) { $Recording } else { Join-Path $rcrRoot $Recording })).Path
$rcrExe = if ([IO.Path]::IsPathRooted($Executable)) { $Executable } else { Join-Path $rcrRoot $Executable }
$rcrOwned = Join-Path $rcrRoot ('.downloads/scrub-pressure-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $rcrOwned | Out-Null
Set-Content -LiteralPath (Join-Path $rcrOwned '.rcr-seek-smoke') -Value 'rcr-owned-sustained-pressure-v1'
$rcrCache = Join-Path $rcrOwned 'seek-cache'
if ($ReuseSeekCache) {
    $rcrCache = (Resolve-Path -LiteralPath $ReuseSeekCache -ErrorAction Stop).Path
    $rcrDownloads = (Resolve-Path -LiteralPath (Join-Path $rcrRoot '.downloads')).Path.TrimEnd('\')
    $rcrCacheParent = Split-Path -Parent $rcrCache
    if (-not $rcrCache.StartsWith($rcrDownloads + '\scrub-pressure-', [StringComparison]::OrdinalIgnoreCase) -or
        (Split-Path -Leaf $rcrCache) -ne 'seek-cache' -or
        (Get-Content -LiteralPath (Join-Path $rcrCacheParent '.rcr-seek-smoke') -Raw).Trim() -ne 'rcr-owned-sustained-pressure-v1') {
        throw 'Cache reuse requires an owned scrub-pressure fixture cache.'
    }
    foreach ($rcrCachePath in @($rcrCache, $rcrCacheParent)) {
        if ((Get-Item -LiteralPath $rcrCachePath).Attributes -band [IO.FileAttributes]::ReparsePoint) { throw 'Owned cache cannot be a reparse point.' }
    }
}
$rcrOut = Join-Path $rcrOwned 'stdout.txt'
$rcrErr = Join-Path $rcrOwned 'stderr.txt'
$rcrBefore = (Get-FileHash -LiteralPath $rcrSource -Algorithm SHA256).Hash
$rcrOldCache = $env:RCR_SEEK_CACHE_DIR
$rcrOldTrace = $env:RCR_TRACE_SEEK
$rcrOldSoftware = $env:RCR_SCRUB_SOFTWARE
$rcrOldProxy = $env:RCR_DISABLE_SEEK_PROXY
$rcrOldNative = $env:RCR_SCRUB_NATIVE_AV1
$rcrOldInteractive = $env:RCR_SCRUB_INTERACTIVE_LANDING
$rcrOldMapped = $env:RCR_SCRUB_NATIVE_MAPPED
$rcrOldFinalFrame = $env:RCR_PRESSURE_FINAL_FRAME
$rcrOldProxyThreads = $env:RCR_SEEK_PROXY_THREADS
$rcrOldProxyThreadType = $env:RCR_SEEK_PROXY_THREAD_TYPE
try {
    $env:RCR_SEEK_CACHE_DIR = $rcrCache
    $env:RCR_TRACE_SEEK = if ($Trace) { '1' } else { $null }
    $env:RCR_SCRUB_SOFTWARE = if ($Software) { '1' } else { $null }
    $env:RCR_DISABLE_SEEK_PROXY = if ($DisableProxy) { '1' } else { $null }
    $env:RCR_SCRUB_NATIVE_AV1 = if ($NativeAv1) { '1' } else { $null }
    $env:RCR_SCRUB_INTERACTIVE_LANDING = if ($InteractiveLanding) { '1' } else { $null }
    $env:RCR_SCRUB_NATIVE_MAPPED = if ($NativeMapped) { '1' } else { $null }
    $env:RCR_PRESSURE_FINAL_FRAME = if ($FinalFrame -ge 0) { $FinalFrame.ToString([Globalization.CultureInfo]::InvariantCulture) } else { $null }
    $env:RCR_SEEK_PROXY_THREADS = if ($ProxyThreads -eq 2) { '2' } else { $null }
    $env:RCR_SEEK_PROXY_THREAD_TYPE = if ($ProxyThreadType -eq 'slice') { 'slice' } else { $null }
    $rcrMode = if ($Cold) { '--smoke-scrub-pressure-cold' } else { '--smoke-scrub-pressure' }
    # Separate owned project/cache state. Read the original media; never copy or edit it.
    $rcrChild = Start-Process -FilePath $rcrExe -WorkingDirectory $rcrOwned -WindowStyle Hidden `
        -ArgumentList @('--open', ('"' + $rcrSource + '"'), $rcrMode, '--snapshot-size', $WindowSize) -PassThru `
        -RedirectStandardOutput $rcrOut -RedirectStandardError $rcrErr
    $rcrTimedOut = -not $rcrChild.WaitForExit(25000)
    if ($rcrTimedOut) { $rcrChild.Kill($true); $rcrChild.WaitForExit() }
    $rcrText = Get-Content -LiteralPath $rcrOut -Raw
    if ($rcrText) { $rcrText.Trim() }
    if ($Trace) {
        $rcrSamples = @(Get-Content -LiteralPath $rcrErr | Where-Object { $_.StartsWith('Native seek trace: ') } | ForEach-Object { $_.Substring(19) | ConvertFrom-Json })
        $rcrSummary = [ordered]@{ samples = $rcrSamples.Count; hardware_samples = @($rcrSamples | Where-Object hardware).Count; surface_hits = @($rcrSamples | Where-Object surface_hit).Count }
        $rcrSummary['backends'] = @($rcrSamples | Group-Object backend | ForEach-Object { @{ name = $_.Name; count = $_.Count } })
        foreach ($rcrPhase in @('seek_ms', 'flush_ms', 'decode_ms', 'output_ms', 'total_ms')) {
            $rcrValues = @($rcrSamples | Where-Object { $_.PSObject.Properties.Name -contains $rcrPhase } | ForEach-Object { [double]$_.$rcrPhase } | Sort-Object)
            if ($rcrValues.Count) { $rcrSummary[$rcrPhase] = @{ median = $rcrValues[[int][Math]::Floor(($rcrValues.Count - 1) / 2)]; p95 = $rcrValues[[int][Math]::Round(($rcrValues.Count - 1) * 0.95)]; max = $rcrValues[-1] } }
        }
        'Native seek phase diagnostics: ' + ($rcrSummary | ConvertTo-Json -Depth 4 -Compress)
        # UI emits at most 256 successful paint receipts. Bound collection here
        # too; retain raw stderr independently for reviewing individual samples.
        $rcrPipelinePrefix = 'Seek pipeline trace: '
        $rcrPipelineSamples = [Collections.Generic.List[object]]::new()
        $rcrPipelineObserved = 0
        $rcrPipelineMalformed = 0
        foreach ($rcrLine in [IO.File]::ReadLines($rcrErr)) {
            if (-not $rcrLine.StartsWith($rcrPipelinePrefix, [StringComparison]::Ordinal)) { continue }
            $rcrPipelineObserved++
            if ($rcrPipelineObserved -gt 256) { continue }
            try {
                $rcrPipelineSample = $rcrLine.Substring($rcrPipelinePrefix.Length) | ConvertFrom-Json
                if ($rcrPipelineSample.original -isnot [bool]) { throw 'Missing pipeline provenance.' }
                $rcrPipelineSamples.Add($rcrPipelineSample)
            } catch { $rcrPipelineMalformed++ }
        }
        $rcrPipelineSummary = [ordered]@{
            scope = 'Successful GPUI image paint call; not GPU completion or monitor scanout'
            units = 'milliseconds'
            sample_limit = 256
            observed = $rcrPipelineObserved
            samples = $rcrPipelineSamples.Count
            malformed = $rcrPipelineMalformed
            truncated = $rcrPipelineObserved -gt 256
        }
        foreach ($rcrPipelineGroup in @('fast', 'original')) {
            $rcrPipelineOriginal = $rcrPipelineGroup -eq 'original'
            $rcrGroupSamples = @($rcrPipelineSamples | Where-Object { $_.original -eq $rcrPipelineOriginal })
            $rcrGroupSummary = [ordered]@{ samples = $rcrGroupSamples.Count }
            foreach ($rcrStage in @('queue_ms', 'core_decode_ms', 'cache_copy_ms', 'ready_to_ui_ms', 'ui_to_bind_ms', 'bind_ms', 'bind_to_paint_ms', 'paint_call_ms', 'total_ms')) {
                $rcrStageValues = @($rcrGroupSamples | Where-Object { $_.PSObject.Properties.Name -contains $rcrStage } | ForEach-Object {
                    $rcrStageValue = 0.0
                    if ($null -ne $_.$rcrStage -and [double]::TryParse([Convert]::ToString($_.$rcrStage, [Globalization.CultureInfo]::InvariantCulture), [Globalization.NumberStyles]::Float, [Globalization.CultureInfo]::InvariantCulture, [ref]$rcrStageValue) -and
                        -not [double]::IsNaN($rcrStageValue) -and -not [double]::IsInfinity($rcrStageValue) -and $rcrStageValue -ge 0) { $rcrStageValue }
                } | Sort-Object)
                $rcrStageSummary = [ordered]@{ samples = $rcrStageValues.Count; median = $null; p95 = $null; max = $null }
                if ($rcrStageValues.Count) {
                    $rcrStageSummary.median = $rcrStageValues[[int][Math]::Floor(($rcrStageValues.Count - 1) / 2)]
                    $rcrStageSummary.p95 = $rcrStageValues[[int][Math]::Round(($rcrStageValues.Count - 1) * 0.95)]
                    $rcrStageSummary.max = $rcrStageValues[-1]
                }
                $rcrGroupSummary[$rcrStage] = $rcrStageSummary
            }
            $rcrPipelineSummary[$rcrPipelineGroup] = $rcrGroupSummary
        }
        $rcrPipelineJson = $rcrPipelineSummary | ConvertTo-Json -Depth 6
        Set-Content -LiteralPath (Join-Path $rcrOwned 'pipeline-summary.json') -Value $rcrPipelineJson -Encoding utf8
        'Seek pipeline phase diagnostics: ' + ($rcrPipelineSummary | ConvertTo-Json -Depth 6 -Compress)
    }
    # Check on failures and timeouts too; a failed performance bar never bypasses this.
    if ((Get-FileHash -LiteralPath $rcrSource -Algorithm SHA256).Hash -ne $rcrBefore) { throw 'Original source SHA-256 changed.' }
    Write-Output "Original source SHA-256 unchanged; isolated artifacts: $rcrOwned"
    if ($NativeMapped) {
        $rcrMapped = @($rcrSamples | Where-Object { $_.direct_mapped -eq $true -and $_.backend -eq 'native_av1_cuda' })
        if (-not $rcrMapped.Count -or $rcrMapped.Count -ne $rcrSamples.Count) {
            throw "The mapped experiment did not use the requested backend for every traced seek: $rcrOwned"
        }
        if (@($rcrMapped | Where-Object { $_.decoder_pool_size -le 0 -or $_.decoder_pool_size -gt 32 -or $_.mapped_retained_peak -gt 1 }).Count) {
            throw "Mapped pool/retention evidence exceeded the experiment's bounds: $rcrOwned"
        }
        'Mapped decoder evidence: ' + (@{ samples = $rcrMapped.Count; pool_sizes = @($rcrMapped.decoder_pool_size | Sort-Object -Unique); retained_peak = ($rcrMapped.mapped_retained_peak | Measure-Object -Maximum).Maximum } | ConvertTo-Json -Compress)
    }
    if ($rcrTimedOut) { throw "Owned pressure fixture exceeded 25 seconds: $rcrOwned" }
    if ($rcrChild.ExitCode -ne 0 -or $rcrText -notmatch 'Sustained scrub pressure:' -or $rcrText -notmatch 'Editor open smoke: ready=true') {
        Get-Content -LiteralPath $rcrErr | Where-Object { -not $_.StartsWith('Native seek trace: ') } | Select-Object -Last 12
        throw "Sustained pressure responsiveness checks failed: $rcrOwned"
    }
} finally {
    $env:RCR_SEEK_CACHE_DIR = $rcrOldCache
    $env:RCR_TRACE_SEEK = $rcrOldTrace
    $env:RCR_SCRUB_SOFTWARE = $rcrOldSoftware
    $env:RCR_DISABLE_SEEK_PROXY = $rcrOldProxy
    $env:RCR_SCRUB_NATIVE_AV1 = $rcrOldNative
    $env:RCR_SCRUB_INTERACTIVE_LANDING = $rcrOldInteractive
    $env:RCR_SCRUB_NATIVE_MAPPED = $rcrOldMapped
    $env:RCR_PRESSURE_FINAL_FRAME = $rcrOldFinalFrame
    $env:RCR_SEEK_PROXY_THREADS = $rcrOldProxyThreads
    $env:RCR_SEEK_PROXY_THREAD_TYPE = $rcrOldProxyThreadType
}
