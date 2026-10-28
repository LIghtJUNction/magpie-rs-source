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
cd "$root"
export CARGO_TARGET_DIR="${MAGPIE_RS_TARGET_DIR:-/tmp/magpie-rs-release-${target}}"
python scripts/package-release.py --version "$version" --check-version
cargo build --release --locked --package magpie-cli --target "$target"
extension=""
case "$target" in *-windows-*) extension=".exe" ;; esac
args=(--version "$version" --target "$target"
      --binary "$CARGO_TARGET_DIR/$target/release/magpie$extension" --out-dir "$out")
if "$desktop"; then
  cargo build --release --locked --manifest-path apps/desktop/Cargo.toml --target "$target"
  args+=(--desktop-binary "$CARGO_TARGET_DIR/$target/release/magpie-desktop$extension")
fi
if "$native"; then args+=(--native-packages); fi
python scripts/package-release.py "${args[@]}"
