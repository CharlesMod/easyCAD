#!/usr/bin/env bash
# (h) Drawing: add a length dimension to the right view (placed where the paper is
# clicked), then Export PDF. Under Xvfb there is no D-Bus session, so no XDG
# portal: the system dialog cannot open and the in-app path prompt takes over.
. "$(dirname "$0")/lib.sh"
start_app --sample

move 700 700
key d
check "drawing mode with the default sheet" '.mode.name == "drawing" and .mode.computed and .mode.views == 4' 60
idle
n0=$(st .mode.annotations)

# Dimension tool is active; the right view's bottom edge, then paper below it
click 713 633
check "edge picked" '.mode.picks == 1'
click 713 662
check "a dimension was added" ".mode.annotations == $((n0 + 1))"
check "horizontal, below the edge (negative offset)" '.mode.last_annotation | test("Horizontal, offset: -")'
check "it measures the straight bottom edge (44 mm between the corner fillets)" '.mode.dims | index(44) != null' 60
idle
shot 00_dimension

click 1297 74 # Export PDF
check "no system dialog here: the path prompt opens" '.path_prompt.title == "Export drawing as PDF"' 10
shot 01_prompt
OUT="$WORK/out/drawing.pdf"
mkdir -p "$WORK/out"
check "it suggests the document and sheet name" '.path_prompt.text | endswith("sensor-node - Sheet 1.pdf")'
retype "$WORK/nope/drawing.pdf"
key Return
check "a missing folder is refused (the prompt stays)" '.path_prompt.text | endswith("nope/drawing.pdf")'
retype "$OUT"
key Return
check "prompt closed" '.path_prompt == null'
check "toast names the file" ".toasts | any(test(\"Exported .*drawing.pdf\"))"
sleep 0.5
if [ -s "$OUT" ] && head -c 5 "$OUT" | grep -q '%PDF-'; then
    log "ok   $OUT is a PDF ($(stat -c %s "$OUT") bytes)"
else
    log "FAIL no PDF at $OUT"
    FAILS=$((FAILS + 1))
fi
idle
shot 02_exported
cp "$OUT" "$SHOTS/${SCENARIO}_drawing.pdf" 2>/dev/null
finish
