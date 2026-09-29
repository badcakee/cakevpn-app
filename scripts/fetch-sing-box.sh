#!/usr/bin/env bash
# Downloads the pinned sing-box release and places it where Tauri's
# externalBin expects it: src-tauri/binaries/sing-box-<target triple>[.exe]
#
#   scripts/fetch-sing-box.sh x86_64-pc-windows-msvc
#   scripts/fetch-sing-box.sh universal-apple-darwin
set -euo pipefail

VERSION=${SING_BOX_VERSION:-1.14.2}
TARGET=${1:?usage: fetch-sing-box.sh <target triple>}
ROOT=$(cd "$(dirname "$0")/.." && pwd)
OUT=$ROOT/src-tauri/binaries
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
BASE=https://github.com/SagerNet/sing-box/releases/download/v$VERSION

fetch_tar() { # arch -> path of the extracted binary
  local name=sing-box-$VERSION-$1
  curl -fsSL -o "$WORK/$name.tar.gz" "$BASE/$name.tar.gz"
  tar -xzf "$WORK/$name.tar.gz" -C "$WORK"
  echo "$WORK/$name/sing-box"
}

mkdir -p "$OUT"
case "$TARGET" in
  x86_64-pc-windows-msvc)
    name=sing-box-$VERSION-windows-amd64
    curl -fsSL -o "$WORK/$name.zip" "$BASE/$name.zip"
    (cd "$WORK" && unzip -q "$name.zip")
    cp "$WORK/$name/sing-box.exe" "$OUT/sing-box-$TARGET.exe"
    ;;
  universal-apple-darwin)
    arm=$(fetch_tar darwin-arm64)
    intel=$(fetch_tar darwin-amd64)
    lipo -create -output "$OUT/sing-box-$TARGET" "$arm" "$intel"
    codesign --force --sign - "$OUT/sing-box-$TARGET"
    ;;
  x86_64-unknown-linux-gnu)
    cp "$(fetch_tar linux-amd64)" "$OUT/sing-box-$TARGET"
    ;;
  *)
    echo "unsupported target $TARGET" >&2
    exit 1
    ;;
esac
ls -la "$OUT"/sing-box-"$TARGET"*
