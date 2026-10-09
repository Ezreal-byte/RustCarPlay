#!/usr/bin/env bash
set -euo pipefail
tag="${RELEASE_TAG:?}"
version="${tag#v}"
notes="docs/releases/$tag.md"
# Upload failures stay drafts. A retry may complete a draft but must not replace
# a public release. The workflow only reaches here after all builds succeeded.
existing="$(gh release list --limit 100 --json tagName,isDraft --jq ".[] | select(.tagName == \"$tag\") | .isDraft")"
if [[ -n "$existing" && "$existing" != true ]]; then
  echo "Release $tag is already public; refusing to replace it." >&2
  exit 1
fi
if [[ -z "$existing" ]]; then
  if [[ -f "$notes" ]]; then
    gh release create "$tag" --verify-tag --draft --title "RustCarPlay $version" --notes-file "$notes"
  else
    gh release create "$tag" --verify-tag --draft --title "RustCarPlay $version" --generate-notes
  fi
fi
gh release upload "$tag" dist/* --clobber
count="$(gh release view "$tag" --json assets --jq '.assets | length')"
if [[ "$count" != 7 ]]; then
  echo "Draft has $count assets, expected 7; leaving it unpublished for inspection." >&2
  exit 1
fi
gh release edit "$tag" --draft=false
