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

/** Files move as base64 chunks through `tools/call`, because the host gives the
 * plugin exactly one request channel (`POST ./rpc`) and caps a frame at 1 MiB. */
const CHUNK = 512 * 1024;

function bytesToBase64(bytes) {
  let binary = '';
  for (let offset = 0; offset < bytes.length; offset += 0x8000) {
    binary += String.fromCharCode.apply(null, bytes.subarray(offset, offset + 0x8000));
  }
  return btoa(binary);
}

function base64ToBytes(text) {
  const binary = atob(text);
  const bytes = new Uint8Array(binary.length);
  for (let index = 0; index < binary.length; index += 1) {
    bytes[index] = binary.charCodeAt(index);
  }
  return bytes;
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

  async function invoke(name, args = {}, { timeoutMs = 30000 } = {}) {
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
      const text = (result.content ?? [])
        .map((part) => (part && typeof part.text === 'string' ? part.text : ''))
        .join('\n');
      if (result.isError) {
        throw new PluginRpcError(-32001, text || 'capability failed');
      }
      return text;
    } finally {
      clearTimeout(timer);
    }
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
   * Upload a File to the plugin, chunk by chunk.
   *
   * The plugin must declare the `file_put` capability; `sdk/rust`'s
   * `FileStore` implements the server half. Each chunk is one `tools/call`, so
   * a slow link degrades into more round trips rather than a stalled frame.
   */
  async function uploadFile(file, { name = file.name, onProgress } = {}) {
    const total = file.size;
    let offset = 0;
    let last = null;
    do {
      const slice = file.slice(offset, offset + CHUNK);
      const data = bytesToBase64(new Uint8Array(await slice.arrayBuffer()));
      const final = offset + slice.size >= total;
      const reply = await invoke(
        'file_put',
        { path: name, offset, total, data, final },
        { timeoutMs: 120000 },
      );
      last = reply ? JSON.parse(reply) : {};
      offset += slice.size;
      onProgress?.({ sent: offset, total });
    } while (offset < total);

    return last;
  }

  /**
   * Download a file the plugin stored, and hand it to the browser.
   *
   * The plugin must declare the `file_get` capability. Returns the assembled
   * Blob so a caller can show a preview instead of saving.
   */
  async function downloadFile(path, { filename = path, onProgress } = {}) {
    const chunks = [];
    let offset = 0;
    for (;;) {
      const reply = JSON.parse(
        await invoke('file_get', { path, offset, length: CHUNK }, { timeoutMs: 120000 }),
      );
      const bytes = base64ToBytes(reply.data ?? '');
      chunks.push(bytes);
      offset += bytes.length;
      onProgress?.({ received: offset, total: reply.size ?? offset });
      if (reply.eof || bytes.length === 0) break;
    }

    const blob = new Blob(chunks, { type: 'application/octet-stream' });
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

  return { url, invoke, http, subscribe, subscribeWebSocket, uploadFile, downloadFile };
}
