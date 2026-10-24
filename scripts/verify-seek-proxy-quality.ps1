#requires -Version 7.0
param(
    [Parameter(Mandatory)][string]$Recording,
    [ValidateRange(0, 86400)][double]$StartSeconds = 30,
    [switch]$IncludeGop12,
    [switch]$BalancedOnly,
    [switch]$SliceOnly,
    [switch]$IntraBalancedOnly
)
$ErrorActionPreference = 'Stop'
$rcrRoot = Split-Path -Parent $PSScriptRoot
$rcrSource = (Resolve-Path -LiteralPath $(if ([IO.Path]::IsPathRooted($Recording)) { $Recording } else { Join-Path $rcrRoot $Recording })).Path
$rcrBefore = (Get-FileHash -LiteralPath $rcrSource -Algorithm SHA256).Hash
$rcrOwned = Join-Path $rcrRoot ('.downloads/seek-proxy-quality-' + [guid]::NewGuid().ToString('N'))
function Resolve-ProxyTool([string]$Override, [string]$Name) {
    $tool = if ($Override) { $Override } else { $Name }
    if (Test-Path -LiteralPath $tool -PathType Leaf) { return (Resolve-Path -LiteralPath $tool).Path }
    return (Get-Command -Name $tool -CommandType Application -ErrorAction Stop | Select-Object -First 1).Source
}
function Invoke-ProxyMedia([string]$Tool, [string[]]$Arguments, [string]$Label) {
    $process = [Diagnostics.Process]::new(); $process.StartInfo = [Diagnostics.ProcessStartInfo]::new()
    $process.StartInfo.FileName = $Tool; $process.StartInfo.WorkingDirectory = $rcrOwned
    $process.StartInfo.UseShellExecute = $false; $process.StartInfo.CreateNoWindow = $true
    $process.StartInfo.RedirectStandardOutput = $true; $process.StartInfo.RedirectStandardError = $true
    foreach ($argument in $Arguments) { $process.StartInfo.ArgumentList.Add($argument) }
    $outTask = $null; $errTask = $null; $started = $false; $timer = [Diagnostics.Stopwatch]::StartNew()
    try {
        $started = $process.Start(); if (-not $started) { throw "Could not start $Label." }
        $outTask = [RcrSeekProxyPipe]::Drain($process.StandardOutput.BaseStream, 4MB)
        $errTask = [RcrSeekProxyPipe]::Drain($process.StandardError.BaseStream, 1MB)
        while (-not $process.WaitForExit(100)) {
            if ($timer.Elapsed.TotalSeconds -ge 59.5) { throw "$Label reached its 60-second deadline." }
            Assert-ProxyBounds
        }
        if ($process.ExitCode -ne 0) { throw "$Label failed with exit code $($process.ExitCode); inspect its bounded stderr log." }
        Assert-ProxyBounds
        $outBytes = $outTask.GetAwaiter().GetResult(); $errBytes = $errTask.GetAwaiter().GetResult()
        if ($outBytes.Length -ge 4MB -or $errBytes.Length -ge 1MB) { throw "$Label exceeded retained log limits." }
        return [ordered]@{ stdout = [Text.Encoding]::UTF8.GetString($outBytes); stderr = [Text.Encoding]::UTF8.GetString($errBytes); elapsed_seconds = $timer.Elapsed.TotalSeconds }
    } finally {
        if ($started -and -not $process.HasExited) { $process.Kill($true); if (-not $process.WaitForExit(10000)) { throw "Could not reap $Label." } }
        if ($outTask) { [IO.File]::WriteAllBytes((Join-Path $rcrOwned "$Label.stdout.txt"), $outTask.GetAwaiter().GetResult()) }
        if ($errTask) { [IO.File]::WriteAllBytes((Join-Path $rcrOwned "$Label.stderr.txt"), $errTask.GetAwaiter().GetResult()) }
        $process.Dispose()
    }
}
function Assert-ProxyBounds {
    $outputs = @(Get-ChildItem -LiteralPath $rcrOwned -File)
    if (@($outputs | Where-Object { $_.Extension -eq '.mkv' -and $_.Length -ge 128MB }).Count -or
        @($outputs | Where-Object { $_.Extension -eq '.png' -and $_.Length -ge 16MB }).Count -or
        @($outputs | Where-Object { $_.Name -match '-(psnr|ssim)\.txt$' -and $_.Length -ge 1MB }).Count -or
        @($outputs | Where-Object Extension -eq '.png').Count -gt 16 -or
        (($outputs | Where-Object Extension -eq '.png' | Measure-Object Length -Sum).Sum -gt 128MB)) { throw 'Owned proxy benchmark output count/size limit exceeded.' }
}
function Assert-ProxySamples([string]$Path) {
    $samples = @(Get-Content -LiteralPath $Path | Where-Object { $_ -match '^n:' })
    if ($samples.Count -ne 360) { throw "$Path lacks 360 paired samples." }
    for ($index = 0; $index -lt 360; $index++) { if ($samples[$index] -notmatch '^n:(\d+)\s' -or [int]$Matches[1] -ne $index + 1) { throw "$Path has missing/repeated sample indices." } }
}
$rcrResult = [ordered]@{ schema = 'rcr-seek-proxy-quality-v1'; source = $rcrSource; source_sha256_before = $rcrBefore
    start_seconds = $StartSeconds; duration_seconds = 6; frame_count = 360; diagnostic_frame_zero_based = 84
    diagnostic_source_seconds = $StartSeconds + 1.4; variants = @(); success = $false; source_unchanged = $false
    metric_note = 'Compression loss against a software-bicubic reference at each proxy resolution. CUDA/software scaling kernels differ; metrics across resolutions are not an equal-detail comparison.'
    size_note = 'Full-source estimates extrapolate this six-second content only; the production file cap remains authoritative.' }
try {
    New-Item -ItemType Directory -Path $rcrOwned | Out-Null
    Set-Content -LiteralPath (Join-Path $rcrOwned '.rcr-seek-proxy-quality') -Value 'rcr-owned-seek-proxy-quality-v1' -NoNewline
    $rcrFfmpeg = Resolve-ProxyTool $env:RCR_FFMPEG 'ffmpeg'; $rcrFfprobe = Resolve-ProxyTool $env:RCR_FFPROBE 'ffprobe'
    if (-not ('RcrSeekProxyPipe' -as [type])) {
        Add-Type -TypeDefinition @'
using System;
using System.IO;
using System.Threading.Tasks;
public static class RcrSeekProxyPipe {
    public static async Task<byte[]> Drain(Stream stream, int limit) {
        using (var kept = new MemoryStream()) {
            var buffer = new byte[8192]; int count;
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
    $version = Invoke-ProxyMedia $rcrFfmpeg @('-version') 'ffmpeg-version'
    $rcrResult['ffmpeg_version'] = ($version.stdout -split "`n")[0].Trim()
    $version = Invoke-ProxyMedia $rcrFfprobe @('-version') 'ffprobe-version'
    $rcrResult['ffprobe_version'] = ($version.stdout -split "`n")[0].Trim()
    $job = Invoke-ProxyMedia $rcrFfprobe @('-v','error','-select_streams','v:0','-show_entries','format=duration:stream=width,height,avg_frame_rate,pix_fmt','-of','json','-i',$rcrSource) 'source-probe'
    $source = $job.stdout | ConvertFrom-Json
    if (@($source.streams).Count -ne 1 -or $source.streams[0].avg_frame_rate -ne '60/1' -or $source.streams[0].pix_fmt -ne 'yuv420p') { throw 'Use the known eight-bit SDR 60 fps owned recording.' }
    $fullDuration = [double]::Parse($source.format.duration, [Globalization.CultureInfo]::InvariantCulture)
    if ($fullDuration -lt $StartSeconds + 6) { throw 'Source section is shorter than six seconds.' }
    $rcrResult['source_duration_seconds'] = $fullDuration
    $recipes = @(
        @{ label = '720-cbr8-all-i'; width = 1280; height = 720; gop = 1; preset = 'p1'; tune = 'ull'; rate = @('-rc','cbr','-b:v','8M','-maxrate','8M','-bufsize','2M') },
        @{ label = '720-qp18-all-i'; width = 1280; height = 720; gop = 1; preset = 'p3'; tune = 'ull'; rate = @('-rc','constqp','-qp','18','-b:v','0') },
        @{ label = '1080-qp18-gop6'; width = 1920; height = 1080; gop = 6; preset = 'p3'; tune = 'ull'; rate = @('-rc','constqp','-qp','18','-b:v','0') }
    )
    if ($IncludeGop12) { $recipes += @{ label = '1080-qp18-gop12'; width = 1920; height = 1080; gop = 12; preset = 'p3'; tune = 'ull'; rate = @('-rc','constqp','-qp','18','-b:v','0') } }
    if ($BalancedOnly) {
        $recipes = @(
            @{ label = '1080-qp24-gop6'; width = 1920; height = 1080; gop = 6; preset = 'p3'; tune = 'ull'; rate = @('-rc','constqp','-qp','24','-b:v','0') },
            @{ label = '1080-qp26-gop6'; width = 1920; height = 1080; gop = 6; preset = 'p3'; tune = 'ull'; rate = @('-rc','constqp','-qp','26','-b:v','0') },
            @{ label = '1080-qp24-gop12'; width = 1920; height = 1080; gop = 12; preset = 'p3'; tune = 'ull'; rate = @('-rc','constqp','-qp','24','-b:v','0') }
        )
    }
    if ($SliceOnly) {
        $recipes = @(
            @{ label = '1080-qp26-gop6-slices2'; width = 1920; height = 1080; gop = 6; slices = 2; preset = 'p3'; tune = 'ull'; rate = @('-rc','constqp','-qp','26','-b:v','0') },
            @{ label = '1080-qp26-all-i-slices2'; width = 1920; height = 1080; gop = 1; slices = 2; preset = 'p3'; tune = 'ull'; rate = @('-rc','constqp','-qp','26','-b:v','0') }
        )
    }
    if ($IntraBalancedOnly) {
        $recipes = @(
            @{ label = '1080-intra-qp24-slices2'; width = 1920; height = 1080; gop = 1; slices = 2; expected_qp = 24; preset = 'p3'; tune = 'ull'; rate = @('-rc','constqp','-qp','24','-i_qfactor','1','-i_qoffset','0','-b:v','0') },
            @{ label = '1080-intra-qp26-slices2'; width = 1920; height = 1080; gop = 1; slices = 2; expected_qp = 26; preset = 'p3'; tune = 'ull'; rate = @('-rc','constqp','-qp','26','-i_qfactor','1','-i_qoffset','0','-b:v','0') }
        )
    }
    $start = $StartSeconds.ToString('0.######', [Globalization.CultureInfo]::InvariantCulture)
    foreach ($recipe in $recipes) {
        $label = $recipe.label; $output = Join-Path $rcrOwned "$label.mkv"
        if (Test-Path -LiteralPath $output) { throw 'Refusing to clobber proxy output.' }
        $maxWidth = [Math]::Min([int]$source.streams[0].width, [int]$recipe.width)
        $maxHeight = [Math]::Min([int]$source.streams[0].height, [int]$recipe.height)
        $scale = "scale_cuda=${maxWidth}:${maxHeight}:force_original_aspect_ratio=decrease:force_divisible_by=2:interp_algo=bicubic,setpts=PTS-STARTPTS"
        $sliceArguments = @(if ($recipe.ContainsKey('slices')) { '-slices'; "$($recipe.slices)" })
        $arguments = @('-nostdin','-hide_banner','-loglevel','error','-n','-hwaccel','cuda','-hwaccel_output_format','cuda','-ss',$start,'-i',$rcrSource,
            '-map','0:v:0','-an','-sn','-dn','-vf',$scale,'-frames:v','360','-c:v','h264_nvenc','-preset',$recipe.preset,'-tune',$recipe.tune,
            '-g',"$($recipe.gop)",'-bf','0','-rc-lookahead','0','-multipass','disabled','-spatial-aq','0','-temporal-aq','0') + $recipe.rate + $sliceArguments +
            @('-fps_mode','passthrough','-fs','125829120','-f','matroska',$output)
        $encode = Invoke-ProxyMedia $rcrFfmpeg $arguments "$label-encode"
        $job = Invoke-ProxyMedia $rcrFfprobe @('-v','error','-select_streams','v:0','-show_frames','-show_streams','-show_entries',
            'stream=width,height,avg_frame_rate,pix_fmt:frame=key_frame,pts_time,width,height','-of','json','-i',$output) "$label-probe"
        $probe = $job.stdout | ConvertFrom-Json; $frames = @($probe.frames)
        if ($frames.Count -ne 360 -or $probe.streams[0].avg_frame_rate -ne '60/1' -or $probe.streams[0].width -gt $source.streams[0].width -or $probe.streams[0].height -gt $source.streams[0].height) { throw "$label changed frame count/cadence or upscaled the source." }
        $keys = @()
        for ($index = 0; $index -lt 360; $index++) {
            $pts = [double]::Parse($frames[$index].pts_time, [Globalization.CultureInfo]::InvariantCulture)
            if (-not [double]::IsFinite($pts) -or [Math]::Abs($pts - $index / 60.0) -gt 0.0011) { throw "$label frame $index lost timestamp alignment." }
            if ($frames[$index].key_frame -eq 1) { $keys += $index }
        }
        $expectedKeys = @(0..359 | Where-Object { $_ % $recipe.gop -eq 0 })
        if (($keys -join ',') -ne ($expectedKeys -join ',')) { throw "$label keyframe positions differ from the requested GOP." }
        $sliceValidation = $null
        if ($recipe.ContainsKey('slices')) {
            # Sample twelve access units, without decoding or creating
            # another media file. The shared runner bounds both time and logs.
            $headers = Invoke-ProxyMedia $rcrFfmpeg @('-nostdin','-hide_banner','-loglevel','info','-nostats','-n','-i',$output,
                '-map','0:v:0','-an','-sn','-dn','-c:v','copy','-frames:v','12','-bsf:v','trace_headers','-f','null','-') "$label-slice-headers"
            $packets = @(); $packet = $null; $picInitQpMinus26 = $null
            foreach ($line in ($headers.stderr -split "`n")) {
                if ($line -match '\bpic_init_qp_minus26\s+[01]+\s*=\s*(-?\d+)\s*$') { $picInitQpMinus26 = [int]$Matches[1] }
                if ($line -match '\bPacket: \d+ bytes') {
                    if ($null -ne $packet) { $packets += $packet }
                    $packet = @{ vcl_count = 0; first_mb = @(); key_frame = $line -match ', key frame'; slice_qp_delta = @(); initial_slice_luma_qp = @() }
                } elseif ($null -ne $packet) {
                    if ($line -match '\bnal_unit_type\s+[01]+\s*=\s*(\d+)\s*$' -and [int]$Matches[1] -in @(1, 5)) { $packet.vcl_count++ }
                    if ($line -match '\bfirst_mb_in_slice\s+[01]+\s*=\s*(\d+)\s*$') { $packet.first_mb += [int]$Matches[1] }
                    if ($line -match '\bslice_qp_delta\s+[01]+\s*=\s*(-?\d+)\s*$') {
                        if ($null -eq $picInitQpMinus26) { throw 'Missing PPS initial QP while inspecting slice quantization.' }
                        $delta = [int]$Matches[1]
                        $packet.slice_qp_delta += $delta
                        $packet.initial_slice_luma_qp += 26 + $picInitQpMinus26 + $delta
                    }
                }
            }
            if ($null -ne $packet) { $packets += $packet }
            if ($packets.Count -lt 12) { throw 'Slice header validation lacks twelve complete access units.' }
            $checked = @($packets | Select-Object -First 12)
            for ($index = 0; $index -lt 12; $index++) {
                $packet = $checked[$index]
                if ($packet.vcl_count -ne 2 -or $packet.first_mb.Count -ne 2 -or $packet.first_mb[0] -ne 0 -or $packet.first_mb[1] -le 0) { throw 'Two distinct VCL slices were not observed in each sampled frame.' }
                if ($packet.key_frame -ne ($index % $recipe.gop -eq 0)) { throw 'Slice header packet keyframe flags differ from the requested GOP.' }
                if ($packet.initial_slice_luma_qp.Count -ne 2) { throw 'Missing sampled initial QP for both slices.' }
                if ($recipe.ContainsKey('expected_qp') -and @($packet.initial_slice_luma_qp | Where-Object { $_ -ne $recipe.expected_qp }).Count) { throw "Sampled slice QP differs from requested actual QP $($recipe.expected_qp)." }
            }
            $sliceValidation = @{ packets_checked = 12; vcl_slices_per_packet = 2; keyframes_checked = @($checked | ForEach-Object { $_.key_frame })
                first_macroblock_indices = @($checked | ForEach-Object { ,$_.first_mb }); slice_qp_delta = @($checked | ForEach-Object { ,$_.slice_qp_delta })
                initial_slice_luma_qp = @($checked | ForEach-Object { ,$_.initial_slice_luma_qp }) }
        }
        $width = [int]$probe.streams[0].width; $height = [int]$probe.streams[0].height
        $psnrName = "$label-psnr.txt"; $ssimName = "$label-ssim.txt"
        $filter = "[0:v:0]fps=60,scale=${width}:${height}:flags=bicubic,format=yuv420p,trim=end_frame=360,settb=expr=1/60,setpts=N,split=4[rp][rs][rf][rc];" +
            "[1:v:0]fps=60,format=yuv420p,trim=end_frame=360,settb=expr=1/60,setpts=N,split=4[dp][ds][df][dc];" +
            "[dp][rp]psnr=stats_file=${psnrName}:eof_action=endall:shortest=1:repeatlast=0[p];" +
            "[ds][rs]ssim=stats_file=${ssimName}:eof_action=endall:shortest=1:repeatlast=0[s];" +
            "[rf]select=eq(n\,84),format=rgb24[ref];[df]select=eq(n\,84),format=rgb24[proxy];" +
            "[rc]select=eq(n\,84),format=rgb24,crop=iw/2:ih/2:0:0[refcrop];[dc]select=eq(n\,84),format=rgb24,crop=iw/2:ih/2:0:0[proxycrop]"
        $qualityArguments = @('-nostdin','-hide_banner','-loglevel','info','-nostats','-n','-hwaccel','none','-threads','4','-ss',$start,'-i',$rcrSource,
            '-hwaccel','none','-threads','4','-i',$output,'-filter_complex_threads','2','-filter_complex',$filter,
            '-map','[p]','-map','[s]','-frames:v:0','360','-frames:v:1','360','-fps_mode','passthrough','-f','null','-')
        $images = @()
        foreach ($image in @('ref','proxy','refcrop','proxycrop')) {
            $path = Join-Path $rcrOwned "$label-$image.png"; if (Test-Path -LiteralPath $path) { throw 'Refusing to clobber diagnostic PNG.' }
            $qualityArguments += @('-map',"[$image]",'-frames:v','1','-fps_mode','vfr','-c:v','png','-threads:v','1','-compression_level','3','-update','1',$path)
            $images += @{ kind = $image; path = $path }
        }
        $quality = Invoke-ProxyMedia $rcrFfmpeg $qualityArguments "$label-quality"
        Assert-ProxySamples (Join-Path $rcrOwned $psnrName); Assert-ProxySamples (Join-Path $rcrOwned $ssimName)
        foreach ($image in $images) { if (-not [IO.File]::Exists($image.path)) { throw 'Missing diagnostic PNG.' } }
        $psnrLine = @($quality.stderr -split "`n" | Where-Object { $_ -match 'PSNR y:' }); $ssimLine = @($quality.stderr -split "`n" | Where-Object { $_ -match 'SSIM Y:' })
        if ($psnrLine.Count -ne 1 -or $psnrLine[0] -notmatch 'average:([^\s]+)') { throw 'Missing PSNR summary.' }
        $psnr = if ($Matches[1] -eq 'inf') { 'inf' } else { [double]::Parse($Matches[1], [Globalization.CultureInfo]::InvariantCulture) }
        if ($ssimLine.Count -ne 1 -or $ssimLine[0] -notmatch 'All:([^\s]+)') { throw 'Missing SSIM summary.' }
        $ssim = [double]::Parse($Matches[1], [Globalization.CultureInfo]::InvariantCulture)
        $bytes = (Get-Item -LiteralPath $output).Length; $estimated = $bytes / 6.0 * $fullDuration
        $rcrResult.variants += [ordered]@{ label = $label; output = $output; width = $width; height = $height; gop_frames = $recipe.gop; keyframes = $keys
            bytes = $bytes; encode_seconds = $encode.elapsed_seconds; frame_count = 360; estimated_full_source_bytes = $estimated; estimated_fits_256_mib = $estimated -lt 256MB
            estimated_fits_512_mib = $estimated -lt 512MB; estimated_fits_576_mib = $estimated -lt 576MB; slice_validation = $sliceValidation
            psnr_average_db = $psnr; ssim_average_all = $ssim; images = $images; encode_arguments = $arguments; quality_arguments = $qualityArguments }
    }
    $rcrResult.success = $true
} catch { $rcrResult['error'] = $_.Exception.Message; throw }
finally {
    try { $rcrResult['source_sha256_after'] = (Get-FileHash -LiteralPath $rcrSource -Algorithm SHA256).Hash; $rcrResult.source_unchanged = $rcrResult.source_sha256_after -eq $rcrBefore }
    finally {
        if (-not $rcrResult.source_unchanged) { $rcrResult.success = $false }
        $rcrResult | ConvertTo-Json -Depth 12 | Set-Content -LiteralPath (Join-Path $rcrOwned 'summary.json') -Encoding utf8
        Write-Output "Seek proxy quality artifacts: $rcrOwned"
    }
    if (-not $rcrResult.source_unchanged) { throw 'Original source SHA-256 changed or could not be checked.' }
}
