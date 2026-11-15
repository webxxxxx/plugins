// Resumable upload: a part session, parallel parts, and a resume that only
// sends what the server does not already hold. Driven through a stubbed fetch so
// the protocol shape is asserted without a daemon.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { pluginUI } from '../../sdk/web/plugin-ui.mjs';

function json(data, status = 200) {
  return new Response(JSON.stringify(data), {
    status,
    headers: { 'content-type': 'application/json' },
  });
}

/** Route the calls the toolkit makes and record them. */
function stubFetch(routes, calls) {
  return async (url, init = {}) => {
    const path = String(url).replace(/^https?:\/\/[^/]+/, '');
    const method = (init.method ?? 'GET').toUpperCase();
    calls.push({ method, path, body: init.body });
    const handler = routes[`${method} ${path}`] ?? routes[`${method} ${path.replace(/\/\d+$/, '/:n')}`];
    if (!handler) return json({ error: `unexpected ${method} ${path}` }, 404);
    return handler(init);
  };
}

test('a part session uploads every part and completes with sorted parts', async () => {
  const calls = [];
  const original = globalThis.fetch;
  try {
    globalThis.fetch = async (url, init = {}) => {
      const path = String(url).replace(/^https?:\/\/[^/]+/, '');
      calls.push({ method: (init.method ?? 'GET').toUpperCase(), path, body: init.body });
      if (path === '/api/v1/blobs/uploads' && init.method === 'POST') {
        return json({ uploadId: 'abc', partsUrl: '/api/v1/blobs/uploads/abc/parts' });
      }
      const put = path.match(/^\/api\/v1\/blobs\/uploads\/abc\/parts\/(\d+)$/);
      if (put && init.method === 'PUT') {
        return json({ partNumber: Number(put[1]), size: 4, etag: `sha256:${put[1]}` });
      }
      if (path === '/api/v1/blobs/uploads/abc/complete' && init.method === 'POST') {
        return json({ id: 'sha256:done', size: 10 });
      }
      return json({}, 404);
    };

    const progress = [];
    const ui = pluginUI({ base: '/' });
    const result = await ui.uploadBlobResumable(new Blob(['0123456789']), {
      partSize: 4,
      onProgress: (step) => progress.push(step),
    });

    assert.equal(result.id, 'sha256:done');
    const complete = calls.find((call) => call.path.endsWith('/complete'));
    const parts = JSON.parse(String(complete.body)).parts;
    assert.deepEqual(
      parts.map((part) => part.partNumber),
      [1, 2, 3],
      'parts must be strictly ascending',
    );
    assert.equal(progress.at(-1).sent, 10);
  } finally {
    globalThis.fetch = original;
  }
});

test('resuming skips the parts the server already holds', async () => {
  const calls = [];
  const original = globalThis.fetch;
  try {
    globalThis.fetch = async (url, init = {}) => {
      const path = String(url).replace(/^https?:\/\/[^/]+/, '');
      calls.push({ method: (init.method ?? 'GET').toUpperCase(), path, body: init.body });
      if (path === '/api/v1/blobs/uploads/abc' && !init.method) {
        return json({ uploadId: 'abc', totalSize: 10, parts: [{ partNumber: 1, size: 4, etag: 'sha256:1' }] });
      }
      const put = path.match(/^\/api\/v1\/blobs\/uploads\/abc\/parts\/(\d+)$/);
      if (put && init.method === 'PUT') {
        return json({ partNumber: Number(put[1]), size: 4, etag: `sha256:${put[1]}` });
      }
      if (path === '/api/v1/blobs/uploads/abc/complete') {
        return json({ id: 'sha256:done', size: 10 });
      }
      return json({}, 404);
    };

    const ui = pluginUI({ base: '/' });
    await ui.uploadBlobResumable(new Blob(['0123456789']), { partSize: 4, uploadId: 'abc' });

    const uploaded = calls.filter((call) => call.method === 'PUT').map((call) => call.path);
    assert.deepEqual(uploaded, [
      '/api/v1/blobs/uploads/abc/parts/2',
      '/api/v1/blobs/uploads/abc/parts/3',
    ]);
  } finally {
    globalThis.fetch = original;
  }
});

test('a failed part leaves the session id on the error so it can resume', async () => {  const original = globalThis.fetch;
  try {
    globalThis.fetch = async (url, init = {}) => {
      const path = String(url).replace(/^https?:\/\/[^/]+/, '');
      if (path === '/api/v1/blobs/uploads') {
        return json({ uploadId: 'abc', partsUrl: '/api/v1/blobs/uploads/abc/parts' });
      }
      return json({ message: 'boom' }, 500);
    };

    const ui = pluginUI({ base: '/' });
    const error = await ui
      .uploadBlobResumable(new Blob(['0123']), { partSize: 4 })
      .then(() => null, (caught) => caught);
    assert.ok(error, 'upload should fail');
    assert.equal(error.uploadId, 'abc');
  } finally {
    globalThis.fetch = original;
  }
});

test('a file package completes with a name and mime type', async () => {
  const calls = [];
  const original = globalThis.fetch;
  try {
    globalThis.fetch = async (url, init = {}) => {
      const path = String(url).replace(/^https?:\/\/[^/]+/, '');
      calls.push({ method: (init.method ?? 'GET').toUpperCase(), path, body: init.body });
      if (path === '/api/v1/file/uploads' && init.method === 'POST') {
        return new Response(JSON.stringify({ uploadId: 'f1', partsUrl: '/api/v1/file/uploads/f1/parts' }), {
          status: 200, headers: { 'content-type': 'application/json' },
        });
      }
      const put = path.match(/^\/api\/v1\/file\/uploads\/f1\/parts\/(\d+)$/);
      if (put && init.method === 'PUT') {
        return new Response(JSON.stringify({ partNumber: Number(put[1]), size: 4, etag: `sha256:${put[1]}` }), {
          status: 200, headers: { 'content-type': 'application/json' },
        });
      }
      if (path === '/api/v1/file/uploads/f1/complete') {
        return new Response(JSON.stringify({ id: 'sha256:file', size: 10, name: 'x.iso', mimeType: 'application/x-iso9660-image' }), {
          status: 200, headers: { 'content-type': 'application/json' },
        });
      }
      return new Response('{}', { status: 404, headers: { 'content-type': 'application/json' } });
    };

    const ui = pluginUI({ base: '/' });
    const result = await ui.uploadFilePackage(new Blob(['0123456789']), {
      partSize: 4,
      name: 'x.iso',
      mimeType: 'application/x-iso9660-image',
    });

    assert.equal(result.id, 'sha256:file');
    const complete = calls.find((call) => call.path.endsWith('/complete'));
    const body = JSON.parse(String(complete.body));
    assert.equal(body.name, 'x.iso');
    assert.equal(body.mimeType, 'application/x-iso9660-image');
    assert.deepEqual(body.parts.map((part) => part.partNumber), [1, 2, 3]);
    assert.equal(typeof ui.fileArg('sha256:file').$file, 'string');
  } finally {
    globalThis.fetch = original;
  }
});
