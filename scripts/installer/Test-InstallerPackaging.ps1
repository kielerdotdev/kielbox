# CPU/file-only installer admission tests. No fake executable or installer runs.
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$kielboxTestRepository = [IO.Path]::GetFullPath((Split-Path -Parent (Split-Path -Parent $PSScriptRoot)))
$kielboxTestDownloads = Join-Path $kielboxTestRepository '.downloads'
$kielboxTestRoot = Join-Path $kielboxTestDownloads ('installer-tests-' + [Guid]::NewGuid().ToString('N'))
[IO.Directory]::CreateDirectory($kielboxTestRoot) | Out-Null
$kielboxTestCount = 0
function Assert-InstallerTest([bool]$Condition, [string]$Reason) {
    if (-not $Condition) { throw $Reason }
    $script:kielboxTestCount++
}
function Expect-InstallerFailure([scriptblock]$Action, [string]$Pattern) {
    $kielboxFailed = $false
    try { & $Action | Out-Null } catch {
        $kielboxFailed = $true
        if ($_.Exception.Message -notmatch $Pattern) { throw "Unexpected installer admission error: $($_.Exception.Message)" }
    }
    Assert-InstallerTest $kielboxFailed "Expected installer refusal matching $Pattern"
}
$kielboxTestSource = Join-Path $kielboxTestRoot 'synthetic.exe'
[IO.File]::WriteAllBytes($kielboxTestSource, [byte[]](77,90,0,1,2,3,4,5))
$kielboxTestPayload = Join-Path $kielboxTestRoot 'payload with spaces'
& (Join-Path $kielboxTestRepository 'scripts/package-windows.ps1') -DevelopmentExecutable $kielboxTestSource -OutputDirectory $kielboxTestPayload | Out-Null
$kielboxTestManifestPath = Join-Path $kielboxTestPayload 'package-manifest.json'
$kielboxTestManifest = Get-Content -LiteralPath $kielboxTestManifestPath -Raw | ConvertFrom-Json
$kielboxTestManifest.package_kind = 'windows'
$kielboxTestManifest.debug_shaders_require_checkout = $false
$kielboxTestManifest.files = @($kielboxTestManifest.files) + @([pscustomobject]@{
    path = 'clean.exe'; sha256 = (Get-FileHash -LiteralPath $kielboxTestSource -Algorithm SHA256).Hash.ToLowerInvariant(); bytes = 8
})
[IO.File]::Copy($kielboxTestSource, (Join-Path $kielboxTestPayload 'clean.exe'))
function Save-InstallerTestManifest {
    $kielboxTestEncoding = New-Object Text.UTF8Encoding($false)
    [IO.File]::WriteAllText($kielboxTestManifestPath, ($kielboxTestManifest | ConvertTo-Json -Depth 8), $kielboxTestEncoding)
    $kielboxTestChecksums = @($kielboxTestManifest.files | ForEach-Object { "$($_.sha256)  $($_.path)" })
    $kielboxTestChecksums += "$((Get-FileHash -LiteralPath $kielboxTestManifestPath -Algorithm SHA256).Hash.ToLowerInvariant())  package-manifest.json"
    [IO.File]::WriteAllLines((Join-Path $kielboxTestPayload 'SHA256SUMS.txt'), [string[]]$kielboxTestChecksums, $kielboxTestEncoding)
}
Save-InstallerTestManifest
$kielboxTestScript = Join-Path $kielboxTestRepository 'scripts/package-installer-windows.ps1'
$kielboxTestDestination = Join-Path $kielboxTestRoot 'never published'
$kielboxTestMissingTool = Join-Path $kielboxTestRoot 'missing-iscc.exe'
Expect-InstallerFailure { & $kielboxTestScript -PackageDirectory $kielboxTestPayload -Version '../unsafe' -OutputDirectory $kielboxTestDestination -IsccExecutable $kielboxTestMissingTool } 'Cannot validate argument|validation failed'
Expect-InstallerFailure { & $kielboxTestScript -PackageDirectory (Join-Path $kielboxTestRoot 'missing') -Version '0.1.0-ci.1.1' -OutputDirectory $kielboxTestDestination -IsccExecutable $kielboxTestMissingTool } 'existing, staged Windows release'
[IO.Directory]::CreateDirectory($kielboxTestDestination) | Out-Null
Expect-InstallerFailure { & $kielboxTestScript -PackageDirectory $kielboxTestPayload -Version '0.1.0-ci.1.1' -OutputDirectory $kielboxTestDestination -IsccExecutable $kielboxTestMissingTool } 'destination already exists'
[IO.Directory]::Delete($kielboxTestDestination)
$kielboxTestSentinel = Join-Path $kielboxTestPayload 'private-note.txt'
[IO.File]::WriteAllText($kielboxTestSentinel, 'synthetic user-data sentinel')
Expect-InstallerFailure { & $kielboxTestScript -PackageDirectory $kielboxTestPayload -Version '0.1.0-ci.1.1' -OutputDirectory $kielboxTestDestination -IsccExecutable $kielboxTestMissingTool } 'Unexpected file outside'
Assert-InstallerTest ([IO.File]::ReadAllText($kielboxTestSentinel) -eq 'synthetic user-data sentinel') 'Rejected user-data sentinel was changed.'
[IO.File]::Delete($kielboxTestSentinel)
$kielboxTestRecordings = Join-Path $kielboxTestPayload 'recordings'
[IO.Directory]::CreateDirectory($kielboxTestRecordings) | Out-Null
Expect-InstallerFailure { & $kielboxTestScript -PackageDirectory $kielboxTestPayload -Version '0.1.0-ci.1.1' -OutputDirectory $kielboxTestDestination -IsccExecutable $kielboxTestMissingTool } 'User data or build workspace'
[IO.Directory]::Delete($kielboxTestRecordings)
$kielboxTestManifest.package_kind = 'windows-dev'
Save-InstallerTestManifest
Expect-InstallerFailure { & $kielboxTestScript -PackageDirectory $kielboxTestPayload -Version '0.1.0-ci.1.1' -OutputDirectory $kielboxTestDestination -IsccExecutable $kielboxTestMissingTool } 'release payload with embedded shaders'
$kielboxTestManifest.package_kind = 'windows'
Save-InstallerTestManifest
Expect-InstallerFailure { & $kielboxTestScript -PackageDirectory $kielboxTestPayload -Version '0.1.0-ci.1.1' -OutputDirectory $kielboxTestDestination -IsccExecutable $kielboxTestMissingTool } 'pinned Inno compiler executable is missing'
Assert-InstallerTest (-not (Test-Path -LiteralPath $kielboxTestDestination)) 'Failed preflight published an installer directory.'
$kielboxTestJunction = Join-Path $kielboxTestRoot 'owned junction'
New-Item -ItemType Junction -Path $kielboxTestJunction -Target $kielboxTestPayload | Out-Null
try {
    Expect-InstallerFailure { & $kielboxTestScript -PackageDirectory $kielboxTestPayload -Version '0.1.0-ci.1.1' -OutputDirectory (Join-Path $kielboxTestJunction 'new installer') -IsccExecutable $kielboxTestMissingTool } 'reparse point'
    Assert-InstallerTest (-not (Test-Path -LiteralPath (Join-Path $kielboxTestPayload 'new installer'))) 'Installer traversed output junction.'
    Expect-InstallerFailure { & $kielboxTestScript -PackageDirectory $kielboxTestJunction -Version '0.1.0-ci.1.1' -OutputDirectory $kielboxTestDestination -IsccExecutable $kielboxTestMissingTool } 'reparse point'
} finally {
    # This is a generated, checked junction path, and this nonrecursive call
    # removes the link itself. It does not enumerate or delete its target.
    if ([IO.Path]::GetFullPath((Split-Path -Parent $kielboxTestJunction)) -ne $kielboxTestRoot) { throw 'Junction cleanup escaped fixture root.' }
    [IO.Directory]::Delete($kielboxTestJunction)
}
& (Join-Path $kielboxTestPayload 'Verify-package.ps1') | Out-Null
Assert-InstallerTest (Test-Path -LiteralPath (Join-Path $kielboxTestPayload 'rcr.exe')) 'Admission tests changed the source payload.'
Write-Output "PASS: $kielboxTestCount installer admission assertions; no executable or installer was run."
# Remove only this successful owned synthetic fixture. No links remain.
if ((Split-Path -Parent $kielboxTestRoot) -ne $kielboxTestDownloads -or (Split-Path -Leaf $kielboxTestRoot) -notmatch '^installer-tests-[0-9a-f]{32}$') {
    throw 'Fixture cleanup target escaped the generated workspace directory.'
}
if (@(Get-ChildItem -LiteralPath $kielboxTestRoot -Recurse -Force | Where-Object { $_.Attributes -band [IO.FileAttributes]::ReparsePoint }).Count) { throw 'Unexpected link in fixture cleanup.' }
Remove-Item -LiteralPath $kielboxTestRoot -Recurse -Force
