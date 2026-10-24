#requires -Version 7.0
# Preparation is opt-in and isolated from production builds. Fetch never runs an
# archive binary. Generate invokes only the reviewed Rust helper and Rust layout
# tests, not a C compiler, decoder, FFmpeg executable, or application.
[CmdletBinding()]
param(
    [Parameter(Mandatory)][ValidateSet('Fetch', 'Generate')][string]$Mode,
    [string]$RuntimeDirectory = 'C:/bin',
    [string]$InputRecord,
    [ValidatePattern('^[a-fA-F0-9]{64}$')][string]$ExpectedArchiveSha256,
    [string]$DependencyLockfile,
    [string]$MsvcInclude,
    [string]$WindowsSdkInclude
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if (-not [Environment]::Is64BitProcess -or $env:OS -ne 'Windows_NT') { throw 'Use Windows x64 PowerShell' }
$rcrRoot = [IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
$rcrTooling = Join-Path $rcrRoot '.downloads/tooling/ffmpeg7-bindings'
$rcrRuntime = (Resolve-Path -LiteralPath $RuntimeDirectory).ProviderPath
$rcrArchiveUrl = 'https://github.com/GyanD/codexffmpeg/releases/download/7.0.2/ffmpeg-7.0.2-full_build-shared.zip'
$rcrWheelUrl = 'https://files.pythonhosted.org/packages/0b/2d/3f480b1e1d31eb3d6de5e3ef641954e5c67430d5ac93b7fa7e07589576c7/libclang-18.1.1-py2.py3-none-win_amd64.whl'
$rcrWheelHash = '4dd2d3b82fab35e2bf9ca717d7b63ac990a3519c7e312f19fa8e86dcc712f7fb'
$rcrDllNames = @('avutil-59.dll', 'avcodec-61.dll', 'avformat-61.dll', 'swscale-8.dll')

function Assert-OwnedPath([string]$Path) {
    $full = [IO.Path]::GetFullPath($Path)
    $prefix = [IO.Path]::GetFullPath($rcrTooling).TrimEnd('\', '/') + [IO.Path]::DirectorySeparatorChar
    if ($full -ne $prefix.TrimEnd('\') -and -not $full.StartsWith($prefix, [StringComparison]::OrdinalIgnoreCase)) {
        throw "Preparation path escapes its workspace directory: $full"
    }
    # Reject existing directory/file links anywhere below the workspace. This
    # avoids accidentally following a prior junction into an unrelated folder.
    $relative = [IO.Path]::GetRelativePath($rcrRoot, $full)
    $current = $rcrRoot
    foreach ($part in $relative.Split([char[]]'\/')) {
        if (-not $part) { continue }
        $current = Join-Path $current $part
        if (Test-Path -LiteralPath $current) {
            $item = Get-Item -LiteralPath $current -Force
            if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) { throw "Linked preparation path refused: $current" }
        }
    }
    return $full
}
function New-OwnedDirectory([string]$Path) {
    $full = Assert-OwnedPath $Path
    [IO.Directory]::CreateDirectory($full) | Out-Null
    return $full
}
function File-Hash([string]$Path) { return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant() }
function Stream-Hash($Entry) {
    $stream = $Entry.Open()
    $sha = [Security.Cryptography.SHA256]::Create()
    try { return ([BitConverter]::ToString($sha.ComputeHash($stream))).Replace('-', '').ToLowerInvariant() }
    finally { $sha.Dispose(); $stream.Dispose() }
}
function Write-OwnedText([string]$Path, [string]$Text) {
    $full = Assert-OwnedPath $Path
    $bytes = [Text.UTF8Encoding]::new($false).GetBytes($Text)
    if (Test-Path -LiteralPath $full) { throw "Refusing to overwrite preparation artifact: $full" }
    $stream = [IO.File]::Open($full, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
    try { $stream.Write($bytes, 0, $bytes.Length) } finally { $stream.Dispose() }
}
function Fetch-Archive([string]$Url, [string]$Path, [string]$ExpectedHash) {
    $full = Assert-OwnedPath $Path
    if (-not (Test-Path -LiteralPath $full)) {
        # Stream with an explicit compressed-download bound; never invoke pip.
        $partial = Assert-OwnedPath ($full + '.fetch-' + [guid]::NewGuid().ToString('N'))
        Add-Type -AssemblyName System.Net.Http
        $client = [Net.Http.HttpClient]::new()
        $client.Timeout = [TimeSpan]::FromMinutes(5)
        $client.DefaultRequestHeaders.UserAgent.ParseAdd('rcr-ffmpeg-binding-preparation/1')
        $response = $null; $source = $null; $target = $null
        try {
            $response = $client.GetAsync($Url, [Net.Http.HttpCompletionOption]::ResponseHeadersRead).GetAwaiter().GetResult()
            $response.EnsureSuccessStatusCode() | Out-Null
            $source = $response.Content.ReadAsStreamAsync().GetAwaiter().GetResult()
            $target = [IO.File]::Open($partial, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
            $buffer = [byte[]]::new(65536); $total = 0L
            while (($count = $source.Read($buffer, 0, $buffer.Length)) -gt 0) {
                $total += $count
                if ($total -gt 128MB) { throw 'Archive exceeds 128 MiB download limit' }
                $target.Write($buffer, 0, $count)
            }
            $target.Dispose(); $target = $null
            if ($ExpectedHash -and (File-Hash $partial) -ne $ExpectedHash.ToLowerInvariant()) { throw "Archive SHA256 mismatch: $Url" }
            [IO.File]::Move($partial, $full) # Never replaces an existing archive.
        } finally {
            if ($target) { $target.Dispose() }; if ($source) { $source.Dispose() }
            if ($response) { $response.Dispose() }; $client.Dispose()
        }
    }
    $hash = File-Hash $full
    if ($ExpectedHash -and $hash -ne $ExpectedHash.ToLowerInvariant()) { throw "Cached archive SHA256 mismatch: $full" }
    return $hash
}
function Extract-Entry($Entry, [string]$Destination) {
    $full = Assert-OwnedPath $Destination
    # Selected headers/licenses and the pinned wheel DLL must be ordinary files.
    if (($Entry.ExternalAttributes -shr 16 -band 0xF000) -eq 0xA000) { throw "Archive link refused: $($Entry.FullName)" }
    if ($Entry.Length -gt 128MB) { throw "Archive entry exceeds size limit: $($Entry.FullName)" }
    New-OwnedDirectory (Split-Path -Parent $full) | Out-Null
    $stream = $Entry.Open()
    $target = $null
    try {
        $target = [IO.File]::Open($full, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
        $buffer = [byte[]]::new(65536); $total = 0L
        while (($count = $stream.Read($buffer, 0, $buffer.Length)) -gt 0) {
            $total += $count
            if ($total -gt $Entry.Length -or $total -gt 128MB) { throw 'Expanded ZIP entry exceeds its declared size limit' }
            $target.Write($buffer, 0, $count)
        }
        if ($total -ne $Entry.Length) { throw 'Truncated ZIP entry' }
    } finally { if ($target) { $target.Dispose() }; $stream.Dispose() }
}
function Header-Inventory([string]$Include) {
    return @(Get-ChildItem -LiteralPath $Include -Recurse -File | Sort-Object FullName | ForEach-Object {
        [ordered]@{ relative = [IO.Path]::GetRelativePath($Include, $_.FullName); sha256 = File-Hash $_.FullName; bytes = $_.Length }
    })
}

New-OwnedDirectory $rcrTooling | Out-Null
Add-Type -AssemblyName System.IO.Compression.FileSystem
if ($Mode -eq 'Fetch') {
    $downloads = New-OwnedDirectory (Join-Path $rcrTooling 'archives')
    $archive = Join-Path $downloads 'ffmpeg-7.0.2-full_build-shared.zip'
    $wheel = Join-Path $downloads 'libclang-18.1.1-py2.py3-none-win_amd64.whl'
    $archiveHash = Fetch-Archive $rcrArchiveUrl $archive $ExpectedArchiveSha256
    Fetch-Archive $rcrWheelUrl $wheel $rcrWheelHash | Out-Null
    $run = New-OwnedDirectory (Join-Path $rcrTooling ('inputs-' + [guid]::NewGuid().ToString('N')))
    $include = New-OwnedDirectory (Join-Path $run 'include')
    $licenses = New-OwnedDirectory (Join-Path $run 'licenses')
    $components = @()
    $zip = [IO.Compression.ZipFile]::OpenRead($archive)
    try {
        $rootEntries = @($zip.Entries | Where-Object { $_.FullName.Replace('\','/').EndsWith('/include/libavcodec/avcodec.h', [StringComparison]::Ordinal) })
        if ($rootEntries.Count -ne 1) { throw 'Provider archive lacks one original public-header tree; no headers will be synthesized' }
        $prefix = $rootEntries[0].FullName.Replace('\','/').Replace('include/libavcodec/avcodec.h', '')
        $selectedCount = 0; $selectedBytes = 0L
        foreach ($entry in $zip.Entries) {
            $name = $entry.FullName.Replace('\','/')
            if (-not $name.StartsWith($prefix, [StringComparison]::Ordinal)) { continue }
            $relative = $name.Substring($prefix.Length)
            if (-not $relative -or $relative.EndsWith('/')) { continue }
            if ($relative.Split('/') -contains '..' -or $relative.Contains(':')) { throw "Unsafe archive path: $relative" }
            $destination = $null
            if ($relative.StartsWith('include/', [StringComparison]::Ordinal) -and $relative.EndsWith('.h', [StringComparison]::OrdinalIgnoreCase)) {
                if ($entry.Length -gt 4MB) { throw 'Public header exceeds limit' }
                $destination = Join-Path $include $relative.Substring('include/'.Length)
            } elseif ($relative -match '(^|/)(LICENSE|COPYING|NOTICE)([.\-_][^/]*)?$' -or $relative -match '^licenses/[^/]+$') {
                $destination = Join-Path $licenses $relative
            }
            if ($destination) {
                $selectedCount++; $selectedBytes += $entry.Length
                if ($selectedCount -gt 2048 -or $selectedBytes -gt 32MB) { throw 'Public-header/license extraction exceeds limit' }
                Extract-Entry $entry $destination
            }
        }
        foreach ($dll in $rcrDllNames) {
            $entry = @($zip.Entries | Where-Object { $_.FullName.Replace('\','/') -eq ($prefix + 'bin/' + $dll) })
            if ($entry.Count -ne 1) { throw "Missing provider DLL entry: $dll" }
            $runtimePath = (Resolve-Path -LiteralPath (Join-Path $rcrRuntime $dll)).ProviderPath
            $archiveDllHash = Stream-Hash $entry[0]
            if ($archiveDllHash -ne (File-Hash $runtimePath)) { throw "Archive/runtime DLL bytes differ for $dll; matching headers are not established" }
            $components += [ordered]@{ name = $dll; runtime_path = $runtimePath; archive_entry = $entry[0].FullName; sha256 = $archiveDllHash }
        }
    } finally { $zip.Dispose() }
    foreach ($required in @('libavcodec/avcodec.h','libavcodec/version.h','libavcodec/version_major.h','libavformat/version.h','libavutil/version.h','libavutil/avconfig.h','libavutil/hwcontext.h','libavutil/hwcontext_d3d11va.h')) {
        if (-not (Test-Path -LiteralPath (Join-Path $include $required) -PathType Leaf)) { throw "Original required header absent: $required. No substitutes are generated." }
    }
    $clangDirectory = New-OwnedDirectory (Join-Path $run 'libclang')
    $zip = [IO.Compression.ZipFile]::OpenRead($wheel)
    try {
        $clangEntries = @($zip.Entries | Where-Object { $_.FullName -eq 'libclang-18.1.1.data/platlib/clang/native/libclang.dll' })
        if ($clangEntries.Count -ne 1) { throw 'Pinned wheel lacks expected libclang DLL' }
        Extract-Entry $clangEntries[0] (Join-Path $clangDirectory 'libclang.dll')
        foreach ($entry in $zip.Entries) {
            if ($entry.FullName -match '^[^/]+\.dist-info/(LICENSE|LICENSE.TXT|COPYING|NOTICE)$') {
                Extract-Entry $entry (Join-Path $licenses ('libclang/' + $entry.FullName))
            }
        }
    } finally { $zip.Dispose() }
    $record = [ordered]@{
        schema = 1; target = 'x86_64-pc-windows-msvc'; root = $rcrRoot
        ffmpeg_release = '7.0.2'; provider_release = 'https://github.com/GyanD/codexffmpeg/releases/tag/7.0.2'
        source_commit = 'e3a61e9103'; source_reference = 'https://github.com/FFmpeg/FFmpeg/commit/e3a61e9103'
        archive_url = $rcrArchiveUrl; archive_path = $archive; archive_sha256 = $archiveHash
        wheel_url = $rcrWheelUrl; wheel_path = $wheel; wheel_sha256 = $rcrWheelHash
        include_directory = $include; headers = Header-Inventory $include; components = $components
        libclang_dll = Join-Path $clangDirectory 'libclang.dll'; libclang_sha256 = File-Hash (Join-Path $clangDirectory 'libclang.dll')
        runtime_directory = $rcrRuntime; licenses_directory = $licenses
        note = 'Public headers/licenses only; DLL entries hashed directly in archive. No runtime installation or generated avconfig.'
    }
    $recordPath = Join-Path $run 'inputs.json'
    Write-OwnedText $recordPath ($record | ConvertTo-Json -Depth 12)
    Write-Host "Fetch record: $recordPath"
    Write-Host "Archive SHA256: $archiveHash"
    Write-Host 'Next: Generate with -InputRecord <record> -ExpectedArchiveSha256 <hash>. This is an integrity pin, not a user approval prompt.'
    return
}

if (-not $InputRecord -or -not $ExpectedArchiveSha256) { throw 'Generate requires -InputRecord and -ExpectedArchiveSha256 from the reviewed Fetch artifact' }
$recordPath = Assert-OwnedPath ((Resolve-Path -LiteralPath $InputRecord).ProviderPath)
$record = Get-Content -LiteralPath $recordPath -Raw | ConvertFrom-Json
if ($record.schema -ne 1 -or $record.target -ne 'x86_64-pc-windows-msvc') { throw 'Unsupported input record' }
if ($record.archive_sha256 -ne $ExpectedArchiveSha256.ToLowerInvariant() -or (File-Hash (Assert-OwnedPath $record.archive_path)) -ne $record.archive_sha256) { throw 'Expected provider archive hash does not match' }
if ($record.wheel_sha256 -ne $rcrWheelHash -or (File-Hash (Assert-OwnedPath $record.wheel_path)) -ne $rcrWheelHash) { throw 'Pinned wheel changed' }
if ((File-Hash (Assert-OwnedPath $record.libclang_dll)) -ne $record.libclang_sha256) { throw 'Extracted libclang changed' }
$include = Assert-OwnedPath $record.include_directory
foreach ($header in $record.headers) {
    if ((File-Hash (Assert-OwnedPath (Join-Path $include $header.relative))) -ne $header.sha256) { throw "Header changed: $($header.relative)" }
}
if (@(Header-Inventory $include).Count -ne @($record.headers).Count) { throw 'Public-header inventory changed' }
foreach ($component in $record.components) {
    if ((File-Hash $component.runtime_path) -ne $component.sha256) { throw "Runtime DLL changed: $($component.name)" }
}
if (-not $MsvcInclude) {
    $MsvcInclude = @('C:/Program Files/Microsoft Visual Studio/2022/Community/VC/Tools/MSVC','C:/Program Files/Microsoft Visual Studio/2022/BuildTools/VC/Tools/MSVC','C:/Program Files/Microsoft Visual Studio/18/Insiders/VC/Tools/MSVC') |
        Where-Object { Test-Path -LiteralPath $_ } | ForEach-Object { Get-ChildItem -LiteralPath $_ -Directory } |
        Sort-Object Name -Descending | Select-Object -First 1 -ExpandProperty FullName
    if ($MsvcInclude) { $MsvcInclude = Join-Path $MsvcInclude 'include' }
}
if (-not $WindowsSdkInclude) {
    $WindowsSdkInclude = Get-ChildItem -LiteralPath 'C:/Program Files (x86)/Windows Kits/10/Include' -Directory |
        Where-Object { Test-Path -LiteralPath (Join-Path $_.FullName 'um/d3d11.h') } |
        Sort-Object Name -Descending | Select-Object -First 1 -ExpandProperty FullName
}
if (-not $MsvcInclude -or -not $WindowsSdkInclude) { throw 'Pass exact -MsvcInclude and -WindowsSdkInclude paths from an installed SDK; no toolchain is installed by this script' }
$MsvcInclude = (Resolve-Path -LiteralPath $MsvcInclude).ProviderPath
$WindowsSdkInclude = (Resolve-Path -LiteralPath $WindowsSdkInclude).ProviderPath
$sdkPaths = @($MsvcInclude, (Join-Path $WindowsSdkInclude 'ucrt'), (Join-Path $WindowsSdkInclude 'shared'), (Join-Path $WindowsSdkInclude 'um'), (Join-Path $WindowsSdkInclude 'winrt'))
foreach ($path in $sdkPaths) { if (-not (Test-Path -LiteralPath $path -PathType Container)) { throw "Missing SDK include directory: $path" } }
$run = New-OwnedDirectory (Join-Path $rcrTooling ('generation-' + [guid]::NewGuid().ToString('N')))
$generator = New-OwnedDirectory (Join-Path $run 'generator')
New-OwnedDirectory (Join-Path $generator 'src') | Out-Null
foreach ($relative in @('Cargo.toml','src/main.rs','wrapper.h')) {
    $template = Join-Path (Join-Path $PSScriptRoot 'ffmpeg7-bindings') $relative
    Write-OwnedText (Join-Path $generator $relative) (Get-Content -LiteralPath $template -Raw)
}
$output = New-OwnedDirectory (Join-Path $run 'output')
$clangArgs = @('--target=x86_64-pc-windows-msvc','-x','c','-std=c11','-fms-extensions','-fms-compatibility','-fms-compatibility-version=19.41','-nostdinc','-DWIN32_LEAN_AND_MEAN','-DNOMINMAX','-DCINTERFACE',('-I' + $include))
foreach ($path in $sdkPaths) { $clangArgs += @('-isystem', $path) }
$configuration = [ordered]@{ wrapper = Join-Path $generator 'wrapper.h'; output = $output; runtime_directory = $record.runtime_directory; libclang_dll = $record.libclang_dll; input_record = $recordPath; clang_args = $clangArgs }
$configurationPath = Join-Path $run 'configuration.json'
Write-OwnedText $configurationPath ($configuration | ConvertTo-Json -Depth 8)
$manifest = Join-Path $generator 'Cargo.toml'
if ($DependencyLockfile) {
    $lockPath = Assert-OwnedPath ((Resolve-Path -LiteralPath $DependencyLockfile).ProviderPath)
    Write-OwnedText (Join-Path $generator 'Cargo.lock') (Get-Content -LiteralPath $lockPath -Raw)
}
$targetDirectory = New-OwnedDirectory (Join-Path $rcrTooling 'cargo-target')
$savedClangPath = [Environment]::GetEnvironmentVariable('LIBCLANG_PATH','Process')
$extraArguments = @(Get-ChildItem Env: | Where-Object { $_.Name.StartsWith('BINDGEN_EXTRA_CLANG_ARGS', [StringComparison]::Ordinal) })
try {
    $env:LIBCLANG_PATH = Split-Path -Parent $record.libclang_dll
    foreach ($extra in $extraArguments) { [Environment]::SetEnvironmentVariable($extra.Name, $null, 'Process') }
    if (-not $DependencyLockfile) {
        & cargo generate-lockfile --offline --manifest-path $manifest
        if ($LASTEXITCODE -ne 0) { throw 'Offline helper dependency resolution failed; no dependency download was attempted' }
    }
    & cargo run --offline --locked -j1 --target x86_64-pc-windows-msvc --target-dir $targetDirectory --manifest-path $manifest -- $configurationPath
    if ($LASTEXITCODE -ne 0) { throw 'Binding generation failed' }
    $layout = New-OwnedDirectory (Join-Path $run 'layout-check')
    New-OwnedDirectory (Join-Path $layout 'src') | Out-Null
    Write-OwnedText (Join-Path $layout 'Cargo.toml') "[package]`nname = `"rcr-ffmpeg7-layout-check`"`nversion = `"0.1.0`"`nedition = `"2021`"`n[workspace]`n"
    $bindingLiteral = (Join-Path $output 'bindings.rs') | ConvertTo-Json -Compress
    Write-OwnedText (Join-Path $layout 'src/lib.rs') ("#![allow(non_camel_case_types, non_snake_case, non_upper_case_globals, dead_code)]`ninclude!($bindingLiteral);`n")
    & cargo test --offline -j1 --target x86_64-pc-windows-msvc --target-dir $targetDirectory --manifest-path (Join-Path $layout 'Cargo.toml')
    if ($LASTEXITCODE -ne 0) { throw 'Generated Rust layout verification failed' }
} finally {
    [Environment]::SetEnvironmentVariable('LIBCLANG_PATH', $savedClangPath, 'Process')
    foreach ($extra in $extraArguments) { [Environment]::SetEnvironmentVariable($extra.Name, $extra.Value, 'Process') }
}
$generation = Get-Content -LiteralPath (Join-Path $output 'generation.json') -Raw | ConvertFrom-Json
$actualIncludes = @($generation.included_files | ForEach-Object {
    $path = (Resolve-Path -LiteralPath $_).ProviderPath
    [ordered]@{ path = $path; sha256 = File-Hash $path }
})
$rustVersion = & rustc -vV
if ($LASTEXITCODE -ne 0) { throw 'Could not record Rust toolchain metadata' }
$rustfmtVersion = & rustfmt --version
if ($LASTEXITCODE -ne 0) { throw 'Could not record Rust formatter metadata' }
$provenance = [ordered]@{ schema = 1; inputs = $record; generation = $generation; configuration = $configuration; msvc_include = $MsvcInclude; windows_sdk_include = $WindowsSdkInclude; actual_included_headers = $actualIncludes; rust_toolchain = $rustVersion; rustfmt = $rustfmtVersion; bindings_sha256 = File-Hash (Join-Path $output 'bindings.rs'); helper_lock_sha256 = File-Hash (Join-Path $generator 'Cargo.lock'); wrapper_sha256 = File-Hash (Join-Path $generator 'wrapper.h'); generator_sha256 = File-Hash (Join-Path $generator 'src/main.rs'); generated_rust_layout_check = 'passed'; media_or_decoder_execution = $false }
Write-OwnedText (Join-Path $output 'provenance.json') ($provenance | ConvertTo-Json -Depth 20)
Write-Host "Generated bindings and provenance: $output"
Write-Host 'No application source or runtime installation was changed. Review these artifacts before any standalone decoder proof.'
