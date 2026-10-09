#Requires -Version 7.0
# SPDX-License-Identifier: GPL-3.0-only
<#
.SYNOPSIS
Prepare the pinned Windows x64 USB user-mode runtime inside this workspace.
.DESCRIPTION
Downloads official MSYS2 packages and checks their pinned SHA-256 hashes. Extracts
DLLs, three diagnostic tools, package metadata and packaged licenses only. Does not
run installers, invoke pacman/hooks, install drivers, change registry or system
PATH, or access an iPhone. Apple Mobile Device Service is a separate prerequisite.
#>
[CmdletBinding()]
param()
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if (-not $IsWindows -or -not [Environment]::Is64BitProcess) {
    throw 'Run this script in 64-bit PowerShell 7 on Windows.'
}
$workspace = [IO.Path]::GetFullPath((Split-Path $PSScriptRoot -Parent))
$destination = Join-Path $workspace '.local/usb-runtime'
$download = Join-Path $destination 'downloads'
$manifestPath = Join-Path $PSScriptRoot 'usb-runtime-packages.json'
$manifest = Get-Content -LiteralPath $manifestPath -Raw -Encoding utf8 | ConvertFrom-Json
if ($manifest.schema -ne 1 -or $manifest.platform -ne 'windows-x86_64-ucrt') {
    throw 'Unsupported USB runtime manifest.'
}
$tar = (Get-Command tar.exe -ErrorAction Stop).Source
New-Item -ItemType Directory -Path $destination,$download -Force | Out-Null
$destinationPrefix = [IO.Path]::GetFullPath($destination) + [IO.Path]::DirectorySeparatorChar
$stage = Join-Path $destination ('staging/' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $stage -Force | Out-Null

function Assert-WithinRuntime([string]$Path) {
    $resolved = [IO.Path]::GetFullPath($Path)
    if (-not $resolved.StartsWith($destinationPrefix, [StringComparison]::OrdinalIgnoreCase)) {
        throw 'USB runtime path escapes workspace destination.'
    }
    return $resolved
}

try {
    $known = @{}
    foreach ($package in $manifest.packages) { $known[$package.name] = $true }
    foreach ($package in $manifest.packages) {
        if ($package.name -notmatch '^mingw-w64-ucrt-x86_64-[a-z0-9+.-]+$' -or
            $package.version -notmatch '^[a-zA-Z0-9.+-]+$' -or
            $package.sha256 -notmatch '^[a-f0-9]{64}$') { throw 'Invalid package manifest entry.' }
        foreach ($dependency in $package.dependencies) {
            if (-not $known.ContainsKey('mingw-w64-ucrt-x86_64-' + $dependency)) {
                throw ('Unpinned runtime dependency: ' + $dependency)
            }
        }
        $file = $package.name + '-' + $package.version + '-any.pkg.tar.zst'
        $archive = Assert-WithinRuntime (Join-Path $download $file)
        if (-not (Test-Path -LiteralPath $archive)) {
            $url = 'https://repo.msys2.org/mingw/ucrt64/' + [Uri]::EscapeDataString($file)
            Write-Output ('Downloading ' + $package.name + ' ' + $package.version)
            $partial = Assert-WithinRuntime ($archive + '.partial')
            Invoke-WebRequest -Uri $url -OutFile $partial -MaximumRetryCount 2 -RetryIntervalSec 2
            if ((Get-FileHash -LiteralPath $partial -Algorithm SHA256).Hash.ToLowerInvariant() -ne $package.sha256) {
                throw ('Archive digest mismatch: ' + $package.name)
            }
            Move-Item -LiteralPath $partial -Destination $archive -Force
        }
        if ((Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant() -ne $package.sha256) {
            throw ('Cached archive digest mismatch: ' + $package.name)
        }
        $entries = @(& $tar -tf $archive)
        if ($LASTEXITCODE -ne 0) { throw ('Cannot list archive: ' + $package.name) }
        $selected = @($entries | Where-Object {
            $_ -match '^ucrt64/bin/[^/]+\.dll$' -or
            $_ -match '^ucrt64/bin/(idevice_id|ideviceinfo|idevicepair)\.exe$' -or
            ($_ -match '^ucrt64/share/licenses/.+[^/]$') -or
            $_ -eq '.PKGINFO'
        })
        foreach ($entry in $selected) {
            if ($entry -match '(^|/)\.\.(/|$)|[\\:]|^[/-]') { throw 'Unsafe archive path.' }
        }
        $packageStage = Assert-WithinRuntime (Join-Path $stage $package.name)
        New-Item -ItemType Directory -Path $packageStage -Force | Out-Null
        if ($selected.Count -gt 0) {
            & $tar -xf $archive -C $packageStage -- @selected
            if ($LASTEXITCODE -ne 0) { throw ('Cannot extract archive: ' + $package.name) }
        }
        foreach ($item in Get-ChildItem -LiteralPath $packageStage -Recurse -Force) {
            if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) { throw 'Archive links are not supported.' }
            if ($item.PSIsContainer) { continue }
            $relative = [IO.Path]::GetRelativePath($packageStage, $item.FullName).Replace('\','/')
            if ($relative.StartsWith('ucrt64/bin/')) {
                $target = Assert-WithinRuntime (Join-Path $stage ('ready/bin/' + $item.Name))
            } else {
                $target = Assert-WithinRuntime (Join-Path $stage ('ready/licenses/' + $package.name + '/' + $relative))
            }
            New-Item -ItemType Directory -Path (Split-Path $target -Parent) -Force | Out-Null
            Copy-Item -LiteralPath $item.FullName -Destination $target -Force
        }
        Write-Output ('Verified ' + $package.name + ' ' + $package.version)
    }
    $ready = Join-Path $stage 'ready'
    $required = @('libimobiledevice-1.0.dll','libusbmuxd-2.0.dll','libplist-2.0.dll',
        'libimobiledevice-glue-1.0.dll','libssl-3-x64.dll','libcrypto-3-x64.dll',
        'idevice_id.exe','ideviceinfo.exe','idevicepair.exe')
    foreach ($name in $required) {
        if (-not (Test-Path -LiteralPath (Join-Path $ready ('bin/' + $name)))) {
            throw ('Required USB runtime file missing: ' + $name)
        }
    }
    foreach ($item in Get-ChildItem -LiteralPath $ready -Recurse -File -Force) {
        $relative = [IO.Path]::GetRelativePath($ready, $item.FullName)
        $target = Assert-WithinRuntime (Join-Path $destination $relative)
        New-Item -ItemType Directory -Path (Split-Path $target -Parent) -Force | Out-Null
        Copy-Item -LiteralPath $item.FullName -Destination $target -Force
    }
    # MSYS2 omits common GNU license text from some binary packages. Keep the
    # canonical texts beside the package-specific notices and source links.
    foreach ($license in $manifest.license_texts) {
        if ($license.name -notmatch '^[a-zA-Z0-9.-]+\.txt$' -or
            $license.sha256 -notmatch '^[a-f0-9]{64}$' -or
            $license.url -notmatch '^https://www\.gnu\.org/licenses/[a-z0-9./-]+\.txt$') {
            throw 'Invalid license manifest entry.'
        }
        $licensePath = Assert-WithinRuntime (Join-Path $destination ('licenses/common/' + $license.name))
        New-Item -ItemType Directory -Path (Split-Path $licensePath -Parent) -Force | Out-Null
        if (-not (Test-Path -LiteralPath $licensePath)) {
            Invoke-WebRequest -Uri $license.url -OutFile $licensePath -MaximumRetryCount 2 -RetryIntervalSec 2
        }
        if ((Get-FileHash -LiteralPath $licensePath -Algorithm SHA256).Hash.ToLowerInvariant() -ne $license.sha256) {
            throw ('License digest mismatch: ' + $license.name)
        }
    }
    Copy-Item -LiteralPath $manifestPath -Destination (Join-Path $destination 'packages.json') -Force
    Write-Output ('USB runtime prepared: ' + (Join-Path $destination 'bin'))
    Write-Output 'Set RUSTCARPLAY_LIBIMOBILEDEVICE_DIR to that bin directory for this process if needed.'
} finally {
    # The stage was created by this invocation and is checked before removal.
    $checkedStage = Assert-WithinRuntime $stage
    if (Test-Path -LiteralPath $checkedStage) { Remove-Item -LiteralPath $checkedStage -Recurse -Force }
}
