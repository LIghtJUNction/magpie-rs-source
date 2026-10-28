#!/usr/bin/env bash
set -euo pipefail

version="${1:?Usage: build-release.sh VERSION RUST_TARGET OUTPUT_DIR [--with-desktop] [--native-packages]}"
target="${2:?Missing Rust target}"
out="${3:?Missing output directory}"
shift 3
desktop=false
native=false
for arg in "$@"; do
  case "$arg" in
    --with-desktop) desktop=true ;;
    --native-packages) native=true ;;
    *) echo "Unknown argument: $arg" >&2; exit 2 ;;
  esac
done
root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
args=(--version "$version" --target "$target" --out-dir "$out" --build)
if "$desktop"; then args+=(--with-desktop); fi
if "$native"; then args+=(--native-packages); fi
exec python3 -B "$root/scripts/package-release.py" "${args[@]}"
