#requires -Version 7.0
param([Parameter(Mandatory)][string]$Summary)
$ErrorActionPreference = 'Stop'
$rcrRoot = Split-Path -Parent $PSScriptRoot
$rcrInputSummary = (Resolve-Path -LiteralPath $(if ([IO.Path]::IsPathRooted($Summary)) { $Summary } else { Join-Path $rcrRoot $Summary })).Path
$rcrEncoding = Get-Content -LiteralPath $rcrInputSummary -Raw | ConvertFrom-Json
$rcrEncodingDirectory = Split-Path -Parent $rcrInputSummary
if ($rcrEncoding.schema -ne 'rcr-gop-encoding-v1' -or -not $rcrEncoding.success -or -not $rcrEncoding.source_unchanged -or
    $rcrEncoding.frame_limit -ne 1200 -or @($rcrEncoding.variants).Count -ne 3) { throw 'Expected a successful 1200-frame GOP encoding summary.' }
if ((Get-Content -LiteralPath (Join-Path $rcrEncodingDirectory '.rcr-gop-encoding') -Raw).Trim() -ne 'rcr-owned-offline-gop-v1') { throw 'Encoding directory lacks its ownership marker.' }
$rcrStart = [double]$rcrEncoding.start_seconds
if (-not [double]::IsFinite($rcrStart) -or $rcrStart -lt 0 -or $rcrStart -gt 86400) { throw 'Invalid source section start.' }
$rcrSource = (Resolve-Path -LiteralPath $rcrEncoding.source).Path
$rcrFiles = @([ordered]@{ kind = 'source'; path = $rcrSource }, [ordered]@{ kind = 'encoding-summary'; path = $rcrInputSummary })
$rcrVariants = @($rcrEncoding.variants | Sort-Object gop_frames -Descending)
if (($rcrVariants.gop_frames -join ',') -ne '120,60,30') { throw 'Expected exactly GOP 120, 60, and 30.' }
foreach ($variant in $rcrVariants) {
    $path = (Resolve-Path -LiteralPath $variant.output).Path
    if ((Split-Path -Parent $path) -ne $rcrEncodingDirectory -or (Split-Path -Leaf $path) -ne "gop-$($variant.gop_frames).mkv") { throw 'Variant must be its encoding directory owned output.' }
    if ($variant.frame_count -ne 1200 -or $variant.duration_seconds -ne 20 -or $variant.stream.avg_frame_rate -ne '60/1' -or
        $variant.stream.width -ne $rcrEncoding.source_stream.width -or $variant.stream.height -ne $rcrEncoding.source_stream.height -or
        $variant.stream.pix_fmt -ne 'yuv420p' -or [Math]::Abs([double]$variant.first_pts_seconds) -gt 0.0011 -or
        [Math]::Abs([double]$variant.last_pts_seconds - 1199 / 60.0) -gt 0.0011) { throw 'Encoding summary does not establish the expected frame alignment.' }
    $encodedArguments = @($variant.arguments)
    $seekIndex = [Array]::IndexOf($encodedArguments, '-ss')
    $filterIndex = [Array]::IndexOf($encodedArguments, '-vf')
    if ($seekIndex -lt 0 -or $filterIndex -lt 0 -or
        [double]::Parse($encodedArguments[$seekIndex + 1], [Globalization.CultureInfo]::InvariantCulture) -ne $rcrStart -or
        $encodedArguments[$filterIndex + 1] -ne 'fps=60,format=nv12,setpts=PTS-STARTPTS') { throw 'Unrecognized source extraction/alignment in encoding summary.' }
    if ((Get-Item -LiteralPath $path).Length -ne $variant.bytes) { throw "Derived file size changed: $path" }
    $rcrFiles += [ordered]@{ kind = "gop-$($variant.gop_frames)"; path = $path }
}
foreach ($file in $rcrFiles) { $file['sha256_before'] = (Get-FileHash -LiteralPath $file.path -Algorithm SHA256).Hash }
if ($rcrFiles[0].sha256_before -ne $rcrEncoding.source_sha256_before -or $rcrFiles[0].sha256_before -ne $rcrEncoding.source_sha256_after) { throw 'Source hash differs from encoding fixture.' }
$rcrOwned = Join-Path $rcrRoot ('.downloads/gop-quality-' + [guid]::NewGuid().ToString('N'))
function Resolve-QualityTool([string]$Override, [string]$Name) {
    if ($Override) {
        if (Test-Path -LiteralPath $Override -PathType Leaf) { return (Resolve-Path -LiteralPath $Override).Path }
        return (Get-Command -Name $Override -CommandType Application -ErrorAction Stop | Select-Object -First 1).Source
    }
    return (Get-Command -Name $Name -CommandType Application -ErrorAction Stop | Select-Object -First 1).Source
}
function Invoke-QualityMedia([string[]]$Arguments, [string]$Label, [string[]]$StatsFiles = @()) {
    $process = [Diagnostics.Process]::new()
    $process.StartInfo = [Diagnostics.ProcessStartInfo]::new()
    $process.StartInfo.FileName = $rcrFfmpeg
    $process.StartInfo.WorkingDirectory = $rcrOwned
    $process.StartInfo.UseShellExecute = $false
    $process.StartInfo.CreateNoWindow = $true
    $process.StartInfo.RedirectStandardOutput = $true
    $process.StartInfo.RedirectStandardError = $true
    foreach ($argument in $Arguments) { $process.StartInfo.ArgumentList.Add($argument) }
    $outTask = $null; $errTask = $null; $started = $false
    $watch = [Diagnostics.Stopwatch]::StartNew()
    try {
        $started = $process.Start()
        if (-not $started) { throw "Could not start $Label." }
        $outTask = [RcrGopQualityPipe]::Drain($process.StandardOutput.BaseStream, 1MB)
        $errTask = [RcrGopQualityPipe]::Drain($process.StandardError.BaseStream, 1MB)
        while (-not $process.WaitForExit(100)) {
            if ($watch.Elapsed.TotalSeconds -ge 59.5) { throw "$Label reached its 60-second deadline." }
            foreach ($stats in $StatsFiles) { if ([IO.File]::Exists($stats) -and (Get-Item -LiteralPath $stats).Length -ge 1MB) { throw "$Label stats exceeded 1 MiB." } }
        }
        if ($process.ExitCode -ne 0) { throw "$Label failed with exit code $($process.ExitCode); inspect its bounded stderr log." }
        foreach ($stats in $StatsFiles) { if (-not [IO.File]::Exists($stats) -or (Get-Item -LiteralPath $stats).Length -ge 1MB) { throw "$Label has missing or oversized stats." } }
        $outBytes = $outTask.GetAwaiter().GetResult()
        $errBytes = $errTask.GetAwaiter().GetResult()
        if ($outBytes.Length -ge 1MB -or $errBytes.Length -ge 1MB) { throw "$Label exceeded a retained log limit." }
        return [ordered]@{ stdout = [Text.Encoding]::UTF8.GetString($outBytes); stderr = [Text.Encoding]::UTF8.GetString($errBytes); elapsed_seconds = $watch.Elapsed.TotalSeconds }
    } finally {
        if ($started -and -not $process.HasExited) { $process.Kill($true); if (-not $process.WaitForExit(10000)) { throw "Could not reap $Label." } }
        if ($outTask) { [IO.File]::WriteAllBytes((Join-Path $rcrOwned "$Label.stdout.txt"), $outTask.GetAwaiter().GetResult()) }
        if ($errTask) { [IO.File]::WriteAllBytes((Join-Path $rcrOwned "$Label.stderr.txt"), $errTask.GetAwaiter().GetResult()) }
        $process.Dispose()
    }
}
function Read-QualitySamples([string]$Path, [string]$Field) {
    $lines = @(Get-Content -LiteralPath $Path | Where-Object { $_ -match '^n:' })
    if ($lines.Count -ne 1200) { throw "$Path has $($lines.Count) paired samples; expected 1200." }
    $values = @()
    for ($index = 0; $index -lt 1200; $index++) {
        if ($lines[$index] -notmatch '^n:(\d+)\s' -or [int]$Matches[1] -ne $index + 1) { throw "$Path has missing/repeated sample indices." }
        if ($lines[$index] -notmatch ('(?:^|\s)' + [regex]::Escape($Field) + ':([^\s]+)')) { throw "$Path lacks $Field." }
        $raw = $Matches[1]
        $number = if ($raw -eq 'inf') { [double]::PositiveInfinity } else { [double]::Parse($raw, [Globalization.CultureInfo]::InvariantCulture) }
        if ([double]::IsNaN($number) -or $number -lt 0) { throw "$Path has invalid $Field." }
        $values += $number
    }
    return ,$values
}
$rcrResult = [ordered]@{ schema = 'rcr-gop-quality-v1'; encoding_summary = $rcrInputSummary; source_start_seconds = $rcrStart
    frame_count = 1200; fps = 60; job_timeout_seconds = 60; files = $rcrFiles; variants = @(); success = $false; files_unchanged = $false
    interpretation = 'Relative additional compression against the decoded owned source, not pristine capture fidelity. Metrics do not replace visual text and motion review.' }
try {
    New-Item -ItemType Directory -Path $rcrOwned | Out-Null
    Set-Content -LiteralPath (Join-Path $rcrOwned '.rcr-gop-quality') -Value 'rcr-owned-offline-gop-quality-v1' -NoNewline
    $rcrFfmpeg = Resolve-QualityTool $env:RCR_FFMPEG 'ffmpeg'
    if (-not ('RcrGopQualityPipe' -as [type])) {
        Add-Type -TypeDefinition @'
using System;
using System.IO;
using System.Threading.Tasks;
public static class RcrGopQualityPipe {
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
    $version = Invoke-QualityMedia @('-version') 'ffmpeg-version'
    $rcrResult['ffmpeg_version'] = ($version.stdout -split "`n")[0].Trim()
    $start = $rcrStart.ToString('0.######', [Globalization.CultureInfo]::InvariantCulture)
    foreach ($variant in $rcrVariants) {
        $gop = [int]$variant.gop_frames
        $psnrName = "gop-$gop-psnr.txt"; $ssimName = "gop-$gop-ssim.txt"
        $psnrPath = Join-Path $rcrOwned $psnrName; $ssimPath = Join-Path $rcrOwned $ssimName
        if ((Test-Path -LiteralPath $psnrPath) -or (Test-Path -LiteralPath $ssimPath)) { throw 'Refusing to clobber quality statistics.' }
        # The same fps normalization and input seek as the encoding fixture select
        # reference frames. Exact integer timestamps pair frames by ordinal, while
        # repeatlast=0/shortest=1 prevent silently filling a short input.
        $filter = "[0:v:0]fps=60,format=yuv420p,trim=end_frame=1200,settb=expr=1/60,setpts=N,split=2[rp][rs];" +
            "[1:v:0]fps=60,format=yuv420p,trim=end_frame=1200,settb=expr=1/60,setpts=N,split=2[dp][ds];" +
            "[dp][rp]psnr=stats_file=${psnrName}:eof_action=endall:shortest=1:repeatlast=0[p];" +
            "[ds][rs]ssim=stats_file=${ssimName}:eof_action=endall:shortest=1:repeatlast=0[s]"
        $arguments = @('-nostdin','-hide_banner','-loglevel','info','-nostats','-n',
            '-hwaccel','none','-threads','4','-ss',$start,'-i',$rcrSource,
            '-hwaccel','none','-threads','4','-i',$variant.output,
            '-filter_complex_threads','2','-filter_complex',$filter,'-map','[p]','-map','[s]',
            '-an','-sn','-dn','-frames:v:0','1200','-frames:v:1','1200','-fps_mode','passthrough','-f','null','-')
        $job = Invoke-QualityMedia $arguments "gop-$gop-quality" @($psnrPath, $ssimPath)
        $mse = @(Read-QualitySamples $psnrPath 'mse_avg')[0]
        $ssim = @(Read-QualitySamples $ssimPath 'All')[0]
        $psnr = @(Read-QualitySamples $psnrPath 'psnr_avg')[0]
        if (@($mse).Count -ne 1200 -or @($ssim).Count -ne 1200 -or @($psnr).Count -ne 1200) { throw 'Metric sample arrays did not retain exact alignment.' }
        $meanMse = ($mse | Measure-Object -Average).Average
        $averageSsim = ($ssim | Measure-Object -Average).Average
        $globalPsnr = if ($meanMse -eq 0) { 'inf' } else { 10 * [Math]::Log10(255.0 * 255.0 / $meanMse) }
        $psnrSummary = @($job.stderr -split "`n" | Where-Object { $_ -match 'PSNR y:' })
        $ssimSummary = @($job.stderr -split "`n" | Where-Object { $_ -match 'SSIM Y:' })
        if ($psnrSummary.Count -ne 1 -or $psnrSummary[0] -notmatch 'average:([^\s]+)') { throw 'Missing unambiguous FFmpeg PSNR summary.' }
        $averagePsnr = if ($Matches[1] -eq 'inf') { 'inf' } else { [double]::Parse($Matches[1], [Globalization.CultureInfo]::InvariantCulture) }
        if ($ssimSummary.Count -ne 1 -or $ssimSummary[0] -notmatch 'All:([^\s]+)') { throw 'Missing unambiguous FFmpeg SSIM summary.' }
        $summarySsim = [double]::Parse($Matches[1], [Globalization.CultureInfo]::InvariantCulture)
        $rcrResult.variants += [ordered]@{ gop_frames = $gop; elapsed_seconds = $job.elapsed_seconds
            paired_psnr_frames = @($mse).Count; paired_ssim_frames = @($ssim).Count
            psnr_average_db = $averagePsnr; ssim_average_all = $summarySsim
            psnr_db_from_rounded_frame_mse = $globalPsnr; ssim_mean_rounded_frame_all = $averageSsim
            per_frame_min_psnr_db = $(if (($psnr | Measure-Object -Minimum).Minimum -eq [double]::PositiveInfinity) { 'inf' } else { ($psnr | Measure-Object -Minimum).Minimum })
            per_frame_min_ssim_all = ($ssim | Measure-Object -Minimum).Minimum
            psnr_stats = $psnrPath; ssim_stats = $ssimPath; arguments = $arguments
            ffmpeg_metric_summary = @($job.stderr -split "`n" | Where-Object { $_ -match '(PSNR y:|SSIM Y:)' }) }
    }
    $rcrResult.success = $true
} catch {
    $rcrResult['error'] = $_.Exception.Message
    throw
} finally {
    $unchanged = $true
    foreach ($file in $rcrFiles) {
        try {
            $file['sha256_after'] = (Get-FileHash -LiteralPath $file.path -Algorithm SHA256).Hash
            $file['unchanged'] = $file.sha256_after -eq $file.sha256_before
            if (-not $file.unchanged) { $unchanged = $false }
        } catch {
            $file['unchanged'] = $false; $file['hash_error'] = $_.Exception.Message; $unchanged = $false
        }
    }
    $rcrResult.files_unchanged = $unchanged
    if (-not $unchanged) { $rcrResult.success = $false }
    $rcrResult | ConvertTo-Json -Depth 12 | Set-Content -LiteralPath (Join-Path $rcrOwned 'summary.json') -Encoding utf8
    Write-Output "Offline GOP quality artifacts: $rcrOwned"
    if (-not $unchanged) { throw 'One or more source/derived fixture hashes changed or could not be checked.' }
}
