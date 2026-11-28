// A plugin page's writes to the host's own API (`/api/v1/...`).
//
//   node sdk/web/tests/plugin-ui-csrf.mjs
//
// The page is served from the console's own origin, so a cookie-authenticated
// write is allowed once it carries the host's `X-Svcd-Csrf: 1` marker next to
// the `Origin` the browser adds to every non-GET request
// (`src/server/http/model.rs::csrf_ok`). Forgetting the marker is invisible in
// review — the request looks right and the host answers `403 forbidden`, which
// is exactly what a "saved, but the plugin did not restart" report is.
//
// So this harness stubs `fetch` and pins it on every write the toolkit makes:
// the restart, the blob and file-package tiers, and the abort. Reads stay
// bare, and `/plugin-ui/<id>/rpc` stays a plain POST — the host keeps a
// same-origin check there instead so a page can call it without ceremony.
import { pluginUI } from '../plugin-ui.mjs';

const CSRF = 'x-svcd-csrf';

let failures = 0;
function check(name, condition, detail) {
  if (condition) {
    console.log('ok   ' + name);
  } else {
    failures += 1;
    console.log('FAIL ' + name + (detail === undefined ? '' : ': ' + JSON.stringify(detail)));
  }
}

// The page's own address decides `restartSelf`'s target id.
globalThis.location = {
  href: 'http://nas:8080/plugin-ui/meow/abc123/',
  pathname: '/plugin-ui/meow/abc123/',
};

let calls = [];
globalThis.fetch = async (url, init = {}) => {
  const target = String(url);
  calls.push({ url: target, init });
  const body = target.endsWith('/uploads')
    ? { uploadId: 'u-1' }
    : target.endsWith('/complete')
      ? { id: 'sha256:' + 'a'.repeat(64) }
      : { services: [] };
  return { ok: true, status: 200, json: async () => body, text: async () => '', blob: async () => ({}) };
};

const ui = pluginUI();
const reset = () => {
  calls = [];
};
const headerOf = (call, name) => {
  const headers = call?.init?.headers ?? {};
  for (const [key, value] of Object.entries(headers)) {
    if (key.toLowerCase() === name) return value;
  }
  return undefined;
};
const last = () => calls[calls.length - 1];

// The restart the pages call after a capability hook answered
// `{"restart":true}` — the case the 403 was reported on.
reset();
await ui.restartSelf();
check(
  'restartSelf posts to the id in the page path',
  last()?.url === '/api/v1/services/meow/restart' && last()?.init?.method === 'POST',
  last()?.url,
);
check('restartSelf carries the CSRF marker', headerOf(last(), CSRF) === '1');
check('restartSelf keeps the session cookie', last()?.init?.credentials === 'same-origin');

reset();
await ui.restartSelf('other');
check(
  'restartSelf takes an explicit id when the page is not under /plugin-ui/',
  last()?.url === '/api/v1/services/other/restart',
  last()?.url,
);

// A bare hostRequest is the general form; it writes nothing by itself.
reset();
await ui.hostRequest('services/meow/start', { method: 'POST' });
check(
  'hostRequest POST reaches /api/v1 with the marker',
  last()?.url === '/api/v1/services/meow/start' && headerOf(last(), CSRF) === '1',
  last()?.url,
);

reset();
await ui.hostRequest('/api/v1/services');
check('a GET is left alone', last()?.url === '/api/v1/services');
check('a GET carries no marker', headerOf(last(), CSRF) === undefined);

// The transfer tier is a cookie write too: creating a blob, a part session, a
// part itself, and completing it. A missing marker here would 403 the upload
// before a byte of the body is read.
reset();
await ui.uploadBlob({ size: 3 });
check(
  'uploadBlob posts the blob with the marker',
  last()?.url === '/api/v1/blobs' && last()?.init?.method === 'POST' && headerOf(last(), CSRF) === '1',
  last()?.url,
);
check(
  'uploadBlob keeps its own content-type',
  headerOf(last(), 'content-type') === 'application/octet-stream',
  last()?.init?.headers,
);

reset();
await ui.uploadBlobResumable({ size: 5, slice: (start, end) => ({ size: end - start }) });
const shapes = calls.map((call) => [
  call.url,
  call.init.method,
  headerOf(call, CSRF) === '1',
  headerOf(call, 'content-type'),
]);
check('a resumable upload is three writes', calls.length === 3, shapes);
check(
  'every part-session write carries the marker',
  shapes.every(([, , marked]) => marked),
  shapes,
);
check(
  'the part PUT still names its content-type',
  shapes.some(([url, method, , type]) => url.endsWith('/parts/1') && method === 'PUT' && type === 'application/octet-stream'),
  shapes,
);

reset();
await ui.abortUpload('blob', 'u-1');
check(
  'abortUpload deletes with the marker',
  last()?.url === '/api/v1/blobs/uploads/u-1' &&
    last()?.init?.method === 'DELETE' &&
    headerOf(last(), CSRF) === '1',
  last()?.url,
);

reset();
await ui.downloadBlob('sha256:' + 'b'.repeat(64), { filename: 'x' }).catch(() => {});
check('a download is a plain GET', last()?.url?.startsWith('/api/v1/blobs/sha256:'), last()?.url);
check('a download carries no marker', headerOf(last(), CSRF) === undefined);

// `/plugin-ui/<id>/rpc` is the one write that stays bare: the host keeps a
// same-origin check on that bridge instead of the marker header.
reset();
await ui.invokeResult('state', {});
check(
  'the rpc bridge stays a plain POST',
  last()?.url?.endsWith('/rpc') && last()?.init?.method === 'POST' && headerOf(last(), CSRF) === undefined,
  last()?.url,
);

if (failures === 0) console.log('\nall checks passed');
process.exit(failures ? 1 : 0);
