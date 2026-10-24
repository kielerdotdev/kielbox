#requires -Version 7.0
param(
    [Parameter(Mandatory)][string]$QualitySummary,
    [ValidateCount(1, 3)][ValidateRange(0, 1199)][int[]]$Frames = @(780, 959, 960),
    [string]$Crop = '' # Optional width:height:x:y, applied identically after RGB conversion.
)
$ErrorActionPreference = 'Stop'
$rcrRoot = Split-Path -Parent $PSScriptRoot
$rcrQualityPath = (Resolve-Path -LiteralPath $(if ([IO.Path]::IsPathRooted($QualitySummary)) { $QualitySummary } else { Join-Path $rcrRoot $QualitySummary })).Path
$rcrQuality = Get-Content -LiteralPath $rcrQualityPath -Raw | ConvertFrom-Json
if ($rcrQuality.schema -ne 'rcr-gop-quality-v1' -or -not $rcrQuality.success -or -not $rcrQuality.files_unchanged) { throw 'Expected successful, unchanged GOP quality results.' }
$rcrEncodingPath = (Resolve-Path -LiteralPath $rcrQuality.encoding_summary).Path
$rcrEncoding = Get-Content -LiteralPath $rcrEncodingPath -Raw | ConvertFrom-Json
$rcrEncodingDirectory = Split-Path -Parent $rcrEncodingPath
if ($rcrEncoding.schema -ne 'rcr-gop-encoding-v1' -or -not $rcrEncoding.success -or -not $rcrEncoding.source_unchanged -or $rcrEncoding.frame_limit -ne 1200 -or @($rcrEncoding.variants).Count -ne 3) { throw 'Expected successful 1200-frame encoding fixture.' }
if ((Get-Content -LiteralPath (Join-Path $rcrEncodingDirectory '.rcr-gop-encoding') -Raw).Trim() -ne 'rcr-owned-offline-gop-v1') { throw 'Missing encoding ownership marker.' }
$rcrFrames = @($Frames | Sort-Object -Unique)
$rcrWidth = [int]$rcrEncoding.source_stream.width; $rcrHeight = [int]$rcrEncoding.source_stream.height
if ($rcrWidth -le 0 -or $rcrHeight -le 0 -or $rcrWidth -gt 4096 -or $rcrHeight -gt 4096) { throw 'Diagnostic export supports dimensions up to 4096x4096.' }
if ($Crop) {
    if ($Crop -notmatch '^(\d+):(\d+):(\d+):(\d+)$') { throw 'Crop must be width:height:x:y in integer pixels.' }
    $cropWidth = [int]$Matches[1]; $cropHeight = [int]$Matches[2]; $cropX = [int]$Matches[3]; $cropY = [int]$Matches[4]
    if ($cropWidth -le 0 -or $cropHeight -le 0 -or $cropX + $cropWidth -gt $rcrWidth -or $cropY + $cropHeight -gt $rcrHeight) { throw 'Crop lies outside the source image.' }
    $rcrWidth = $cropWidth; $rcrHeight = $cropHeight
}
$rcrSource = (Resolve-Path -LiteralPath $rcrEncoding.source).Path
$rcrFiles = @([ordered]@{ label = 'source'; path = $rcrSource })
$rcrVariants = @($rcrEncoding.variants | Sort-Object gop_frames -Descending)
if (($rcrVariants.gop_frames -join ',') -ne '120,60,30') { throw 'Expected exactly GOP 120/60/30.' }
foreach ($variant in $rcrVariants) {
    $path = (Resolve-Path -LiteralPath $variant.output).Path
    if ((Split-Path -Parent $path) -ne $rcrEncodingDirectory -or (Split-Path -Leaf $path) -ne "gop-$($variant.gop_frames).mkv") { throw 'Unexpected derived file location.' }
    if ($variant.frame_count -ne 1200 -or $variant.stream.avg_frame_rate -ne '60/1' -or $variant.stream.width -ne $rcrEncoding.source_stream.width -or $variant.stream.height -ne $rcrEncoding.source_stream.height -or
        [Math]::Abs([double]$variant.first_pts_seconds) -gt 0.0011 -or [Math]::Abs([double]$variant.last_pts_seconds - 1199 / 60.0) -gt 0.0011) { throw 'Fixture frame alignment mismatch.' }
    $argsRecorded = @($variant.arguments)
    $seekIndex = [Array]::IndexOf($argsRecorded, '-ss'); $filterIndex = [Array]::IndexOf($argsRecorded, '-vf')
    if ($seekIndex -lt 0 -or $filterIndex -lt 0 -or $argsRecorded[$filterIndex + 1] -ne 'fps=60,format=nv12,setpts=PTS-STARTPTS' -or
        [double]::Parse($argsRecorded[$seekIndex + 1], [Globalization.CultureInfo]::InvariantCulture) -ne [double]$rcrEncoding.start_seconds) { throw 'Unrecognized source extraction alignment.' }
    $rcrFiles += [ordered]@{ label = "gop-$($variant.gop_frames)"; path = $path }
}
foreach ($file in $rcrFiles) {
    $file['sha256_before'] = (Get-FileHash -LiteralPath $file.path -Algorithm SHA256).Hash
    $baseline = @($rcrQuality.files | Where-Object { $_.path -eq $file.path })
    if ($baseline.Count -ne 1 -or -not $baseline[0].unchanged -or $baseline[0].sha256_after -ne $file.sha256_before) { throw "Input no longer matches quality results: $($file.path)" }
}
$rcrOwned = Join-Path $rcrRoot ('.downloads/gop-frames-' + [guid]::NewGuid().ToString('N'))
$rcrManifest = [ordered]@{ schema = 'rcr-gop-frames-v1'; quality_summary = $rcrQualityPath; encoding_summary = $rcrEncodingPath
    frame_indices_zero_based = $rcrFrames; crop = $Crop; width = $rcrWidth; height = $rcrHeight; files = $rcrFiles
    images = @(); success = $false; files_unchanged = $false; note = 'Decoded diagnostic pixels with common RGB conversion; no enhancement, resizing, annotations or compositing.' }
function Assert-PngBounds {
    $images = @(Get-ChildItem -LiteralPath $rcrOwned -Filter '*.png' -File)
    if ($images.Count -gt 12 -or ($images | Measure-Object Length -Sum).Sum -gt 96MB -or @($images | Where-Object Length -ge 16MB).Count) { throw 'Diagnostic PNG count/size limit exceeded (12 total, 16 MiB each, 96 MiB combined).' }
}
function Invoke-FrameExport([string[]]$Arguments, [string]$Label) {
    $process = [Diagnostics.Process]::new(); $process.StartInfo = [Diagnostics.ProcessStartInfo]::new()
    $process.StartInfo.FileName = $rcrFfmpeg; $process.StartInfo.WorkingDirectory = $rcrOwned
    $process.StartInfo.UseShellExecute = $false; $process.StartInfo.CreateNoWindow = $true
    $process.StartInfo.RedirectStandardOutput = $true; $process.StartInfo.RedirectStandardError = $true
    foreach ($argument in $Arguments) { $process.StartInfo.ArgumentList.Add($argument) }
    $outTask = $null; $errTask = $null; $started = $false; $timer = [Diagnostics.Stopwatch]::StartNew()
    try {
        $started = $process.Start(); if (-not $started) { throw "Could not start $Label." }
        $outTask = [RcrGopFramePipe]::Drain($process.StandardOutput.BaseStream, 1MB)
        $errTask = [RcrGopFramePipe]::Drain($process.StandardError.BaseStream, 1MB)
        while (-not $process.WaitForExit(100)) { if ($timer.Elapsed.TotalSeconds -ge 29.5) { throw "$Label reached its 30-second deadline." }; Assert-PngBounds }
        if ($process.ExitCode -ne 0) { throw "$Label failed with exit code $($process.ExitCode); inspect its bounded stderr log." }
        Assert-PngBounds
        if ($outTask.GetAwaiter().GetResult().Length -ge 1MB -or $errTask.GetAwaiter().GetResult().Length -ge 1MB) { throw "$Label exceeded its retained log limit." }
    } finally {
        if ($started -and -not $process.HasExited) { $process.Kill($true); if (-not $process.WaitForExit(10000)) { throw "Could not reap $Label." } }
        if ($outTask) { [IO.File]::WriteAllBytes((Join-Path $rcrOwned "$Label.stdout.txt"), $outTask.GetAwaiter().GetResult()) }
        if ($errTask) { [IO.File]::WriteAllBytes((Join-Path $rcrOwned "$Label.stderr.txt"), $errTask.GetAwaiter().GetResult()) }
        $process.Dispose()
    }
}
try {
    New-Item -ItemType Directory -Path $rcrOwned | Out-Null
    Set-Content -LiteralPath (Join-Path $rcrOwned '.rcr-gop-frames') -Value 'rcr-owned-gop-frame-diagnostics-v1' -NoNewline
    $tool = if ($env:RCR_FFMPEG) { $env:RCR_FFMPEG } else { 'ffmpeg' }
    $rcrFfmpeg = if (Test-Path -LiteralPath $tool -PathType Leaf) { (Resolve-Path -LiteralPath $tool).Path } else { (Get-Command -Name $tool -CommandType Application -ErrorAction Stop | Select-Object -First 1).Source }
    if (-not ('RcrGopFramePipe' -as [type])) {
        Add-Type -TypeDefinition @'
using System;
using System.IO;
using System.Threading.Tasks;
public static class RcrGopFramePipe {
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
    $select = ($rcrFrames | ForEach-Object { "eq(n\,$_)" }) -join '+'
    $filter = "fps=60,format=yuv420p,trim=end_frame=1200,settb=expr=1/60,setpts=N,select=$select,format=rgb24"
    if ($Crop) { $filter += ",crop=$Crop" }
    $start = ([double]$rcrEncoding.start_seconds).ToString('0.######', [Globalization.CultureInfo]::InvariantCulture)
    foreach ($file in $rcrFiles) {
        $arguments = @('-nostdin','-hide_banner','-loglevel','error','-nostats','-n','-hwaccel','none','-threads','4')
        if ($file.label -eq 'source') { $arguments += @('-ss',$start) }
        $arguments += @('-i',$file.path,'-map','0:v:0','-an','-sn','-dn','-vf',$filter,'-frames:v',"$($rcrFrames.Count)",
            '-fps_mode','vfr','-c:v','png','-threads:v','1','-compression_level','3',(Join-Path $rcrOwned "$($file.label)-%02d.png"))
        Invoke-FrameExport $arguments $file.label
        $actual = @(Get-ChildItem -LiteralPath $rcrOwned -Filter "$($file.label)-*.png" -File)
        if ($actual.Count -ne $rcrFrames.Count) { throw "Wrong image count for $($file.label)." }
        for ($index = 0; $index -lt $rcrFrames.Count; $index++) {
            $imagePath = Join-Path $rcrOwned ('{0}-{1:00}.png' -f $file.label, ($index + 1))
            $header = [byte[]]::new(24); $stream = [IO.File]::OpenRead($imagePath)
            try { if ($stream.Read($header, 0, 24) -ne 24) { throw 'Truncated PNG header.' } } finally { $stream.Dispose() }
            $width = [Net.IPAddress]::NetworkToHostOrder([BitConverter]::ToInt32($header, 16)); $height = [Net.IPAddress]::NetworkToHostOrder([BitConverter]::ToInt32($header, 20))
            if ([BitConverter]::ToString($header, 0, 8) -ne '89-50-4E-47-0D-0A-1A-0A' -or $width -ne $rcrWidth -or $height -ne $rcrHeight) { throw 'Unexpected PNG signature/dimensions.' }
            $rcrManifest.images += [ordered]@{ label = $file.label; frame_zero_based = $rcrFrames[$index]; fixture_seconds = $rcrFrames[$index] / 60.0
                source_seconds = [double]$rcrEncoding.start_seconds + $rcrFrames[$index] / 60.0; path = $imagePath; bytes = (Get-Item -LiteralPath $imagePath).Length }
        }
    }
    $rcrManifest.success = $true
} catch {
    $rcrManifest['error'] = $_.Exception.Message; throw
} finally {
    $unchanged = $true
    foreach ($file in $rcrFiles) {
        try { $file['sha256_after'] = (Get-FileHash -LiteralPath $file.path -Algorithm SHA256).Hash; $file['unchanged'] = $file.sha256_after -eq $file.sha256_before; if (-not $file.unchanged) { $unchanged = $false } }
        catch { $file['unchanged'] = $false; $file['hash_error'] = $_.Exception.Message; $unchanged = $false }
    }
    $rcrManifest.files_unchanged = $unchanged; if (-not $unchanged) { $rcrManifest.success = $false }
    $rcrManifest | ConvertTo-Json -Depth 12 | Set-Content -LiteralPath (Join-Path $rcrOwned 'manifest.json') -Encoding utf8
    Write-Output "GOP diagnostic images: $rcrOwned"
    if (-not $unchanged) { throw 'Source or derived fixture hashes changed or could not be checked.' }
}
