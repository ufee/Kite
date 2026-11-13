# Packs a release build into the files that go to GitHub Releases:
#   Kite-v<version>-setup.exe     NSIS installer built by `tauri build`
#   Kite-v<version>-portable.zip  Kite\Kite.exe with a data\ folder, which turns portable mode on
#   SHA256SUMS.txt
# Usage: scripts/package.ps1 [-Out release]   (run from the repository root after `npx tauri build`)
param([string]$Out = 'release')

$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
$version = (Get-Content (Join-Path $root 'src-tauri/tauri.conf.json') -Raw | ConvertFrom-Json).version
$target = Join-Path $root 'src-tauri/target/release'
$out = Join-Path $root $Out

Remove-Item $out -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory $out | Out-Null

$setup = Join-Path $out "Kite-v$version-setup.exe"
Copy-Item (Join-Path $target "bundle/nsis/Kite_${version}_x64-setup.exe") $setup

# The archive holds one folder, so unpacking it never spills files next to others.
$stage = Join-Path ([IO.Path]::GetTempPath()) "kite-portable-$PID"
$app = Join-Path $stage 'Kite'
New-Item -ItemType Directory (Join-Path $app 'data') -Force | Out-Null
Copy-Item (Join-Path $target 'kite.exe') (Join-Path $app 'Kite.exe')
Copy-Item (Join-Path $root 'LICENSE') (Join-Path $app 'LICENSE.txt')
Copy-Item (Join-Path $root 'THIRD_PARTY_LICENSES.txt') (Join-Path $app 'THIRD_PARTY_LICENSES.txt')
$zip = Join-Path $out "Kite-v$version-portable.zip"
Add-Type -AssemblyName System.IO.Compression.FileSystem
# CreateFromDirectory keeps the empty data\ folder; Compress-Archive would drop it.
[IO.Compression.ZipFile]::CreateFromDirectory($stage, $zip, [IO.Compression.CompressionLevel]::Optimal, $false)
Remove-Item $stage -Recurse -Force

$sums = Get-ChildItem $out -File | ForEach-Object { '{0}  {1}' -f (Get-FileHash $_.FullName -Algorithm SHA256).Hash.ToLower(), $_.Name }
[IO.File]::WriteAllText((Join-Path $out 'SHA256SUMS.txt'), ($sums -join "`n") + "`n")
Get-ChildItem $out -File | ForEach-Object { '{0,-36} {1,10} bytes' -f $_.Name, $_.Length }
