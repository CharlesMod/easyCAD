#!/usr/bin/env bash
# (j) Assemblies: New enclosure from PCB with the sample STEP board -> components
# (Enclosure master, Base, Lid, the board); a revolute hinge along the front seam
# (snap to the Base's edge, then the Lid's, with Tab through the pick stack); jog
# it open on the slider; OK; interference check (clear when open); jog it into
# the base and check again (one pair, red).
. "$(dirname "$0")/lib.sh"
"$BIN" --sample-board "$WORK/boards" >/dev/null
start_app
click 695 462 # New enclosure from PCB…
click 923 300 # Browse…
check "path prompt" '.path_prompt.title | test("Board file")'
retype "$WORK/boards/sensor-node.step"
key Return
check "STEP board read" '.wizard.board.source == "STEP" and .wizard.board.holes == 4'
click 969 669 # Create enclosure
check "assembly: master, Base, Lid, board" '[.assembly.occurrences[] | select(.depth == 1) | .name] == ["Enclosure:1", "Base:1", "Lid:1", "sensor-node:1"]' 180
check "regenerated without errors" '(.regenerating | not) and .assembly.errors == []' 180
idle
shot 00_components

occ() { st "[.assembly.occurrences[] | select(.name == \"$1\")][0].anchors.$2"; }
lid_top0=$(occ Lid:1 'bounds[1][2]')

# the move gizmo: select the Lid in the browser, G, drag the Z arrow up
click 120 278
check "Lid selected" '.assembly.selected == "Lid:1"'
key g
check "gizmo shown" '.assembly.gizmo and .assembly.gizmo_screen != null'
read -r ox oy zx zy <<<"$(st '.assembly.gizmo_screen | [.origin[0], .origin[1], .tips[2][0], .tips[2][1]] | map(round) | join(" ")')"
mx=$(((ox + zx) / 2)); my=$(((oy + zy) / 2))
drag "$mx" "$my" "$mx" $((my - 60))
check "the lid moved up (whole mm)" "[.assembly.occurrences[] | select(.name == \"Lid:1\")][0].anchors.bounds[1][2] > $lid_top0 + 5" 30
check "one undo step: Move Lid:1" '.undo == "Move Lid:1"'
idle
shot 00b_gizmo_drag
move 700 700
key ctrl+z
check "undo puts it back" "[.assembly.occurrences[] | select(.name == \"Lid:1\")][0].anchors.bounds[1][2] < $lid_top0 + 0.5" 30
key Escape
check "Esc hides the gizmo" '.assembly.gizmo == false'

# A opens the mates dialog (Revolute by default)
move 700 700
key a
check "mates dialog open" '.assembly.mate_dialog.kind == "Revolute"'

# hover the front seam and Tab to a straight edge of the wanted part, then click
pick_edge() { # $1 = body name regex, $2 = side label to wait for
    local x y
    x=$(occ Base:1 'front_top[0] | round'); y=$(occ Base:1 'front_top[1] | round')
    for dy in 0 2 -2 4 -4 6 -6 8 -8 10 -10; do
        move "$x" $((y + dy)) 0.3
        for _ in 1 2 3 4 5 6 7 8; do
            if [ "$(st ".stack.items[.stack.index] // \"\" | test(\"Line edge.*@ $1\")")" = "true" ]; then
                click "$x" $((y + dy))
                return 0
            fi
            [ "$(st '.stack.items | length')" -gt 1 ] || break
            key Tab
        done
    done
    return 1
}
pick_edge "Base" || log "no Base edge under the seam"
check "side 1: the Base's edge axis" '.assembly.mate_dialog.a | test("Base:1 · edge axis")'
pick_edge "Lid" || log "no Lid edge under the seam"
check "side 2: the Lid's edge axis" '.assembly.mate_dialog.b | test("Lid:1 · edge axis")'
check "the hinge solves" '.assembly.mate_dialog.status == "Solved"' 30
idle
shot 01_hinge_picked

# jog it open: drag the slider handle (centre = 0°) to about +100°
r=$(st '.assembly.mate_dialog.jog_rect | map(round) | join(" ")')
read -r x0 y0 x1 y1 <<<"$r"
yc=$(((y0 + y1) / 2))
drag $(((x0 + x1) / 2)) "$yc" $((x0 + (x1 - x0) * 78 / 100)) "$yc"
check "jogged open (60°..140°)" '.assembly.mate_dialog.angle > 60 and .assembly.mate_dialog.angle < 140'
idle
check "the lid swung up" "[.assembly.occurrences[] | select(.name == \"Lid:1\")][0].anchors.bounds[1][2] > $lid_top0 + 20" 30
shot 02_jog_open
key Return
check "mate added, solved" '(.assembly.mates | length) == 1 and .assembly.mates[0].status == "Solved" and .assembly.mate_dialog == null'
check "undo names it" '.undo | test("Revolute")'

# interference with the lid open: none
move 700 700
key i
check "interference: clear when open" '.assembly.interference.running == false and (.assembly.interference.pairs | length) == 0' 120
idle
shot 03_clear

# swing it into the base (edit the mate from the browser's Mates folder)
dclick 80 371
check "editing the mate" '.assembly.mate_dialog != null and .assembly.mate_dialog.a != null'
r=$(st '.assembly.mate_dialog.jog_rect | map(round) | join(" ")')
read -r x0 y0 x1 y1 <<<"$r"
yc=$(((y0 + y1) / 2))
hx=$((x0 + (x1 - x0) * 78 / 100))
drag "$hx" "$yc" $((x0 + (x1 - x0) * 47 / 100)) "$yc"
check "jogged into the base (< 0°)" '.assembly.mate_dialog.angle < -2 and .assembly.mate_dialog.angle > -40'
key Return
check "mate updated" '(.assembly.mates[0].angle | tonumber) < 0'
move 700 700
key i
check "interference: base and lid overlap" '.assembly.interference.running == false and (.assembly.interference.pairs | length) >= 1' 120
check "the pair is Base and Lid" '.assembly.interference.pairs[0] | (.a + .b) | test("Base") and test("Lid")'
idle
shot 04_interference
finish
