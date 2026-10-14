#!/bin/bash
# Drive the rig: sketch a rectangle on Top, extrude 30, fillet the front top edge.
X=$(dirname "$0")/x.sh
$X mousemove 800 500 key shift+s; sleep 0.8
$X key r; $X mousemove 700 350 click 1; sleep 0.2; $X mousemove 1100 650 click 1; sleep 0.4
$X key Return; sleep 0.8; $X key 0; sleep 0.5
$X mousemove 800 480 click 1; sleep 0.3; $X key shift+e; sleep 1.5
$X mousemove 490 206 click --repeat 3 1; sleep 0.2; $X type 30; $X key Return; sleep 2
$X mousemove 777 535; sleep 0.3; $X click 1; sleep 0.3; $X key shift+f; sleep 1
$X mousemove 490 150 click --repeat 3 1; $X type 6; $X key Return; sleep 2
