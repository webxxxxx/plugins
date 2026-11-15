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

  return { url, invoke, http, subscribe, subscribeWebSocket };
}
