# CPU/file-only regression checks. Never execute the fixture .exe or touch dist.
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$rcrTestRoot = Join-Path (Split-Path -Parent $PSScriptRoot) ('.downloads/package-tests-' + [Guid]::NewGuid().ToString('N'))
[IO.Directory]::CreateDirectory($rcrTestRoot) | Out-Null
$rcrTestSource = Join-Path $rcrTestRoot 'fixture app.exe'
[IO.File]::WriteAllBytes($rcrTestSource, [byte[]](77,90,0,1,2,3,4,5))
$rcrTestPackager = Join-Path $PSScriptRoot 'package-windows.ps1'
$rcrTestCount = 0
function Assert-PackageTest([bool]$Condition, [string]$Reason) {
    if (-not $Condition) { throw $Reason }
    $script:rcrTestCount++
}
function Expect-PackageFailure([scriptblock]$Action, [string]$Pattern) {
    $rcrTestFailed = $false
    try { & $Action | Out-Null } catch {
        $rcrTestFailed = $true
        if ($_.Exception.Message -notmatch $Pattern) { throw "Unexpected failure: $($_.Exception.Message)" }
    }
    Assert-PackageTest $rcrTestFailed "Expected failure matching: $Pattern"
}
$rcrTestDestination = Join-Path $rcrTestRoot 'new package with spaces'
& $rcrTestPackager -DevelopmentExecutable $rcrTestSource -SkipBuild -OutputDirectory $rcrTestDestination -Version 'test-0.1' | Out-Null
& (Join-Path $rcrTestDestination 'Verify-package.ps1') | Out-Null
$rcrTestManifest = Get-Content -LiteralPath (Join-Path $rcrTestDestination 'package-manifest.json') -Raw | ConvertFrom-Json
Assert-PackageTest (-not $rcrTestManifest.debug_shaders_require_checkout -and $rcrTestManifest.version_label -eq 'test-0.1') 'Rune shader/version provenance missing.'
Assert-PackageTest (Test-Path -LiteralPath (Join-Path $rcrTestDestination 'licenses/Rune-MIT.txt')) 'Rune license is missing.'
Assert-PackageTest ($rcrTestManifest.files.Count -ge 20) 'License/helper payload inventory incomplete.'
Assert-PackageTest ((Get-FileHash -LiteralPath $rcrTestSource).Hash -eq (Get-FileHash -LiteralPath (Join-Path $rcrTestDestination 'rcr.exe')).Hash) 'Executable copy changed.'
Assert-PackageTest (-not (Test-Path -LiteralPath (Join-Path $rcrTestDestination 'ffmpeg.exe'))) 'FFmpeg must not be bundled.'
$rcrTestData = Join-Path $rcrTestRoot 'existing data'
[IO.Directory]::CreateDirectory((Join-Path $rcrTestData 'recordings')) | Out-Null
[IO.Directory]::CreateDirectory((Join-Path $rcrTestData 'projects')) | Out-Null
[IO.File]::WriteAllText((Join-Path $rcrTestData 'recordings/keep.mkv'), 'user recording')
[IO.File]::WriteAllText((Join-Path $rcrTestData 'projects/keep.json'), 'user project')
$rcrTestLaunch = & (Join-Path $rcrTestDestination 'Start-rcr.ps1') -WorkingDirectory $rcrTestData -ValidateOnly
Assert-PackageTest ($rcrTestLaunch.working_directory -eq $rcrTestData) 'Launcher silently changed working/data folder.'
Expect-PackageFailure { & $rcrTestPackager -DevelopmentExecutable $rcrTestSource -OutputDirectory $rcrTestData } 'Destination already exists'
Assert-PackageTest ([IO.File]::ReadAllText((Join-Path $rcrTestData 'recordings/keep.mkv')) -eq 'user recording') 'Existing recording changed.'
Assert-PackageTest ([IO.File]::ReadAllText((Join-Path $rcrTestData 'projects/keep.json')) -eq 'user project') 'Existing project changed.'
Expect-PackageFailure { & $rcrTestPackager -DevelopmentExecutable $rcrTestSource -OutputDirectory $rcrTestDestination } 'Destination already exists'
$rcrTestLocked = [IO.File]::Open((Join-Path $rcrTestDestination 'rcr.exe'), [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
try { Expect-PackageFailure { & $rcrTestPackager -DevelopmentExecutable $rcrTestSource -OutputDirectory $rcrTestDestination } 'Destination already exists' } finally { $rcrTestLocked.Dispose() }
$rcrTestMissingData = Join-Path $rcrTestRoot 'not yet created'
Expect-PackageFailure { & (Join-Path $rcrTestDestination 'Start-rcr.ps1') -WorkingDirectory $rcrTestMissingData -ValidateOnly } 'does not exist|Cannot find path'
Assert-PackageTest (-not (Test-Path -LiteralPath $rcrTestMissingData)) 'Launcher created/reanchored data unexpectedly.'
[IO.File]::WriteAllText((Join-Path $rcrTestDestination 'rcr.exe'), 'tampered')
Expect-PackageFailure { & (Join-Path $rcrTestDestination 'Verify-package.ps1') } 'integrity mismatch'
$rcrTestEmpty = Join-Path $rcrTestRoot 'empty existing'
[IO.Directory]::CreateDirectory($rcrTestEmpty) | Out-Null
Expect-PackageFailure { & $rcrTestPackager -DevelopmentExecutable $rcrTestSource -OutputDirectory $rcrTestEmpty } 'Destination already exists'
$rcrTestJunction = Join-Path $rcrTestRoot 'owned-junction'
New-Item -ItemType Junction -Path $rcrTestJunction -Target $rcrTestData | Out-Null
Expect-PackageFailure { & $rcrTestPackager -DevelopmentExecutable $rcrTestSource -OutputDirectory (Join-Path $rcrTestJunction 'new-package') } 'reparse point'
Assert-PackageTest (-not (Test-Path -LiteralPath (Join-Path $rcrTestData 'new-package'))) 'Destination traversed junction.'
$rcrTestUnpublished = Join-Path $rcrTestRoot 'must remain unpublished'
$rcrTestLocked = [IO.File]::Open($rcrTestSource, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::None)
try {
    Expect-PackageFailure { & $rcrTestPackager -DevelopmentExecutable $rcrTestSource -OutputDirectory $rcrTestUnpublished } 'Packaging failed without replacing'
} finally { $rcrTestLocked.Dispose() }
Assert-PackageTest (-not (Test-Path -LiteralPath $rcrTestUnpublished)) 'A failed copy published an incomplete package.'
Expect-PackageFailure { & $rcrTestPackager -DevelopmentExecutable $rcrTestSource -OutputDirectory (Join-Path $rcrTestRoot 'invalid version') -Version '../unsafe' } 'Cannot validate argument'
$rcrTestArchive = Join-Path $rcrTestRoot 'caller-source.zip'
[IO.File]::WriteAllBytes($rcrTestArchive, [byte[]](80,75,5,6,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0))
$rcrTestWithArchive = Join-Path $rcrTestRoot 'source archive package'
& $rcrTestPackager -DevelopmentExecutable $rcrTestSource -OutputDirectory $rcrTestWithArchive -SourceArchive $rcrTestArchive | Out-Null
$rcrTestArchiveManifestPath = Join-Path $rcrTestWithArchive 'package-manifest.json'
$rcrTestArchiveManifest = Get-Content -LiteralPath $rcrTestArchiveManifestPath -Raw | ConvertFrom-Json
Assert-PackageTest ($rcrTestArchiveManifest.corresponding_source_included -and (Test-Path -LiteralPath (Join-Path $rcrTestWithArchive 'corresponding-source.zip'))) 'Explicit source archive was not inventoried.'
$rcrTestArchiveManifest.version_label = 'manifest changed without updating checksums'
$rcrTestArchiveManifest | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $rcrTestArchiveManifestPath -Encoding UTF8
Expect-PackageFailure { & (Join-Path $rcrTestWithArchive 'Verify-package.ps1') } 'Checksum mismatch: package-manifest.json'
Write-Output "PASS: $rcrTestCount packaging assertions. Owned fixtures preserved at $rcrTestRoot"
