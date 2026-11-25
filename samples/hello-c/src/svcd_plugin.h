/* svcd_plugin.h — the protocol shell for C plugins.
 *
 * The control channel is newline-delimited JSON-RPC 2.0 on stdin/stdout, with
 * no handshake. This header owns everything the protocol demands, so a plugin
 * author writes one handler and nothing else:
 *
 *     static svcd_outcome handle(const char *name, const char *args_json, void *user);
 *     int main(void) {
 *         svcd_context context;
 *         svcd_context_init(&context);
 *         return svcd_serve(&context, handle, NULL, CAPABILITIES);
 *     }
 *
 * stdout is the protocol. Log with svcd_log(), which writes to stderr — the
 * host captures it and rotates it into <logs>/<id>.log.
 *
 * Scope: a flat JSON scanner, not a general JSON parser. `arguments` is handed
 * to the handler as a raw JSON substring, so capabilities that need real JSON
 * handling should be written in Rust.
 */
#ifndef SVCD_PLUGIN_H
#define SVCD_PLUGIN_H

#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#ifdef _WIN32
#include <direct.h>
#define svcd_mkdir(path) _mkdir(path)
#else
#include <sys/stat.h>
#define svcd_mkdir(path) mkdir(path, 0755)
#endif

#define SVCD_MAX_FRAME_BYTES (1024 * 1024)

typedef struct {
    char id[64];
    char version[32];
    char config[512];
    /* The install directory (`SVCD_PLUGIN_DIR`) and `<plugin_dir>/data` under it. */
    char plugin_dir[512];
    char data_dir[512];
    char log_dir[512];
    char temp_dir[512];
} svcd_context;

/* kind: 0 = text result, 1 = error result, 2 = blob result (`$blobPath`). */
typedef struct {
    int kind;
    char *text;
    char *path;
    char *name;
    char *mime;
} svcd_outcome;

typedef svcd_outcome (*svcd_handler)(const char *name, const char *args_json, void *user);

/* Allocate an outcome. The runtime frees the strings. */
static svcd_outcome svcd_text(const char *text);
static svcd_outcome svcd_error(const char *text);
/* Hand the host a file written under `$SVCD_TEMP_DIR/incoming/`. */
static svcd_outcome svcd_blob(const char *path, const char *name, const char *mime);

/* Read the SVCD_* environment the host injects. Never fails. */
static void svcd_context_init(svcd_context *context);

/* Structured-ish logging to stderr. */
static void svcd_log(const char *level, const char *fmt, ...);

/* Push a notification to the host (it mirrors it to UI subscribers). */
static int svcd_notify(const char *method, const char *params_json);

/* Serve the protocol until stdin closes or the host asks us to stop.
 * `capabilities` is a NULL-terminated list of capability names. */
static int svcd_serve(svcd_context *context, svcd_handler handler, void *user,
                      const char *const *capabilities);

/* ------------------------------------------------------------------ */

static char *svcd_strdup(const char *text) {
    size_t length = strlen(text);
    char *copy = (char *)malloc(length + 1);
    if (copy != NULL) {
        memcpy(copy, text, length + 1);
    }
    return copy;
}

static svcd_outcome svcd_text(const char *text) {
    svcd_outcome outcome;
    outcome.kind = 0;
    outcome.text = svcd_strdup(text == NULL ? "" : text);
    outcome.path = NULL;
    outcome.name = NULL;
    outcome.mime = NULL;
    return outcome;
}

static svcd_outcome svcd_error(const char *text) {
    svcd_outcome outcome;
    outcome.kind = 1;
    outcome.text = svcd_strdup(text == NULL ? "" : text);
    outcome.path = NULL;
    outcome.name = NULL;
    outcome.mime = NULL;
    return outcome;
}

static svcd_outcome svcd_blob(const char *path, const char *name, const char *mime) {
    svcd_outcome outcome;
    outcome.kind = 2;
    outcome.text = NULL;
    outcome.path = svcd_strdup(path == NULL ? "" : path);
    outcome.name = svcd_strdup(name == NULL ? "blob" : name);
    outcome.mime = svcd_strdup(mime == NULL ? "application/octet-stream" : mime);
    return outcome;
}

static void svcd_outcome_free(svcd_outcome *outcome) {
    free(outcome->text);
    free(outcome->path);
    free(outcome->name);
    free(outcome->mime);
    outcome->text = NULL;
    outcome->path = NULL;
    outcome->name = NULL;
    outcome->mime = NULL;
}

static void svcd_copy_env(char *slot, size_t size, const char *name, const char *fallback) {
    const char *value = getenv(name);
    if (value == NULL || value[0] == '\0') {
        value = fallback;
    }
    snprintf(slot, size, "%s", value);
}

static void svcd_context_init(svcd_context *context) {
    svcd_copy_env(context->id, sizeof context->id, "SVCD_PLUGIN_ID", "plugin");
    svcd_copy_env(context->version, sizeof context->version, "SVCD_PLUGIN_VERSION", "0.0.0");
    svcd_copy_env(context->config, sizeof context->config, "SVCD_CONFIG", "");
    /* A plugin child gets its install directory, the log directory and the
     * transient root; state goes to <plugin_dir>/data, never anywhere else. */
    svcd_copy_env(context->plugin_dir, sizeof context->plugin_dir, "SVCD_PLUGIN_DIR", "");
    if (context->plugin_dir[0] != '\0') {
        /* The precision keeps the whole suffix inside the buffer, so this can
         * never truncate (and gcc's -Wformat-truncation agrees). */
        snprintf(context->data_dir, sizeof context->data_dir, "%.*s/data",
                 (int)(sizeof context->data_dir - sizeof "/data"), context->plugin_dir);
    } else {
        snprintf(context->data_dir, sizeof context->data_dir, "data");
    }
    svcd_copy_env(context->log_dir, sizeof context->log_dir, "SVCD_LOG_DIR", "");
    svcd_copy_env(context->temp_dir, sizeof context->temp_dir, "SVCD_TEMP_DIR", "");
}

static void svcd_log(const char *level, const char *fmt, ...) {
    va_list args;
    fputs("[", stderr);
    fputs(level, stderr);
    fputs("] ", stderr);
    va_start(args, fmt);
    vfprintf(stderr, fmt, args);
    va_end(args);
    fputc('\n', stderr);
    fflush(stderr);
}

/* --- a flat JSON scanner ------------------------------------------- */

/* Find `"key"` and return a pointer to the first byte of its value, or NULL. */
static const char *svcd_find(const char *json, const char *key) {
    char pattern[96];
    snprintf(pattern, sizeof pattern, "\"%s\"", key);
    const char *at = strstr(json, pattern);
    if (at == NULL) {
        return NULL;
    }
    at += strlen(pattern);
    while (*at == ' ' || *at == '\t') {
        at++;
    }
    if (*at != ':') {
        return NULL;
    }
    at++;
    while (*at == ' ' || *at == '\t') {
        at++;
    }
    return at;
}

/* Copy a JSON string value, unescaping \" \\ \n \r \t. Caller frees. */
static char *svcd_string(const char *json, const char *key) {
    const char *at = svcd_find(json, key);
    if (at == NULL || *at != '"') {
        return NULL;
    }
    at++;
    size_t capacity = strlen(at) + 1;
    char *out = (char *)malloc(capacity);
    if (out == NULL) {
        return NULL;
    }
    size_t used = 0;
    while (*at != '\0' && *at != '"') {
        char value = *at++;
        if (value == '\\' && *at != '\0') {
            char escape = *at++;
            switch (escape) {
            case 'n':
                value = '\n';
                break;
            case 'r':
                value = '\r';
                break;
            case 't':
                value = '\t';
                break;
            default:
                value = escape;
                break;
            }
        }
        out[used++] = value;
    }
    out[used] = '\0';
    return out;
}

/* Copy the raw token for `id` (string or number) so replies echo it verbatim. */
static char *svcd_raw(const char *json, const char *key) {
    const char *at = svcd_find(json, key);
    if (at == NULL) {
        return NULL;
    }
    const char *end = at;
    if (*end == '"') {
        end++;
        while (*end != '\0' && *end != '"') {
            if (*end == '\\' && end[1] != '\0') {
                end++;
            }
            end++;
        }
        if (*end == '"') {
            end++;
        }
    } else {
        while (*end != '\0' && *end != ',' && *end != '}' && *end != ' ' && *end != '\n') {
            end++;
        }
    }
    size_t length = (size_t)(end - at);
    char *out = (char *)malloc(length + 1);
    if (out == NULL) {
        return NULL;
    }
    memcpy(out, at, length);
    out[length] = '\0';
    return out;
}

/* Copy the raw object/array value for `key`, matching nested braces. */
static char *svcd_raw_value(const char *json, const char *key) {
    const char *at = svcd_find(json, key);
    if (at == NULL) {
        return NULL;
    }
    if (*at != '{' && *at != '[') {
        return NULL;
    }
    const char *open = at;
    char close = (*at == '{') ? '}' : ']';
    int depth = 0;
    int in_string = 0;
    while (*at != '\0') {
        char value = *at;
        if (in_string) {
            if (value == '\\' && at[1] != '\0') {
                at += 2;
                continue;
            }
            if (value == '"') {
                in_string = 0;
            }
        } else if (value == '"') {
            in_string = 1;
        } else if (value == *open) {
            depth++;
        } else if (value == close) {
            depth--;
            if (depth == 0) {
                at++;
                break;
            }
        }
        at++;
    }
    size_t length = (size_t)(at - open);
    char *out = (char *)malloc(length + 1);
    if (out == NULL) {
        return NULL;
    }
    memcpy(out, open, length);
    out[length] = '\0';
    return out;
}

/* Write a JSON string literal with the escapes the protocol needs. */
static void svcd_write_string(FILE *out, const char *text) {
    fputc('"', out);
    for (const unsigned char *at = (const unsigned char *)text; *at != '\0'; at++) {
        switch (*at) {
        case '"':
            fputs("\\\"", out);
            break;
        case '\\':
            fputs("\\\\", out);
            break;
        case '\n':
            fputs("\\n", out);
            break;
        case '\r':
            fputs("\\r", out);
            break;
        case '\t':
            fputs("\\t", out);
            break;
        default:
            if (*at < 0x20) {
                fprintf(out, "\\u%04x", *at);
            } else {
                fputc(*at, out);
            }
            break;
        }
    }
    fputc('"', out);
}

/* --- protocol ------------------------------------------------------ */

static void svcd_reply_result(const char *id, const char *result_json) {
    if (id == NULL) {
        return;
    }
    printf("{\"jsonrpc\":\"2.0\",\"id\":%s,\"result\":%s}\n", id, result_json);
    fflush(stdout);
}

static void svcd_reply_error(const char *id, int code, const char *message) {
    printf("{\"jsonrpc\":\"2.0\",\"id\":%s,\"error\":{\"code\":%d,\"message\":", id == NULL ? "null" : id,
           code);
    svcd_write_string(stdout, message);
    fputs("}}\n", stdout);
    fflush(stdout);
}

static void svcd_reply_outcome(const char *id, svcd_outcome outcome) {
    if (outcome.kind == 2) {
        printf("{\"jsonrpc\":\"2.0\",\"id\":%s,\"result\":{\"$blobPath\":",
               id == NULL ? "null" : id);
        svcd_write_string(stdout, outcome.path == NULL ? "" : outcome.path);
        fputs(",\"name\":", stdout);
        svcd_write_string(stdout, outcome.name == NULL ? "blob" : outcome.name);
        fputs(",\"mimeType\":", stdout);
        svcd_write_string(stdout, outcome.mime == NULL ? "application/octet-stream" : outcome.mime);
        fputs("}}\n", stdout);
        fflush(stdout);
        svcd_outcome_free(&outcome);
        return;
    }

    printf("{\"jsonrpc\":\"2.0\",\"id\":%s,\"result\":{\"content\":[{\"type\":\"text\",\"text\":",
           id == NULL ? "null" : id);
    svcd_write_string(stdout, outcome.text == NULL ? "" : outcome.text);
    printf("}],\"isError\":%s}}\n", outcome.kind == 1 ? "true" : "false");
    fflush(stdout);
    svcd_outcome_free(&outcome);
}

static int svcd_known(const char *const *capabilities, const char *name) {
    if (capabilities == NULL || name == NULL) {
        return 0;
    }
    for (const char *const *entry = capabilities; *entry != NULL; entry++) {
        if (strcmp(*entry, name) == 0) {
            return 1;
        }
    }
    return 0;
}

/* Read one LF-terminated line. Returns NULL at EOF. Caller frees. */
static char *svcd_read_line(FILE *in) {
    size_t capacity = 4096;
    size_t used = 0;
    char *buffer = (char *)malloc(capacity);
    if (buffer == NULL) {
        return NULL;
    }
    int any = 0;
    for (;;) {
        int value = fgetc(in);
        if (value == EOF) {
            break;
        }
        any = 1;
        if (value == '\n') {
            break;
        }
        if (used + 1 >= capacity) {
            if (capacity >= SVCD_MAX_FRAME_BYTES) {
                free(buffer);
                return NULL;
            }
            capacity *= 2;
            char *grown = (char *)realloc(buffer, capacity);
            if (grown == NULL) {
                free(buffer);
                return NULL;
            }
            buffer = grown;
        }
        buffer[used++] = (char)value;
    }
    if (!any) {
        free(buffer);
        return NULL;
    }
    while (used > 0 && (buffer[used - 1] == '\r')) {
        used--;
    }
    buffer[used] = '\0';
    return buffer;
}

static int svcd_serve(svcd_context *context, svcd_handler handler, void *user,
                      const char *const *capabilities) {
    (void)context;
    char *line;
    while ((line = svcd_read_line(stdin)) != NULL) {
        if (line[0] == '\0') {
            free(line);
            continue;
        }
        char *method = svcd_string(line, "method");
        char *id = svcd_raw(line, "id");

        if (method == NULL) {
            svcd_reply_error(NULL, -32600, "invalid request");
        } else if (strcmp(method, "svcd.ping") == 0) {
            svcd_reply_result(id, "{\"ok\":true}");
        } else if (strcmp(method, "svcd.status") == 0) {
            svcd_reply_result(id, "{\"summary\":\"running\",\"details\":{}}");
        } else if (strcmp(method, "svcd.shutdown") == 0) {
            svcd_reply_result(id, "null");
            free(method);
            free(id);
            free(line);
            return 0;
        } else if (strcmp(method, "tools/call") == 0) {
            char *name = svcd_string(line, "name");
            char *arguments = svcd_raw_value(line, "arguments");
            if (name == NULL || !svcd_known(capabilities, name)) {
                svcd_reply_error(id, -32602, "unknown tool");
            } else {
                svcd_outcome outcome = handler(name, arguments == NULL ? "{}" : arguments, user);
                svcd_reply_outcome(id, outcome);
            }
            free(arguments);
            free(name);
        } else if (id != NULL) {
            svcd_reply_error(id, -32601, "method not found");
        }

        free(method);
        free(id);
        free(line);
    }
    return 0;
}

static int svcd_notify(const char *method, const char *params_json) {
    printf("{\"jsonrpc\":\"2.0\",\"method\":");
    svcd_write_string(stdout, method);
    printf(",\"params\":%s}\n", params_json == NULL ? "null" : params_json);
    fflush(stdout);
    return 0;
}

/* --- helpers a capability usually wants ------------------------------ */

/* Seconds since the epoch; a monotonic-enough start for an uptime counter. */
static long svcd_now(void) {
    return (long)time(NULL);
}

/* Read a whole file into a malloc'd buffer. Caller frees. */
static char *svcd_read_file(const char *path, size_t *size) {
    FILE *file = fopen(path, "rb");
    if (file == NULL) {
        return NULL;
    }
    if (fseek(file, 0, SEEK_END) != 0) {
        fclose(file);
        return NULL;
    }
    long length = ftell(file);
    if (length < 0) {
        fclose(file);
        return NULL;
    }
    rewind(file);
    char *buffer = (char *)malloc((size_t)length + 1);
    if (buffer == NULL) {
        fclose(file);
        return NULL;
    }
    size_t read = fread(buffer, 1, (size_t)length, file);
    fclose(file);
    buffer[read] = '\0';
    if (size != NULL) {
        *size = read;
    }
    return buffer;
}

/* Collect the integers stored under `"key": <n>` (e.g. partNumber). */
static int svcd_json_numbers(const char *json, const char *key, long *out, int max) {
    char pattern[96];
    snprintf(pattern, sizeof pattern, "\"%s\"", key);
    const char *at = json;
    int found = 0;
    while (found < max && (at = strstr(at, pattern)) != NULL) {
        at += strlen(pattern);
        const char *colon = strchr(at, ':');
        if (colon == NULL) {
            break;
        }
        at = colon + 1;
        while (*at == ' ' || *at == '\t') {
            at++;
        }
        long value = strtol(at, NULL, 10);
        out[found++] = value;
    }
    return found;
}

/* Build a unique path under `$SVCD_TEMP_DIR/incoming/` for a blob result. */
static int svcd_incoming_path(const svcd_context *context, const char *name, char *out, size_t size) {
    if (context->temp_dir[0] == '\0') {
        return -1;
    }
    char incoming[600];
    snprintf(incoming, sizeof incoming, "%s/incoming", context->temp_dir);
    svcd_mkdir(incoming);
    char stem[128];
    snprintf(stem, sizeof stem, "%s", name == NULL || name[0] == '\0' ? "blob" : name);
    for (char *at = stem; *at != '\0'; at++) {
        if (!((*at >= 'a' && *at <= 'z') || (*at >= 'A' && *at <= 'Z') || (*at >= '0' && *at <= '9') ||
              *at == '.' || *at == '-' || *at == '_')) {
            *at = '_';
        }
    }
    snprintf(out, size, "%s/%s-%ld-%ld", incoming, stem, svcd_now(), (long)rand());
    return 0;
}

#endif /* SVCD_PLUGIN_H */
