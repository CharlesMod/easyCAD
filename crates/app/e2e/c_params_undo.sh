#!/usr/bin/env bash
# (c) Edit `wall` in the Parameters panel -> live regen -> Ctrl+Z / Ctrl+Shift+Z.
. "$(dirname "$0")/lib.sh"
start_app --sample
move 330 760
m0=$(st .model_id)
rev0=$(st .revision)

# the `wall` expression field (4th row of the Parameters panel)
click 1330 519
retype 2.5
key Return
check "wall is 2.5" '.params.wall == "2.5"'
check "regenerated after the edit" ".model_id > $m0 and (.regenerating | not)" 60
check "standoffs follow the floor (no feature errors)" '[.features[].status] | all(test("Error|Unresolved") | not)' 30
check "undo label names the edit" '.undo != null'
idle
shot 00_wall_2_5
m1=$(st .model_id)

# Ctrl+Z straight after Enter (the field has given up focus)
move 700 760
key ctrl+z
check "Ctrl+Z restores wall = 2" '.params.wall == "2"'
check "regenerated after undo" ".model_id > $m1 and (.regenerating | not)" 60
check "the field shows the restored value" '.revision > '"$rev0"
idle
shot 01_undo

key ctrl+shift+z
check "Ctrl+Shift+Z re-applies wall = 2.5" '.params.wall == "2.5"'
idle
shot 02_redo

# a bad expression is refused with a message, the document is untouched
click 1330 519
retype "wall_typo * 2"
key Return
check "bad expression is not stored" '.params.wall == "2.5"'
idle
shot 03_bad_expr
click 1330 519
key Escape
check "Esc leaves the field" '.typing | not'
check "no feature errors after all that" '[.features[].status] | all(test("Error|Unresolved") | not)' 30
shot 04_escaped
finish
