param([switch]$SkipBuild, [string]$GpuAdapter = '')
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$runeRoot = Split-Path -Parent $PSScriptRoot
$runeProof = Join-Path $runeRoot ('.downloads/rune-proof-' + [Guid]::NewGuid().ToString('N'))
[IO.Directory]::CreateDirectory($runeProof) | Out-Null
Push-Location $runeRoot
try {
    if (-not $SkipBuild) {
        & cargo build --locked -p desktop -j1
        if ($LASTEXITCODE -ne 0) { throw 'Rune desktop build failed.' }
    }
    $runeExe = Join-Path $runeRoot 'target/debug/desktop.exe'
    foreach ($runeScene in @('library', 'capture', 'editor', 'disk', 'tools', 'settings')) {
        & $runeExe --scene $runeScene --snap (Join-Path $runeProof "$runeScene.png") --tree (Join-Path $runeProof "$runeScene.json")
        if ($LASTEXITCODE -ne 0) { throw "Rune scene failed: $runeScene" }
        $runeTree = Get-Content -LiteralPath (Join-Path $runeProof "$runeScene.json") -Raw | ConvertFrom-Json
        if (@($runeTree.elements).Count -lt 10) { throw "Rune scene has no usable element tree: $runeScene" }
    }
    if ($GpuAdapter) {
        $runePreviousAdapter = [Environment]::GetEnvironmentVariable('WGPU_ADAPTER_NAME', 'Process')
        try {
            [Environment]::SetEnvironmentVariable('WGPU_ADAPTER_NAME', $GpuAdapter, 'Process')
            $runeOutput = & $runeExe --scene gpu-fixture --at 1 --snap (Join-Path $runeProof 'gpu.png') --tree (Join-Path $runeProof 'gpu.json') 2>&1
            if ($LASTEXITCODE -ne 0) { throw 'Generated GPU fixture failed.' }
            $runeOutput | Set-Content -LiteralPath (Join-Path $runeProof 'gpu.log')
            if (($runeOutput -join "`n") -notmatch 'native surface proof: \([1-9][0-9]*, [1-9][0-9]*\); error=None') {
                throw "Shared GPU preview did not complete. Choose the adapter used by the D3D11 producer. See $runeProof/gpu.log"
            }
            Add-Type -AssemblyName System.Drawing
            $runeTree = Get-Content -LiteralPath (Join-Path $runeProof 'gpu.json') -Raw | ConvertFrom-Json
            $runePreview = @($runeTree.elements | Where-Object { $_.PSObject.Properties['key'] -and $_.key -eq 'capture-preview' })[0]
            $runeRect = $runePreview.rect
            $runeSide = [Math]::Min($runeRect[2], $runeRect[3])
            $runeBitmap = [Drawing.Bitmap]::new((Join-Path $runeProof 'gpu.png'))
            $runeGreen = 0; $runePurple = 0
            try {
                foreach ($runeRow in 0..7) { foreach ($runeColumn in 0..7) {
                    $runeX = [int](($runeRect[0] + ($runeRect[2] - $runeSide) / 2 + $runeSide * ($runeColumn + 0.5) / 8) * $runeTree.scale)
                    $runeY = [int](($runeRect[1] + ($runeRect[3] - $runeSide) / 2 + $runeSide * ($runeRow + 0.5) / 8) * $runeTree.scale)
                    $runePixel = $runeBitmap.GetPixel($runeX, $runeY)
                    if ($runePixel.G -gt 150 -and $runePixel.R -lt 100 -and $runePixel.B -lt 100) { $runeGreen++ }
                    if ($runePixel.R -gt 140 -and $runePixel.B -gt 150 -and $runePixel.G -lt 100) { $runePurple++ }
                } }
            } finally { $runeBitmap.Dispose() }
            if ($runeGreen -lt 16 -or $runePurple -lt 16) { throw 'GPU preview pixels do not match the generated checkerboard.' }
        } finally { [Environment]::SetEnvironmentVariable('WGPU_ADAPTER_NAME', $runePreviousAdapter, 'Process') }
    }
    Write-Output "PASS: Rune scenes and requested GPU proof. Evidence: $runeProof"
} finally { Pop-Location }
