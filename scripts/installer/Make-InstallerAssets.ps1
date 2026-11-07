# Deterministic installer artwork. No external fonts, images, or drawing tools.
param([Parameter(Mandatory = $true)][string]$OutputDirectory)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
Add-Type -AssemblyName System.Drawing
$kielboxAssetDirectory = [IO.Path]::GetFullPath($OutputDirectory)
[IO.Directory]::CreateDirectory($kielboxAssetDirectory) | Out-Null

function Draw-KielboxMark([Drawing.Graphics]$Graphics, [single]$X, [single]$Y, [single]$Size) {
    $kielboxScale = $Size / 80
    $kielboxMarkPen = New-Object Drawing.Pen ([Drawing.ColorTranslator]::FromHtml('#B6FF5D')), ([single](4 * $kielboxScale))
    $kielboxMarkPen.LineJoin = [Drawing.Drawing2D.LineJoin]::Round
    $kielboxMarkPen.StartCap = [Drawing.Drawing2D.LineCap]::Round
    $kielboxMarkPen.EndCap = [Drawing.Drawing2D.LineCap]::Round
    try {
        $kielboxPoints = @(
            [Drawing.PointF]::new($X + 40 * $kielboxScale, $Y + 12 * $kielboxScale),
            [Drawing.PointF]::new($X + 65 * $kielboxScale, $Y + 26 * $kielboxScale),
            [Drawing.PointF]::new($X + 65 * $kielboxScale, $Y + 54 * $kielboxScale),
            [Drawing.PointF]::new($X + 40 * $kielboxScale, $Y + 68 * $kielboxScale),
            [Drawing.PointF]::new($X + 15 * $kielboxScale, $Y + 54 * $kielboxScale),
            [Drawing.PointF]::new($X + 15 * $kielboxScale, $Y + 26 * $kielboxScale),
            [Drawing.PointF]::new($X + 40 * $kielboxScale, $Y + 12 * $kielboxScale)
        )
        $Graphics.DrawLines($kielboxMarkPen, [Drawing.PointF[]]$kielboxPoints)
        $Graphics.DrawLine($kielboxMarkPen, $kielboxPoints[1], [Drawing.PointF]::new($X + 40 * $kielboxScale, $Y + 40 * $kielboxScale))
        $Graphics.DrawLine($kielboxMarkPen, $kielboxPoints[5], [Drawing.PointF]::new($X + 40 * $kielboxScale, $Y + 40 * $kielboxScale))
        $Graphics.DrawLine($kielboxMarkPen, $X + 40 * $kielboxScale, $Y + 40 * $kielboxScale, $X + 40 * $kielboxScale, $Y + 68 * $kielboxScale)
    } finally { $kielboxMarkPen.Dispose() }
}

$kielboxIconBitmap = New-Object Drawing.Bitmap 256, 256
$kielboxIconGraphics = [Drawing.Graphics]::FromImage($kielboxIconBitmap)
try {
    $kielboxIconGraphics.SmoothingMode = [Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $kielboxIconGraphics.Clear([Drawing.ColorTranslator]::FromHtml('#151719'))
    Draw-KielboxMark $kielboxIconGraphics 16 16 224
    # A PNG-compressed 256px ICO frame is supported on every target Windows version.
    $kielboxPngStream = New-Object IO.MemoryStream
    try {
        $kielboxIconBitmap.Save($kielboxPngStream, [Drawing.Imaging.ImageFormat]::Png)
        $kielboxPngBytes = $kielboxPngStream.ToArray()
        $kielboxIconStream = [IO.File]::Create((Join-Path $kielboxAssetDirectory 'kielbox.ico'))
        $kielboxIconWriter = New-Object IO.BinaryWriter $kielboxIconStream
        try {
            $kielboxIconWriter.Write([uint16]0)
            $kielboxIconWriter.Write([uint16]1)
            $kielboxIconWriter.Write([uint16]1)
            $kielboxIconWriter.Write([byte]0)
            $kielboxIconWriter.Write([byte]0)
            $kielboxIconWriter.Write([byte]0)
            $kielboxIconWriter.Write([byte]0)
            $kielboxIconWriter.Write([uint16]1)
            $kielboxIconWriter.Write([uint16]32)
            $kielboxIconWriter.Write([uint32]$kielboxPngBytes.Length)
            $kielboxIconWriter.Write([uint32]22)
            $kielboxIconWriter.Write([byte[]]$kielboxPngBytes)
        } finally { $kielboxIconWriter.Dispose(); $kielboxIconStream.Dispose() }
    } finally { $kielboxPngStream.Dispose() }
} finally { $kielboxIconGraphics.Dispose(); $kielboxIconBitmap.Dispose() }

$kielboxSplash = New-Object Drawing.Bitmap 164, 314
$kielboxSplashGraphics = [Drawing.Graphics]::FromImage($kielboxSplash)
$kielboxTitleFont = New-Object Drawing.Font 'Segoe UI', 30, ([Drawing.FontStyle]::Bold), ([Drawing.GraphicsUnit]::Pixel)
$kielboxDetailFont = New-Object Drawing.Font 'Segoe UI', 16, ([Drawing.FontStyle]::Regular), ([Drawing.GraphicsUnit]::Pixel)
$kielboxWhite = New-Object Drawing.SolidBrush ([Drawing.ColorTranslator]::FromHtml('#F3F4F5'))
$kielboxMuted = New-Object Drawing.SolidBrush ([Drawing.ColorTranslator]::FromHtml('#A2A8AD'))
try {
    $kielboxSplashGraphics.SmoothingMode = [Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $kielboxSplashGraphics.TextRenderingHint = [Drawing.Text.TextRenderingHint]::AntiAliasGridFit
    $kielboxSplashGraphics.Clear([Drawing.ColorTranslator]::FromHtml('#151719'))
    Draw-KielboxMark $kielboxSplashGraphics 22 68 120
    $kielboxSplashGraphics.DrawString('Kielbox', $kielboxTitleFont, $kielboxWhite, [single]24, [single]210)
    $kielboxSplash.Save((Join-Path $kielboxAssetDirectory 'wizard.bmp'), [Drawing.Imaging.ImageFormat]::Bmp)
    $kielboxSmall = New-Object Drawing.Bitmap 55, 55
    $kielboxSmallGraphics = [Drawing.Graphics]::FromImage($kielboxSmall)
    try {
        $kielboxSmallGraphics.SmoothingMode = [Drawing.Drawing2D.SmoothingMode]::AntiAlias
        $kielboxSmallGraphics.Clear([Drawing.ColorTranslator]::FromHtml('#151719'))
        Draw-KielboxMark $kielboxSmallGraphics 0 0 55
        $kielboxSmall.Save((Join-Path $kielboxAssetDirectory 'wizard-small.bmp'), [Drawing.Imaging.ImageFormat]::Bmp)
    } finally { $kielboxSmallGraphics.Dispose(); $kielboxSmall.Dispose() }
} finally {
    $kielboxSplashGraphics.Dispose(); $kielboxSplash.Dispose()
    $kielboxTitleFont.Dispose(); $kielboxDetailFont.Dispose(); $kielboxWhite.Dispose(); $kielboxMuted.Dispose()
}
