#!/usr/bin/env bash
# Write THIRD-PARTY-NOTICES.html (every Rust crate's license, plus OpenCascade's LGPL
# exception) to the path given (default: dist/THIRD-PARTY-NOTICES.html).
# Needs cargo-about: cargo install --locked cargo-about --features cli
set -euo pipefail
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${1:-$REPO/dist/THIRD-PARTY-NOTICES.html}"
cd "$REPO"
command -v cargo-about >/dev/null || { echo "cargo-about not found: cargo install --locked cargo-about --features cli" >&2; exit 1; }
mkdir -p "$(dirname "$OUT")"
cargo about generate --locked --workspace -c about.toml about.hbs -o "$OUT.tmp"
python3 - "$OUT.tmp" licenses/OCCT_LGPL_EXCEPTION.txt <<'PY'
import html, sys
page, exc = sys.argv[1], sys.argv[2]
s = open(page, encoding="utf-8").read()
s = s.replace("@@OCCT_EXCEPTION@@", html.escape(open(exc, encoding="utf-8", errors="replace").read()))
open(page, "w", encoding="utf-8").write(s)
PY
mv "$OUT.tmp" "$OUT"
echo "notices -> $OUT"
