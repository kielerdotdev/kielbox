# Removes obsolete local application copies, not Cargo caches or captured content.
# Preview: powershell -NoProfile -File scripts/clean-local-builds.ps1 -WhatIf
[CmdletBinding(SupportsShouldProcess = $true)]
param()

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$rcrRepository = [IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
$rcrRepositoryPrefix = $rcrRepository.TrimEnd('\', '/') + [IO.Path]::DirectorySeparatorChar

function Assert-RcrOwnedFile([string]$Path) {
    $rcrAbsolute = [IO.Path]::GetFullPath($Path)
    if (-not $rcrAbsolute.StartsWith($rcrRepositoryPrefix, [StringComparison]::OrdinalIgnoreCase)) {
        throw "Cleanup path is outside the checkout: $rcrAbsolute"
    }
    # Check every existing ancestor before enumerating or deleting. Never follow junctions.
    $rcrAncestor = $rcrAbsolute
    while ($rcrAncestor -and $rcrAncestor -ne $rcrRepository) {
        if (Test-Path -LiteralPath $rcrAncestor) {
            if ((Get-Item -LiteralPath $rcrAncestor -Force).Attributes -band [IO.FileAttributes]::ReparsePoint) {
                throw "Cleanup refuses a reparse point: $rcrAncestor"
            }
        }
        $rcrAncestor = Split-Path -Parent $rcrAncestor
    }
    return $rcrAbsolute
}

$rcrCandidates = [Collections.Generic.List[string]]::new()
# Deliberately explicit: a future package is never classified as obsolete by its date.
$rcrOldDirectories = @(
    'dist/windows-dev',
    'dist/windows-dev-ntfs',
    'dist/windows-reviewed-dev-2026-11-05',
    '.downloads/disk-usage-build',
    '.downloads/native-crates-build',
    '.downloads/autonomous-eval',
    '.downloads/replacement-qa',
    '.downloads/ci-tools/verified-download'
)
foreach ($rcrRelative in $rcrOldDirectories) {
    $rcrDirectory = Assert-RcrOwnedFile (Join-Path $rcrRepository $rcrRelative)
    if (Test-Path -LiteralPath $rcrDirectory -PathType Container) {
        foreach ($rcrFile in Get-ChildItem -LiteralPath $rcrDirectory -File -Force) {
            if ($rcrFile.Name -like 'rcr*.exe' -or $rcrFile.Name -in @('rcr.pdb', 'rcr.d')) {
                $rcrCandidates.Add((Assert-RcrOwnedFile $rcrFile.FullName))
            }
        }
    }
}

function Find-RcrFixtureBinaries([string]$Directory) {
    foreach ($rcrEntry in Get-ChildItem -LiteralPath $Directory -Force) {
        # Packaging tests intentionally contain a junction. Leave it and its target alone.
        if ($rcrEntry.Attributes -band [IO.FileAttributes]::ReparsePoint) { continue }
        $rcrSafePath = Assert-RcrOwnedFile $rcrEntry.FullName
        if ($rcrEntry.PSIsContainer) {
            Find-RcrFixtureBinaries $rcrSafePath
        } elseif ($rcrEntry.Extension -eq '.exe') {
            $rcrCandidates.Add($rcrSafePath)
        }
    }
}
$rcrDownloads = Assert-RcrOwnedFile (Join-Path $rcrRepository '.downloads')
if (Test-Path -LiteralPath $rcrDownloads -PathType Container) {
    foreach ($rcrFixture in Get-ChildItem -LiteralPath $rcrDownloads -Directory -Force) {
        if ($rcrFixture.Name -cmatch '^package-tests-[0-9a-f]{32}$') {
            Find-RcrFixtureBinaries (Assert-RcrOwnedFile $rcrFixture.FullName)
        }
    }
}

# Do not stop an app or remove its backing executable while it is in use.
foreach ($rcrProcess in Get-Process | Where-Object { $_.ProcessName -like 'rcr*' }) {
    $rcrProcessPath = $rcrProcess.Path
    if (-not $rcrProcessPath) { throw "Cannot verify path of running RCR process $($rcrProcess.Id). Quit RCR before cleanup." }
    if ($rcrCandidates -contains $rcrProcessPath) {
        throw "Quit RCR process $($rcrProcess.Id) before removing $rcrProcessPath"
    }
}

$rcrRemovedCount = 0
$rcrRemovedBytes = [long]0
foreach ($rcrCandidate in $rcrCandidates) {
    $rcrSafePath = Assert-RcrOwnedFile $rcrCandidate
    $rcrLength = (Get-Item -LiteralPath $rcrSafePath).Length
    if ($PSCmdlet.ShouldProcess($rcrSafePath, 'Remove obsolete build/test binary')) {
        Remove-Item -LiteralPath $rcrSafePath -Force
        $rcrRemovedCount++
        $rcrRemovedBytes += $rcrLength
    }
}
[pscustomobject]@{
    Candidates = $rcrCandidates.Count
    Removed = $rcrRemovedCount
    BytesFreed = $rcrRemovedBytes
    KeptPackage = 'dist/windows-reviewed-2026-11-05'
}
