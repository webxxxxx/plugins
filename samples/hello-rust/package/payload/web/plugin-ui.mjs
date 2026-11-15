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
// Auth is a path-scoped cookie plus an Origin check, both handled by the
// browser: this file never touches a token.

const RPC = 'rpc';
const EVENTS = 'events';
const WS = 'ws';
const BLOBS = '/api/v1/blobs';

/** Blob ids are content addresses; reject anything else before it reaches a URL. */
const BLOB_ID = /^sha256:[0-9a-f]{64}$/;

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
   * Upload a file to the host's blob store and get back its id.
   *
   * Blobs are how large payloads cross the boundary: the control channel caps a
   * frame at 1 MiB, and `POST /api/v1/blobs` takes the raw bytes instead. Pass
   * the id to a capability as `{ $blob: id }`; the host resolves it to a file
   * path before the plugin sees it.
   */
  async function uploadBlob(file) {
    const response = await fetch(BLOBS, {
      method: 'POST',
      headers: { 'content-type': 'application/octet-stream' },
      credentials: 'same-origin',
      body: file,
    });
    if (!response.ok) {
      throw new PluginRpcError(-32000, `blob upload failed: HTTP ${response.status}`);
    }
    return response.json();
  }

  /** Fetch a blob by id and hand it to the browser (or return the Blob). */
  async function downloadBlob(id, { filename = 'blob' } = {}) {
    if (!BLOB_ID.test(id)) {
      throw new PluginRpcError(-32602, `not a blob id: ${id}`);
    }
    const response = await fetch(`${BLOBS}/${id}`, { credentials: 'same-origin' });
    if (!response.ok) {
      throw new PluginRpcError(-32000, `blob download failed: HTTP ${response.status}`);
    }
    const blob = await response.blob();
    if (typeof document !== 'undefined') {
      const href = URL.createObjectURL(blob);
      const anchor = document.createElement('a');
      anchor.href = href;
      anchor.download = filename;
      anchor.click();
      URL.revokeObjectURL(href);
    }
    return blob;
  }

  /** Build the argument object a capability expects for a blob input. */
  function blobArg(id) {
    return { $blob: id };
  }

  return { url, invoke, invokeResult, http, subscribe, subscribeWebSocket, uploadBlob, downloadBlob, blobArg };
}
