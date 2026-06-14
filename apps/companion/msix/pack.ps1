<#
.SYNOPSIS
  Build + package RDOC SquadLink Lite as an MSIX for the Microsoft Store (Weg B).

.DESCRIPTION
  One command: builds the Tauri release exe, generates the MSIX visual assets from
  the app logo, stages the payload, and packs it with makeappx. Optionally self-signs
  for local install testing (the Store re-signs on submission -- do NOT ship the
  self-signed package).

  Fill the placeholders in AppxManifest.xml (Identity Name + Publisher from Partner
  Center) BEFORE running. The script reads Publisher/Version back from the manifest.

.PARAMETER NoBuild
  Skip 'pnpm tauri build' and reuse the existing release exe.

.PARAMETER SelfSign
  Create/reuse a self-signed cert matching the manifest Publisher and sign the .msix,
  so it can be installed locally (Add-AppxPackage). Not for Store upload.

.EXAMPLE
  powershell -File apps/companion/msix/pack.ps1 -SelfSign
#>
[CmdletBinding()]
param(
  [switch]$NoBuild,
  [switch]$SelfSign
)

$ErrorActionPreference = "Stop"
$msixDir   = $PSScriptRoot                                  # apps/companion/msix
$appDir    = Split-Path $msixDir -Parent                    # apps/companion
$manifest  = Join-Path $msixDir "AppxManifest.xml"
$logo      = Join-Path $appDir  "src\Squad_Link_Lite.png"
$exe       = Join-Path $appDir  "src-tauri\target\release\rdoc-squadlink-lite.exe"
$stage     = Join-Path $msixDir "staging"
$assets    = Join-Path $stage   "Assets"

# Read identity back from the manifest (single source of truth).
[xml]$m = Get-Content $manifest
$publisher = $m.Package.Identity.Publisher
$version   = $m.Package.Identity.Version
if ($publisher -match "REPLACE") {
  throw "AppxManifest.xml still has placeholders. Fill Identity Name + Publisher from Partner Center first."
}
$out = Join-Path $msixDir "RDOCSquadLinkLite_${version}_x64.msix"
Write-Host "Packaging $version  ($publisher)" -ForegroundColor Cyan

# Locate Windows SDK tools (latest).
function Find-SdkTool([string]$name, [string]$arch) {
  $root = "${env:ProgramFiles(x86)}\Windows Kits\10\bin"
  $hit = Get-ChildItem $root -Recurse -Filter $name -ErrorAction SilentlyContinue |
         Where-Object { $_.FullName -match "\\$arch\\" } |
         Sort-Object FullName -Descending | Select-Object -First 1
  if (-not $hit) { throw ($name + " not found. Install the Windows 10/11 SDK.") }
  return $hit.FullName
}
$makeappx = Find-SdkTool "makeappx.exe" "x64"

# 1. Build the release exe.
if (-not $NoBuild) {
  Push-Location $appDir
  try { pnpm tauri build } finally { Pop-Location }
}
if (-not (Test-Path $exe)) { throw ("Release exe missing: " + $exe + " (run without -NoBuild)") }

# 2. Stage payload.
if (Test-Path $stage) { Remove-Item $stage -Recurse -Force }
New-Item -ItemType Directory $stage, $assets | Out-Null
Copy-Item $manifest (Join-Path $stage "AppxManifest.xml")
Copy-Item $exe      $stage
# WebView2Loader.dll only if the build emitted one beside the exe.
$loader = Join-Path (Split-Path $exe) "WebView2Loader.dll"
if (Test-Path $loader) { Copy-Item $loader $stage }

# 3. Generate the visual assets from the logo (System.Drawing, no extra deps).
Add-Type -AssemblyName System.Drawing
$sizes = @{
  "StoreLogo.png"         = @(50, 50)
  "Square44x44Logo.png"   = @(44, 44)
  "Square150x150Logo.png" = @(150, 150)
  "Wide310x150Logo.png"   = @(310, 150)
}
$src = [System.Drawing.Image]::FromFile($logo)
foreach ($name in $sizes.Keys) {
  $w, $h = $sizes[$name]
  $bmp = New-Object System.Drawing.Bitmap $w, $h
  $g = [System.Drawing.Graphics]::FromImage($bmp)
  $g.InterpolationMode = "HighQualityBicubic"
  $g.Clear([System.Drawing.Color]::Transparent)
  # Contain the (square) logo centered inside the target rect.
  $scale = [Math]::Min($w / $src.Width, $h / $src.Height)
  $dw = [int]($src.Width * $scale); $dh = [int]($src.Height * $scale)
  $g.DrawImage($src, [int](($w - $dw) / 2), [int](($h - $dh) / 2), $dw, $dh)
  $bmp.Save((Join-Path $assets $name), [System.Drawing.Imaging.ImageFormat]::Png)
  $g.Dispose(); $bmp.Dispose()
}
$src.Dispose()

# 4. Pack.
if (Test-Path $out) { Remove-Item $out -Force }
& $makeappx pack /d $stage /p $out /o
if ($LASTEXITCODE -ne 0) { throw ("makeappx failed (" + $LASTEXITCODE + ")") }
Write-Host "Packed: $out" -ForegroundColor Green

# 5. Optional self-sign for local install testing.
if ($SelfSign) {
  $signtool = Find-SdkTool "signtool.exe" "x64"
  $cert = Get-ChildItem Cert:\CurrentUser\My | Where-Object { $_.Subject -eq $publisher } | Select-Object -First 1
  if (-not $cert) {
    Write-Host "Creating self-signed test cert for $publisher" -ForegroundColor Yellow
    $cert = New-SelfSignedCertificate -Type Custom -Subject $publisher -KeyUsage DigitalSignature `
      -FriendlyName "SquadLink MSIX test" -CertStoreLocation "Cert:\CurrentUser\My" `
      -TextExtension @("2.5.29.37={text}1.3.6.1.5.5.7.3.3", "2.5.29.19={text}")
  }
  & $signtool sign /fd SHA256 /a /sha1 $cert.Thumbprint $out
  if ($LASTEXITCODE -ne 0) { throw ("signtool failed (" + $LASTEXITCODE + ")") }
  Write-Host "Self-signed. To install: trust the cert in LocalMachine\TrustedPeople, then Add-AppxPackage '$out'" -ForegroundColor Green
  Write-Host "NOTE: upload the UNSIGNED package to the Store (re-run without -SelfSign)." -ForegroundColor Yellow
}
