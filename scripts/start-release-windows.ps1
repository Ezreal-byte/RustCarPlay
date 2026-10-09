# Launch the downloaded application with its locally prepared native libraries.
param([switch]$Cli, [string[]]$Arguments = @())
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$launcher = Join-Path $root 'RustCarPlay.exe'
if (Test-Path -LiteralPath $launcher) {
    if ($Cli) { & $launcher --cli @Arguments } else { & $launcher @Arguments }
    exit $LASTEXITCODE
}
$application = if ($Cli) { 'rustcarplay.exe' } else { 'carplay-desktop.exe' }
$executable = Join-Path $root $application
if (-not (Test-Path -LiteralPath $executable)) { throw 'Extract the complete Windows release archive before launching.' }
Push-Location -LiteralPath $root
try {
    & (Join-Path $PSScriptRoot 'with-gstreamer.ps1') -Command (@($executable) + $Arguments)
    $result = $LASTEXITCODE
} finally {
    Pop-Location
}
exit $result
