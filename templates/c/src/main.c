/* __NAME__ - a svcd plugin in C.
 *
 * Only the handler below is yours: the protocol shell in svcd_plugin.h owns the
 * readiness probe, the shutdown request, framing and the MCP result shape.
 */
#include <stdio.h>
#include <string.h>

#include "svcd_plugin.h"

/* Must equal `capabilities[].name` in package/manifest.json. */
static const char *const CAPABILITIES[] = {"echo", "info", NULL};

static svcd_outcome handle(const char *name, const char *args_json, void *user) {
    (void)user;

    if (strcmp(name, "echo") == 0) {
        char *message = svcd_string(args_json, "message");
        if (message == NULL) {
            return svcd_error("echo: missing \"message\"");
        }
        svcd_outcome outcome = svcd_text(message);
        free(message);
        return outcome;
    }
    if (strcmp(name, "info") == 0) {
        /* JSON, so `info.outputSchema` in the manifest has something to describe. */
        return svcd_text("{\"implementation\":\"c\",\"header_only\":true}");
    }
    return svcd_error("unknown tool");
}

int main(void) {
    svcd_context context;
    svcd_context_init(&context);
    svcd_log("info", "%s %s starting", context.id, context.version);
    return svcd_serve(&context, handle, NULL, CAPABILITIES);
}
