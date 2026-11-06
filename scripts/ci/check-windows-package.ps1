# Read-only command checks; no capture, encoding, or graphical window is started.
param(
    [Parameter(Mandatory = $true)][string]$PackageDirectory,
    [Parameter(Mandatory = $true)][string]$ReportDirectory
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$kielboxPackage = (Resolve-Path -LiteralPath $PackageDirectory).ProviderPath
$kielboxReports = [IO.Path]::GetFullPath($ReportDirectory)
if ($kielboxReports.StartsWith($kielboxPackage + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase) -or $kielboxReports -eq $kielboxPackage) {
    throw 'Keep command-check reports outside the package.'
}
& (Join-Path $kielboxPackage 'Verify-package.ps1')
$kielboxManifest = Get-Content -LiteralPath (Join-Path $kielboxPackage 'package-manifest.json') -Raw | ConvertFrom-Json
if ($kielboxManifest.package_kind -ne 'windows' -or $kielboxManifest.debug_shaders_require_checkout) { throw 'Expected a release package with embedded shaders.' }
[IO.Directory]::CreateDirectory($kielboxReports) | Out-Null
$kielboxCheckRoot = Join-Path $kielboxReports ([Guid]::NewGuid().ToString('N'))
[IO.Directory]::CreateDirectory($kielboxCheckRoot) | Out-Null

function Invoke-KielboxPackagedCommand([string]$Executable, [string]$Argument, [string]$Label) {
    $kielboxStdout = Join-Path $kielboxCheckRoot "$Label.stdout.txt"
    $kielboxStderr = Join-Path $kielboxCheckRoot "$Label.stderr.txt"
    $kielboxStart = New-Object System.Diagnostics.ProcessStartInfo
    $kielboxStart.FileName = Join-Path $kielboxPackage $Executable
    # Arguments are fixed switches supplied below, not interpolated user paths.
    $kielboxStart.Arguments = $Argument
    $kielboxStart.WorkingDirectory = $kielboxCheckRoot
    $kielboxStart.UseShellExecute = $false
    $kielboxStart.CreateNoWindow = $true
    $kielboxStart.WindowStyle = [Diagnostics.ProcessWindowStyle]::Hidden
    $kielboxStart.RedirectStandardOutput = $true
    $kielboxStart.RedirectStandardError = $true
    $kielboxProcess = New-Object System.Diagnostics.Process
    $kielboxProcess.StartInfo = $kielboxStart
    try {
        if (-not $kielboxProcess.Start()) { throw "Could not start $Label." }
        $kielboxOutputTask = $kielboxProcess.StandardOutput.ReadToEndAsync()
        $kielboxErrorTask = $kielboxProcess.StandardError.ReadToEndAsync()
        if (-not $kielboxProcess.WaitForExit(30000)) {
            $kielboxProcess.Kill()
            throw "$Label exceeded its 30-second command-check budget."
        }
        $kielboxOutput = $kielboxOutputTask.GetAwaiter().GetResult()
        $kielboxError = $kielboxErrorTask.GetAwaiter().GetResult()
        if ($kielboxOutput.Length -gt 1MB -or $kielboxError.Length -gt 1MB) { throw "$Label produced an oversized report." }
        [IO.File]::WriteAllText($kielboxStdout, $kielboxOutput)
        [IO.File]::WriteAllText($kielboxStderr, $kielboxError)
        [pscustomobject]@{ exit_code = $kielboxProcess.ExitCode; stdout = $kielboxOutput }
    } finally { $kielboxProcess.Dispose() }
}

$kielboxHelp = Invoke-KielboxPackagedCommand 'rcr-clean.exe' '--help' 'cleanup-help'
if ($kielboxHelp.exit_code -ne 0 -or $kielboxHelp.stdout -notmatch 'Usage:') { throw 'Packaged cleanup CLI did not return help.' }
$kielboxDoctor = Invoke-KielboxPackagedCommand 'rcr.exe' '--diagnose' 'diagnose'
# Hosted runners need not have NVIDIA hardware or optional FFmpeg. Exit 1 is a
# valid setup report about missing capabilities, not a failed executable launch.
if ($kielboxDoctor.exit_code -notin @(0, 1)) { throw "Packaged doctor exited $($kielboxDoctor.exit_code)." }
$kielboxReport = $kielboxDoctor.stdout | ConvertFrom-Json
if ($kielboxReport.schema_version -ne 1 -or $kielboxReport.build_profile -ne 'release' -or -not $kielboxReport.app_version -or @($kielboxReport.checks).Count -eq 0) {
    throw 'Packaged doctor returned an invalid report or a development build.'
}
& (Join-Path $kielboxPackage 'Verify-package.ps1')
Write-Output 'PASS: packaged cleanup help and release setup report. Capture, GPU rendering, and recording were not tested.'
