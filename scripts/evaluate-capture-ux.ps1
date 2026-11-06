param(
    [string]$Executable = 'target/debug/desktop.exe',
    [string]$OutputDirectory = '.downloads/capture-ux-evaluations',
    [ValidateRange(1, 10)][int]$Runs = 3,
    [switch]$Build,
    [switch]$EnforceViewerBudgets,
    [switch]$IncludeAudio,
    [switch]$SkipPixelBenchmarks,
    [switch]$SkipRecordingBenchmark
)
$ErrorActionPreference = 'Stop'
$rcrEvalRoot = [IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
Push-Location $rcrEvalRoot
$rcrEvalEnvironment = @{}
foreach ($name in @('TEMP', 'TMP', 'RCR_CLIP_SHOW_EVAL_OUTPUT', 'RCR_STOP_EVAL_OUTPUT', 'RCR_GPU_PLAYBACK', 'RCR_SCRUB_SOFTWARE')) {
    $rcrEvalEnvironment[$name] = [Environment]::GetEnvironmentVariable($name, 'Process')
}
try {
    $rcrEvalBase = if ([IO.Path]::IsPathRooted($OutputDirectory)) { $OutputDirectory } else { Join-Path $rcrEvalRoot $OutputDirectory }
    $rcrEvalDirectory = Join-Path $rcrEvalBase ([DateTime]::UtcNow.ToString('yyyyMMdd-HHmmss') + '-' + [Guid]::NewGuid().ToString('N').Substring(0, 8))
    New-Item -ItemType Directory -Path $rcrEvalDirectory -Force | Out-Null
    $rcrEvalTemp = Join-Path $rcrEvalDirectory 'temp'
    New-Item -ItemType Directory -Path $rcrEvalTemp | Out-Null
    $env:TEMP = $rcrEvalTemp
    $env:TMP = $rcrEvalTemp
    $env:RCR_GPU_PLAYBACK = '0'
    $env:RCR_SCRUB_SOFTWARE = '1'
    $rcrEvalRecords = [Collections.Generic.List[object]]::new()

    function Invoke-RcrEvaluation {
        param([string]$Name, [string]$Command, [string[]]$Arguments, [string]$RequiredOutput = '')
        $rcrEvalLog = Join-Path $rcrEvalDirectory ($Name + '.log')
        $rcrEvalTimer = [Diagnostics.Stopwatch]::StartNew()
        $rcrEvalPreviousError = $ErrorActionPreference
        try {
            # Keep native diagnostics in the log; exit codes decide success.
            $ErrorActionPreference = 'Continue'
            $rcrEvalLines = @(& $Command @Arguments 2>&1)
            $rcrEvalExit = $LASTEXITCODE
        } finally { $ErrorActionPreference = $rcrEvalPreviousError }
        $rcrEvalTimer.Stop()
        $rcrEvalLines | ForEach-Object { $_.ToString() } | Set-Content -LiteralPath $rcrEvalLog -Encoding UTF8
        $rcrEvalRecords.Add([ordered]@{ name = $Name; exit_code = $rcrEvalExit; elapsed_ms = $rcrEvalTimer.Elapsed.TotalMilliseconds; log = $rcrEvalLog })
        Write-Output "$Name : exit=$rcrEvalExit, elapsed=$([Math]::Round($rcrEvalTimer.Elapsed.TotalSeconds, 2))s"
        if ($rcrEvalExit -ne 0) {
            Get-Content -LiteralPath $rcrEvalLog -Tail 35 | Write-Output
            throw "Evaluation failed: $Name. See $rcrEvalLog"
        }
        if ($RequiredOutput -and -not (($rcrEvalLines | ForEach-Object { $_.ToString() }) -join "`n" -match $RequiredOutput)) {
            throw "Evaluation produced no required measurements: $Name. See $rcrEvalLog"
        }
    }

    if ($Build) { Invoke-RcrEvaluation 'build' 'cargo' @('build', '--offline', '-p', 'desktop', '-j1') }
    $rcrEvalExecutable = (Resolve-Path -LiteralPath $Executable).Path
    $rcrEvalSource = Join-Path $rcrEvalDirectory 'silent-source.mp4'
    Invoke-RcrEvaluation 'generate-silent-source' 'ffmpeg' @('-hide_banner', '-loglevel', 'error', '-n', '-f', 'lavfi', '-i', 'testsrc2=size=640x360:rate=30:duration=6', '-an', '-c:v', 'libx264', '-preset', 'veryfast', '-threads', '2', '-pix_fmt', 'yuv420p', $rcrEvalSource)
    $rcrEvalSources = [ordered]@{ video = $rcrEvalSource }
    if ($IncludeAudio) {
        $rcrEvalSources.aac = Join-Path $rcrEvalDirectory 'aac-source.mp4'
        Invoke-RcrEvaluation 'generate-aac-source' 'ffmpeg' @('-hide_banner', '-loglevel', 'error', '-n', '-i', $rcrEvalSource, '-f', 'lavfi', '-i', 'sine=frequency=440:sample_rate=48000:duration=6', '-map', '0:v:0', '-map', '1:a:0', '-c:v', 'copy', '-c:a', 'aac', '-ar', '48000', '-ac', '2', '-threads', '1', $rcrEvalSources.aac)
        $rcrEvalSources.pcm = Join-Path $rcrEvalDirectory 'pcm-source.mkv'
        Invoke-RcrEvaluation 'generate-pcm-source' 'ffmpeg' @('-hide_banner', '-loglevel', 'error', '-n', '-i', $rcrEvalSource, '-f', 'lavfi', '-i', 'sine=frequency=440:sample_rate=48000:duration=6', '-f', 'lavfi', '-i', 'sine=frequency=880:sample_rate=48000:duration=6', '-map', '0:v:0', '-map', '1:a:0', '-map', '2:a:0', '-c:v', 'copy', '-c:a', 'pcm_f32le', '-ar', '48000', '-ac', '2', '-threads', '1', $rcrEvalSources.pcm)
    }
    $rcrEvalMetrics = [Collections.Generic.List[object]]::new()
    for ($rcrEvalRun = 1; $rcrEvalRun -le $Runs; $rcrEvalRun++) {
        foreach ($rcrEvalSize in @('900x600', '1440x900')) {
            foreach ($rcrEvalFixture in $rcrEvalSources.Keys) {
                $rcrEvalName = "viewer-$rcrEvalSize-$rcrEvalFixture-$rcrEvalRun"
                $env:RCR_CLIP_SHOW_EVAL_OUTPUT = Join-Path $rcrEvalDirectory ($rcrEvalName + '.json')
                Invoke-RcrEvaluation $rcrEvalName $rcrEvalExecutable @('--eval-clip-show', '--open', $rcrEvalSources[$rcrEvalFixture], '--snapshot-size', $rcrEvalSize)
                if (-not (Test-Path -LiteralPath $env:RCR_CLIP_SHOW_EVAL_OUTPUT)) { throw "Missing viewer metrics: $rcrEvalName" }
                $rcrEvalMetrics.Add([ordered]@{ name = $rcrEvalName; fixture = $rcrEvalFixture; metrics = (Get-Content -LiteralPath $env:RCR_CLIP_SHOW_EVAL_OUTPUT -Raw | ConvertFrom-Json) })
            }
        }
    }
    foreach ($rcrEvalSize in @('900x600', '1440x900')) {
        Invoke-RcrEvaluation "layout-$rcrEvalSize" $rcrEvalExecutable @('--ui-snapshot', 'editor-clip-show', '--open', $rcrEvalSource, '--snapshot-size', $rcrEvalSize, '--snapshot-output', (Join-Path $rcrEvalDirectory "viewer-$rcrEvalSize.png"))
    }
    Invoke-RcrEvaluation 'capture-menu' $rcrEvalExecutable @('--smoke-capture-menu', '--proof-output', (Join-Path $rcrEvalDirectory 'capture-menu')) 'Capture proof passed'
    if ($IncludeAudio) {
        Invoke-RcrEvaluation 'editor-keyboard' $rcrEvalExecutable @('--smoke-editor-keyboard', '--open', $rcrEvalSources.aac) 'Editor keyboard smoke passed'
    }
    if (-not $SkipPixelBenchmarks) {
        Invoke-RcrEvaluation 'hdr' 'cargo' @('test', '--offline', '-p', 'hdr', 'timing_cached_screenshot_pipeline', '--', '--ignored', '--nocapture', '--test-threads=1') 'HDR synthetic.*identical_pixels=true'
        Invoke-RcrEvaluation 'png' 'cargo' @('test', '--offline', '-p', 'workflows', 'timing_screenshot_png_compression', '--', '--ignored', '--nocapture', '--test-threads=1') 'PNG synthetic.*identical_pixels=true'
        Invoke-RcrEvaluation 'png-allocation' 'cargo' @('test', '--offline', '-p', 'image-types', 'timing_borrowed_png_encode', '--', '--ignored', '--nocapture', '--test-threads=1') 'avoided_pixel_allocation_bytes=.*identical_encoded_bytes=true'
        Invoke-RcrEvaluation 'picker-handoff' 'cargo' @('test', '--offline', '-p', 'desktop', 'timing_picker_preparation_handoff', '--', '--ignored', '--nocapture', '--test-threads=1') 'new_ui_handle_handoff_ms='
    }
    if (-not $SkipRecordingBenchmark) {
        $env:RCR_STOP_EVAL_OUTPUT = Join-Path $rcrEvalDirectory 'recording-stop.json'
        Invoke-RcrEvaluation 'recording-stop' 'cargo' @('test', '--offline', '-p', 'desktop', 'quick_recording::stress_tests::generated_stop_latency_eval', '--', '--ignored', '--exact', '--nocapture', '--test-threads=1')
        if (-not (Test-Path -LiteralPath $env:RCR_STOP_EVAL_OUTPUT)) { throw 'Missing recording metrics' }
        Invoke-RcrEvaluation 'native-mux-finalization' 'cargo' @('run', '--offline', '-p', 'media-io', '--example', 'eval-finalization', '-j1', '--', (Join-Path $rcrEvalDirectory 'native-mux-finalization.json'))
    }
    # Local regression targets for this fixture/runtime, not hardware guarantees.
    $rcrEvalBudgets = @{
        cold_open_to_first_paint = 1200
        switch_A_to_B = 1200
        switch_B_to_C = 1200
        switch_C_to_B = 300
        switch_B_to_edited_A = 300
        exact_seek_1 = 300
        exact_seek_2 = 300
        exact_seek_3 = 300
        rapid_100_switches_to_correct_first_paint = 350
        automatic_next_including_quarter_second_tail = 1000
        'switch_C_to_B:audio_clock' = 400
        'switch_B_to_edited_A:audio_clock' = 400
        'rapid_100_switches_to_correct_first_paint:audio_clock' = 400
        'skip_unavailable_to_next_first_paint:audio_clock' = 400
        'reopen_edited_clip_after_error_and_autoplay:audio_clock' = 400
    }
    $rcrEvalGroups = @{}
    foreach ($run in $rcrEvalMetrics) {
        $viewport = $run.name.Split('-')[1]
        $fixture = $run.fixture
        foreach ($sample in $run.metrics.samples) {
            $key = "$viewport|$fixture|$($sample.operation)"
            if (-not $rcrEvalGroups.ContainsKey($key)) { $rcrEvalGroups[$key] = [Collections.Generic.List[double]]::new() }
            $rcrEvalGroups[$key].Add([double]$sample.latency_ms)
            if ($null -ne $sample.audio_clock_advancing_ms) {
                $clockKey = "$key`:audio_clock"
                if (-not $rcrEvalGroups.ContainsKey($clockKey)) { $rcrEvalGroups[$clockKey] = [Collections.Generic.List[double]]::new() }
                $rcrEvalGroups[$clockKey].Add([double]$sample.audio_clock_advancing_ms)
            }
        }
    }
    $rcrEvalChecks = @(
        foreach ($key in ($rcrEvalGroups.Keys | Sort-Object)) {
            $viewport, $fixture, $operation = $key.Split('|')
            if (-not $rcrEvalBudgets.ContainsKey($operation)) { continue }
            $values = @($rcrEvalGroups[$key] | Sort-Object)
            $middle = [int][Math]::Floor($values.Count / 2)
            $median = if ($values.Count % 2) { $values[$middle] } else { ($values[$middle - 1] + $values[$middle]) / 2 }
            # This case deliberately seeks to a fractional-frame audio tail.
            # Exact legacy audio seeking still uses the external decoder.
            $budget = if ($fixture -ne 'video' -and $operation -eq 'automatic_next_including_quarter_second_tail') { 1500 } else { $rcrEvalBudgets[$operation] }
            [ordered]@{ viewport = $viewport; fixture = $fixture; operation = $operation; median_ms = $median; budget_ms = $budget; passed = ($median -le $budget) }
        }
    )
    [ordered]@{
        scope = 'Generated silent media and hidden native windows. No desktop/audio-device capture, audible playback, OS input, clipboard, messaging or live GPU encoding.'
        executable = $rcrEvalExecutable
        sha256 = (Get-FileHash -LiteralPath $rcrEvalExecutable -Algorithm SHA256).Hash
        completed_utc = [DateTime]::UtcNow.ToString('o')
        runs = $rcrEvalRecords.ToArray()
        viewer = $rcrEvalMetrics.ToArray()
        viewer_budget_checks = $rcrEvalChecks
    } | ConvertTo-Json -Depth 16 | Set-Content -LiteralPath (Join-Path $rcrEvalDirectory 'summary.json') -Encoding UTF8
    Write-Output "Evaluation report: $(Join-Path $rcrEvalDirectory 'summary.json')"
    if ($EnforceViewerBudgets -and @($rcrEvalChecks | Where-Object { -not $_.passed }).Count -gt 0) {
        throw 'Viewer responsiveness targets failed. See viewer_budget_checks in summary.json.'
    }
} finally {
    foreach ($name in $rcrEvalEnvironment.Keys) { [Environment]::SetEnvironmentVariable($name, $rcrEvalEnvironment[$name], 'Process') }
    Pop-Location
}
