# Compile a native Inno Setup wizard from an already verified release payload.
# This never installs Kielbox or launches the application.
param(
    [Parameter(Mandatory = $true)][string]$PackageDirectory,
    [Parameter(Mandatory = $true)][ValidatePattern('^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-[0-9A-Za-z.-]+)?$')][string]$Version,
    [ValidateSet('preview', 'stable')][string]$Channel = 'preview',
    [string]$OutputDirectory = 'dist/windows-installer',
    [string]$IsccExecutable = '',
    # Isolates registry, shortcut and installation identities for qualification.
    # Public publication refuses proof packages.
    [ValidatePattern('^$|^[A-Za-z0-9_-]{1,32}$')][string]$ProofId = ''
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$kielboxInstallerRoot = [IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
$kielboxInnoVersion = '6.7.4'
function Resolve-KielboxInstallerPath([string]$Path) {
    if (-not [IO.Path]::IsPathRooted($Path)) { $Path = Join-Path $kielboxInstallerRoot $Path }
    return [IO.Path]::GetFullPath($Path)
}
function Assert-KielboxNoReparse([string]$Path) {
    $kielboxAncestor = $Path
    while ($kielboxAncestor) {
        if ((Test-Path -LiteralPath $kielboxAncestor) -and ((Get-Item -LiteralPath $kielboxAncestor -Force).Attributes -band [IO.FileAttributes]::ReparsePoint)) { throw "Installer paths must not traverse a reparse point: $kielboxAncestor" }
        $kielboxNext = Split-Path -Parent $kielboxAncestor
        if ($kielboxNext -eq $kielboxAncestor) { break }
        $kielboxAncestor = $kielboxNext
    }
}
function Get-KielboxPayloadFiles([string]$Directory) {
    foreach ($kielboxItem in Get-ChildItem -LiteralPath $Directory -Force) {
        Assert-KielboxNoReparse $kielboxItem.FullName
        if ($kielboxItem.PSIsContainer) {
            if ($kielboxItem.Name -in @('recordings', 'projects', 'screenshots', 'captures', 'exports', 'cache', '.git', '.downloads')) { throw "User data or build workspace is not installer content: $($kielboxItem.FullName)" }
            Get-KielboxPayloadFiles $kielboxItem.FullName
        } else { $kielboxItem }
    }
}
function Write-KielboxJson([string]$Path, $Value) {
    [IO.File]::WriteAllText($Path, (($Value | ConvertTo-Json -Depth 8) + [Environment]::NewLine), (New-Object Text.UTF8Encoding($false)))
}
$kielboxPayloadSource = Resolve-KielboxInstallerPath $PackageDirectory
$kielboxInstallerDestination = Resolve-KielboxInstallerPath $OutputDirectory
Assert-KielboxNoReparse $kielboxPayloadSource
Assert-KielboxNoReparse $kielboxInstallerDestination
if (Test-Path -LiteralPath $kielboxInstallerDestination) { throw "Installer destination already exists; nothing was changed: $kielboxInstallerDestination" }
if (-not (Test-Path -LiteralPath $kielboxPayloadSource -PathType Container)) { throw 'Choose an existing, staged Windows release package.' }
& (Join-Path $kielboxPayloadSource 'Verify-package.ps1')
$kielboxPayloadManifest = Get-Content -LiteralPath (Join-Path $kielboxPayloadSource 'package-manifest.json') -Raw | ConvertFrom-Json
if ($kielboxPayloadManifest.package_kind -ne 'windows' -or $kielboxPayloadManifest.debug_shaders_require_checkout) { throw 'The installer requires a release payload with embedded shaders, not a development executable.' }
$kielboxAllowedFiles = New-Object 'Collections.Generic.HashSet[string]' ([StringComparer]::OrdinalIgnoreCase)
foreach ($kielboxEntry in $kielboxPayloadManifest.files) {
    if (-not $kielboxAllowedFiles.Add([string]$kielboxEntry.path)) { throw 'Duplicate payload inventory path.' }
}
$null = $kielboxAllowedFiles.Add('package-manifest.json')
$null = $kielboxAllowedFiles.Add('SHA256SUMS.txt')
foreach ($kielboxRequired in @('rcr.exe', 'clean.exe', 'LICENSE', 'LICENSE-GPL-3.0', 'THIRD_PARTY.md')) {
    if (-not $kielboxAllowedFiles.Contains($kielboxRequired)) { throw "Installer payload is missing $kielboxRequired" }
}
$kielboxSourceFiles = @(Get-KielboxPayloadFiles $kielboxPayloadSource)
foreach ($kielboxFile in $kielboxSourceFiles) {
    $kielboxRelative = $kielboxFile.FullName.Substring($kielboxPayloadSource.Length).TrimStart('\', '/').Replace('\', '/')
    if (-not $kielboxAllowedFiles.Contains($kielboxRelative)) { throw "Unexpected file outside the verified payload inventory: $kielboxRelative" }
}
. (Join-Path $PSScriptRoot 'installer/Get-InnoCompiler.ps1')
if (-not $IsccExecutable) {
    $IsccExecutable = Get-KielboxInnoCompiler -RepositoryRoot $kielboxInstallerRoot
}
$IsccExecutable = Resolve-KielboxInstallerPath $IsccExecutable
Assert-KielboxNoReparse $IsccExecutable
if (-not (Test-Path -LiteralPath $IsccExecutable -PathType Leaf)) { throw 'The pinned Inno compiler executable is missing.' }
Assert-KielboxInnoCompiler $IsccExecutable
$kielboxVersionParts = $Version.Split('-')[0].Split('.')
foreach ($kielboxPart in $kielboxVersionParts) { if ([long]$kielboxPart -gt 65535) { throw 'Numeric version components must fit the Windows file-version range.' } }
$kielboxInstallAppId = 'Kielbox'
$kielboxInstallAppName = 'Kielbox'
if ($ProofId) { $kielboxInstallAppId += ".Proof.$ProofId"; $kielboxInstallAppName += " Proof $ProofId" }
$kielboxInstallerParent = Split-Path -Parent $kielboxInstallerDestination
[IO.Directory]::CreateDirectory($kielboxInstallerParent) | Out-Null
Assert-KielboxNoReparse $kielboxInstallerParent
$kielboxStage = Join-Path $kielboxInstallerParent ('.kielbox-installer-' + [Guid]::NewGuid().ToString('N'))
$kielboxPackDirectory = Join-Path $kielboxStage 'payload'
$kielboxReleaseDirectory = Join-Path $kielboxStage 'release'
$kielboxBrandDirectory = Join-Path $kielboxStage 'branding'
[IO.Directory]::CreateDirectory($kielboxPackDirectory) | Out-Null
try {
    # Manual PowerShell launchers belong to the portable distribution, not the
    # installed product. Keep executable helpers, attribution and source intact.
    $kielboxPortableOnly = @('Start-rcr.ps1', 'Diagnose-rcr.ps1', 'Verify-package.ps1', 'README-WINDOWS.txt', 'package-manifest.json', 'SHA256SUMS.txt')
    foreach ($kielboxFile in $kielboxSourceFiles) {
        $kielboxRelative = $kielboxFile.FullName.Substring($kielboxPayloadSource.Length).TrimStart('\', '/')
        if ($kielboxRelative -in $kielboxPortableOnly) { continue }
        if ($kielboxRelative -eq 'rcr.exe') { $kielboxRelative = 'Kielbox.exe' }
        $kielboxTarget = Join-Path $kielboxPackDirectory $kielboxRelative
        [IO.Directory]::CreateDirectory((Split-Path -Parent $kielboxTarget)) | Out-Null
        [IO.File]::Copy($kielboxFile.FullName, $kielboxTarget, $false)
        if ((Get-FileHash -LiteralPath $kielboxTarget -Algorithm SHA256).Hash -ne (Get-FileHash -LiteralPath $kielboxFile.FullName -Algorithm SHA256).Hash) { throw 'Payload changed while staging.' }
    }
    Write-KielboxJson (Join-Path $kielboxPackDirectory 'kielbox-install.json') ([ordered]@{ schema_version = 1; appid = 'Kielbox'; channel = $Channel; version = $Version })
    & (Join-Path $PSScriptRoot 'installer/Make-InstallerAssets.ps1') -OutputDirectory $kielboxBrandDirectory
    $kielboxAppIcon = Join-Path $kielboxInstallerRoot 'assets/kielbox.ico'
    if (Test-Path -LiteralPath $kielboxAppIcon -PathType Leaf) { [IO.File]::Copy($kielboxAppIcon, (Join-Path $kielboxBrandDirectory 'kielbox.ico'), $true) }
    $kielboxArguments = @('/Qp', "/DPayloadDir=$kielboxPackDirectory", "/DReleaseDir=$kielboxReleaseDirectory", "/DBrandDir=$kielboxBrandDirectory", "/DAppVersion=$Version", "/DNumericVersion=$($kielboxVersionParts -join '.').0", "/DInstallAppId=$kielboxInstallAppId", "/DInstallAppName=$kielboxInstallAppName", (Join-Path $PSScriptRoot 'installer/Kielbox.iss'))
    & $IsccExecutable @kielboxArguments
    if ($LASTEXITCODE -ne 0) { throw 'Inno Setup compilation failed; inspect its preceding diagnostics.' }
    $kielboxSetupPath = Join-Path $kielboxReleaseDirectory 'Kielbox-Setup.exe'
    if (-not (Test-Path -LiteralPath $kielboxSetupPath -PathType Leaf)) { throw 'Inno Setup did not produce Kielbox-Setup.exe.' }
    $kielboxVersionedName = "Kielbox-$Version-Setup.exe"
    [IO.File]::Copy($kielboxSetupPath, (Join-Path $kielboxReleaseDirectory $kielboxVersionedName))
    Write-KielboxJson (Join-Path $kielboxReleaseDirectory 'update.json') ([ordered]@{
        schema_version = 1; appid = 'Kielbox'; channel = $Channel; version = $Version; file = $kielboxVersionedName
        sha256 = (Get-FileHash -LiteralPath $kielboxSetupPath -Algorithm SHA256).Hash.ToLowerInvariant(); size = (Get-Item -LiteralPath $kielboxSetupPath).Length
    })
    $kielboxInstallerFiles = @(Get-ChildItem -LiteralPath $kielboxReleaseDirectory -File | Sort-Object Name | ForEach-Object { [pscustomobject]@{ path = $_.Name; sha256 = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant(); bytes = $_.Length } })
    Write-KielboxJson (Join-Path $kielboxReleaseDirectory 'installer-build.json') ([ordered]@{
        schema_version = 1; product = 'Kielbox'; package_id = 'Kielbox'; runtime = 'win-x64'; channel = $Channel; version = $Version
        inno_version = $kielboxInnoVersion; install_appid = $kielboxInstallAppId; proof_id = $ProofId; runtime_dependencies = @(); crt = 'static'
        source_commit = $kielboxPayloadManifest.source_commit; source_tree_dirty = $kielboxPayloadManifest.source_tree_dirty; files = $kielboxInstallerFiles
    })
    $kielboxReleaseChecksums = @($kielboxInstallerFiles | ForEach-Object { "$($_.sha256)  $($_.path)" })
    $kielboxReleaseChecksums += "$((Get-FileHash -LiteralPath (Join-Path $kielboxReleaseDirectory 'installer-build.json') -Algorithm SHA256).Hash.ToLowerInvariant())  installer-build.json"
    [IO.File]::WriteAllLines((Join-Path $kielboxReleaseDirectory 'INSTALLER-SHA256SUMS.txt'), [string[]]$kielboxReleaseChecksums, (New-Object Text.UTF8Encoding($false)))
    Assert-KielboxNoReparse $kielboxInstallerDestination
    [IO.Directory]::Move($kielboxReleaseDirectory, $kielboxInstallerDestination)
} catch { throw "Installer was not published; private packaging stage retained at $kielboxStage. $($_.Exception.Message)" }
if ((Split-Path -Parent ([IO.Path]::GetFullPath($kielboxStage))) -ne $kielboxInstallerParent -or (Split-Path -Leaf $kielboxStage) -notmatch '^\.kielbox-installer-[0-9a-f]{32}$') { throw 'Intermediate cleanup target escaped the generated packaging stage.' }
$null = @(Get-KielboxPayloadFiles $kielboxStage)
Remove-Item -LiteralPath $kielboxStage -Recurse -Force
Write-Output "Installer: $(Join-Path $kielboxInstallerDestination 'Kielbox-Setup.exe')"
Write-Output "Update channel: $Channel; version: $Version; Inno Setup: $kielboxInnoVersion"
Write-Output 'No installation or application launch was performed. Preview installers are currently unsigned.'
