# Run the production cache module's tests without Windows, GPUI, or Cargo dependencies.
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repository = Split-Path -Parent $PSScriptRoot
$source = Join-Path $repository 'vendor/gpui-0.2.2/src/platform/windows/directx_renderer/bytecode_cache.rs'
$work = Join-Path $repository ('.downloads/shader-bytecode-cache-' + [Guid]::NewGuid().ToString('N'))
[IO.Directory]::CreateDirectory($work) | Out-Null
$binary = Join-Path $work 'bytecode_cache_tests.exe'

& rustc --edition=2024 --test $source -o $binary
if ($LASTEXITCODE -ne 0) { throw "Shader cache test compilation failed with exit code $LASTEXITCODE." }
& $binary --nocapture
if ($LASTEXITCODE -ne 0) { throw "Shader cache regressions failed with exit code $LASTEXITCODE." }
