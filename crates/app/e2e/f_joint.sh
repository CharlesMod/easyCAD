#!/usr/bin/env bash
# (f) Parting joint on the sample (which is already cut by a plain Split): the
# dialog takes the Split's place, drag the plane handle, switch to O-ring, OK.
. "$(dirname "$0")/lib.sh"
start_app --sample
n0=$(st '.features | length')

move 300 800
key j
check "Joint dialog replaces the existing Split" '.dialog.kind == "Parting joint" and .dialog.editing != null and .dialog.joint.z == "lid_split"'
check "the joint cuts the (unsplit) body" '.dialog.joint.report and .dialog.joint.status == "Ok"' 60
idle
shot 00_open

drag 153 342 153 380 # the plane handle, down
check "dragging the handle lowers the plane" '(.dialog.joint.z | tonumber) < 28'
check "still a valid joint after the drag" '.dialog.joint.status == "Ok"' 60
idle
shot 01_dragged

click 985 298 # O-ring
check "O-ring selected" '.dialog.joint.kind == "O-ring"'
check "O-ring preview computed" '.dialog.joint.report and (.dialog.joint.status | test("Ok|Warning"))' 60
idle
shot 02_oring

key Return
check "dialog closed" '.dialog == null'
check "the Split became the joint (feature count unchanged)" "(.features | length) == $n0"
check "joint feature in place" '[.features[].kind] | index("Parting joint") != null and index("Split body") == null'
check "two halves again" '.bodies | index("Base") != null and index("Lid") != null' 60
check "later features still resolve" '[.features[].status] | all(test("Error|Unresolved") | not)' 60
check "undo names the replacement" '.undo | test("replaces")'
idle
shot 03_done
finish
