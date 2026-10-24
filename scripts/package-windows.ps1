param(
    # Explicitly stage an existing development build instead of building a release.
    [string]$DevelopmentExecutable = '',
    [switch]$SkipBuild
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$rcrPackageRoot = [IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
if ($DevelopmentExecutable) {
    $rcrPackageSource = if ([IO.Path]::IsPathRooted($DevelopmentExecutable)) {
        $DevelopmentExecutable
    } else { Join-Path $rcrPackageRoot $DevelopmentExecutable }
    $rcrPackageKind = 'windows-dev'
} else {
    if (-not $SkipBuild) {
        Push-Location $rcrPackageRoot
        try {
            & cargo build --release -p rcr-desktop -j1
            if ($LASTEXITCODE -ne 0) { throw "Release build failed with exit code $LASTEXITCODE" }
        } finally { Pop-Location }
    }
    $rcrPackageSource = Join-Path $rcrPackageRoot 'target/release/rcr-desktop.exe'
    $rcrPackageKind = 'windows'
}
$rcrPackageSource = (Resolve-Path -LiteralPath $rcrPackageSource).Path
if ([IO.Path]::GetExtension($rcrPackageSource) -ne '.exe') { throw 'Expected a built Windows executable.' }
$rcrPackageFolder = Join-Path $rcrPackageRoot ('dist/' + $rcrPackageKind)
if (Test-Path -LiteralPath $rcrPackageFolder) {
    if ((Get-Item -LiteralPath $rcrPackageFolder).Attributes -band [IO.FileAttributes]::ReparsePoint) {
        throw 'Package destination must not be a reparse point.'
    }
} else { [IO.Directory]::CreateDirectory($rcrPackageFolder) | Out-Null }
$rcrPackageOutput = Join-Path $rcrPackageFolder 'rcr.exe'
# Never clean Cargo output, recordings or existing user data to make a package.
Copy-Item -LiteralPath $rcrPackageSource -Destination $rcrPackageOutput -Force
$rcrPackageSourceHash = (Get-FileHash -LiteralPath $rcrPackageSource -Algorithm SHA256).Hash
if ((Get-FileHash -LiteralPath $rcrPackageOutput -Algorithm SHA256).Hash -ne $rcrPackageSourceHash) {
    throw 'Packaged executable hash does not match its source.'
}
Write-Output "Packaged application: $rcrPackageOutput"
Write-Output "SHA256: $rcrPackageSourceHash"
if ($DevelopmentExecutable) {
    Write-Output 'Development staging only: debug GPUI shaders still require this source checkout.'
}
Write-Output 'Video editing uses external FFmpeg/FFprobe and shared FFmpeg libraries; FFplay is only used by legacy examples/proofs.'
Write-Output 'Settings, project data, recordings and caches remain separate writable data; they are not compiler dependencies.'
