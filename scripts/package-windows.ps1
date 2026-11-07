param(
    [string]$DevelopmentExecutable = '',
    [switch]$SkipBuild,
    # Fresh destinations only: never overwrite an earlier package or its data.
    [string]$OutputDirectory = '',
    [ValidatePattern('^$|^[A-Za-z0-9][A-Za-z0-9._-]{0,79}$')][string]$Version = '',
    # Caller-prepared corresponding source only. Never auto-collect user files.
    [string]$SourceArchive = ''
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$rcrPackageRoot = [IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
function Resolve-PackagePath([string]$Path) {
    if (-not [IO.Path]::IsPathRooted($Path)) { $Path = Join-Path $rcrPackageRoot $Path }
    [IO.Path]::GetFullPath($Path)
}
function Assert-NoReparseAncestors([string]$Path) {
    $rcrCheckedPath = $Path
    while ($rcrCheckedPath) {
        if (Test-Path -LiteralPath $rcrCheckedPath) {
            if ((Get-Item -LiteralPath $rcrCheckedPath -Force).Attributes -band [IO.FileAttributes]::ReparsePoint) {
                throw "Package paths must not traverse a reparse point: $rcrCheckedPath"
            }
        }
        $rcrCheckedParent = Split-Path -Parent $rcrCheckedPath
        if ($rcrCheckedParent -eq $rcrCheckedPath) { break }
        $rcrCheckedPath = $rcrCheckedParent
    }
}
$rcrPackageKind = if ($DevelopmentExecutable) { 'windows-dev' } else { 'windows' }
if (-not $OutputDirectory) {
    $rcrPackageSuffix = if ($Version) { "-$Version" } else { '' }
    $OutputDirectory = "dist/$rcrPackageKind$rcrPackageSuffix"
}
$rcrPackageFolder = Resolve-PackagePath $OutputDirectory
Assert-NoReparseAncestors $rcrPackageFolder
if (Test-Path -LiteralPath $rcrPackageFolder) {
    throw "Destination already exists; nothing was changed: $rcrPackageFolder. Choose -Version or a new -OutputDirectory. Existing executables, recordings and projects are never replaced."
}
if ($DevelopmentExecutable) {
    $rcrPackageSource = Resolve-PackagePath $DevelopmentExecutable
} else {
    if (-not $SkipBuild) {
        Push-Location $rcrPackageRoot
        $rcrPreviousEncodedFlags = [Environment]::GetEnvironmentVariable('CARGO_ENCODED_RUSTFLAGS', 'Process')
        try {
            $rcrReleaseFlags = @()
            if ($null -ne $rcrPreviousEncodedFlags) { $rcrReleaseFlags += $rcrPreviousEncodedFlags -split [char]31 }
            elseif ($env:RUSTFLAGS) { throw 'Use CARGO_ENCODED_RUSTFLAGS rather than RUSTFLAGS for release packaging, so static CRT flags can be appended without changing quoting.' }
            $rcrReleaseFlags += @('-C', 'target-feature=+crt-static')
            [Environment]::SetEnvironmentVariable('CARGO_ENCODED_RUSTFLAGS', ($rcrReleaseFlags -join [char]31), 'Process')
            & cargo build --locked --release -p desktop -p clean -j1
            if ($LASTEXITCODE -ne 0) { throw "Release build failed with exit code $LASTEXITCODE" }
        } finally {
            if ($null -eq $rcrPreviousEncodedFlags) { [Environment]::SetEnvironmentVariable('CARGO_ENCODED_RUSTFLAGS', [NullString]::Value, 'Process') }
            else { [Environment]::SetEnvironmentVariable('CARGO_ENCODED_RUSTFLAGS', $rcrPreviousEncodedFlags, 'Process') }
            Pop-Location
        }
    }
    $rcrPackageSource = Join-Path $rcrPackageRoot 'target/release/desktop.exe'
}
Assert-NoReparseAncestors $rcrPackageSource
$rcrPackageSource = (Resolve-Path -LiteralPath $rcrPackageSource).Path
if ([IO.Path]::GetExtension($rcrPackageSource) -ne '.exe' -or -not (Test-Path -LiteralPath $rcrPackageSource -PathType Leaf)) { throw 'Expected a built Windows executable.' }
$rcrPackageFiles = [ordered]@{
    'rcr.exe' = $rcrPackageSource
    'README.md' = (Join-Path $rcrPackageRoot 'README.md')
    'README-WINDOWS.txt' = (Join-Path $PSScriptRoot 'packaging/README-WINDOWS.txt')
    'Start-rcr.ps1' = (Join-Path $PSScriptRoot 'packaging/Start-rcr.ps1')
    'Diagnose-rcr.ps1' = (Join-Path $PSScriptRoot 'packaging/Diagnose-rcr.ps1')
    'Verify-package.ps1' = (Join-Path $PSScriptRoot 'packaging/Verify-package.ps1')
    'LICENSE' = (Join-Path $rcrPackageRoot 'LICENSE')
    'LICENSE-GPL-3.0' = (Join-Path $rcrPackageRoot 'LICENSE-GPL-3.0')
    'THIRD_PARTY.md' = (Join-Path $rcrPackageRoot 'THIRD_PARTY.md')
    'Cargo.lock' = (Join-Path $rcrPackageRoot 'Cargo.lock')
    'licenses/Geist-OFL.txt' = (Join-Path $rcrPackageRoot 'assets/fonts/OFL.txt')
    'licenses/LiberationSans-NOTICE.md' = (Join-Path $rcrPackageRoot 'crates/image-editor/THIRD_PARTY.md')
    'licenses/DPDFNet-Apache-2.0.txt' = (Join-Path $rcrPackageRoot 'crates/dpdfnet/LICENSE-CEVA')
    'licenses/HushMic-Apache-2.0.txt' = (Join-Path $rcrPackageRoot 'crates/dpdfnet/LICENSE-HUSHMIC')
    'licenses/DPDFNet-NOTICE.md' = (Join-Path $rcrPackageRoot 'crates/dpdfnet/NOTICE.md')
    'licenses/NVENC-MIT.txt' = (Join-Path $rcrPackageRoot 'crates/nvenc-sys/LICENSE')
    'licenses/NVENC-NOTICE.md' = (Join-Path $rcrPackageRoot 'crates/nvenc-sys/NOTICE.md')
    'licenses/Rune-MIT.txt' = (Join-Path $rcrPackageRoot 'assets/licenses/Rune-MIT.txt')
    'licenses/Rune-Inter-OFL.txt' = (Join-Path $rcrPackageRoot 'assets/licenses/Rune-Inter-OFL.txt')
    'licenses/Rune-IBMPlexMono-OFL.txt' = (Join-Path $rcrPackageRoot 'assets/licenses/Rune-IBMPlexMono-OFL.txt')
    'licenses/FFmpeg-HEADER-NOTICES.txt' = (Join-Path $rcrPackageRoot 'crates/editor-core/src/playback_gpu/ffmpeg7/HEADER-NOTICES.txt')
    'licenses/FFmpeg-PROVIDER-LICENSE' = (Join-Path $rcrPackageRoot 'crates/editor-core/src/playback_gpu/ffmpeg7/PROVIDER-LICENSE')
}
if (-not $DevelopmentExecutable) {
    $rcrPackageFiles['clean.exe'] = Join-Path $rcrPackageRoot 'target/release/clean.exe'
    $rcrPackageFiles['CLEANUP.md'] = Join-Path $rcrPackageRoot 'crates/clean/README.md'
}
if ($SourceArchive) {
    $rcrPackageArchiveSuffix = if ($SourceArchive -match '\.tar\.gz$') { '.tar.gz' } else { [IO.Path]::GetExtension($SourceArchive).ToLowerInvariant() }
    if ($rcrPackageArchiveSuffix -notin @('.zip', '.tar.gz', '.tgz', '.tar')) { throw 'SourceArchive must be an explicitly prepared .zip, .tar, .tar.gz or .tgz archive.' }
    $rcrPackageFiles['corresponding-source' + $rcrPackageArchiveSuffix] = Resolve-PackagePath $SourceArchive
}
foreach ($rcrPackageInput in $rcrPackageFiles.Values) {
    Assert-NoReparseAncestors $rcrPackageInput
    if (-not (Test-Path -LiteralPath $rcrPackageInput -PathType Leaf)) { throw "Required package input missing: $rcrPackageInput" }
}
$rcrPackageParent = Split-Path -Parent $rcrPackageFolder
[IO.Directory]::CreateDirectory($rcrPackageParent) | Out-Null
Assert-NoReparseAncestors $rcrPackageParent
$rcrPackageStage = Join-Path $rcrPackageParent ('.rcr-staging-' + [Guid]::NewGuid().ToString('N'))
[IO.Directory]::CreateDirectory($rcrPackageStage) | Out-Null
try {
    $rcrPackageEntries = @()
    foreach ($rcrPackageEntry in $rcrPackageFiles.GetEnumerator()) {
        $rcrPackageTarget = Join-Path $rcrPackageStage $rcrPackageEntry.Key
        [IO.Directory]::CreateDirectory((Split-Path -Parent $rcrPackageTarget)) | Out-Null
        $rcrPackageInputStream = [IO.File]::OpenRead($rcrPackageEntry.Value)
        try {
            $rcrPackageOutputStream = [IO.File]::Open($rcrPackageTarget, [IO.FileMode]::CreateNew)
            try { $rcrPackageInputStream.CopyTo($rcrPackageOutputStream) } finally { $rcrPackageOutputStream.Dispose() }
        } finally { $rcrPackageInputStream.Dispose() }
        $rcrPackageHash = (Get-FileHash -LiteralPath $rcrPackageTarget -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($rcrPackageHash -ne (Get-FileHash -LiteralPath $rcrPackageEntry.Value -Algorithm SHA256).Hash.ToLowerInvariant()) { throw "Source changed during staging or copy failed: $($rcrPackageEntry.Key)" }
        $rcrPackageEntries += [ordered]@{ path = $rcrPackageEntry.Key; sha256 = $rcrPackageHash; bytes = (Get-Item -LiteralPath $rcrPackageTarget).Length }
    }
    $rcrPackageCommit = (& git -C $rcrPackageRoot rev-parse HEAD 2>$null | Out-String).Trim()
    $rcrPackageDirty = [bool](& git -C $rcrPackageRoot status --porcelain --untracked-files=normal 2>$null | Out-String).Trim()
    $rcrPackageManifest = [ordered]@{
        schema_version = 1; version_label = $Version; package_kind = $rcrPackageKind
        created_utc = [DateTime]::UtcNow.ToString('o'); source_commit = $rcrPackageCommit; source_tree_dirty = $rcrPackageDirty
        debug_shaders_require_checkout = $false
        release_shader_policy = 'Rune embeds WGSL shader sources and uses wgpu. Verify on the target GPU before distribution.'
        corresponding_source_included = [bool]$SourceArchive
        distribution_review = 'Staging is not a license/source-compliance certification. Review transitive dependency notices and provide exact corresponding source including local changes before external distribution.'
        files = @($rcrPackageEntries)
    }
    $rcrPackageUtf8 = New-Object System.Text.UTF8Encoding($false)
    [IO.File]::WriteAllText((Join-Path $rcrPackageStage 'package-manifest.json'), (($rcrPackageManifest | ConvertTo-Json -Depth 8) + [Environment]::NewLine), $rcrPackageUtf8)
    $rcrPackageChecksums = @($rcrPackageEntries | ForEach-Object { "$($_.sha256)  $($_.path)" })
    $rcrPackageManifestHash = (Get-FileHash -LiteralPath (Join-Path $rcrPackageStage 'package-manifest.json') -Algorithm SHA256).Hash.ToLowerInvariant()
    $rcrPackageChecksums += "$rcrPackageManifestHash  package-manifest.json"
    [IO.File]::WriteAllLines((Join-Path $rcrPackageStage 'SHA256SUMS.txt'), [string[]]$rcrPackageChecksums, $rcrPackageUtf8)
    & (Join-Path $rcrPackageStage 'Verify-package.ps1')
    # Same-volume rename publishes a complete package, failing if destination appeared.
    Assert-NoReparseAncestors $rcrPackageFolder
    [IO.Directory]::Move($rcrPackageStage, $rcrPackageFolder)
} catch {
    throw "Packaging failed without replacing the destination. Private staging preserved at $rcrPackageStage. $($_.Exception.Message)"
}
Write-Output "Packaged application: $(Join-Path $rcrPackageFolder 'rcr.exe')"
Write-Output "Manifest: $(Join-Path $rcrPackageFolder 'package-manifest.json')"
Write-Output 'Choose your existing working/data folder with Start-rcr.ps1; no user data was moved.'
if ($DevelopmentExecutable) { Write-Output 'DEVELOPMENT ONLY: this package contains an unoptimized development build.' }
Write-Output 'No FFmpeg, FFprobe, FFplay, driver DLLs, recordings or projects are bundled.'
