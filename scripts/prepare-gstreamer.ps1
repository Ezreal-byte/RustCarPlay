# Download official GStreamer PyPI library archives into this workspace only.
# No installer, package setup.py, registry changes, or system PATH changes.
param([string]$Version = '1.28.7')
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
$download = Join-Path $root '.local/downloads'
$destination = Join-Path $root '.local/gstreamer'
New-Item -ItemType Directory -Path $download,$destination -Force | Out-Null
$prefix = [IO.Path]::GetFullPath($destination) + [IO.Path]::DirectorySeparatorChar
foreach ($package in @('gstreamer-libs','gstreamer-cli','gstreamer-plugins-libs','gstreamer-plugins','gstreamer-plugins-gpl','gstreamer-plugins-restricted','gstreamer-plugins-gpl-restricted','gstreamer-ext-runtime')) {
    $metadata = Invoke-RestMethod -Uri ('https://pypi.org/pypi/' + $package + '/' + $Version + '/json')
    $asset = $metadata.urls | Where-Object filename -Like '*cp39-abi3-win_amd64.whl' | Select-Object -First 1
    if ($null -eq $asset) { $asset = $metadata.urls | Where-Object filename -Like '*py3-none-win_amd64.whl' | Select-Object -First 1 }
    if ($null -eq $asset) { throw ('No compatible official library archive: ' + $package) }
    $archive = Join-Path $download $asset.filename
    if (-not (Test-Path -LiteralPath $archive)) { Invoke-WebRequest -Uri $asset.url -OutFile $archive }
    if ((Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant() -ne $asset.digests.sha256) { throw ('Archive digest mismatch: ' + $package) }
    $zip = [IO.Compression.ZipFile]::OpenRead($archive)
    try {
        foreach ($entry in $zip.Entries) {
            # Read only native library/runtime resources. Never execute or install Python files.
            if ($entry.FullName -notmatch '/(?:purelib|platlib)/[^/]+/(bin/|lib/|libexec/|share/)(.+)$') { continue }
            $relative = $Matches[1] + $Matches[2]
            if ($relative.EndsWith('/')) { continue }
            $target = [IO.Path]::GetFullPath((Join-Path $destination $relative))
            if (-not $target.StartsWith($prefix,[StringComparison]::OrdinalIgnoreCase)) { throw 'Archive traversal rejected' }
            New-Item -ItemType Directory -Path (Split-Path $target -Parent) -Force | Out-Null
            [IO.Compression.ZipFileExtensions]::ExtractToFile($entry,$target,$true)
        }
    } finally { $zip.Dispose() }
    Write-Output ('Prepared ' + $package + ' ' + $Version)
}
Write-Output ('Native archives extracted to ' + $destination)
