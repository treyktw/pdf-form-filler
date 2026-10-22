#!/usr/bin/env bash
# Downloads a prebuilt PDFium (bblanchon/pdfium-binaries) into ./.pdfium,
# verifies its SHA-256, and prints the export line for PDFIUM_DYNAMIC_LIB_PATH.
#
#   scripts/fetch-pdfium.sh            # build 7881, host platform
#   PDFIUM_BUILD=7881 scripts/fetch-pdfium.sh
#
# Build 7881 is the PDFium ABI pdfium-render 0.9 is compiled against (feature
# "pdfium_7881"); a different build may load but is not supported. Checksums
# are pinned for the platforms CI and the Dockerfile use; elsewhere the script
# prints the hash it got so you can pin it.
set -euo pipefail

build="${PDFIUM_BUILD:-7881}"
dest="${PDFIUM_DIR:-$(cd "$(dirname "$0")/.." && pwd)/.pdfium}"

case "$(uname -s)-$(uname -m)" in
  Linux-x86_64)  asset=pdfium-linux-x64.tgz;   lib=libpdfium.so ;;
  Linux-aarch64) asset=pdfium-linux-arm64.tgz; lib=libpdfium.so ;;
  Darwin-arm64)  asset=pdfium-mac-arm64.tgz;   lib=libpdfium.dylib ;;
  Darwin-x86_64) asset=pdfium-mac-x64.tgz;     lib=libpdfium.dylib ;;
  *) echo "unsupported platform: $(uname -s)-$(uname -m)" >&2; exit 1 ;;
esac

expected=""
if [ "$build" = "7881" ]; then
  case "$asset" in
    pdfium-linux-x64.tgz)   expected=1470e21b8b4a3b4ad7f85684e2da11d94f3b69a86d81dee11b9b6709d927ac1d ;;
    pdfium-linux-arm64.tgz) expected=ee7f7b7d5468958336a818c1cd580bdd20972846b7377b13f9a923d92d1d4674 ;;
  esac
fi

if [ -f "$dest/lib/$lib" ] && grep -q "BUILD=$build" "$dest/VERSION" 2>/dev/null; then
  echo "PDFium $build already in $dest" >&2
else
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' EXIT
  url="https://github.com/bblanchon/pdfium-binaries/releases/download/chromium%2F${build}/${asset}"
  echo "downloading $url" >&2
  curl -fsSL --retry 3 -o "$tmp/pdfium.tgz" "$url"
  if command -v sha256sum >/dev/null; then
    actual="$(sha256sum "$tmp/pdfium.tgz" | cut -d' ' -f1)"
  else
    actual="$(shasum -a 256 "$tmp/pdfium.tgz" | cut -d' ' -f1)"
  fi
  if [ -n "$expected" ] && [ "$actual" != "$expected" ]; then
    echo "checksum mismatch for $asset: got $actual, expected $expected" >&2
    exit 1
  fi
  [ -z "$expected" ] && echo "sha256 $actual (not pinned for $asset)" >&2
  rm -rf "$dest"
  mkdir -p "$dest"
  tar -xzf "$tmp/pdfium.tgz" -C "$dest"
fi

echo "export PDFIUM_DYNAMIC_LIB_PATH=$dest/lib"
