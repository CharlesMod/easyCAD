#!/usr/bin/env bash
# (k) Settings window, `?` cheat sheet, autosave + crash recovery: edit `wall` on the
# sample, wait for the recovery copy, kill -9, relaunch, Restore, the edit is back.
. "$(dirname "$0")/lib.sh"
export CASEWORK_AUTOSAVE_SECS=2
start_app --sample
move 700 760

# cheat sheet
key shift+slash
check "? opens the cheat sheet" '.overlay == "keys"'
idle
shot 00_cheat_sheet
key Escape
check "Esc closes it" '.overlay == null'

# settings window: switch navigation preset, close
key ctrl+comma
check "Ctrl+, opens settings" '.overlay == "settings"'
idle
shot 01_settings
key Escape
check "Esc closes settings" '.overlay == null'

# an unsaved edit (wall, 4th row of the Parameters panel)
click 1330 519
retype 3.25
key Return
check "wall is 3.25" '.params.wall == "3.25"'
move 700 760
check "a recovery copy was written" '.autosave != null' 30
sleep 1
crash_app
log "killed the app (kill -9)"

restart_app --sample
check "relaunch offers the unsaved work" '(.recovery | length) == 1' 20
idle
shot 02_offer
key Return  # Enter restores the newest copy
check "restored" '(.recovery | length) == 0 and .params.wall == "3.25"' 30
check "restored work is unsaved" '.dirty == true'
idle
shot 03_restored
finish
