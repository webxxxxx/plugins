#!/bin/sh
# svcd-plugin.sh - the protocol shell for shell plugins.
#
# The control channel is newline-delimited JSON-RPC 2.0 on stdin/stdout. This
# library owns everything the protocol demands, so a plugin author writes one
# handler and nothing else:
#
#   handle() {
#     case "$1" in
#       uname) uname -a ;;
#       *)     return 1 ;;            # the shell reports isError for you
#     esac
#   }
#   svcd_plugin_serve handle
#
# The handler receives $1 = capability name and $2 = the raw `arguments` JSON
# object. Whatever it prints on stdout becomes the capability's text result;
# its stderr goes straight to svcd's log, so log with `echo ... >&2`.
#
# Scope: this shell is for wrapping existing CLIs whose output is text. There is
# no JSON parser in POSIX sh; a plugin that needs to dig into `arguments` beyond
# a flat string field should be written in Rust instead.

# One frame may not exceed this many bytes (the host enforces the same limit).
SVCD_MAX_FRAME_BYTES=1048576

# Escape stdin text for use inside a JSON string literal.
svcd_json_escape() {
    LC_ALL=C sed -e 's/\\/\\\\/g' \
                   -e 's/"/\\"/g' \
                   -e 's/	/\\t/g' \
                   -e 's//\\r/g' |
    LC_ALL=C awk 'BEGIN { ORS = "" } { if (NR > 1) printf "\\n"; printf "%s", $0 }'
}

# The FIRST string field named $2. The dispatcher needs this for `name`: the
# capability name comes before the handler arguments, which may carry their own
# `"name"` field (a file's display name, for instance).
svcd_field_first() {
    printf '%s\n' "$1" | LC_ALL=C awk -v key="\"$2\"" '
        {
            at = index($0, key)
            if (at == 0) exit
            rest = substr($0, at + length(key))
            sub(/^[ \t]*:[ \t]*"/, "", rest)
            sub(/".*$/, "", rest)
            gsub(/\\\\/, "\\", rest)
            gsub(/\\"/, "\"", rest)
            print rest
            exit
        }'
}

# First string field named $2 in the frame $1. The frames we parse are flat, so
# a greedy match on the last occurrence is acceptable and documented. Escaped
# backslashes and quotes are decoded, because Windows paths arrive as `D:\\…`.
svcd_field() {
    printf '%s\n' "$1" |
        LC_ALL=C sed -n 's/.*"'"$2"'"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' |
        head -n 1 |
        LC_ALL=C sed -e 's/\\\\/\\/g' -e 's/\\"/"/g'
}

# First raw (unquoted) field named $2, for `id` and numbers.
svcd_raw_field() {
    printf '%s\n' "$1" | LC_ALL=C sed -n 's/.*"'"$2"'"[[:space:]]*:[[:space:]]*\([^,}]*\).*/\1/p' | head -n 1
}

svcd_reply_result() {
    [ -z "$1" ] && return 0
    printf '{"jsonrpc":"2.0","id":%s,"result":%s}\n' "$1" "$2"
}

svcd_reply_error() {
    [ -z "$1" ] && return 0
    _svcd_message=$(printf '%s' "$3" | svcd_json_escape)
    printf '{"jsonrpc":"2.0","id":%s,"error":{"code":%s,"message":"%s"}}\n' "$1" "$2" "$_svcd_message"
}

# Push a notification to the host; it mirrors it to UI subscribers over SSE and
# WebSocket. $1 = method, $2 = params as a raw JSON value.
svcd_notify() {
    printf '{"jsonrpc":"2.0","method":"%s","params":%s}\n' "$1" "$2"
}

# Override this to feed `svc status`.
svcd_status() {
    printf 'running'
}

# Read one flat key from the user-editable config copy.
svcd_config_get() {
    [ -n "${SVCD_CONFIG:-}" ] && [ -f "${SVCD_CONFIG}" ] || return 0
    svcd_field "$(cat "${SVCD_CONFIG}")" "$1"
}

# Seconds since the epoch, for uptime counters.
svcd_now() {
    date +%s
}

# Print every integer stored under `"key": <n>` in a JSON file, one per line.
# Shell has no JSON parser; this is the one array-shaped read we support, and it
# covers `manifest.json`'s partNumber list.
svcd_json_numbers() {
    LC_ALL=C awk -v key="\"$2\"" '
        {
            rest = $0
            while ((at = index(rest, key)) > 0) {
                rest = substr(rest, at + length(key))
                sub(/^[ \t]*:[ \t]*/, "", rest)
                if (match(rest, /^[0-9]+/) > 0) {
                    print substr(rest, 1, RLENGTH)
                    rest = substr(rest, RLENGTH + 1)
                } else {
                    rest = substr(rest, 2)
                }
            }
        }' "$1"
}

# Serve the protocol until stdin closes or the host asks us to stop.
svcd_plugin_serve() {
    _svcd_handler=$1
    trap 'exit 0' TERM INT

    while IFS= read -r _svcd_frame || [ -n "$_svcd_frame" ]; do
        [ -z "$_svcd_frame" ] && continue

        _svcd_method=$(svcd_field "$_svcd_frame" method)
        _svcd_id=$(svcd_raw_field "$_svcd_frame" id)

        case "$_svcd_method" in
        svcd.ping)
            svcd_reply_result "$_svcd_id" '{"ok":true}'
            ;;
        svcd.status)
            _svcd_summary=$(svcd_status | svcd_json_escape)
            svcd_reply_result "$_svcd_id" "{\"summary\":\"$_svcd_summary\",\"details\":{}}"
            ;;
        svcd.shutdown)
            svcd_reply_result "$_svcd_id" 'null'
            exit 0
            ;;
        tools/call)
            _svcd_name=$(svcd_field_first "$_svcd_frame" name)
            _svcd_args=$(printf '%s\n' "$_svcd_frame" |
                LC_ALL=C sed -n 's/.*"arguments"[[:space:]]*:[[:space:]]*\({.*}\).*/\1/p' | head -n 1)
            [ -z "$_svcd_args" ] && _svcd_args='{}'

            # fd 3 is the real stderr: the handler's own logs bypass the capture.
            # The handler runs in *this* shell (stdout to a temp file, not a
            # command substitution), so a handler that wants to return a blob can
            # set SVCD_BLOB_PATH and have the loop see it.
            SVCD_BLOB_PATH=""
            SVCD_BLOB_NAME=""
            SVCD_BLOB_MIME=""
            _svcd_out_file="${TMPDIR:-/tmp}/svcd-reply.$$"
            "$_svcd_handler" "$_svcd_name" "$_svcd_args" > "$_svcd_out_file" 2>&3
            _svcd_code=$?
            _svcd_out=$(cat "$_svcd_out_file")
            rm -f "$_svcd_out_file"

            if [ -n "$SVCD_BLOB_PATH" ]; then
                svcd_reply_result "$_svcd_id" \
                    "{\"\$blobPath\":\"$(printf '%s' "$SVCD_BLOB_PATH" | svcd_json_escape)\",\"name\":\"$(printf '%s' "${SVCD_BLOB_NAME:-blob}" | svcd_json_escape)\",\"mimeType\":\"$(printf '%s' "${SVCD_BLOB_MIME:-application/octet-stream}" | svcd_json_escape)\"}"
                continue
            fi

            _svcd_text=$(printf '%s' "$_svcd_out" | svcd_json_escape)
            if [ "$_svcd_code" -eq 0 ]; then
                svcd_reply_result "$_svcd_id" \
                    "{\"content\":[{\"type\":\"text\",\"text\":\"$_svcd_text\"}],\"isError\":false}"
            else
                svcd_reply_result "$_svcd_id" \
                    "{\"content\":[{\"type\":\"text\",\"text\":\"$_svcd_text\"}],\"isError\":true}"
            fi
            ;;
        '')
            # A notification we do not know about: no reply is expected.
            ;;
        *)
            [ -n "$_svcd_id" ] && svcd_reply_error "$_svcd_id" -32601 "method not found: $_svcd_method"
            ;;
        esac
    done
}

# The handler needs the real stderr even though its stdout is captured, so open
# fd 3 before the loop starts.
exec 3>&2
