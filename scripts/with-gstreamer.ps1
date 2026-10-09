# Process-local SDK environment. Pass arguments as an explicit array so PowerShell
# does not interpret cargo's -p as its own -ProgressAction / -PipelineVariable.
# Example: ./scripts/with-gstreamer.ps1 -Command @('cargo','test','-p','carplay-media','--features','gstreamer','--','--include-ignored')
param([Parameter(ValueFromRemainingArguments=$true)][string[]]$Command)
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
$gst = Join-Path $root '.local/gstreamer'
if (-not (Test-Path -LiteralPath (Join-Path $gst 'bin/gstreamer-1.0-0.dll'))) { throw 'Use RustCarPlay.exe in a portable release, or run scripts/prepare-gstreamer.ps1 for a source checkout.' }
$env:PATH = (Join-Path $gst 'bin') + [IO.Path]::PathSeparator + $env:PATH
$env:PKG_CONFIG_PATH = Join-Path $gst 'lib/pkgconfig'
if ($IsWindows -and [string]::IsNullOrWhiteSpace($env:PKG_CONFIG)) {
    # std::process::Command does not use PowerShell's PATHEXT resolution for .bat.
    # Keep an explicitly configured PKG_CONFIG; otherwise pass an absolute path.
    $pkgConfigTool = Get-Command 'pkg-config.exe','pkg-config.bat','pkgconf.exe' -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($null -ne $pkgConfigTool) {
        $env:PKG_CONFIG = $pkgConfigTool.Source
    } else {
        $strawberryPkgConfig = Join-Path $env:SystemDrive 'Strawberry/perl/bin/pkg-config.bat'
        if (Test-Path -LiteralPath $strawberryPkgConfig) { $env:PKG_CONFIG = $strawberryPkgConfig }
    }
}
$env:GST_PLUGIN_PATH_1_0 = Join-Path $gst 'lib/gstreamer-1.0'
$env:GST_PLUGIN_SYSTEM_PATH_1_0 = $env:GST_PLUGIN_PATH_1_0
$env:GST_REGISTRY_1_0 = Join-Path $root '.local/gst-registry.bin'
# USB runtimes are loaded by absolute path with a restricted DLL search; do not
# mix their OpenSSL or compiler DLLs into GStreamer's PATH.
if (-not $env:RUSTCARPLAY_LIBIMOBILEDEVICE_DIR -and (Test-Path -LiteralPath (Join-Path $root '.local/usb-runtime/bin/libimobiledevice-1.0.dll'))) {
    $env:RUSTCARPLAY_LIBIMOBILEDEVICE_DIR = Join-Path $root '.local/usb-runtime/bin'
}
if (-not $env:RUSTCARPLAY_USB_FILTER_DIR -and (Test-Path -LiteralPath (Join-Path $root '.local/downloads/libusb-win32-verified/libusb-win32-bin-1.4.0.2/bin/amd64/libusb0.dll'))) {
    $env:RUSTCARPLAY_USB_FILTER_DIR = Join-Path $root '.local/downloads/libusb-win32-verified/libusb-win32-bin-1.4.0.2/bin/amd64'
}
if ($Command.Length -eq 0) { throw "Pass an argument array, for example: -Command @('cargo','test','-p','carplay-media','--features','gstreamer')" }
& $Command[0] @($Command | Select-Object -Skip 1)
exit $LASTEXITCODE
