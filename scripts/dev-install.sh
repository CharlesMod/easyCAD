#!/usr/bin/env bash
# Build easyCAD (release) from this checkout and install it atomically over
# ~/.local/bin/easycad. A running easyCAD notices the new file and restarts into
# it, restoring its session (docs/DEV-RELOAD.md).
#
#   scripts/dev-install.sh
#   DEST=/some/other/easycad scripts/dev-install.sh   # install elsewhere
#
# EASYCAD_OCCT_DIR must point at the prebuilt OCCT (default: the main checkout's
# target/OCCT if it exists).
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO"
DEST="${DEST:-$HOME/.local/bin/easycad}"

if [[ -z "${EASYCAD_OCCT_DIR:-}" && -d "$HOME/easyCAD/target/OCCT" ]]; then
  export EASYCAD_OCCT_DIR="$HOME/easyCAD/target/OCCT"
fi

cargo build --release -p easycad

mkdir -p "$(dirname "$DEST")"
tmp="$(dirname "$DEST")/.$(basename "$DEST").new"
# copy beside, then rename over: the running process's file is never written into
cp -f "$REPO/target/release/easycad" "$tmp"
chmod 755 "$tmp"
mv -f "$tmp" "$DEST"

sha="$(git -C "$REPO" rev-parse --short=9 HEAD)"
git -C "$REPO" diff --quiet HEAD -- || sha="$sha+"
echo "installed easycad $sha -> $DEST"
