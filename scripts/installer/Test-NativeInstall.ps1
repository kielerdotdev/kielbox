#Requires -Version 7.0
# Opt-in native lifecycle qualification. Leaves its owned evidence folder intact;
# never terminates a process or touches a normal Kielbox installation/profile.
param(
    [Parameter(Mandatory = $true)][string]$PackageDirectory,
    [string]$IsccExecutable = ''
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if (-not $IsWindows) { throw 'Native installer qualification requires Windows and PowerShell 7.' }
$kielboxRepository = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../..'))
$kielboxDownloads = Join-Path $kielboxRepository '.downloads'
$kielboxProofId = [Guid]::NewGuid().ToString('N')
$kielboxRoot = Join-Path $kielboxDownloads "native-install-$kielboxProofId"
$kielboxApp = Join-Path $kielboxRoot 'application'
$kielboxProfile = Join-Path $kielboxRoot 'private profile'
$kielboxData = Join-Path $kielboxRoot 'existing data 日本語'
$kielboxAppId = "Kielbox.Proof.$kielboxProofId"
$kielboxName = "Kielbox Proof $kielboxProofId"
$kielboxRegistryPath = "Software\Microsoft\Windows\CurrentVersion\Uninstall\${kielboxAppId}_is1"
$kielboxPrograms = [Environment]::GetFolderPath([Environment+SpecialFolder]::Programs)
$kielboxShortcut = Join-Path $kielboxPrograms "$kielboxName.lnk"
$kielboxExecutable = Join-Path $kielboxApp 'Kielbox.exe'
$kielboxReceiptPath = Join-Path $kielboxRoot 'native-install-receipt.json'
$kielboxEncoding = [Text.UTF8Encoding]::new($false)
$kielboxOwnsRoot = $false
$kielboxProcessIds = [Collections.Generic.HashSet[int]]::new()
$kielboxReceipt = [ordered]@{ success = $false; root = $kielboxRoot; proof_id = $kielboxProofId; versions = @('0.0.0', '0.1.0-native.1'); native_exe_imports = $null; import_inspection = 'Not performed by this lifecycle test.' }

function Assert-Kielbox([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
}
function Assert-KielboxNoReparse([string]$Path) {
    $ancestor = [IO.Path]::GetFullPath($Path)
    while ($ancestor) {
        if (Test-Path -LiteralPath $ancestor) {
            Assert-Kielbox (-not ((Get-Item -LiteralPath $ancestor -Force).Attributes -band [IO.FileAttributes]::ReparsePoint)) "Refusing reparse path: $ancestor"
        }
        $next = Split-Path -Parent $ancestor
        if ($next -eq $ancestor) { break }
        $ancestor = $next
    }
}
function Assert-KielboxWithin([string]$Path, [string]$Parent) {
    $prefix = [IO.Path]::GetFullPath($Parent).TrimEnd('\') + '\'
    Assert-Kielbox ([IO.Path]::GetFullPath($Path).StartsWith($prefix, [StringComparison]::OrdinalIgnoreCase)) "Path escapes its owned location: $Path"
    Assert-KielboxNoReparse $Path
}
function Normalize-KielboxPath([string]$Path) {
    $value = [IO.Path]::GetFullPath($Path)
    if ($value.StartsWith('\\?\UNC\', [StringComparison]::OrdinalIgnoreCase)) { $value = '\\' + $value.Substring(8) }
    elseif ($value.StartsWith('\\?\')) { $value = $value.Substring(4) }
    return $value.TrimEnd('\')
}
function Write-KielboxJson([string]$Path, $Value) {
    Assert-KielboxWithin $Path $kielboxRoot
    [IO.File]::WriteAllText($Path, (($Value | ConvertTo-Json -Depth 12) + "`n"), $kielboxEncoding)
}
function Read-KielboxRegistration {
    $hive = [Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]::CurrentUser, [Microsoft.Win32.RegistryView]::Registry64)
    try {
        $key = $hive.OpenSubKey($kielboxRegistryPath)
        if ($null -eq $key) { return $null }
        try { return @{ name = $key.GetValue('DisplayName'); version = $key.GetValue('DisplayVersion'); directory = $key.GetValue('InstallLocation'); uninstall = $key.GetValue('UninstallString') } }
        finally { $key.Dispose() }
    } finally { $hive.Dispose() }
}
function Invoke-KielboxChild([string]$File, [string[]]$Arguments, [string]$Label, [int]$TimeoutSeconds = 120) {
    Assert-KielboxWithin (Join-Path $kielboxRoot "$Label.stdout.log") $kielboxRoot
    $info = [Diagnostics.ProcessStartInfo]::new()
    $info.FileName = $File
    foreach ($argument in $Arguments) { $info.ArgumentList.Add($argument) }
    $info.WorkingDirectory = $kielboxRoot
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    $info.WindowStyle = [Diagnostics.ProcessWindowStyle]::Hidden
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    $info.Environment['LOCALAPPDATA'] = $kielboxProfile
    $process = [Diagnostics.Process]::new()
    $process.StartInfo = $info
    $timer = [Diagnostics.Stopwatch]::StartNew()
    try {
        Assert-Kielbox ($process.Start()) "Could not start $Label."
        Write-KielboxJson (Join-Path $kielboxRoot "$Label.process.json") @{ pid = $process.Id; executable = $File; arguments = $Arguments; isolated_localappdata = $kielboxProfile }
        $stdout = $process.StandardOutput.ReadToEndAsync()
        $stderr = $process.StandardError.ReadToEndAsync()
        Assert-Kielbox ($process.WaitForExit($TimeoutSeconds * 1000)) "$Label timed out; no process was killed. Evidence: $kielboxRoot"
        $exitCode = $process.ExitCode
        foreach ($stream in @(@('stdout', $stdout), @('stderr', $stderr))) {
            $remaining = [Math]::Max(1, ($TimeoutSeconds * 1000) - [int]$timer.ElapsedMilliseconds)
            Assert-Kielbox ($stream[1].Wait($remaining)) "$Label descendants still hold its log stream; no process was killed. Evidence: $kielboxRoot"
            [IO.File]::WriteAllText((Join-Path $kielboxRoot "$Label.$($stream[0]).log"), $stream[1].GetAwaiter().GetResult(), $kielboxEncoding)
        }
        Assert-Kielbox ($exitCode -eq 0) "$Label exited $exitCode. See $Label.stderr.log and installer logs in $kielboxRoot"
    } finally { $process.Dispose() }
}
function Read-KielboxStatus([string]$Label, [string]$ExpectedVersion) {
    $output = Join-Path $kielboxRoot "$Label.json"
    Invoke-KielboxChild $kielboxExecutable @('--smoke-installed-status', '--working-directory', $kielboxData, '--proof-output', $output) $Label
    $status = Get-Content -LiteralPath $output -Raw | ConvertFrom-Json
    Assert-Kielbox ($status.success -and $status.managed -and $status.installed_version -eq $ExpectedVersion) "Unexpected installed status for $Label."
    Assert-Kielbox ((Normalize-KielboxPath ([string]$status.data_root)).Equals((Normalize-KielboxPath $kielboxData), [StringComparison]::OrdinalIgnoreCase)) "Data root changed in $Label."
    return $status
}
function Assert-KielboxDataPreserved {
    foreach ($entry in $kielboxSentinels.GetEnumerator()) {
        Assert-Kielbox ((Get-FileHash -LiteralPath $entry.Key -Algorithm SHA256).Hash -eq $entry.Value) "Existing data changed: $($entry.Key)"
    }
    Assert-Kielbox ((Get-FileHash -LiteralPath $kielboxBootstrap -Algorithm SHA256).Hash -eq $kielboxBootstrapHash) 'The saved data-folder selection changed.'
}

try {
    Assert-KielboxWithin $kielboxRoot $kielboxDownloads
    Assert-KielboxWithin $kielboxShortcut ([Environment]::GetFolderPath([Environment+SpecialFolder]::UserProfile))
    Assert-Kielbox (-not (Test-Path -LiteralPath $kielboxRoot)) 'The generated proof folder already exists.'
    Assert-Kielbox ($null -eq (Read-KielboxRegistration)) 'The generated proof registration already exists.'
    Assert-Kielbox (-not (Test-Path -LiteralPath $kielboxShortcut)) 'The generated proof shortcut already exists.'
    $PackageDirectory = (Resolve-Path -LiteralPath $PackageDirectory).Path
    Assert-KielboxNoReparse $PackageDirectory
    [IO.Directory]::CreateDirectory($kielboxDownloads) | Out-Null
    New-Item -ItemType Directory -Path $kielboxRoot -ErrorAction Stop | Out-Null
    $kielboxOwnsRoot = $true
    foreach ($directory in @($kielboxProfile, $kielboxData, (Join-Path $kielboxData 'recordings'), (Join-Path $kielboxData 'projects'))) { [IO.Directory]::CreateDirectory($directory) | Out-Null }
    [IO.File]::WriteAllText((Join-Path $kielboxRoot '.kielbox-installed-test'), "kielbox-installed-test-v1`n", $kielboxEncoding)
    $kielboxSentinels = @{}
    foreach ($relative in @('recordings/keep.bin', 'projects/preferences.json', 'projects/keep.rcr.json')) {
        $path = Join-Path $kielboxData $relative
        $content = if ($relative -eq 'projects/preferences.json') { '{"folder":"recordings","microphone":false}' } else { "Preserve this existing data: $relative`n" }
        [IO.File]::WriteAllText($path, $content, $kielboxEncoding)
        $kielboxSentinels[$path] = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash
    }
    $pwsh = (Get-Process -Id $PID).Path
    foreach ($build in @(@('A', '0.0.0'), @('B', '0.1.0-native.1'))) {
        $arguments = @('-NoProfile', '-File', (Join-Path $kielboxRepository 'scripts/package-installer-windows.ps1'), '-PackageDirectory', $PackageDirectory, '-Version', $build[1], '-ProofId', $kielboxProofId, '-OutputDirectory', (Join-Path $kielboxRoot $build[0]))
        if ($IsccExecutable) { $arguments += @('-IsccExecutable', $IsccExecutable) }
        Invoke-KielboxChild $pwsh $arguments "build-$($build[0])" 300
    }
    $setupA = Join-Path $kielboxRoot 'A/Kielbox-Setup.exe'
    Invoke-KielboxChild $setupA @('/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', '/NOAUTORUN', '/TASKS=', "/DIR=$kielboxApp", "/LOG=$(Join-Path $kielboxRoot 'install-A.log')") 'install-A'
    $registration = Read-KielboxRegistration
    Assert-Kielbox ($null -ne $registration -and $registration.name -in @($kielboxName, "$kielboxName 0.0.0") -and $registration.version -eq '0.0.0') 'Install A did not create its unique HKCU registration.'
    Assert-Kielbox (([string]$registration.directory).TrimEnd('\').Equals($kielboxApp, [StringComparison]::OrdinalIgnoreCase)) 'Uninstall registration points outside the proof application.'
    Assert-Kielbox (Test-Path -LiteralPath $kielboxShortcut -PathType Leaf) 'The real Start-menu shortcut was not created.'
    $shell = New-Object -ComObject WScript.Shell
    $shortcut = $shell.CreateShortcut($kielboxShortcut)
    try { Assert-Kielbox ($shortcut.TargetPath.Equals($kielboxExecutable, [StringComparison]::OrdinalIgnoreCase)) 'Start-menu shortcut points at the wrong executable.' }
    finally { [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($shortcut); [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($shell) }
    $statusA = Read-KielboxStatus 'status-A' '0.0.0'
    $publicOutput = Join-Path $kielboxRoot 'public-check.json'
    Invoke-KielboxChild $kielboxExecutable @('--smoke-installed-check', '--working-directory', $kielboxData, '--proof-output', $publicOutput) 'public-check'
    $publicStatus = Get-Content -LiteralPath $publicOutput -Raw | ConvertFrom-Json
    Assert-Kielbox ($publicStatus.success -and $publicStatus.source -eq 'public release catalog') 'The installed HTTP updater check failed.'
    $kielboxBootstrap = Join-Path $kielboxProfile 'KielboxData/bootstrap.json'
    $kielboxBootstrapHash = (Get-FileHash -LiteralPath $kielboxBootstrap -Algorithm SHA256).Hash
    $feed = Join-Path $kielboxRoot 'B'
    $handoff = Join-Path $kielboxRoot 'handoff.json'
    Invoke-KielboxChild $kielboxExecutable @('--smoke-installed-update', $feed, '--working-directory', $kielboxData, '--proof-output', $handoff) 'update-controller'
    $handoffReport = Get-Content -LiteralPath $handoff -Raw | ConvertFrom-Json
    Assert-Kielbox ($handoffReport.success -and $handoffReport.apply_handoff_started -and $handoffReport.target_version -eq '0.1.0-native.1') 'The real update controller did not start the installer handoff.'
    $cachedSetup = Join-Path $kielboxProfile 'KielboxData/updates/0.1.0-native.1/Kielbox-0.1.0-native.1-Setup.exe'
    $deadline = [DateTime]::UtcNow.AddSeconds(120)
    do {
        $running = $false
        foreach ($process in [Diagnostics.Process]::GetProcessesByName([IO.Path]::GetFileNameWithoutExtension($cachedSetup))) {
            try {
                if ((Normalize-KielboxPath $process.MainModule.FileName).Equals((Normalize-KielboxPath $cachedSetup), [StringComparison]::OrdinalIgnoreCase)) { $running = $true; [void]$kielboxProcessIds.Add($process.Id) }
            } catch { if (-not $process.HasExited) { throw 'Cannot inspect an active installer process; evidence was preserved.' } }
            finally { $process.Dispose() }
        }
        $installedB = $false
        try { $installedB = ((Get-Content -LiteralPath (Join-Path $kielboxApp 'kielbox-install.json') -Raw | ConvertFrom-Json).version -eq '0.1.0-native.1') } catch { }
        if ($installedB -and -not $running) { break }
        Assert-Kielbox ([DateTime]::UtcNow -lt $deadline) "Native update timed out; no process was killed. Evidence: $kielboxRoot"
        Start-Sleep -Milliseconds 200
    } while ($true)
    $statusB = Read-KielboxStatus 'status-B' '0.1.0-native.1'
    $registrationB = Read-KielboxRegistration
    Assert-Kielbox ($null -ne $registrationB -and $registrationB.version -eq '0.1.0-native.1') 'Update B did not update its unique uninstall registration.'
    Assert-KielboxDataPreserved
    $kielboxReceipt.sentinel_sha256 = $kielboxSentinels
    $kielboxReceipt.bootstrap_sha256 = $kielboxBootstrapHash
    $kielboxReceipt.exe_sha256 = (Get-FileHash -LiteralPath $kielboxExecutable -Algorithm SHA256).Hash
    $kielboxReceipt.setup_sha256 = @{ A = (Get-FileHash -LiteralPath $setupA -Algorithm SHA256).Hash; B = (Get-FileHash -LiteralPath (Join-Path $feed 'Kielbox-Setup.exe') -Algorithm SHA256).Hash }
    $kielboxReceipt.installer_pids_observed = @($kielboxProcessIds | ForEach-Object { $_ })
    $kielboxReceipt.registration = $registration
    $kielboxReceipt.registration_B = $registrationB
    $kielboxReceipt.status_A = $statusA
    $kielboxReceipt.public_check = $publicStatus
    $kielboxReceipt.status_B = $statusB
    $uninstaller = Join-Path $kielboxApp 'unins000.exe'
    Assert-KielboxWithin $uninstaller $kielboxRoot
    Invoke-KielboxChild $uninstaller @('/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', "/LOG=$(Join-Path $kielboxRoot 'uninstall.log')") 'uninstall'
    $deadline = [DateTime]::UtcNow.AddSeconds(120)
    while (($null -ne (Read-KielboxRegistration)) -or (Test-Path -LiteralPath $kielboxShortcut) -or (Test-Path -LiteralPath $kielboxExecutable)) {
        Assert-Kielbox ([DateTime]::UtcNow -lt $deadline) 'Uninstall completion timed out; no process was killed.'
        Start-Sleep -Milliseconds 200
    }
    Assert-Kielbox ($null -eq (Read-KielboxRegistration)) 'The unique uninstall registration remains.'
    Assert-Kielbox (-not (Test-Path -LiteralPath $kielboxShortcut)) 'The proof Start-menu shortcut remains.'
    Assert-Kielbox (-not (Test-Path -LiteralPath $kielboxExecutable)) 'The installed application remains after uninstall.'
    Assert-KielboxDataPreserved
    $kielboxReceipt.success = $true
    Write-KielboxJson $kielboxReceiptPath $kielboxReceipt
    Write-Output ($kielboxReceipt | ConvertTo-Json -Depth 12)
} catch {
    $kielboxReceipt.error = $_.Exception.Message
    if ($kielboxOwnsRoot) { Write-KielboxJson $kielboxReceiptPath $kielboxReceipt }
    throw "Native installer qualification failed: $($_.Exception.Message) Evidence retained at $kielboxRoot. No process was killed and no cleanup was attempted."
}
