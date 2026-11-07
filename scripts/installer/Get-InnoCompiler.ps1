# Portable, checksum-pinned build tool. No global installation or file association.
function Get-KielboxInnoCompiler {
    param([Parameter(Mandatory = $true)][string]$RepositoryRoot)
    $innoVersion = '6.7.4'
    $innoSha256 = 'a549929319f9120716677c685535ca4671e4b937b5fdc87e2585f32d8eb30e8c'
    $innoRoot = Join-Path $RepositoryRoot ".downloads/inno-setup/$innoVersion"
    $innoDistribution = Join-Path $innoRoot "innosetup-$innoVersion.exe"
    $innoCompilerRoot = Join-Path $innoRoot 'compiler'
    $innoCompiler = Join-Path $innoCompilerRoot 'ISCC.exe'
    Assert-KielboxNoReparse $innoRoot
    [IO.Directory]::CreateDirectory($innoRoot) | Out-Null
    if (-not (Test-Path -LiteralPath $innoDistribution -PathType Leaf)) {
        $innoPending = Join-Path $innoRoot ('download-' + [Guid]::NewGuid().ToString('N') + '.exe')
        Invoke-WebRequest -UseBasicParsing -Uri "https://github.com/jrsoftware/issrc/releases/download/is-6_7_4/innosetup-$innoVersion.exe" -OutFile $innoPending
        if ((Get-FileHash -LiteralPath $innoPending -Algorithm SHA256).Hash -ne $innoSha256) { throw 'Inno Setup download does not match the pinned official release checksum.' }
        [IO.File]::Move($innoPending, $innoDistribution)
    }
    Assert-KielboxNoReparse $innoDistribution
    if ((Get-FileHash -LiteralPath $innoDistribution -Algorithm SHA256).Hash -ne $innoSha256) { throw 'Cached Inno Setup distribution failed its pinned checksum.' }
    if (-not (Test-Path -LiteralPath $innoCompiler -PathType Leaf)) {
        $innoSignature = Get-AuthenticodeSignature -LiteralPath $innoDistribution
        if ($innoSignature.Status -ne 'Valid' -or $innoSignature.SignerCertificate.Subject -notmatch '(^|,\s*)CN=Pyrsys B[.]V[.]') { throw 'The official Inno Setup distribution has no valid expected publisher signature.' }
        # /PORTABLE=1 is implemented by upstream isportable.iss: no uninstall
        # registration, shortcuts or file associations; /CURRENTUSER avoids UAC.
        $innoProcess = Start-Process -FilePath $innoDistribution -ArgumentList @('/PORTABLE=1', '/CURRENTUSER', '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', ('/DIR="' + $innoCompilerRoot + '"')) -WindowStyle Hidden -PassThru -Wait
        if ($innoProcess.ExitCode -ne 0) { throw "Portable Inno compiler extraction failed: $($innoProcess.ExitCode)" }
    }
    Assert-KielboxNoReparse $innoCompiler
    Assert-KielboxInnoCompiler $innoCompiler
    return $innoCompiler
}

function Assert-KielboxInnoCompiler([string]$Executable) {
    if ([IO.Path]::GetFileName($Executable) -ne 'ISCC.exe') { throw 'The compiler executable must be the pinned ISCC.exe.' }
    # ISCC's PE version resource is 0.0.0.0; pin the extracted compiler/engine
    # bytes from the checksum- and Authenticode-verified official distribution.
    $innoExpected = @{
        'ISCC.exe' = 'f02fa0e5f237b47d7ed1edae9b991866ec94fe02af61eb6a73e94f92fe8e0604'
        'ISCmplr.dll' = '236555054ca65194c69544b4ef1d49ea345259745f5c4a2005aca7b5674e7cff'
    }
    foreach ($innoName in $innoExpected.Keys) {
        $innoFile = Join-Path (Split-Path -Parent $Executable) $innoName
        Assert-KielboxNoReparse $innoFile
        if (-not (Test-Path -LiteralPath $innoFile -PathType Leaf) -or (Get-FileHash -LiteralPath $innoFile -Algorithm SHA256).Hash -ne $innoExpected[$innoName]) { throw 'Expected the exact pinned Inno Setup 6.7.4 compiler and engine.' }
    }
}
