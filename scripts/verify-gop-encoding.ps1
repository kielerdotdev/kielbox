#requires -Version 7.0
param(
    [Parameter(Mandatory)][string]$Recording,
    [ValidateRange(0, 86400)][double]$StartSeconds = 0
)
$ErrorActionPreference = 'Stop'
$rcrRoot = Split-Path -Parent $PSScriptRoot
$rcrSource = (Resolve-Path -LiteralPath $(if ([IO.Path]::IsPathRooted($Recording)) { $Recording } else { Join-Path $rcrRoot $Recording })).Path
if (-not [IO.File]::Exists($rcrSource)) { throw 'Recording must be a regular owned media file.' }
function Resolve-MediaTool([string]$Override, [string]$Name) {
    if ($Override) {
        if (Test-Path -LiteralPath $Override -PathType Leaf) { return (Resolve-Path -LiteralPath $Override).Path }
        return (Get-Command -Name $Override -CommandType Application -ErrorAction Stop | Select-Object -First 1).Source
    }
    return (Get-Command -Name $Name -CommandType Application -ErrorAction Stop | Select-Object -First 1).Source
}
$rcrFfmpeg = Resolve-MediaTool $env:RCR_FFMPEG 'ffmpeg'
$rcrFfprobe = Resolve-MediaTool $env:RCR_FFPROBE 'ffprobe'
$rcrBefore = (Get-FileHash -LiteralPath $rcrSource -Algorithm SHA256).Hash
$rcrOwned = Join-Path $rcrRoot ('.downloads/gop-encoding-' + [guid]::NewGuid().ToString('N'))
function Invoke-OwnedMedia([string]$Tool, [string[]]$Arguments, [string]$Label, [string]$OutputFile) {
    $process = [Diagnostics.Process]::new()
    $process.StartInfo = [Diagnostics.ProcessStartInfo]::new()
    $process.StartInfo.FileName = $Tool
    $process.StartInfo.WorkingDirectory = $rcrOwned
    $process.StartInfo.UseShellExecute = $false
    $process.StartInfo.CreateNoWindow = $true
    $process.StartInfo.RedirectStandardOutput = $true
    $process.StartInfo.RedirectStandardError = $true
    foreach ($argument in $Arguments) { $process.StartInfo.ArgumentList.Add($argument) }
    $outTask = $null
    $errTask = $null
    $started = $false
    $watch = [Diagnostics.Stopwatch]::StartNew()
    try {
        $started = $process.Start()
        if (-not $started) { throw "Could not start $Label" }
        $outTask = [RcrGopBoundedPipe]::Drain($process.StandardOutput.BaseStream, 4MB)
        $errTask = [RcrGopBoundedPipe]::Drain($process.StandardError.BaseStream, 256KB)
        while (-not $process.WaitForExit(100)) {
            if ($watch.Elapsed.TotalSeconds -ge 59.5) { throw "$Label reached its 60-second deadline." }
            if ($OutputFile -and [IO.File]::Exists($OutputFile) -and (Get-Item -LiteralPath $OutputFile).Length -ge 128MB) {
                throw "$Label reached the 128 MiB output ceiling."
            }
        }
        if ($process.ExitCode -ne 0) { throw "$Label failed with exit code $($process.ExitCode); inspect its bounded stderr log." }
        if ($OutputFile -and -not [IO.File]::Exists($OutputFile)) { throw "$Label did not produce its owned output." }
        if ($OutputFile -and (Get-Item -LiteralPath $OutputFile).Length -ge 128MB) { throw "$Label reached the 128 MiB output ceiling." }
        $outBytes = $outTask.GetAwaiter().GetResult()
        if ($outBytes.Length -ge 4MB) { throw "$Label stdout exceeded its retained 4 MiB limit." }
        return [Text.Encoding]::UTF8.GetString($outBytes)
    } finally {
        if ($started -and -not $process.HasExited) {
            $process.Kill($true)
            if (-not $process.WaitForExit(10000)) { throw "Could not reap $Label after termination." }
        }
        if ($outTask) { [IO.File]::WriteAllBytes((Join-Path $rcrOwned "$Label.stdout.txt"), $outTask.GetAwaiter().GetResult()) }
        if ($errTask) { [IO.File]::WriteAllBytes((Join-Path $rcrOwned "$Label.stderr.txt"), $errTask.GetAwaiter().GetResult()) }
        $process.Dispose()
    }
}
$rcrSummary = [ordered]@{
    schema = 'rcr-gop-encoding-v1'; source = $rcrSource; source_sha256_before = $rcrBefore
    start_seconds = $StartSeconds; frame_limit = 1200; job_timeout_seconds = 60; output_ceiling_bytes = 128MB
    configuration = 'AV1 Main, P3/ll, VBR 25M/max37.5M, no B/lookahead/AQ, multipass disabled, BT709 limited'
    vbv_note = 'VBV is left at FFmpeg/preset defaults; capture also inherits its preset. Equivalence of those defaults is unverified. AV1 CBR padding is irrelevant to this VBR test.'
    variants = @(); success = $false; source_unchanged = $false
}
try {
    New-Item -ItemType Directory -Path $rcrOwned | Out-Null
    Set-Content -LiteralPath (Join-Path $rcrOwned '.rcr-gop-encoding') -Value 'rcr-owned-offline-gop-v1' -NoNewline
    # Drain both pipes continuously, retaining bounded prefixes, including on failure.
    if (-not ('RcrGopBoundedPipe' -as [type])) {
        Add-Type -TypeDefinition @'
using System;
using System.IO;
using System.Threading.Tasks;
public static class RcrGopBoundedPipe {
    public static async Task<byte[]> Drain(Stream stream, int limit) {
        using (var kept = new MemoryStream()) {
            var buffer = new byte[8192];
            int count;
            while ((count = await stream.ReadAsync(buffer, 0, buffer.Length).ConfigureAwait(false)) != 0) {
                int retain = (int)Math.Min(count, Math.Max(0, limit - kept.Length));
                if (retain != 0) kept.Write(buffer, 0, retain);
            }
            return kept.ToArray();
        }
    }
}
'@
    }
    $rcrSummary['ffmpeg_version'] = ((Invoke-OwnedMedia $rcrFfmpeg @('-version') 'ffmpeg-version' '') -split "`n")[0].Trim()
    $rcrSummary['ffprobe_version'] = ((Invoke-OwnedMedia $rcrFfprobe @('-version') 'ffprobe-version' '') -split "`n")[0].Trim()
    $sourceProbe = Invoke-OwnedMedia $rcrFfprobe @('-v','error','-select_streams','v:0','-show_entries','stream=codec_name,width,height,avg_frame_rate,pix_fmt','-of','json','-i',$rcrSource) 'source-probe' '' | ConvertFrom-Json
    if (@($sourceProbe.streams).Count -ne 1 -or $sourceProbe.streams[0].avg_frame_rate -notin @('60/1','120/2')) {
        throw 'This controlled comparison requires an existing 60 fps video source.'
    }
    $rcrSummary['source_stream'] = $sourceProbe.streams[0]
    foreach ($gop in @(120, 60, 30)) {
        $output = Join-Path $rcrOwned "gop-$gop.mkv"
        if (Test-Path -LiteralPath $output) { throw "Refusing to clobber $output" }
        $start = $StartSeconds.ToString('0.######', [Globalization.CultureInfo]::InvariantCulture)
        $arguments = @('-nostdin','-hide_banner','-loglevel','error','-n','-ss',$start,'-i',$rcrSource,
            '-map','0:v:0','-an','-sn','-dn','-map_metadata','-1','-frames:v','1200',
            '-vf','fps=60,format=nv12,setpts=PTS-STARTPTS','-c:v','av1_nvenc','-profile:v','0',
            '-preset','p3','-tune','ll','-rc','vbr','-b:v','25M','-maxrate','37.5M',
            '-bf','0','-rc-lookahead','0','-spatial-aq','0','-temporal-aq','0','-multipass','disabled',
            '-color_primaries','bt709','-color_trc','bt709','-colorspace','bt709','-color_range','tv',
            '-g',"$gop",'-fps_mode','passthrough','-fs','125829120','-f','matroska',$output)
        $timer = [Diagnostics.Stopwatch]::StartNew()
        $null = Invoke-OwnedMedia $rcrFfmpeg $arguments "gop-$gop-encode" $output
        $seconds = $timer.Elapsed.TotalSeconds
        $probe = Invoke-OwnedMedia $rcrFfprobe @('-v','error','-select_streams','v:0','-show_frames','-show_streams',
            '-show_entries','stream=codec_name,profile,pix_fmt,width,height,avg_frame_rate,color_range,color_space,color_transfer,color_primaries:frame=key_frame,pts_time,best_effort_timestamp_time,width,height,pict_type',
            '-of','json','-i',$output) "gop-$gop-probe" '' | ConvertFrom-Json
        $frames = @($probe.frames)
        if ($probe.streams[0].codec_name -ne 'av1' -or $probe.streams[0].profile -ne 'Main' -or $probe.streams[0].pix_fmt -ne 'yuv420p') { throw "GOP $gop did not produce eight-bit AV1 Main." }
        if ($frames.Count -ne 1200) { throw "GOP $gop has $($frames.Count) frames; expected 1200. Choose an earlier source section." }
        $keys = @()
        $pts = @()
        for ($index = 0; $index -lt $frames.Count; $index++) {
            $frame = $frames[$index]
            if ($frame.width -ne $sourceProbe.streams[0].width -or $frame.height -ne $sourceProbe.streams[0].height) { throw "GOP $gop changed dimensions." }
            if ($null -eq $frame.pts_time) { throw "GOP $gop frame $index has no PTS." }
            $stamp = [double]::Parse($frame.pts_time, [Globalization.CultureInfo]::InvariantCulture)
            if (-not [double]::IsFinite($stamp) -or [Math]::Abs($stamp - $index / 60.0) -gt 0.0011) { throw "GOP $gop has an unexpected timestamp at frame $index." }
            $pts += $stamp
            if ($frame.key_frame -eq 1) { $keys += @{ frame = $index; pts_seconds = $stamp } }
        }
        $keyIndexes = @($keys | ForEach-Object { $_.frame })
        if ($keyIndexes.Count -eq 0 -or $keyIndexes[0] -ne 0) { throw "GOP $gop lacks its initial keyframe." }
        $maxGap = 0
        for ($index = 1; $index -lt $keyIndexes.Count; $index++) { $maxGap = [Math]::Max($maxGap, $keyIndexes[$index] - $keyIndexes[$index - 1]) }
        $maxGap = [Math]::Max($maxGap, $frames.Count - $keyIndexes[-1])
        if ($maxGap -gt $gop) { throw "GOP $gop actual keyframe gap is $maxGap frames." }
        $bytes = (Get-Item -LiteralPath $output).Length
        $rcrSummary.variants += [ordered]@{ gop_frames = $gop; output = $output; bytes = $bytes
            encode_seconds = $seconds; encode_fps = 1200 / $seconds; frame_count = $frames.Count
            stream = $probe.streams[0]; first_pts_seconds = $pts[0]; last_pts_seconds = $pts[-1]
            duration_seconds = 20; container_bitrate_bps = $bytes * 8 / 20.0
            keyframes = $keys; maximum_keyframe_gap_frames = $maxGap; arguments = $arguments }
    }
    $rcrSummary.success = $true
} catch {
    $rcrSummary['error'] = $_.Exception.Message
    throw
} finally {
    try {
        $after = (Get-FileHash -LiteralPath $rcrSource -Algorithm SHA256).Hash
        $rcrSummary['source_sha256_after'] = $after
        $rcrSummary.source_unchanged = $after -eq $rcrBefore
        if (-not $rcrSummary.source_unchanged) { $rcrSummary.success = $false }
    } finally {
        if (-not $rcrSummary.source_unchanged) { $rcrSummary.success = $false }
        $rcrSummary | ConvertTo-Json -Depth 12 | Set-Content -LiteralPath (Join-Path $rcrOwned 'summary.json') -Encoding utf8
        Write-Output "Offline GOP artifacts: $rcrOwned"
    }
    if (-not $rcrSummary.source_unchanged) { throw 'Original source SHA-256 changed or could not be checked.' }
}
