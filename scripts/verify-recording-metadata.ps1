param([Parameter(Mandatory)][string]$Recording)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$rcrMetadataRoot = Split-Path -Parent $PSScriptRoot
$rcrMetadataSource = (Resolve-Path -LiteralPath $Recording).Path
$rcrMetadataHash = (Get-FileHash -LiteralPath $rcrMetadataSource -Algorithm SHA256).Hash
$rcrMetadataWork = Join-Path $rcrMetadataRoot ('.downloads/mkv-metadata-' + [guid]::NewGuid().ToString('N'))
[IO.Directory]::CreateDirectory($rcrMetadataWork) | Out-Null
$rcrMetadataPacket = Join-Path $rcrMetadataWork 'first.obu'
$rcrMetadataFFmpeg = if ($env:RCR_FFMPEG) { $env:RCR_FFMPEG } else { 'ffmpeg' }
$rcrMetadataFFprobe = if ($env:RCR_FFPROBE) { $env:RCR_FFPROBE } else { 'ffprobe' }
Push-Location $rcrMetadataRoot
try {
    $rcrMetadataProbe = & $rcrMetadataFFprobe -v error -select_streams v:0 -show_entries stream=codec_name,width,height,r_frame_rate -of json $rcrMetadataSource
    if ($LASTEXITCODE -ne 0) { throw 'Source probe failed.' }
    $rcrMetadataVideo = ($rcrMetadataProbe | ConvertFrom-Json).streams[0]
    if ($rcrMetadataVideo.codec_name -ne 'av1') { throw 'Proof requires an AV1 source.' }
    $rcrMetadataRate = $rcrMetadataVideo.r_frame_rate.Split('/')
    $rcrMetadataFps = [int]$rcrMetadataRate[0] / [int]$rcrMetadataRate[1]
    if ($rcrMetadataFps -ne [math]::Truncate($rcrMetadataFps)) { throw 'Proof requires an integer frame rate.' }
    & $rcrMetadataFFmpeg -nostdin -v error -i $rcrMetadataSource -map 0:v:0 -frames:v 1 -c copy -f obu $rcrMetadataPacket
    if ($LASTEXITCODE -ne 0) { throw 'Keyframe extraction failed.' }
    & cargo run -p media-io --example metadata-proof -j1 -- $rcrMetadataPacket $rcrMetadataWork $rcrMetadataVideo.width $rcrMetadataVideo.height $rcrMetadataFps
    if ($LASTEXITCODE -ne 0) { throw 'Native metadata writer failed.' }
    foreach ($rcrMetadataKind in @('recording','replay')) {
        $rcrMetadataFile = Join-Path $rcrMetadataWork ($rcrMetadataKind + '.mkv')
        $rcrMetadataResult = & $rcrMetadataFFprobe -v error -show_streams -show_format -of json $rcrMetadataFile
        if ($LASTEXITCODE -ne 0) { throw 'Independent container probe failed.' }
        $rcrMetadataContainer = $rcrMetadataResult | ConvertFrom-Json
        $rcrMetadataTag = $rcrMetadataContainer.format.tags._RCR_METADATA | ConvertFrom-Json
        if ($rcrMetadataTag.schema_version -ne 1 -or $rcrMetadataTag.kind -ne $rcrMetadataKind -or $rcrMetadataTag.clip.video_frames -ne 1 -or $rcrMetadataTag.clip.packet_count -ne 3) { throw 'Embedded metadata mismatch.' }
        if (@($rcrMetadataContainer.streams | Where-Object codec_type -eq 'audio').Count -ne 2) { throw 'Missing separate audio tracks.' }
        if ($rcrMetadataKind -eq 'recording' -and $rcrMetadataTag.session.statistics.encoded -ne 1) { throw 'Session statistics missing.' }
        if (Test-Path -LiteralPath ([IO.Path]::ChangeExtension($rcrMetadataFile,'stats.json'))) { throw 'Unexpected sidecar.' }
        & $rcrMetadataFFmpeg -nostdin -v error -threads 1 -i $rcrMetadataFile -map 0 -f null NUL
        if ($LASTEXITCODE -ne 0) { throw 'Independent AV1/audio decode failed.' }
        $rcrMetadataCopy = Join-Path $rcrMetadataWork ($rcrMetadataKind + '-roundtrip.obu')
        & $rcrMetadataFFmpeg -nostdin -v error -i $rcrMetadataFile -map 0:v:0 -c copy -f obu $rcrMetadataCopy
        if ($LASTEXITCODE -ne 0 -or (Get-FileHash -LiteralPath $rcrMetadataCopy).Hash -ne (Get-FileHash -LiteralPath $rcrMetadataPacket).Hash) { throw 'Compressed AV1 packet changed.' }
        Write-Output "Verified embedded $rcrMetadataKind metadata, separate audio, unchanged AV1 packet and decode."
    }
} finally {
    Pop-Location
    if ((Get-FileHash -LiteralPath $rcrMetadataSource -Algorithm SHA256).Hash -ne $rcrMetadataHash) { throw 'Original source changed.' }
    Write-Output "Original source unchanged; proof files: $rcrMetadataWork"
}
