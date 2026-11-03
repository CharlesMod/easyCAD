#!/usr/bin/env bash
# (a) Navigation on the sample: Shift+MMB orbit, MMB pan, wheel zoom, view-cube
# face / edge / corner regions, the ±15° step buttons and the home button.
. "$(dirname "$0")/lib.sh"
start_app --sample
shot 00_start

near() { # near <jq vec> x y z : camera forward close to the given direction
    printf '(%s) as $v | ([%s,%s,%s]) as $w | (($v[0]-$w[0])|fabs) < 0.03 and (($v[1]-$w[1])|fabs) < 0.03 and (($v[2]-$w[2])|fabs) < 0.03' "$1" "$2" "$3" "$4"
}

# park the pointer off the model so the pick popover stays away
move 330 760

rot0=$(st '.camera.rot | map(. * 1000 | round) | tostring')
drag 600 600 760 520 2 shift
idle
check "Shift+MMB orbits" "(.camera.rot | map(. * 1000 | round) | tostring) != \"$rot0\""
shot 01_orbit

t0=$(st '.camera.target | map(. * 100 | round) | tostring')
drag 600 600 700 650 2
idle
check "MMB pans" "(.camera.target | map(. * 100 | round) | tostring) != \"$t0\""
shot 02_pan

d0=$(st '.camera.distance')
wheel 700 470 5 up
idle
check "wheel up zooms in" ".camera.distance < $d0"
d1=$(st '.camera.distance')
wheel 700 470 3 down
idle
check "wheel down zooms out" ".camera.distance > $d1"
shot 03_zoom

# --- view cube (iso home pose: TOP at the top, FRONT left, RIGHT right)
click 1044 145 # home first so the cube is in its known iso pose
idle
check "home button returns to iso" "$(near .camera.forward -0.577 0.577 -0.577)"

click 1080 158
idle
check "cube TOP face -> top view" "$(near .camera.forward 0 0 -1)"
check "top view keeps +Y up" ".camera.up[1] > 0.99"
shot 04_cube_top

click 1044 145
idle
click 1061 192
idle
check "cube FRONT face -> front view" "$(near .camera.forward 0 1 0)"
shot 05_cube_front

click 1044 145
idle
click 1080 206
idle
check "cube front-right edge" "$(near .camera.forward -0.7071 0.7071 0)"
shot 06_cube_edge

click 1044 145
idle
click 1080 180
idle
check "cube top-front-right corner" "$(near .camera.forward -0.577 0.577 -0.577)"
shot 07_cube_corner

click 1044 145
idle
click 1062 170
idle
check "cube top-front edge" "$(near .camera.forward 0 0.7071 -0.7071)"

# --- 15° steps from the front view: X turns about world X
click 1044 145
idle
click 1061 192
idle
click 1108 237
idle
check "X +15° step tilts the front view" ".camera.forward[2] | fabs | . > 0.2 and . < 0.3"
f1=$(st '.camera.forward | map(. * 1000 | round) | tostring')
click 1049 237
idle
check "X -15° step undoes it" "$(near .camera.forward 0 1 0)"
click 1108 277
idle
check "Z +15° step turns about Z" ".camera.forward[0] | fabs | . > 0.2 and . < 0.3"
shot 08_steps
[ -n "$f1" ]

key Home
idle
check "Home key returns to iso" "$(near .camera.forward -0.577 0.577 -0.577)"
shot 09_home
finish
