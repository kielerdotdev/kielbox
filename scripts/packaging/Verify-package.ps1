$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$rcrVerifyManifest = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'package-manifest.json') -Raw | ConvertFrom-Json
if ($rcrVerifyManifest.schema_version -ne 1) { throw 'Unsupported package manifest version.' }
$rcrVerifyNames = @{}
function Get-VerifiedPackagePath([string]$Relative) {
    if (-not $Relative -or [IO.Path]::IsPathRooted($Relative) -or $Relative -match '(^|[\\/])\.\.([\\/]|$)|:') { throw "Invalid manifest path: $Relative" }
    $rcrVerifyPath = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot $Relative))
    if (-not $rcrVerifyPath.StartsWith($PSScriptRoot + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) { throw "Path outside package: $Relative" }
    $rcrVerifyAncestor = $rcrVerifyPath
    while ($rcrVerifyAncestor -and $rcrVerifyAncestor.Length -ge $PSScriptRoot.Length) {
        if ((Get-Item -LiteralPath $rcrVerifyAncestor -Force).Attributes -band [IO.FileAttributes]::ReparsePoint) { throw "Reparse point in package: $Relative" }
        $rcrVerifyAncestor = Split-Path -Parent $rcrVerifyAncestor
    }
    $rcrVerifyPath
}
foreach ($rcrVerifyEntry in $rcrVerifyManifest.files) {
    $rcrVerifyRelative = [string]$rcrVerifyEntry.path
    if ([IO.Path]::IsPathRooted($rcrVerifyRelative) -or $rcrVerifyRelative -match '(^|[\\/])\.\.([\\/]|$)' -or $rcrVerifyNames.ContainsKey($rcrVerifyRelative)) { throw "Invalid manifest path: $rcrVerifyRelative" }
    $rcrVerifyNames[$rcrVerifyRelative] = $true
    $rcrVerifyFile = Get-VerifiedPackagePath $rcrVerifyRelative
    $rcrVerifyItem = Get-Item -LiteralPath $rcrVerifyFile -Force
    if ($rcrVerifyItem.PSIsContainer -or ($rcrVerifyItem.Attributes -band [IO.FileAttributes]::ReparsePoint)) { throw "Not a regular package file: $rcrVerifyRelative" }
    if ($rcrVerifyItem.Length -ne $rcrVerifyEntry.bytes -or (Get-FileHash -LiteralPath $rcrVerifyFile -Algorithm SHA256).Hash.ToLowerInvariant() -ne $rcrVerifyEntry.sha256) { throw "Package integrity mismatch: $rcrVerifyRelative" }
}
$rcrVerifyChecksumNames = @{}
foreach ($rcrVerifyLine in (Get-Content -LiteralPath (Join-Path $PSScriptRoot 'SHA256SUMS.txt'))) {
    if ($rcrVerifyLine -notmatch '^([a-fA-F0-9]{64})  (.+)$') { throw 'Malformed SHA256SUMS.txt line.' }
    $rcrVerifyExpected = $Matches[1]
    $rcrVerifyChecksumName = $Matches[2]
    if ($rcrVerifyChecksumNames.ContainsKey($rcrVerifyChecksumName)) { throw 'Duplicate checksum entry.' }
    $rcrVerifyChecksumNames[$rcrVerifyChecksumName] = $true
    $rcrVerifyChecksumPath = Get-VerifiedPackagePath $rcrVerifyChecksumName
    if ((Get-FileHash -LiteralPath $rcrVerifyChecksumPath -Algorithm SHA256).Hash -ne $rcrVerifyExpected) { throw "Checksum mismatch: $rcrVerifyChecksumName" }
}
if ($rcrVerifyChecksumNames.Count -ne $rcrVerifyNames.Count + 1 -or -not $rcrVerifyChecksumNames.ContainsKey('package-manifest.json')) { throw 'Checksum inventory does not match manifest.' }
foreach ($rcrVerifyName in $rcrVerifyNames.Keys) { if (-not $rcrVerifyChecksumNames.ContainsKey($rcrVerifyName)) { throw "Missing checksum: $rcrVerifyName" } }
foreach ($rcrVerifyRequired in @('rcr.exe', 'README-WINDOWS.txt', 'LICENSE', 'LICENSE-GPL-3.0', 'THIRD_PARTY.md', 'Start-rcr.ps1', 'Diagnose-rcr.ps1', 'Verify-package.ps1', 'Cargo.lock')) {
    if (-not $rcrVerifyNames.ContainsKey($rcrVerifyRequired)) { throw "Required package entry missing: $rcrVerifyRequired" }
}
Write-Output "Verified $($rcrVerifyManifest.files.Count) packaged files. Hashes establish integrity, not publisher authenticity; builds are unsigned."
