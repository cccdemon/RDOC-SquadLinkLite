# Builds subraum.streamDeckPlugin (a zip with the .sdPlugin folder at its root).
# Run from streamdeck-plugin/: powershell -ExecutionPolicy Bypass -File pack.ps1
# Prereq: node_modules inside the plugin folder (npm install --omit=dev there).
$ErrorActionPreference = "Stop"
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$src = Join-Path $here "org.raumdock.subraum.sdPlugin"
$out = Join-Path $here "subraum.streamDeckPlugin"

if (-not (Test-Path (Join-Path $src "node_modules\ws"))) {
    throw "node_modules/ws missing - run 'npm install --omit=dev' inside $src first"
}
if (Test-Path $out) { Remove-Item $out -Force }

# Compress-Archive writes .zip; the .streamDeckPlugin extension is just a rename.
$tmp = "$out.zip"
if (Test-Path $tmp) { Remove-Item $tmp -Force }
Compress-Archive -Path $src -DestinationPath $tmp
Rename-Item $tmp $out
Write-Host "OK: $out"
