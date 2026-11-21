#!/bin/bash
# Launch Soundpad inside the hidden "spdesk" Wine desktop. Extra args go to Soundpad.exe.
export WINEPREFIX=/home/user/Projects/clipdeck/re/wine/prefix WINEDEBUG=-all WINEDLLOVERRIDES="winemenubuilder.exe=d;mscoree=d;mshtml=d"
unset WAYLAND_DISPLAY
cd "$WINEPREFIX/drive_c/Program Files/Soundpad" || exit 1
exec wine explorer /desktop=spdesk,1560x960 'C:\Program Files\Soundpad\Soundpad.exe' "$@"
