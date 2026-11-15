export declare class PluginRpcError extends Error {
  readonly code: number;
  readonly data?: unknown;
  constructor(code: number, message: string, data?: unknown);
}

export interface Subscription {
  close(): void;
  readonly readyState: number;
}

export interface SocketSubscription {
  close(): void;
}

export interface PluginUI {
  /** Absolute URL of a package-local asset. */
  url(path: string): string;
  /** Tools/call over POST ./rpc. Resolves to the joined text content. */
  invoke(name: string, args?: Record<string, unknown>, options?: { timeoutMs?: number }): Promise<string>;
  http: {
    text(path: string, init?: RequestInit): Promise<string>;
    json<T = unknown>(path: string, init?: RequestInit): Promise<T>;
  };
  /** Server push over SSE, with automatic reconnection. */
  subscribe(handlers?: {
    onMessage?: (method: string, params: unknown, message: unknown) => void;
    onReset?: (info: unknown) => void;
    onError?: (event: Event) => void;
  }): Subscription;
  /** Server push over WebSocket, with backoff. Not a request path. */
  subscribeWebSocket(handlers?: {
    onMessage?: (method: string, params: unknown, message: unknown) => void;
    onReset?: (info: unknown) => void;
    onOpen?: () => void;
    onClose?: (retryDelayMs: number) => void;
  }): SocketSubscription;
}

export function pluginUI(options?: { base?: string }): PluginUI;
