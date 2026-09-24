$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.IO.Compression
Add-Type -AssemblyName System.IO.Compression.FileSystem

$root = $PSScriptRoot
$version = '0.3.0'
$releaseDir = Join-Path $root 'release'
New-Item -ItemType Directory -Force -Path $releaseDir | Out-Null
$zipPath = Join-Path $releaseDir "JIZURA-AviUtl2-v$version.zip"
if (Test-Path -LiteralPath $zipPath) { Remove-Item -LiteralPath $zipPath -Force }

$files = @(
    @('dist/JIZURA.aux2', 'JIZURA.aux2'),
    @('dist/web/editor.html', 'web/editor.html'),
    @('dist/web/render.html', 'web/render.html'),
    @('dist/web/engine.js', 'web/engine.js'),
    @('dist/web/bridge.js', 'web/bridge.js'),
    @('README.md', 'README.md'),
    @('LICENSE', 'LICENSE'),
    @('dist/LICENSE-JIZURA.txt', 'licenses/LICENSE-JIZURA.txt'),
    @('dist/LICENSE-aviutl2-rs.txt', 'licenses/LICENSE-aviutl2-rs.txt'),
    @('dist/LICENSE-AviUtl2-SDK.txt', 'licenses/LICENSE-AviUtl2-SDK.txt'),
    @('dist/LICENSE-WebView2.txt', 'licenses/LICENSE-WebView2.txt'),
    @('dist/LICENSE.mp4-muxer.txt', 'licenses/LICENSE.mp4-muxer.txt'),
    @('dist/THIRD_PARTY_NOTICES.md', 'licenses/THIRD_PARTY_NOTICES.md')
)

$zip = [IO.Compression.ZipFile]::Open($zipPath, [IO.Compression.ZipArchiveMode]::Create)
try {
    foreach ($file in $files) {
        $source = Join-Path $root $file[0]
        if (-not (Test-Path -LiteralPath $source)) { throw "Missing release file: $source" }
        [IO.Compression.ZipFileExtensions]::CreateEntryFromFile($zip, $source, $file[1]) | Out-Null
    }
} finally { $zip.Dispose() }
Write-Output $zipPath
