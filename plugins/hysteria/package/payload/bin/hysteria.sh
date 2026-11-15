#!/bin/sh
# hysteria - a shell plugin for svcd.
#
# Only the cases below are yours. The library next door owns the protocol: the
# readiness probe, the shutdown request, framing, the MCP result shape.
set -u

HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
. "$HERE/../lib/svcd-plugin.sh"

# $1 = capability name, $2 = raw `arguments` JSON object.
# Print the text result on stdout; log with `>&2` (that becomes svcd's log).
handle() {
    case "$1" in
    uname)
        uname -a
        ;;
    date)
        date -u
        ;;
    *)
        echo "unknown tool: $1" >&2
        return 1
        ;;
    esac
}

svcd_plugin_serve handle
