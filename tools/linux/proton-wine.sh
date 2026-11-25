#!/bin/sh
# Runs a Windows console program with a Proton build's wine outside Steam,
# in a prefix of its own, with its output on this terminal: what the tests and `victory`
# are run with on Linux (docs/linux.md).
#
#     tools/linux/proton-wine.sh target/x86_64-pc-windows-msvc/release/victory.exe config
#
# PROTON: the Proton build's folder (the one that holds the `proton`
# script). Unset: Proton Experimental if Steam has it (its vkd3d-proton is
# the newest, and takes most of a title's shaders: docs/linux.md), else
# the newest build found where Steam keeps them.
# VICTORY_PROTON_DATA: where Proton keeps the prefix (default
# ~/.local/share/victory/proton).
set -eu
if [ -z "${PROTON:-}" ] && [ -f "$HOME/.steam/steam/steamapps/common/Proton - Experimental/proton" ]; then
    PROTON="$HOME/.steam/steam/steamapps/common/Proton - Experimental"
fi
if [ -z "${PROTON:-}" ]; then
    newest=0
    for d in "$HOME"/.steam/steam/compatibilitytools.d/* /usr/share/steam/compatibilitytools.d/* \
        "$HOME"/.steam/steam/steamapps/common/Proton*; do
        [ -f "$d/proton" ] && [ -f "$d/version" ] || continue
        # A build's `version` begins with the time it was made.
        made=$(cut -d' ' -f1 "$d/version")
        case $made in '' | *[!0-9]*) continue ;; esac
        if [ "$made" -gt "$newest" ]; then newest=$made PROTON=$d; fi
    done
fi
[ -n "${PROTON:-}" ] || { echo "proton-wine.sh: no Proton found; set PROTON to its folder" >&2; exit 1; }
export STEAM_COMPAT_DATA_PATH="${VICTORY_PROTON_DATA:-$HOME/.local/share/victory/proton}"
export STEAM_COMPAT_CLIENT_INSTALL_PATH="${STEAM_COMPAT_CLIENT_INSTALL_PATH:-$HOME/.steam/steam}"
export SteamAppId="${SteamAppId:-0}" SteamGameId="${SteamGameId:-0}"
export WINEDEBUG="${WINEDEBUG:--all}"
mkdir -p "$STEAM_COMPAT_DATA_PATH"
sys=$STEAM_COMPAT_DATA_PATH/pfx/drive_c/windows/system32
if [ ! -f "$STEAM_COMPAT_DATA_PATH/.made-by" ] || [ "$(cat "$STEAM_COMPAT_DATA_PATH/.made-by")" != "$PROTON" ]; then
    # A first `run` has Proton make the prefix whole (with `runinprefix`
    # alone it kept Wine's own Direct3D 12, which knows no shader model 6).
    python3 "$PROTON/proton" run cmd.exe /c exit >"$STEAM_COMPAT_DATA_PATH/setup.log" 2>&1 ||
        { echo "proton-wine.sh: Proton could not make the prefix; see $STEAM_COMPAT_DATA_PATH/setup.log" >&2; exit 1; }
    # vkd3d-proton's d3d12core.dll is megabytes; Wine's own a stub.
    [ "$(wc -c < "$sys/d3d12core.dll" 2>/dev/null || echo 0)" -gt 1000000 ] ||
        { echo "proton-wine.sh: the prefix has no vkd3d-proton; see $STEAM_COMPAT_DATA_PATH/setup.log" >&2; exit 1; }
    echo "$PROTON" > "$STEAM_COMPAT_DATA_PATH/.made-by"
fi
# Proton's wine itself, not its launcher: the program keeps the terminal and
# its exit code is ours. Direct3D 12 and DXGI are the prefix's own files,
# which Proton put there (vkd3d-proton and DXVK), not Wine's.
files=$PROTON/files
[ -x "$files/bin/wine" ] || files=$PROTON/dist
export WINEPREFIX="$STEAM_COMPAT_DATA_PATH/pfx"
export WINEDLLPATH="$files/lib/vkd3d:$files/lib/wine${WINEDLLPATH:+:$WINEDLLPATH}"
# No debugger of Wine's either: a program that faults ends, as it does on
# Windows, where Wine would have its debugger attach and wait.
export WINEDLLOVERRIDES="d3d12,d3d12core,dxgi=n;winemenubuilder.exe,winedbg.exe=d${WINEDLLOVERRIDES:+;$WINEDLLOVERRIDES}"
exec "$files/bin/wine" "$@"
