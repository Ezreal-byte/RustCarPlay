#!/usr/bin/env bash
# Official universal framework packages; no Homebrew/Intel bottle dependency.
set -euo pipefail
version=1.28.7
base="https://gstreamer.freedesktop.org/data/pkg/osx/$version"
download="${RUNNER_TEMP:?}/rustcarplay-gstreamer"
mkdir -p "$download"
for kind in runtime devel; do
  if [[ "$kind" == runtime ]]; then
    file="gstreamer-1.0-$version-universal.pkg"
    expected=529fdf4a4027d942e59b5b3564f6400adaa008f63ce5f3fed4ffe35d73911994
  else
    file="gstreamer-1.0-devel-$version-universal.pkg"
    expected=72a44870cf02472cbf6e9a84bcc25ee6807dd1c26659a112066373544d365e7a
  fi
  curl --fail --location --retry 3 "$base/$file" --output "$download/$file"
  printf '%s  %s\n' "$expected" "$download/$file" | shasum -a 256 --check
  sudo installer -pkg "$download/$file" -target /
done
prefix=/Library/Frameworks/GStreamer.framework/Versions/1.0
if [[ ! -x "$prefix/bin/pkg-config" ]] && ! command -v pkg-config >/dev/null; then
  brew install pkgconf
fi
echo "$prefix/bin" >> "${GITHUB_PATH:?}"
echo "PKG_CONFIG_PATH=$prefix/lib/pkgconfig" >> "${GITHUB_ENV:?}"
echo "DYLD_FALLBACK_LIBRARY_PATH=$prefix/lib" >> "$GITHUB_ENV"
echo "GST_PLUGIN_SYSTEM_PATH_1_0=$prefix/lib/gstreamer-1.0" >> "$GITHUB_ENV"
