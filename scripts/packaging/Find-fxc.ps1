# Read-only discovery, with explicit inputs so fixtures need no installed SDK.
function Find-RcrFxcCompiler {
    param([string]$ExplicitPath = '', [string[]]$PathDirectories = @(), [string[]]$SdkBinRoots = @())
    if ($ExplicitPath) {
        if (-not (Test-Path -LiteralPath $ExplicitPath -PathType Leaf) -or [IO.Path]::GetExtension($ExplicitPath) -ne '.exe') {
            throw "GPUI_FXC_PATH does not name an existing .exe: $ExplicitPath. Correct or remove this explicit override; no fallback was selected."
        }
        return (Resolve-Path -LiteralPath $ExplicitPath).ProviderPath
    }
    foreach ($rcrFxcDirectory in $PathDirectories) {
        if (-not $rcrFxcDirectory) { continue }
        $rcrFxcCandidate = Join-Path $rcrFxcDirectory 'fxc.exe'
        if (Test-Path -LiteralPath $rcrFxcCandidate -PathType Leaf) { return (Resolve-Path -LiteralPath $rcrFxcCandidate).ProviderPath }
    }
    $rcrFxcSdkCandidates = @(
        foreach ($rcrFxcRoot in $SdkBinRoots) {
            if (-not $rcrFxcRoot -or -not (Test-Path -LiteralPath $rcrFxcRoot -PathType Container)) { continue }
            foreach ($rcrFxcVersionDirectory in (Get-ChildItem -LiteralPath $rcrFxcRoot -Directory)) {
                $rcrFxcVersion = $null
                if (-not [Version]::TryParse($rcrFxcVersionDirectory.Name, [ref]$rcrFxcVersion)) { continue }
                $rcrFxcCandidate = Join-Path $rcrFxcVersionDirectory.FullName 'x64/fxc.exe'
                if (Test-Path -LiteralPath $rcrFxcCandidate -PathType Leaf) {
                    [pscustomobject]@{ version = $rcrFxcVersion; path = (Resolve-Path -LiteralPath $rcrFxcCandidate).ProviderPath }
                }
            }
        }
    )
    $rcrFxcNewest = $rcrFxcSdkCandidates | Sort-Object version -Descending | Select-Object -First 1
    if ($null -ne $rcrFxcNewest) { return $rcrFxcNewest.path }
    throw 'Windows shader compiler fxc.exe was not found. Install a Windows SDK with its x64 shader compiler via Visual Studio Build Tools, or set GPUI_FXC_PATH to an existing fxc.exe. Release build was not started.'
}

function Invoke-RcrWithFxc {
    param([Parameter(Mandatory = $true)][string]$Compiler, [Parameter(Mandatory = $true)][scriptblock]$Build)
    $rcrFxcPrevious = [Environment]::GetEnvironmentVariable('GPUI_FXC_PATH', 'Process')
    try {
        [Environment]::SetEnvironmentVariable('GPUI_FXC_PATH', $Compiler, 'Process')
        & $Build
    } finally {
        [Environment]::SetEnvironmentVariable('GPUI_FXC_PATH', $rcrFxcPrevious, 'Process')
    }
}
