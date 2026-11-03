#!/usr/bin/env bash
# One-off helper: rewrite old "Casework" names in a tree (e.g. an older WIP branch) to easyCAD.
set -euo pipefail
cd "$1"
git ls-files -z | xargs -0 grep -IlZE 'Casework|casework|CASEWORK|cwk|Cwk' 2>/dev/null | xargs -0 -r sed -i -E \
  -e 's/CaseworkApp/EasyCadApp/g' \
  -e 's/CASEWORK/EASYCAD/g' \
  -e 's/Casework/easyCAD/g' \
  -e 's/casework/easycad/g' \
  -e 's/InsertCwk/InsertEcad/g' \
  -e 's/insert_cwk/insert_ecad/g' \
  -e 's/\bCwk\b/Ecad/g' \
  -e 's/(\.|")cwk\b/\1ecad/g' \
  -e 's/\bcwk\b/ecad/g'
