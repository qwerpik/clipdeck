#!/bin/bash
# key.sh MODS KEY [shot-name]  -> send a key to the hidden Wine desktop window, optional screenshot
A=$(hyprctl clients -j | python3 -I -c "import json,sys;print([x['address'] for x in json.load(sys.stdin) if x['class']=='explorer.exe'][0])")
hyprctl eval "hl.dispatch(hl.dsp.send_shortcut({ mods = '$1', key = '$2', window = 'address:$A' }))" >/dev/null
timeout 1.5 tail -f /dev/null
hyprctl monitors -j | python3 -I -c "import json,sys;d={x['name']:x['focused'] for x in json.load(sys.stdin)};print('FOCUS-WARN' if d.get('GLASS') else 'ok')"
[ -n "$3" ] && grim -o GLASS "/home/user/Projects/clipdeck/re/samples/screens/$3.png"
true
