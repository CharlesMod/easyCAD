#!/usr/bin/env bash
# (e) Fillet: pick the two long edges of the USB-C cutout, ask for a radius that
# does not fit, take the reduced radius from the self-heal report, OK.
. "$(dirname "$0")/lib.sh"
start_app --sample

click 219 75 # Fillet
check "Fillet dialog opens with the edge filter" '.dialog.kind == "Fillet" and .dialog.sets[0].edges == 0'
idle
# the dialog layout moves the model: the USB-C cutout's 9 mm edges
move 455 482 0.5
check "hover finds the cutout's top edge" '.stack.items[0] == "Line edge · 9.00 @ Base"'
click 455 482
click 470 505
check "two edges in one set" '.dialog.sets[0].edges == 2'
idle

click 1005 201 # the set's radius field
retype 3
key Tab
check "r 3 does not fit: the report offers the largest radius" \
    '.dialog.sets[0].results | length == 2 and all(test("Failed .*max: Some\\(1\\.9"))' 60
idle
shot 00_too_big

click 801 411 # Use 1.99
check "both edges reduced to r_max" \
    '.dialog.sets[0].results | length == 2 and all(test("Reduced .*applied: 1\\.9"))' 60
idle
shot 01_reduced

key Return
check "dialog closed" '.dialog == null'
check "fillet added with a reduced-radius warning" \
    '.features[-1].kind == "Fillet" and (.features[-1].status | test("reduced to 1.99"))' 60
check "undo names it" '.undo == "Fillet"'
idle
shot 02_done
finish
