#!/bin/bash
# Run winctl.exe inside the "spdesk" Wine virtual desktop; print its output.
export WINEPREFIX=/home/user/Projects/clipdeck/re/wine/prefix WINEDEBUG=-all
unset WAYLAND_DISPLAY
O=/home/user/Projects/clipdeck/re/tools/winctl.out
rm -f "$O"
timeout 20 wine explorer /desktop=spdesk,1560x960 /home/user/Projects/clipdeck/re/tools/winctl.exe "$@" >/dev/null 2>&1
for i in 1 2 3 4 5 6 7 8 9 10; do [ -s "$O" ] && break; timeout 0.3 tail -f /dev/null; done
timeout 0.5 tail -f /dev/null
cat "$O" 2>/dev/null
