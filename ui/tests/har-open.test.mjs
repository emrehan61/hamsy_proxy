import assert from 'node:assert/strict';
import { test, after } from 'node:test';
import { execFileSync } from 'node:child_process';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);
const output = mkdtempSync(join(tmpdir(), 'hamsy-open-tests-'));
execFileSync(process.execPath, [require.resolve('typescript/bin/tsc'), '--outDir', output,
  '--noEmit', 'false', '--module', 'commonjs', '--moduleResolution', 'node', '--target', 'es2022',
  '--skipLibCheck', 'src/lib/api.ts'], { cwd: new URL('..', import.meta.url) });
writeFileSync(join(output, 'package.json'), '{"type":"commonjs"}');
after(() => rmSync(output, { recursive: true, force: true }));
const { getOpenHarFiles, ApiError } = require(join(output, 'api.js'));

test('redeems an encoded ticket on the same origin without caching and keeps multiple files', async (t) => {
  const files = [{ name: 'one.har', text: '{"log":{"entries":[]}}' }, { name: 'two.har', text: 'invalid HAR' }];
  const fetch = t.mock.method(globalThis, 'fetch', async () => Response.json({ files }));
  assert.deepEqual(await getOpenHarFiles('ticket/?secret'), files);
  assert.equal(fetch.mock.calls.length, 1);
  const [url, init] = fetch.mock.calls[0].arguments;
  assert.equal(url, '/api/har/open/ticket%2F%3Fsecret');
  assert.equal(init.credentials, 'same-origin');
  assert.equal(init.cache, 'no-store');
  // No POST /har/import: the caller imports these into client-side tabs.
  assert.equal(init.method, undefined);
});

test('rejects malformed handoff responses before any files are imported', async (t) => {
  const payloads = [null, [], {}, { files: [] }, { files: {} }, { files: [null] },
    { files: [{ name: '', text: '{}' }] }, { files: [{ name: 'x.har', text: 1 }] },
    { files: [{ name: 'valid.har', text: '{}' }, { name: 'bad.har' }] }];
  let payload;
  t.mock.method(globalThis, 'fetch', async () => Response.json(payload));
  for (payload of payloads) await assert.rejects(getOpenHarFiles('ticket'), /Invalid HAR open response/);
});

test('rejects blank tickets without making a request', async (t) => {
  const fetch = t.mock.method(globalThis, 'fetch', async () => { throw new Error('should not fetch'); });
  for (const ticket of ['', '   ']) await assert.rejects(getOpenHarFiles(ticket), /Invalid HAR open link/);
  assert.equal(fetch.mock.calls.length, 0);
});

test('preserves unavailable/expired HTTP status for the helpful UI error', async (t) => {
  let status = 404;
  t.mock.method(globalThis, 'fetch', async () => new Response('Unavailable', { status }));
  for (status of [404, 410, 500]) {
    await assert.rejects(getOpenHarFiles('ticket'), (err) => err instanceof ApiError && err.status === status);
  }
});

test('reports connection failures rather than treating them as empty imports', async (t) => {
  t.mock.method(globalThis, 'fetch', async () => { throw new TypeError('Failed to fetch'); });
  await assert.rejects(getOpenHarFiles('ticket'), /Failed to fetch/);
});
