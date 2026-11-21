#!/bin/bash
# gh.sh OUTFILE needle... | fn:ADDR   — query the persistent Ghidra project (no re-analysis)
export JAVA_HOME=$HOME/.local/opt/jdk-21.0.13.1+2 PATH=$HOME/.local/opt/jdk-21.0.13.1+2/bin:$PATH
G=$HOME/.local/opt/ghidra_12.1.4_PUBLIC
D=/home/user/Projects/clipdeck/re/ghidra
out=$1; shift
exec "$G/support/analyzeHeadless" "$D/proj" soundpad -process Soundpad.exe -noanalysis -readOnly \
  -scriptPath "$D/scripts" -postScript DecompRefs.java "$out" "$@" > "$D/last-run.log" 2>&1
