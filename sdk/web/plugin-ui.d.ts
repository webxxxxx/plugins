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

export interface CallResult {
  text: string;
  isError: boolean;
  content: Array<{ type: string; text?: string; uri?: string; name?: string; mimeType?: string }>;
  structuredContent: { $blob?: string; size?: number } | null;
  raw: unknown;
}

export interface BlobMeta {
  id: string;
  size: number;
}

export interface PluginUI {
  /** Absolute URL of a package-local asset. */
  url(path: string): string;
  /** Tools/call over POST ./rpc. Resolves to the joined text content. */
  invoke(name: string, args?: Record<string, unknown>, options?: { timeoutMs?: number }): Promise<string>;
  /** Tools/call that keeps the whole result (blob results lose their id otherwise). */
  invokeResult(
    name: string,
    args?: Record<string, unknown>,
    options?: { timeoutMs?: number },
  ): Promise<CallResult>;
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
  /** Upload raw bytes to POST /api/v1/blobs and get the content address back. */
  uploadBlob(file: Blob): Promise<BlobMeta>;
  /** GET /api/v1/blobs/<id>; returns the Blob and triggers a download in a browser. */
  downloadBlob(id: string, options?: { filename?: string }): Promise<Blob>;
  /** `{ $blob: id }` — the argument shape a capability expects for blob input. */
  blobArg(id: string): { $blob: string };
}

export function pluginUI(options?: { base?: string }): PluginUI;
