// plugin-ui.mjs - the browser side of a .spk plugin.
//
// A plugin never runs its own HTTP server. svcd reverse-proxies the package's
// `payload/web/` tree under /plugin-ui/<id>/ and exposes four things next to it:
//
//   ./rpc     POST JSON-RPC  tools/call        (method invoke)
//   ./events  SSE        id:<seq> event:message (server push)
//   ./ws      WebSocket  text frames            (server push, same stream)
//   ./        the static assets themselves
//
// Large payloads never travel through those frames (1 MiB cap). The host owns a
// transfer surface instead, and this file wraps both tiers:
//
//   /api/v1/blobs      one object, <= 512 MiB, resumable part sessions
//   /api/v1/file       a "file package" (parts + manifest), <= 16 GiB
//
// Auth is a path-scoped cookie plus an Origin check, both handled by the
// browser: this file never touches a token.
//
// The host's own API sits on the same origin as the page, so a plugin that has
// to do something to itself — restart after a startup-config change — uses the
// console's session cookie through `hostRequest` / `restartSelf` below; every
// write those make carries the host's `X-Svcd-Csrf: 1` marker (`withCsrf`). A
// capability hook is a short-lived process: it writes state and stops there,
// because anything it started would be orphaned the moment it exits.

const RPC = 'rpc';
const EVENTS = 'events';
const WS = 'ws';
const BLOBS = '/api/v1/blobs';
const FILES = '/api/v1/file';

/** Content addresses only; reject anything else before it reaches a URL. */
const CONTENT_ID = /^sha256:[0-9a-f]{64}$/;

const WRITE_METHODS = ['POST', 'PUT', 'PATCH', 'DELETE'];

/**
 * A cookie-authenticated write needs a same-origin `Origin` plus
 * `X-Svcd-Csrf: 1` (`src/server/http/model.rs::csrf_ok`). The browser adds
 * `Origin` to every non-GET request by itself, so only the marker header is
 * ours; a custom header on a same-origin request costs no preflight, and the
 * plugin page shares the console's origin, so its cookie is what authorises
 * the write. Without this the host answers `403 forbidden`.
 */
function withCsrf(init) {
  if (!WRITE_METHODS.includes(String(init.method ?? 'GET').toUpperCase())) return init;
  if (typeof Headers !== 'undefined' && init.headers instanceof Headers) {
    const headers = new Headers(init.headers);
    headers.set('x-svcd-csrf', '1');
    return { ...init, headers };
  }
  return { ...init, headers: { ...init.headers, 'x-svcd-csrf': '1' } };
}

export class PluginRpcError extends Error {
  constructor(code, message, data) {
    super(message);
    this.name = 'PluginRpcError';
    this.code = code;
    this.data = data;
  }
}

function baseHref() {
  return globalThis.location?.href ?? 'http://localhost/';
}

/** The plugin id from `/plugin-ui/<id>/...`; empty when the page is elsewhere. */
function pagePluginId() {
  const match = globalThis.location?.pathname?.match(/^\/plugin-ui\/([^/]+)\//);
  return match ? decodeURIComponent(match[1]) : '';
}

function parse(raw) {
  try {
    return JSON.parse(raw);
  } catch {
    return null;
  }
}

async function jsonRequest(url, init = {}) {
  const response = await fetch(url, withCsrf({ credentials: 'same-origin', ...init }));
  if (!response.ok) {
    const detail = await response.text().catch(() => '');
    throw new PluginRpcError(-32000, `HTTP ${response.status}${detail ? `: ${detail}` : ''}`);
  }
  return response.json();
}

async function putPart(partsUrl, partNumber, slice, attempts = 3) {
  let lastError = null;
  for (let attempt = 1; attempt <= attempts; attempt += 1) {
    try {
      return await jsonRequest(`${partsUrl}/${partNumber}`, {
        method: 'PUT',
        headers: { 'content-type': 'application/octet-stream' },
        body: slice,
      });
    } catch (error) {
      lastError = error;
      // The same part number may be re-uploaded; only the last attempt decides.
      await new Promise((resolve) => setTimeout(resolve, 250 * attempt));
    }
  }
  throw lastError;
}

function saveBlob(blob, filename) {
  if (typeof document === 'undefined') return;
  const href = URL.createObjectURL(blob);
  const anchor = document.createElement('a');
  anchor.href = href;
  anchor.download = filename;
  anchor.click();
  URL.revokeObjectURL(href);
}

/**
 * Open the plugin's UI surface.
 *
 * @param {{ base?: string }} [options] `base` defaults to the page's own
 *   directory, which is what the reverse proxy serves.
 */
export function pluginUI(options = {}) {
  const base = new URL(options.base ?? './', baseHref());
  let sequence = 0;

  const url = (path) => new URL(path, base).toString();

  /**
   * Call a capability and return the whole result, not just its text.
   *
   * A blob-producing capability answers with `structuredContent.$blob` plus a
   * `resource_link`, and the text alone would drop that; use this form when the
   * result shape matters (`invoke` is the text-only convenience).
   */
  async function invokeResult(name, args = {}, { timeoutMs = 30000 } = {}) {
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), timeoutMs);
    try {
      const response = await fetch(url(RPC), {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        credentials: 'same-origin',
        body: JSON.stringify({
          jsonrpc: '2.0',
          id: ++sequence,
          method: 'tools/call',
          params: { name, arguments: args },
        }),
        signal: controller.signal,
      });
      if (!response.ok) {
        throw new PluginRpcError(-32000, `HTTP ${response.status}`);
      }
      const message = await response.json();
      if (message.error) {
        throw new PluginRpcError(message.error.code, message.error.message, message.error.data);
      }
      const result = message.result ?? {};
      const content = result.content ?? [];
      const text = content
        .map((part) => (part && typeof part.text === 'string' ? part.text : ''))
        .join('\n');
      return {
        text,
        isError: Boolean(result.isError),
        content,
        structuredContent: result.structuredContent ?? null,
        raw: result,
      };
    } finally {
      clearTimeout(timer);
    }
  }

  /** Text-only call. Throws a `PluginRpcError` when the capability fails. */
  async function invoke(name, args = {}, options = {}) {
    const result = await invokeResult(name, args, options);
    if (result.isError) {
      throw new PluginRpcError(-32001, result.text || 'capability failed');
    }
    return result.text;
  }

  const http = {
    async text(path, init = {}) {
      const response = await fetch(url(path), { credentials: 'same-origin', ...init });
      if (!response.ok) throw new PluginRpcError(-32000, `HTTP ${response.status}`);
      return response.text();
    },
    async json(path, init = {}) {
      return JSON.parse(await http.text(path, init));
    },
  };

  /**
   * Call the host's own API (`/api/v1/...`).
   *
   * The page is served from `/plugin-ui/<id>/<token>/`, which is the console's
   * own origin, so the console's session cookie travels with the request
   * (`credentials: 'same-origin'`). A failure throws the same `PluginRpcError`
   * `invoke` throws, so a page has one error path rather than two.
   */
  async function hostRequest(path, init = {}) {
    const target = String(path).startsWith('/api/')
      ? String(path)
      : `/api/v1/${String(path).replace(/^\/+/, '')}`;
    return jsonRequest(target, init);
  }

  /**
   * Restart this plugin (`POST /api/v1/services/<id>/restart`), synchronously.
   *
   * A capability hook writes state and stops there: it is a short-lived process,
   * so a child it started would be orphaned when it exits, and it must not signal
   * the process group it does not own. Changing a plugin's own startup
   * configuration therefore ends with the host running stop → start → spawn
   * again, and the page is what asks for it — pass the id only when the page is
   * not served under `/plugin-ui/<id>/`.
   */
  async function restartSelf(id) {
    const owner = id || pagePluginId();
    if (!owner) {
      throw new PluginRpcError(
        -32000,
        'restartSelf: cannot tell the plugin id (the page is not under /plugin-ui/<id>/)'
      );
    }
    return hostRequest(`services/${encodeURIComponent(owner)}/restart`, { method: 'POST' });
  }

  /**
   * Subscribe over SSE.
   *
   * The host keeps a bounded ring per plugin (256 events) and honours
   * `Last-Event-ID` on reconnect: the browser's EventSource sends it
   * automatically because every frame carries `id: <seq>`, so a dropped
   * connection resumes instead of losing events. When the requested id has
   * already fallen out of the ring the host sends `event: reset` first — that
   * is the signal to refetch state with `invoke`.
   */
  function subscribe({ onMessage, onReset, onError } = {}) {
    const source = new EventSource(url(EVENTS));
    source.onmessage = (event) => {
      const message = parse(event.data);
      if (message && typeof message.method === 'string') {
        const seq = event.lastEventId ? Number(event.lastEventId) : null;
        onMessage?.(message.method, message.params ?? {}, message, seq);
      }
    };
    source.addEventListener('reset', (event) => onReset?.(parse(event.data) ?? {}));
    source.onerror = (event) => onError?.(event);
    return {
      close: () => source.close(),
      get readyState() {
        return source.readyState;
      },
    };
  }

  /**
   * The same stream over WebSocket. Unlike SSE this connection is symmetric:
   * notifications arrive as JSON-RPC notifications, and the host accepts
   * `tools/call` frames from the client on the same socket, so `invoke` here
   * avoids a second round trip.
   */
  function subscribeWebSocket({ onMessage, onReset, onOpen, onClose } = {}) {
    let socket = null;
    let retry = 0;
    let closing = false;
    let timer = null;
    let sequence = 0;
    const pending = new Map();

    const settle = (error) => {
      for (const { reject } of pending.values()) {
        reject(error ?? new PluginRpcError(-32000, 'socket closed'));
      }
      pending.clear();
    };

    const connect = () => {
      socket = new WebSocket(url(WS).replace(/^http/, 'ws'));
      socket.onopen = () => {
        retry = 0;
        onOpen?.();
      };
      socket.onmessage = (event) => {
        const message = parse(event.data);
        if (!message) return;

        if (message.id != null && pending.has(message.id)) {
          const waiter = pending.get(message.id);
          pending.delete(message.id);
          if (message.error) {
            waiter.reject(
              new PluginRpcError(message.error.code, message.error.message, message.error.data),
            );
          } else {
            waiter.resolve(message.result ?? {});
          }
          return;
        }
        if (typeof message.method === 'string') {
          onMessage?.(message.method, message.params ?? {}, message, null);
        } else if (message.reason) {
          onReset?.(message);
        }
      };
      socket.onclose = () => {
        settle();
        if (closing) return;
        const delay = Math.min(30000, 500 * 2 ** retry);
        retry += 1;
        onClose?.(delay);
        timer = setTimeout(connect, delay);
      };
    };

    connect();
    return {
      close() {
        closing = true;
        clearTimeout(timer);
        settle();
        socket?.close();
      },
      /** `tools/call` over this socket; resolves to the raw JSON-RPC result. */
      invoke(name, args = {}) {
        if (!socket || socket.readyState !== WebSocket.OPEN) {
          return Promise.reject(new PluginRpcError(-32000, 'socket is not open'));
        }
        const id = ++sequence;
        return new Promise((resolve, reject) => {
          pending.set(id, { resolve, reject });
          socket.send(
            JSON.stringify({
              jsonrpc: '2.0',
              id,
              method: 'tools/call',
              params: { name, arguments: args },
            }),
          );
        });
      },
    };
  }

  /**
   * One streaming request for the whole file (`POST /api/v1/blobs`). The server
   * hashes while it writes, so daemon memory stays flat; 512 MiB is the ceiling
   * and there is no resume. Use `uploadBlobResumable` past a few hundred MB.
   */
  async function uploadBlob(file) {
    return jsonRequest(BLOBS, {
      method: 'POST',
      headers: { 'content-type': 'application/octet-stream' },
      body: file,
    });
  }

  /**
   * Resumable upload of one blob (<= 512 MiB): create a part session, PUT the
   * parts, then complete. Pass `uploadId` from a previous failure to resume —
   * the server reports the parts it already holds and only the rest is sent.
   */
  async function uploadBlobResumable(file, options = {}) {
    return uploadParts(file, {
      base: BLOBS,
      partSize: options.partSize ?? 32 * 1024 * 1024,
      concurrency: options.concurrency ?? 3,
      resumeId: options.uploadId ?? null,
      onProgress: options.onProgress,
      complete: (parts) => ({ parts }),
    });
  }

  /**
   * Resumable upload of a **file package** (`/api/v1/file`), for files above the
   * 512 MiB blob ceiling. Parts plus a manifest; the id is the whole file's
   * SHA-256, so re-submitting identical content reuses the package.
   *
   * The capability argument is `{ $file: id }`, not `{ $blob: id }`.
   */
  async function uploadFilePackage(file, options = {}) {
    const name = options.name ?? file.name ?? 'file';
    const mimeType = options.mimeType ?? file.type ?? 'application/octet-stream';
    return uploadParts(file, {
      base: FILES,
      partSize: options.partSize ?? 64 * 1024 * 1024,
      concurrency: options.concurrency ?? 3,
      resumeId: options.uploadId ?? null,
      onProgress: options.onProgress,
      complete: (parts) => ({ name, mimeType, parts }),
    });
  }

  /** Shared S3-style part session used by both tiers. */
  async function uploadParts(
    file,
    { base, partSize, concurrency, resumeId, onProgress, complete },
  ) {
    const total = file.size;
    const partCount = Math.max(1, Math.ceil(total / partSize));
    if (partCount > 10000) {
      throw new PluginRpcError(-32602, `too many parts (${partCount}); use a larger partSize`);
    }

    let uploadId = resumeId;
    let partsUrl;
    try {
      if (uploadId) {
        partsUrl = `${base}/uploads/${uploadId}/parts`;
      } else {
        const created = await jsonRequest(`${base}/uploads`, { method: 'POST' });
        uploadId = created.uploadId;
        partsUrl = created.partsUrl ?? `${base}/uploads/${uploadId}/parts`;
      }

      const done = new Map();
      if (resumeId) {
        const status = await jsonRequest(`${base}/uploads/${uploadId}`);
        for (const part of status.parts ?? []) done.set(part.partNumber, part);
      }

      const queue = [];
      for (let index = 1; index <= partCount; index += 1) {
        if (!done.has(index)) queue.push(index);
      }

      let sent = 0;
      for (const part of done.values()) sent += part.size ?? 0;
      const results = new Map(done);
      onProgress?.({ sent, total, partCount, resumed: done.size });

      const worker = async () => {
        for (;;) {
          const partNumber = queue.shift();
          if (partNumber === undefined) return;
          const start = (partNumber - 1) * partSize;
          const slice = file.slice(start, Math.min(start + partSize, total));
          const info = await putPart(partsUrl, partNumber, slice);
          results.set(partNumber, info);
          sent += slice.size;
          onProgress?.({ sent: Math.min(sent, total), total, partCount, partNumber });
        }
      };
      const workers = Math.min(Math.max(1, concurrency), Math.max(1, queue.length));
      await Promise.all(Array.from({ length: workers }, worker));

      const parts = [...results.values()]
        .sort((left, right) => left.partNumber - right.partNumber)
        .map((part) => ({ partNumber: part.partNumber, etag: part.etag }));

      return await jsonRequest(`${base}/uploads/${uploadId}/complete`, {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify(complete(parts)),
      });
    } catch (error) {
      if (error instanceof PluginRpcError) error.uploadId = uploadId;
      throw error;
    }
  }

  /** Drop a part session and its temp files. `kind` is 'blob' or 'file'. */
  async function abortUpload(kind, uploadId) {
    const base = kind === 'file' ? FILES : BLOBS;
    const response = await fetch(
      `${base}/uploads/${uploadId}`,
      withCsrf({ method: 'DELETE', credentials: 'same-origin' }),
    );
    if (!response.ok && response.status !== 404) {
      throw new PluginRpcError(-32000, `abort failed: HTTP ${response.status}`);
    }
  }

  /** Fetch a blob by id (streamed by the server; Range is available if needed). */
  async function downloadBlob(id, { filename = 'blob' } = {}) {
    return downloadFrom(BLOBS, id, filename);
  }

  /** Fetch a file package by id; the server streams it part by part. */
  async function downloadFile(id, { filename = 'file' } = {}) {
    return downloadFrom(FILES, id, filename);
  }

  async function downloadFrom(base, id, filename) {
    if (!CONTENT_ID.test(id)) {
      throw new PluginRpcError(-32602, `not a content id: ${id}`);
    }
    const response = await fetch(`${base}/${id}`, { credentials: 'same-origin' });
    if (!response.ok) {
      throw new PluginRpcError(-32000, `download failed: HTTP ${response.status}`);
    }
    const blob = await response.blob();
    saveBlob(blob, filename);
    return blob;
  }

  /** Argument shape for a blob input. */
  function blobArg(id) {
    return { $blob: id };
  }

  /** Argument shape for a file-package input (large files; not an alias of $blob). */
  function fileArg(id) {
    return { $file: id };
  }

  return {
    url,
    invoke,
    invokeResult,
    http,
    hostRequest,
    restartSelf,
    subscribe,
    subscribeWebSocket,
    uploadBlob,
    uploadBlobResumable,
    uploadFilePackage,
    abortUpload,
    downloadBlob,
    downloadFile,
    blobArg,
    fileArg,
  };
}
