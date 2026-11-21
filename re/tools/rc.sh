#!/bin/bash
# Run one Soundpad remote-control command inside the throwaway prefix.
export WINEPREFIX=/home/user/Projects/clipdeck/re/wine/prefix WINEDEBUG=-all WINEDLLOVERRIDES="winemenubuilder.exe=d;mscoree=d;mshtml=d"
unset WAYLAND_DISPLAY
exec timeout 20 wine /home/user/Projects/clipdeck/re/tools/sprc.exe "$@" 2>&1 | tr -d '\r'
