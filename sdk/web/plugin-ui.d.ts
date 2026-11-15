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
  /** `tools/call` over this socket; resolves to the raw JSON-RPC result. */
  invoke(name: string, args?: Record<string, unknown>): Promise<unknown>;
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
  /** Server push over SSE; the browser resumes with Last-Event-ID automatically. */
  subscribe(handlers?: {
    onMessage?: (method: string, params: unknown, message: unknown, seq: number | null) => void;
    onReset?: (info: unknown) => void;
    onError?: (event: Event) => void;
  }): Subscription;
  /** Symmetric WebSocket: notifications in, `tools/call` out on one socket. */
  subscribeWebSocket(handlers?: {
    onMessage?: (method: string, params: unknown, message: unknown, seq: number | null) => void;
    onReset?: (info: unknown) => void;
    onOpen?: () => void;
    onClose?: (retryDelayMs: number) => void;
  }): SocketSubscription;
  /** Upload raw bytes to POST /api/v1/blobs and get the content address back. */
  uploadBlob(file: Blob): Promise<BlobMeta>;
  /** Resumable upload of one blob (<= 512 MiB) through a part session. */
  uploadBlobResumable(
    file: Blob,
    options?: {
      partSize?: number;
      concurrency?: number;
      uploadId?: string | null;
      onProgress?: (progress: {
        sent: number;
        total: number;
        partCount: number;
        partNumber?: number;
        resumed?: number;
      }) => void;
    },
  ): Promise<BlobMeta>;
  /** Resumable upload of a file package (`/api/v1/file`, up to 16 GiB). */
  uploadFilePackage(
    file: File | Blob,
    options?: {
      name?: string;
      mimeType?: string;
      partSize?: number;
      concurrency?: number;
      uploadId?: string | null;
      onProgress?: (progress: {
        sent: number;
        total: number;
        partCount: number;
        partNumber?: number;
        resumed?: number;
      }) => void;
    },
  ): Promise<BlobMeta & { name?: string; mimeType?: string }>;
  /** DELETE a part session and its temp files. */
  abortUpload(kind: 'blob' | 'file', uploadId: string): Promise<void>;
  /** GET /api/v1/blobs/<id>; returns the Blob and triggers a download in a browser. */
  downloadBlob(id: string, options?: { filename?: string }): Promise<Blob>;
  /** GET /api/v1/file/<id>; the server streams the parts. */
  downloadFile(id: string, options?: { filename?: string }): Promise<Blob>;
  /** `{ $blob: id }` — the argument shape a capability expects for blob input. */
  blobArg(id: string): { $blob: string };
  /** `{ $file: id }` — the argument shape for a file package (not a blob). */
  fileArg(id: string): { $file: string };
}

export function pluginUI(options?: { base?: string }): PluginUI;
