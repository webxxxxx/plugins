#!/bin/sh
# __NAME__ - the Unix side of a script plugin.
#
# Only the cases below are yours: the library next door owns the protocol.
# `plugin-tools build` copies this file into payload/bin/ for Unix packages
# only; the Windows side lives in __NAME__.ps1 next door, and one of the two
# never enters a package.
set -u

HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
. "$HERE/../lib/svcd-plugin.sh"

# $1 = capability name, $2 = raw `arguments` JSON object.
handle() {
    case "$1" in
    uname)
        uname -a
        ;;
    date)
        printf '{"utc":"%s"}\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
        ;;
    *)
        echo "unknown tool: $1" >&2
        return 1
        ;;
    esac
}

svcd_plugin_serve handle
