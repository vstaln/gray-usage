#!/bin/sh
here="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
sys="$(uname -s)"
case "$sys" in
MINGW* | MSYS* | CYGWIN*) rel="x86_64-pc-windows-msvc/gray-usage.exe" ;;
Darwin*)
    case "$(uname -m)" in
    arm64) rel="aarch64-apple-darwin/gray-usage" ;;
    *) rel="x86_64-apple-darwin/gray-usage" ;;
    esac ;;
*)
    case "$(uname -m)" in
    aarch64 | arm64) rel="aarch64-unknown-linux-musl/gray-usage" ;;
    *) rel="x86_64-unknown-linux-musl/gray-usage" ;;
    esac ;;
esac
if [ -x "$here/$rel" ]; then exec "$here/$rel" "$@"; fi
for cand in "$here/target/release/gray-usage.exe" "$here/target/release/gray-usage" "$here/target/debug/gray-usage.exe" "$here/target/debug/gray-usage"; do
    if [ -x "$cand" ]; then exec "$cand" "$@"; fi
done
echo "gray-usage binary not found in $here" >&2
exit 1
