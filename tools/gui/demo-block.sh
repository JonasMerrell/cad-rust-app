#!/bin/bash
# Drive the rig: rectangle on Top, extrude 30, fillet the front top edge,
# circle on the top face dimensioned to 25, then cut it through all.
X=$(dirname "$0")/x.sh
S=$(dirname "$0")/shot.sh
$X mousemove 800 500 key shift+s; sleep 0.8
$X key r; $X mousemove 700 350 click 1; sleep 0.2; $X mousemove 1100 650 click 1; sleep 0.4
$X key Return; sleep 0.8; $X key 0; sleep 0.5
$X mousemove 800 480 click 1; sleep 0.3; $X key shift+e; sleep 1.5
$X mousemove 490 206 click --repeat 3 1; sleep 0.2; $X type 30; $X key Return; sleep 3
$X mousemove 1500 950; $X key f; sleep 0.5
$X mousemove 777 566; sleep 0.5; $X click 1; sleep 0.3; $X key shift+f; sleep 1
$X mousemove 490 150 click --repeat 3 1; $X type 6; $X key Return; sleep 3
$S demo-fillet
$X mousemove 1000 380 click 1; sleep 0.4; $X key shift+s; sleep 1.2
$X key c; $X mousemove 900 500 click 1; $X mousemove 960 500 click 1; sleep 0.4
$X key Escape; $X key d; sleep 0.2
$X mousemove 967 508 click 1; sleep 0.6; $X type 25; $X key Return; sleep 0.6
$X key s; $X mousemove 1300 300; sleep 0.3
$S demo-sketch
$X key Return; sleep 1; $X key 0; sleep 0.5
$X mousemove 905 470 click 1; sleep 0.3; $X key shift+e; sleep 1.2
$X mousemove 490 150 click 1; sleep 0.4; $X mousemove 430 237 click 1; sleep 0.6
$X mousemove 490 178 click 1; sleep 0.4; $X mousemove 440 265 click 1; sleep 3
$X key Return; sleep 0.5; $X mousemove 1500 950; sleep 0.3
$S demo-cut
