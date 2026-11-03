#!/usr/bin/env bash
# (d) Sketch on the enclosure floor (picked through the PCB with Tab), draw a
# rectangle with typed W/H, fix a corner, finish, then extrude-cut it.
. "$(dirname "$0")/lib.sh"
start_app --sample

click 223 309 # hide the lid with its eye
idle
check "lid hidden" '.hidden == ["Lid"]'

# the floor is under the PCB reference: Tab to it
move 705 525 0.8
for i in $(seq 1 8); do
    [ "$(st '.stack.items[.stack.index] | test("(End face|Floor) · Floor @ Base")')" = true ] && break
    key Tab
done
click 705 525
check "floor face selected" '(.selection | length) == 1 and (.selection[0] | test("(End face|Floor) · Floor @ Base"))'
shot 00_floor

key s
check "sketch mode on the floor plane (z = 2)" '.mode.name == "sketch" and (.mode.plane | test("@ 2"))'

# sketch -> screen: canvas centre (697, 456), y up
scr() { # scr x y -> "sx sy"
    jq -r --argjson x "$1" --argjson y "$2" \
        '.mode.view as $v | "\((697 + ($x - $v[0]) * $v[2]) | round) \((456 - ($y - $v[1]) * $v[2]) | round)"' "$STATE"
}

key r
read -r X0 Y0 <<<"$(scr -10 -5)"
click "$X0" "$Y0"
typ 20
key Tab
typ 10
shot 01_typing
key Return
check "rectangle 20 x 10 from the clicked corner" '.mode.bbox == [-10, -5, 10, 5]'
check "one closed profile" '.mode.profiles == 1'
check "under-constrained before the fix" '.mode.status == "UnderConstrained"'
c0=$(st .mode.constraints)

key Escape # back to the select tool
click "$X0" "$Y0"
check "corner point selected" '.mode.selection == 1'
click 1210 423 # Fix
check "Fix adds a constraint" ".mode.constraints == $((c0 + 1))"
check "fully constrained" '.mode.status == "FullyConstrained"'
shot 02_constrained

click 1290 840 # Finish sketch
check "back in the model" '.mode.name == "model"'
check "finish opens nothing" '.dialog == null'

check "the floor face is no longer selected" '.selection == []'
n0=$(st '.features | length')

key e
check "Extrude dialog opens on the new sketch" '.dialog.kind == "Extrude" and .dialog.extrude.sketch != null'
check "it targets the floor's body" '.dialog.extrude.target == "Base"'
click 992 271 # Cut
click 1016 202 # Flip (down through the floor)
click 1042 167 # Distance
retype wall
key Tab
check "cut through the floor by wall" '.dialog.extrude.op == "Cut" and .dialog.extrude.distance == "wall"'
idle
shot 03_extrude_cut
click 1080 362 # OK
check "dialog closed" '.dialog == null'
check "one feature added" "(.features | length) == $((n0 + 1))" 30
check "the cut regenerates cleanly" '.features[-1].status == "ok" and .features[-1].kind == "Extrude"' 60
idle
# look through the hole from below
key ctrl+z; key ctrl+shift+z
idle
shot 04_cut
finish
