#!/bin/bash
# Run the app on a headless 1600x1000 Xvfb display (software Vulkan) for screenshots.
# start.sh [part.json]   then use x.sh (xdotool) and shot.sh NAME.
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
podman rm -f ${CADGUI:-cadgui} >/dev/null 2>&1
mkdir -p "$ROOT/shots"
exec podman run -d --name ${CADGUI:-cadgui} \
  -v "$ROOT":"$ROOT":Z -w "$ROOT" \
  -e DISPLAY=:1 -e WAYLAND_DISPLAY= -e WGPU_BACKEND=vulkan -e RUST_LOG=warn -e CAD_THEME=${CAD_THEME:-light} \
  localhost/cad-gui bash -c '
    Xvfb :1 -screen 0 1600x1040x24 -nolisten tcp >/tmp/xvfb.log 2>&1 &
    sleep 1; openbox >/tmp/openbox.log 2>&1 &
    sleep 1; exec '"$ROOT"'/target/release/cad-rust-app "$@" >'"$ROOT"'/shots/app.log 2>&1' _ "$@"
