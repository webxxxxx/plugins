/* hello-c - the reference plugin, in C.
 *
 * Same five capabilities as `samples/hello`, so the two can be compared line by
 * line: echo, stats, summarize (blob in), inspect_file (file package in),
 * report (blob out). The protocol shell lives in svcd_plugin.h.
 */
#include <stdio.h>
#include <string.h>
#include <time.h>

#ifdef _WIN32
#include <process.h>
#define svcd_pid() _getpid()
#else
#include <unistd.h>
#define svcd_pid() getpid()
#endif

#include "svcd_plugin.h"

static const char *const CAPABILITIES[] = {"echo", "stats", "summarize", "inspect_file", "report",
                                           NULL};

static long calls = 0;
static long started = 0;

/* Stream a resolved `$blob` and count its bytes, lines and words. */
static svcd_outcome summarize(const char *args_json) {
    char *path = svcd_string(args_json, "path");
    if (path == NULL) {
        return svcd_error("summarize: \"input\" must be a blob argument ({$blob: …})");
    }
    FILE *file = fopen(path, "rb");
    if (file == NULL) {
        free(path);
        return svcd_error("summarize: cannot open the blob");
    }

    long bytes = 0, lines = 0, words = 0;
    int in_word = 0;
    int last = '\n';
    int value;
    while ((value = fgetc(file)) != EOF) {
        bytes++;
        if (value == '\n') {
            lines++;
        }
        if (value == ' ' || value == '\n' || value == '\t' || value == '\r') {
            in_word = 0;
        } else if (!in_word) {
            in_word = 1;
            words++;
        }
        last = value;
    }
    // A final line without a trailing newline still counts as a line, which is
    // what awk / ReadLine report in the other samples.
    if (bytes > 0 && last != '\n') {
        lines++;
    }
    fclose(file);
    free(path);

    char buffer[256];
    snprintf(buffer, sizeof buffer, "%ld bytes, %ld lines, %ld words", bytes, lines, words);
    return svcd_text(buffer);
}

/* Read the parts a file package declares and sum their sizes. */
static svcd_outcome inspect_file(const char *args_json) {
    char *manifest = svcd_string(args_json, "manifest");
    char *dir = svcd_string(args_json, "dir");
    char *name = svcd_string(args_json, "name");
    if (manifest == NULL || dir == NULL) {
        free(manifest);
        free(dir);
        free(name);
        return svcd_error("inspect_file: \"input\" must be a file argument ({$file: …})");
    }

    size_t length = 0;
    char *json = svcd_read_file(manifest, &length);
    if (json == NULL) {
        free(manifest);
        free(dir);
        free(name);
        return svcd_error("inspect_file: cannot read manifest.json");
    }

    long parts[1000];
    int count = svcd_json_numbers(json, "partNumber", parts, 1000);
    free(json);
    if (count == 0) {
        free(manifest);
        free(dir);
        free(name);
        return svcd_error("inspect_file: the manifest lists no parts");
    }

    long total = 0;
    for (int index = 0; index < count; index++) {
        char path[1024];
        snprintf(path, sizeof path, "%s/part-%05ld", dir, parts[index]);
        FILE *part = fopen(path, "rb");
        if (part == NULL) {
            free(manifest);
            free(dir);
            free(name);
            return svcd_error("inspect_file: a part listed by the manifest is missing");
        }
        fseek(part, 0, SEEK_END);
        total += ftell(part);
        fclose(part);
    }

    char buffer[512];
    snprintf(buffer, sizeof buffer, "%s: %ld bytes in %d part(s)", name == NULL ? "file" : name,
             total, count);
    free(manifest);
    free(dir);
    free(name);
    return svcd_text(buffer);
}

/* Produce a small report as a blob the host stores and links. */
static svcd_outcome report(const svcd_context *context) {
    char path[1024];
    if (svcd_incoming_path(context, "report.txt", path, sizeof path) != 0) {
        return svcd_error("report: SVCD_TEMP_DIR is not set");
    }
    FILE *file = fopen(path, "wb");
    if (file == NULL) {
        return svcd_error("report: cannot create the result file");
    }
    fprintf(file, "hello-c report\ncalls: %ld\nuptime_secs: %ld\npid: %ld\n", calls,
            svcd_now() - started, (long)svcd_pid());
    fflush(file);
    fclose(file);
    return svcd_blob(path, "report.txt", "text/plain; charset=utf-8");
}

static svcd_outcome handle(const char *name, const char *args_json, void *user) {
    calls++;
    const svcd_context *context = (const svcd_context *)user;

    if (strcmp(name, "echo") == 0) {
        char *message = svcd_string(args_json, "message");
        if (message == NULL) {
            return svcd_error("echo: \"message\" is required");
        }
        svcd_outcome outcome = svcd_text(message);
        free(message);
        return outcome;
    }
    if (strcmp(name, "stats") == 0) {
        char buffer[256];
        snprintf(buffer, sizeof buffer, "{\"calls\":%ld,\"uptime_secs\":%ld,\"pid\":%ld}", calls,
                 svcd_now() - started, (long)svcd_pid());
        return svcd_text(buffer);
    }
    if (strcmp(name, "summarize") == 0) {
        return summarize(args_json);
    }
    if (strcmp(name, "inspect_file") == 0) {
        return inspect_file(args_json);
    }
    if (strcmp(name, "report") == 0) {
        return report(context);
    }
    return svcd_error("unknown tool");
}

int main(void) {
    svcd_context context;
    svcd_context_init(&context);
    started = svcd_now();
    svcd_log("info", "%s %s starting (temp: %s)", context.id, context.version, context.temp_dir);
    return svcd_serve(&context, handle, &context, CAPABILITIES);
}
