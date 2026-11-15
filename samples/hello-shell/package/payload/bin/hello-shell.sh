#!/bin/sh
# hello-shell - the reference plugin, in POSIX sh.
#
# Same five capabilities as the other reference samples: echo, stats, summarize
# (blob in), inspect_file (file package in), report (blob out). The protocol
# shell lives in ../lib/svcd-plugin.sh.
set -u

HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
. "$HERE/../lib/svcd-plugin.sh"

STARTED=$(svcd_now)
CALLS=0

handle() {
    name=$1
    args=$2
    CALLS=$((CALLS + 1))

    case "$name" in
    echo)
        message=$(svcd_field "$args" message)
        if [ -z "$message" ]; then
            echo 'echo: "message" is required' >&2
            return 1
        fi
        printf '%s' "$message"
        ;;
    stats)
        uptime=$(( $(svcd_now) - STARTED ))
        printf '{"calls":%s,"uptime_secs":%s,"pid":%s}' "$CALLS" "$uptime" "$$"
        ;;
    summarize)
        # The host replaced {"$blob": id} with {path, size, …}.
        path=$(svcd_field "$args" path)
        if [ -z "$path" ]; then
            echo 'summarize: "input" must be a blob argument ({$blob: …})' >&2
            return 1
        fi
        # awk counts a final line without a trailing newline; wc -l would not.
        bytes=$(wc -c < "$path")
        set -- $(awk '{ lines++; words += NF } END { printf "%d %d", lines, words }' "$path")
        printf '%s bytes, %s lines, %s words' "$bytes" "$1" "$2"
        ;;
    inspect_file)
        dir=$(svcd_field "$args" dir)
        manifest=$(svcd_field "$args" manifest)
        label=$(svcd_field "$args" name)
        if [ -z "$dir" ] || [ -z "$manifest" ]; then
            echo 'inspect_file: "input" must be a file argument ({$file: …})' >&2
            return 1
        fi
        total=0
        count=0
        for number in $(svcd_json_numbers "$manifest" partNumber); do
            part=$(printf '%s/part-%05d' "$dir" "$number")
            if [ ! -f "$part" ]; then
                echo 'inspect_file: a part listed by the manifest is missing' >&2
                return 1
            fi
            total=$((total + $(wc -c < "$part")))
            count=$((count + 1))
        done
        if [ "$count" -eq 0 ]; then
            echo 'inspect_file: the manifest lists no parts' >&2
            return 1
        fi
        printf '%s: %s bytes in %s part(s)' "${label:-file}" "$total" "$count"
        ;;
    report)
        if [ -z "${SVCD_TEMP_DIR:-}" ]; then
            echo 'report: SVCD_TEMP_DIR is not set' >&2
            return 1
        fi
        incoming="$SVCD_TEMP_DIR/incoming"
        mkdir -p "$incoming"
        target="$incoming/report.txt-$$-$(svcd_now)"
        {
            printf 'hello-shell report\n'
            printf 'calls: %s\n' "$CALLS"
            printf 'uptime_secs: %s\n' "$(( $(svcd_now) - STARTED ))"
        } > "$target"
        # The shell SDK turns these into {"$blobPath":…}.
        SVCD_BLOB_PATH=$target
        SVCD_BLOB_NAME=report.txt
        SVCD_BLOB_MIME='text/plain; charset=utf-8'
        ;;
    *)
        echo "unknown tool: $name" >&2
        return 1
        ;;
    esac
}

svcd_plugin_serve handle
