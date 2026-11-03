#!/usr/bin/env bash
# (i) First run: the welcome card on an empty document, "New enclosure from PCB"
# with the sample board's DXF (picked through Browse… → in-app path prompt), Lip
# joint, Create; then edit a generated parameter.
. "$(dirname "$0")/lib.sh"
"$BIN" --sample-board "$WORK/boards" >/dev/null
start_app
check "empty document shows the welcome card" '.welcome == true'
shot 00_welcome

click 695 462 # New enclosure from PCB…
check "wizard open" '.wizard != null and .wizard.board == null'
click 923 300 # Browse…
check "no system dialog: path prompt" '.path_prompt.title | test("Board file")'
retype "$WORK/boards/sensor-node.dxf"
key Return
check "board read: 86 x 50, 4 holes, DXF" '.wizard.board.size == [86, 50] and .wizard.board.holes == 4 and .wizard.board.source == "DXF"'
click 829 576 # Lip
check "lip joint chosen" '.wizard.joint == "Lip"'
shot 01_wizard

click 969 669 # Create enclosure
check "wizard closed" '.wizard == null'
check "two halves" '.bodies == ["Base", "Lid"]' 120
check "the history is ordinary features" '[.features[].kind] == ["Extrude", "Fillet", "Shell", "Standoffs from PCB", "Parting joint"]'
check "all features regenerate" '[.features[].status] | all(. == "ok")' 120
check "named parameters" '.params | has("clearance") and has("wall") and has("height") and has("standoff_h") and has("lid_split")'
check "round board corners: no unused corner_r" '.params | has("corner_r") | not'
idle
shot 02_created

# it edits like any model: thicker walls
click 1330 541 # wall
retype 2.5
key Return
check "wall is 2.5" '.params.wall == "2.5"'
check "still regenerates without errors" '(.regenerating | not) and ([.features[].status] | all(test("Error|Unresolved") | not))' 120
idle
shot 03_wall_2_5
finish
