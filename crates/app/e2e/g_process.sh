#!/usr/bin/env bash
# (g) Process profile: Molded -> FDM (fit table, teardrops appear), toggle the
# teardrop off / on, switch the style, back to Molded. The picker keeps its order.
. "$(dirname "$0")/lib.sh"
start_app --sample
check "sample starts molded" '.profile == "Injection molded (nominal)"'
order=$(st '.profiles | join("|")')

click 1285 182 # profile picker
sleep 0.4
shot 00_picker
click 1222 215 # FDM 0.4 generic (first row)
check "FDM active" '.profile == "FDM 0.4 generic" and .undo == "Process: FDM 0.4 generic"'
check "FDM turns teardrops on" '.teardrops == true'
check "regenerates cleanly" '(.regenerating | not) and ([.features[].status] | all(test("Error|Unresolved") | not))' 60
idle
shot 01_fdm

click 1162 334 # teardrop checkbox
check "teardrops off, undo says so" '.teardrops == false and .undo == "Teardrops off"'
idle
shot 02_teardrop_off
click 1162 334
check "teardrops back on" '.teardrops == true and .undo == "Teardrops on"'
idle
click 1219 366 # Pointed 45°
check "pointed style" '(.teardrop_style | test("Pointed")) and .undo == "Teardrop style"'
check "editing the profile keeps the picker order" ".profiles | join(\"|\") == \"$order\""
idle
shot 03_pointed

click 1285 182
sleep 0.4
click 1285 275 # Injection molded (third row, same place as before)
check "back to molded" '.profile == "Injection molded (nominal)"'
check "molded has no teardrop stage" '.teardrops == false'
idle
shot 04_molded

move 700 700
key ctrl+z
check "undo returns to FDM with the pointed teardrops" '.profile == "FDM 0.4 generic" and (.teardrop_style | test("Pointed"))'
idle
finish
