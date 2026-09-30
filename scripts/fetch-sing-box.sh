#!/usr/bin/env bash
# Downloads the pinned sing-box release and places it where Tauri's
# externalBin expects it: src-tauri/binaries/sing-box-<target triple>[.exe]
#
#   scripts/fetch-sing-box.sh x86_64-pc-windows-msvc
#   scripts/fetch-sing-box.sh universal-apple-darwin
#   scripts/fetch-sing-box.sh android [arm64-v8a x86_64 ...]
#
# For Android it goes into the app as libsingbox.so, per processor type
# (Android only runs an app's programs from its unpacked library folder).
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
    # Tauri builds each architecture first and checks for its own copy too.
    cp "$arm" "$OUT/sing-box-aarch64-apple-darwin"
    cp "$intel" "$OUT/sing-box-x86_64-apple-darwin"
    for f in "$OUT"/sing-box-*-apple-darwin; do codesign --force --sign - "$f"; done
    ;;
  android)
    shift
    abis=("${@:-arm64-v8a}")
    for abi in "${abis[@]}"; do
      case "$abi" in
        arm64-v8a) arch=android-arm64 ;;
        armeabi-v7a) arch=android-arm ;;
        x86_64) arch=android-amd64 ;;
        *) echo "unsupported Android processor type $abi" >&2; exit 1 ;;
      esac
      dir=$ROOT/src-tauri/gen/android/app/src/main/singbox/$abi
      mkdir -p "$dir"
      cp "$(fetch_tar "$arch")" "$dir/libsingbox.so"
      chmod 755 "$dir/libsingbox.so"
      ls -la "$dir/libsingbox.so"
    done
    exit 0
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
