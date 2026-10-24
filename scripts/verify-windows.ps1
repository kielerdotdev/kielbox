param([switch]$Audio)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$workspace = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
Push-Location -LiteralPath $workspace
try {
    & cargo test --locked -j1
    if ($LASTEXITCODE -ne 0) { throw 'Application workspace tests failed.' }
    & cargo test --locked -p rcr-workflows -p rcr-image-editor -p rcr-image-effects -p rcr-upload -j1
    if ($LASTEXITCODE -ne 0) { throw 'Screenshot and workflow tests failed.' }
    & cargo build -p rcr-windows -j1
    if ($LASTEXITCODE -ne 0) { throw 'Capture build failed.' }
    $stamp = Get-Date -Format 'yyyyMMdd-HHmmss-fff'
    $output = Join-Path $workspace "recordings/verify-$stamp.mkv"
    $replay = Join-Path $workspace "recordings/verify-$stamp-replay.mkv"
    $sources = if ($Audio) { 'system,mic' } else { 'none' }
    & ./target/debug/rcr-capture.exe --seconds 6 --output $output --audio $sources --save-at 3 --save-replay $replay
    if ($LASTEXITCODE -ne 0) { throw 'Native recording/replay proof failed.' }
    $rcrStatsProbe = & ffprobe -v error -show_entries format_tags=_RCR_METADATA -of json $output
    if ($LASTEXITCODE -ne 0) { throw 'Embedded statistics probe failed.' }
    $rcrMetadata = (($rcrStatsProbe | ConvertFrom-Json).format.tags._RCR_METADATA | ConvertFrom-Json)
    $stats = $rcrMetadata.session.statistics
    if (Test-Path -LiteralPath ([IO.Path]::ChangeExtension($output, 'stats.json'))) { throw 'Recording unexpectedly created a statistics sidecar.' }
    if ($stats.encoded -ne 360 -or $stats.raw_cpu_readbacks -ne 0 -or $stats.audio_errors.Count -ne 0) {
        throw 'Unexpected frame count, raw readbacks or audio errors. Inspect embedded capture statistics.'
    }
    foreach ($path in @($output, $replay)) {
        $metadataText = & ffprobe -v error -show_streams -show_format -of json $path
        if ($LASTEXITCODE -ne 0) { throw "FFprobe failed: $path" }
        $metadata = $metadataText | ConvertFrom-Json
        $rcrEmbedded = $metadata.format.tags._RCR_METADATA | ConvertFrom-Json
        $rcrExpectedKind = if ($path -eq $output) { 'recording' } else { 'replay' }
        if ($rcrEmbedded.kind -ne $rcrExpectedKind -or $rcrEmbedded.schema_version -ne 1) { throw "Unexpected embedded metadata: $path" }
        $video = @($metadata.streams | Where-Object codec_type -eq 'video')
        $audioStreams = @($metadata.streams | Where-Object codec_type -eq 'audio')
        if ($video.Count -ne 1 -or $video[0].codec_name -ne 'av1' -or $video[0].width -ne 2560 -or $video[0].height -ne 1440 -or $video[0].r_frame_rate -ne '60/1') {
            throw "Unexpected video profile: $path"
        }
        if ($audioStreams.Count -ne $(if ($Audio) { 2 } else { 0 })) { throw "Unexpected audio stream count: $path" }
        $packetText = & ffprobe -v error -show_packets -show_entries packet=stream_index,pts_time,flags -of json $path
        if ($LASTEXITCODE -ne 0) { throw "Packet inspection failed: $path" }
        $packets = ($packetText | ConvertFrom-Json).packets
        $previous = @{}
        $firstVideo = $true
        foreach ($packet in $packets) {
            $index = [int]$packet.stream_index
            $pts = [double]::Parse($packet.pts_time, [Globalization.CultureInfo]::InvariantCulture)
            if ($previous.ContainsKey($index) -and $pts -lt $previous[$index]) { throw "Non-monotonic PTS: $path stream $index" }
            $previous[$index] = $pts
            if ($index -eq [int]$video[0].index -and $firstVideo) {
                if (-not $packet.flags.Contains('K')) { throw "Replay does not begin on a keyframe: $path" }
                $firstVideo = $false
            }
        }
        & ffmpeg -v error -threads 2 -i $path -map 0 -f null NUL
        if ($LASTEXITCODE -ne 0) { throw "Full decode failed: $path" }
        Write-Host "Verified $path"
    }
    Write-Host "Capture and replay verified. Missed deadlines: $($stats.missed_deadlines); audio discontinuities: $($stats.audio_discontinuities)."
} finally { Pop-Location }
