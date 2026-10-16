#!/bin/sh
# Installs aibrowse from its latest GitHub release into ~/.aibrowse/bin (macOS).
# usage: curl -fsSL https://github.com/thejens/aibrowse/releases/latest/download/install.sh | sh
set -eu
repo=thejens/aibrowse
dir="$HOME/.aibrowse/bin"
case "$(uname -s)" in
  Darwin) asset=aibrowse-macos-universal.tar.gz ;;
  *) echo "this installer is for macOS; on Linux build from source (README)" >&2; exit 1 ;;
esac
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
base="https://github.com/$repo/releases/latest/download"
curl -fsSL "$base/$asset" -o "$tmp/$asset"
curl -fsSL "$base/SHA256SUMS" -o "$tmp/SHA256SUMS"
(cd "$tmp" && grep " $asset\$" SHA256SUMS | shasum -a 256 -c -)
tar -xzf "$tmp/$asset" -C "$tmp"
mkdir -p "$dir"
# A downloaded file carries the quarantine mark; curl's doesn't, but a copy from a
# browser download would, and Gatekeeper would refuse the unsigned binary.
install -m 755 "$tmp/aibrowse/aibrowse" "$dir/aibrowse"
xattr -d com.apple.quarantine "$dir/aibrowse" 2>/dev/null || true
echo "installed $dir/aibrowse ($("$dir/aibrowse" --version 2>/dev/null || echo aibrowse))"
echo "next: put JEV_API_KEY=… in ~/.aibrowse/env, then register it with your MCP client (README)"
