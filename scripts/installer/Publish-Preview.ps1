#requires -Version 7.0
# Trusted main-branch job only. GH_TOKEN remains solely in the environment.
param(
    [Parameter(Mandatory = $true)][string]$ReleaseDirectory,
    [Parameter(Mandatory = $true)][ValidatePattern('^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$')][string]$Repository,
    [Parameter(Mandatory = $true)][ValidatePattern('^[a-fA-F0-9]{40}$')][string]$ExpectedCommit
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if ($Repository -ne 'kielerdotdev/kielbox' -or $env:GITHUB_EVENT_NAME -ne 'push' -or $env:GITHUB_REF -ne 'refs/heads/main') { throw 'Preview publication is restricted to a trusted main-branch push in kielerdotdev/kielbox.' }
$kielboxReleaseRoot = (Resolve-Path -LiteralPath $ReleaseDirectory).ProviderPath
$kielboxBuild = Get-Content -LiteralPath (Join-Path $kielboxReleaseRoot 'installer-build.json') -Raw | ConvertFrom-Json
if ($kielboxBuild.schema_version -ne 1 -or $kielboxBuild.package_id -ne 'Kielbox' -or $kielboxBuild.channel -ne 'preview' -or $kielboxBuild.source_tree_dirty -or $kielboxBuild.source_commit -ne $ExpectedCommit -or $kielboxBuild.proof_id -or $kielboxBuild.install_appid -ne 'Kielbox' -or $kielboxBuild.inno_version -ne '6.7.4' -or $kielboxBuild.crt -ne 'static' -or @($kielboxBuild.runtime_dependencies).Count) { throw 'Installer provenance does not match the clean reviewed production preview build.' }
$kielboxNames = New-Object 'Collections.Generic.HashSet[string]' ([StringComparer]::OrdinalIgnoreCase)
foreach ($kielboxEntry in $kielboxBuild.files) {
    $kielboxName = [string]$kielboxEntry.path
    if ($kielboxName -notmatch '^[A-Za-z0-9][A-Za-z0-9._+-]*$' -or -not $kielboxNames.Add($kielboxName)) { throw 'Invalid or duplicate installer receipt path.' }
    $kielboxPath = Join-Path $kielboxReleaseRoot $kielboxName
    $kielboxFile = Get-Item -LiteralPath $kielboxPath -Force
    if ($kielboxFile.PSIsContainer -or ($kielboxFile.Attributes -band [IO.FileAttributes]::ReparsePoint) -or $kielboxFile.Length -ne $kielboxEntry.bytes -or (Get-FileHash -LiteralPath $kielboxPath -Algorithm SHA256).Hash -ne $kielboxEntry.sha256) { throw "Installer asset failed verification: $kielboxName" }
}
$kielboxVersionedName = "Kielbox-$($kielboxBuild.version)-Setup.exe"
foreach ($kielboxRequired in @('Kielbox-Setup.exe', $kielboxVersionedName, 'update.json')) { if (-not $kielboxNames.Contains($kielboxRequired)) { throw "Missing release asset: $kielboxRequired" } }
$kielboxCatalog = Get-Content -LiteralPath (Join-Path $kielboxReleaseRoot 'update.json') -Raw | ConvertFrom-Json
$kielboxImmutable = @($kielboxBuild.files | Where-Object { $_.path -eq $kielboxVersionedName })[0]
$kielboxFriendly = @($kielboxBuild.files | Where-Object { $_.path -eq 'Kielbox-Setup.exe' })[0]
if ($kielboxCatalog.schema_version -ne 1 -or $kielboxCatalog.appid -ne 'Kielbox' -or $kielboxCatalog.channel -ne 'preview' -or $kielboxCatalog.version -ne $kielboxBuild.version -or $kielboxCatalog.file -ne $kielboxVersionedName -or $kielboxCatalog.size -ne $kielboxImmutable.bytes -or $kielboxCatalog.sha256 -ne $kielboxImmutable.sha256 -or $kielboxFriendly.sha256 -ne $kielboxImmutable.sha256) { throw 'The update catalog does not identify the exact immutable installer in the build receipt.' }
$kielboxCandidateVersion = [semver]::Parse([string]$kielboxCatalog.version)
function Assert-KielboxCurrentMain {
    $kielboxCurrentHead = (& gh api "repos/$Repository/git/ref/heads/main" --jq '.object.sha' | Out-String).Trim()
    if ($LASTEXITCODE -ne 0) { throw 'Cannot verify current main branch.' }
    if ($kielboxCurrentHead -ne $ExpectedCommit) { throw 'A newer main commit superseded this preview; channel metadata was not promoted.' }
}
Assert-KielboxCurrentMain
$kielboxReleaseJson = (& gh api "repos/$Repository/releases/tags/preview" 2>$null | Out-String)
if ($LASTEXITCODE -eq 0) {
    $kielboxExisting = $kielboxReleaseJson | ConvertFrom-Json
    if ($kielboxExisting.body -notlike '*<!-- kielbox-managed-preview:1 -->*' -or -not $kielboxExisting.prerelease -or $kielboxExisting.draft) { throw 'The preview release is not owned by this automation. Refusing to change it.' }
    $kielboxPreviousAssets = @($kielboxExisting.assets | Where-Object { $_.name -eq 'update.json' })
    if ($kielboxPreviousAssets.Count) {
        if ($kielboxPreviousAssets.Count -ne 1 -or $kielboxPreviousAssets[0].size -gt 65536) { throw 'The current preview catalog has invalid size or duplicate assets.' }
        $kielboxPreviousDirectory = Join-Path $env:RUNNER_TEMP ('kielbox-previous-' + [Guid]::NewGuid().ToString('N'))
        & gh release download preview --repo $Repository --pattern update.json --dir $kielboxPreviousDirectory
        if ($LASTEXITCODE -ne 0) { throw 'Cannot inspect the published preview version.' }
        $kielboxPreviousPath = Join-Path $kielboxPreviousDirectory 'update.json'
        if ((Get-Item -LiteralPath $kielboxPreviousPath).Length -gt 65536 -or $kielboxPreviousAssets[0].digest -ne ('sha256:' + (Get-FileHash -LiteralPath $kielboxPreviousPath -Algorithm SHA256).Hash.ToLowerInvariant())) { throw 'The existing catalog failed its server digest check.' }
        $kielboxPrevious = Get-Content -LiteralPath $kielboxPreviousPath -Raw | ConvertFrom-Json
        if ($kielboxPrevious.schema_version -ne 1 -or $kielboxPrevious.appid -ne 'Kielbox' -or $kielboxPrevious.channel -ne 'preview') { throw 'Unexpected published catalog identity.' }
        $kielboxPreviousVersion = [semver]::Parse([string]$kielboxPrevious.version)
        if ($kielboxPreviousVersion -gt $kielboxCandidateVersion) { Write-Output 'A newer preview is already published; no release changes were made.'; return }
        if ($kielboxPreviousVersion -eq $kielboxCandidateVersion -and ($kielboxPrevious.sha256 -ne $kielboxCatalog.sha256 -or $kielboxPrevious.file -ne $kielboxCatalog.file)) { throw 'A published version cannot be replaced with different bytes.' }
    }
} else {
    $kielboxNotes = Join-Path $env:RUNNER_TEMP ('kielbox-preview-' + [Guid]::NewGuid().ToString('N') + '.md')
    $kielboxNotesText = @"
<!-- kielbox-managed-preview:1 -->
Download [Kielbox-Setup.exe](https://github.com/$Repository/releases/download/preview/Kielbox-Setup.exe) and run it.

The native Windows wizard installs for your account, adds a Start menu entry and an uninstaller, and offers optional desktop and startup shortcuts. Existing captures and settings stay outside the application folder.

These are unsigned preview builds. Windows may show a publisher warning. Installed builds can check for updates in Settings.
"@
    [IO.File]::WriteAllText($kielboxNotes, $kielboxNotesText)
    & gh release create preview --repo $Repository --target $ExpectedCommit --title 'Kielbox Preview' --prerelease --notes-file $kielboxNotes
    if ($LASTEXITCODE -ne 0) { throw 'Creating the managed preview release failed.' }
    $kielboxExisting = (& gh api "repos/$Repository/releases/tags/preview" | Out-String) | ConvertFrom-Json
    if ($LASTEXITCODE -ne 0) { throw 'Cannot read the created preview release.' }
}
$kielboxPreviousImmutable = @($kielboxExisting.assets | Where-Object { $_.name -eq $kielboxVersionedName })
if ($kielboxPreviousImmutable.Count) {
    if ($kielboxPreviousImmutable.Count -ne 1 -or $kielboxPreviousImmutable[0].digest -ne ('sha256:' + $kielboxImmutable.sha256)) { throw 'The immutable versioned installer already exists with an unverified or different digest.' }
} else {
    & gh release upload preview (Join-Path $kielboxReleaseRoot $kielboxVersionedName) --repo $Repository
    if ($LASTEXITCODE -ne 0) { throw 'Uploading the immutable installer failed.' }
}
& gh release upload preview (Join-Path $kielboxReleaseRoot 'Kielbox-Setup.exe') --repo $Repository --clobber
if ($LASTEXITCODE -ne 0) { throw 'Uploading the direct-download installer failed.' }
$kielboxReceiptUpload = Join-Path $env:RUNNER_TEMP ("installer-build-$($kielboxBuild.version).json")
[IO.File]::Copy((Join-Path $kielboxReleaseRoot 'installer-build.json'), $kielboxReceiptUpload, $true)
& gh release upload preview $kielboxReceiptUpload --repo $Repository --clobber
if ($LASTEXITCODE -ne 0) { throw 'Uploading build evidence failed.' }
$kielboxServer = (& gh api "repos/$Repository/releases/tags/preview" | Out-String) | ConvertFrom-Json
if ($LASTEXITCODE -ne 0) { throw 'Cannot verify uploaded release assets.' }
foreach ($kielboxExpected in @($kielboxImmutable, $kielboxFriendly)) {
    $kielboxObserved = @($kielboxServer.assets | Where-Object { $_.name -eq $kielboxExpected.path })
    if ($kielboxObserved.Count -ne 1 -or $kielboxObserved[0].digest -ne ('sha256:' + $kielboxExpected.sha256)) { throw "GitHub's uploaded digest does not match: $($kielboxExpected.path)" }
}
Assert-KielboxCurrentMain
& gh api --method PATCH "repos/$Repository/git/refs/tags/preview" -f "sha=$ExpectedCommit" -F 'force=true' | Out-Null
if ($LASTEXITCODE -ne 0) { throw 'Updating the automation-owned preview tag failed.' }
# Publish the small catalog last. Older catalogs retain valid immutable installers.
& gh release upload preview (Join-Path $kielboxReleaseRoot 'update.json') --repo $Repository --clobber
if ($LASTEXITCODE -ne 0) { throw 'Publishing the verified preview catalog failed.' }
Write-Output "Published Kielbox $($kielboxBuild.version): direct EXE plus immutable installer and catalog."
