#!/bin/bash
# shot.sh NAME -> shots/NAME.png
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
podman exec -e DISPLAY=:1 ${CADGUI:-cadgui} import -window root "$ROOT/shots/$1.png"
