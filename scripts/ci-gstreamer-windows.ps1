# SPDX-License-Identifier: GPL-3.0-only
# Hosted CI only. Official native wheel archives stay in this workspace and
# are not redistributed in RustCarPlay's application archives.
$ErrorActionPreference = 'Stop'
if (-not $env:GITHUB_ACTIONS) { throw 'This helper is intended for GitHub-hosted CI.' }
& (Join-Path $PSScriptRoot 'prepare-gstreamer.ps1') -Version '1.28.7'
python (Join-Path $PSScriptRoot 'gstreamer-import-libs.py')
if ($LASTEXITCODE -ne 0) { throw 'Generating GStreamer import libraries failed.' }
choco install pkgconfiglite --yes --no-progress --limit-output
if ($LASTEXITCODE -ne 0) { throw 'Installing the pkg-config build tool failed.' }
$pkgConfig = Get-Command 'pkg-config.exe' -CommandType Application -ErrorAction Stop
$prefix = Join-Path (Split-Path -Parent $PSScriptRoot) '.local/gstreamer'
Join-Path $prefix 'bin' | Add-Content -LiteralPath $env:GITHUB_PATH -Encoding utf8
@(
    ('PKG_CONFIG=' + $pkgConfig.Source)
    ('PKG_CONFIG_PATH=' + (Join-Path $prefix 'lib/pkgconfig'))
    ('GST_PLUGIN_PATH_1_0=' + (Join-Path $prefix 'lib/gstreamer-1.0'))
    ('GST_PLUGIN_SYSTEM_PATH_1_0=' + (Join-Path $prefix 'lib/gstreamer-1.0'))
    ('GST_REGISTRY_1_0=' + (Join-Path $env:RUNNER_TEMP 'rustcarplay-gst-registry.bin'))
) | Add-Content -LiteralPath $env:GITHUB_ENV -Encoding utf8
