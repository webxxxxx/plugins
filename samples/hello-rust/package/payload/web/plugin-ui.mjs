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

const RPC = 'rpc';
const EVENTS = 'events';
const WS = 'ws';
const BLOBS = '/api/v1/blobs';
const FILES = '/api/v1/file';

/** Content addresses only; reject anything else before it reaches a URL. */
const CONTENT_ID = /^sha256:[0-9a-f]{64}$/;

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

function parse(raw) {
  try {
    return JSON.parse(raw);
  } catch {
    return null;
  }
}

async function jsonRequest(url, init = {}) {
  const response = await fetch(url, { credentials: 'same-origin', ...init });
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
   * Subscribe over SSE. The host replays recent history on connect (bounded),
   * and emits `event: reset` when history is unavailable — that is the signal to
   * refetch whatever state you were displaying.
   */
  function subscribe({ onMessage, onReset, onError } = {}) {
    const source = new EventSource(url(EVENTS));
    source.onmessage = (event) => {
      const message = parse(event.data);
      if (message && typeof message.method === 'string') {
        onMessage?.(message.method, message.params ?? {}, message);
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
   * Same stream over WebSocket. Use it when the page holds one long-lived
   * connection; it is a push channel exactly like SSE, not a request path.
   * Requests still go through `invoke`.
   */
  function subscribeWebSocket({ onMessage, onReset, onOpen, onClose } = {}) {
    let socket = null;
    let retry = 0;
    let closing = false;
    let timer = null;

    const connect = () => {
      socket = new WebSocket(url(WS).replace(/^http/, 'ws'));
      socket.onopen = () => {
        retry = 0;
        onOpen?.();
      };
      socket.onmessage = (event) => {
        const message = parse(event.data);
        if (!message) return;
        if (typeof message.method === 'string') {
          onMessage?.(message.method, message.params ?? {}, message);
        } else if (message.reason) {
          onReset?.(message);
        }
      };
      socket.onclose = () => {
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
        socket?.close();
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
    const response = await fetch(`${base}/uploads/${uploadId}`, {
      method: 'DELETE',
      credentials: 'same-origin',
    });
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
