#!/bin/bash
# x.sh <xdotool args...>
podman exec -e DISPLAY=:1 cadgui xdotool "$@"
