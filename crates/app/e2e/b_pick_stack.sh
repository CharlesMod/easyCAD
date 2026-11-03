#!/usr/bin/env bash
# (b) Hover the back-left standoff through the closed lid, Tab-cycle the pick stack
# down to the standoff's top face (hidden behind the lid), click to select it.
. "$(dirname "$0")/lib.sh"
start_app --sample

# the back-left standoff ring in the default iso view (lid on top of it)
X=643 Y=336
move $((X - 4)) $((Y - 4))
move $X $Y 0.8
check "pick stack lists several hits" '.stack.items | length >= 4'
check "frontmost hit is the lid" '.stack.items[0] | test("@ Lid")'
shot 00_hover

# Tab until the current candidate is a face on Base facing up (the standoff top)
for i in $(seq 1 10); do
    [ "$(st '.stack.items[.stack.index] | test("facing up @ Base")')" = true ] && break
    key Tab
done
check "Tab reaches the hidden standoff top" '.stack.index > 0 and (.stack.items[.stack.index] | test("facing up @ Base"))'
shot 01_tabbed

key shift+Tab
check "Shift+Tab steps back" '.stack.items[.stack.index] | test("facing up @ Base") | not'
key Tab
click $X $Y
check "click selects the hidden face" '.selection == ["Planar face · facing up @ Base"]'
move 330 760 0.5
shot 02_selected

# popover row click: hover again and click the 2nd row directly
move $X $Y 0.8
shot 03_popover
finish
