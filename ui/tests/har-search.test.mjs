import assert from 'node:assert/strict';
import { test, after } from 'node:test';
import { execFileSync } from 'node:child_process';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);
const output = mkdtempSync(join(tmpdir(), 'hamsy-search-tests-'));
execFileSync(process.execPath, [require.resolve('typescript/bin/tsc'), '--outDir', output,
  '--noEmit', 'false', '--module', 'commonjs', '--moduleResolution', 'node', '--target', 'es2022',
  '--skipLibCheck', 'src/lib/harSearch.ts', 'src/lib/har.ts', 'src/lib/filter.ts', 'src/lib/sessionSearch.ts'], { cwd: new URL('..', import.meta.url) });
writeFileSync(join(output, 'package.json'), '{"type":"commonjs"}');
after(() => rmSync(output, { recursive: true, force: true }));
const { searchHar } = require(join(output, 'harSearch.js'));
const { parseHar } = require(join(output, 'har.js'));
const { filterFlows } = require(join(output, 'filter.js'));
const { loadSearchSnapshot } = require(join(output, 'sessionSearch.js'));

const flows = parseHar({ log: { entries: ['api.test', 'ads.test'].map(host => ({
  request: { method: 'POST', url: `https://${host}/needle?term=hello`,
    headers: [{ name: 'X-Needle', value: 'header value' }],
    queryString: [{ name: 'term', value: 'needle query' }],
    postData: { mimeType: 'text/plain', text: 'Needle request [a+b].*' } },
  response: { status: 200, statusText: 'OK', headers: [{ name: 'X-Test', value: 'needle response' }],
    content: { mimeType: 'application/json', encoding: 'base64', text: Buffer.from('{"message":"needle café"}').toString('base64') } },
  _webSocketMessages: [{ type: 'send', time: 1, opcode: 1, data: 'needle socket' }],
})) } }).flows;
const filters = { query: '', methods: [], statusClasses: [], resourceTypes: [], onlyModified: false, host: '', excludedHosts: [], apps: [] };

test('searches URL, headers, query, both bodies and WebSocket content with correct destinations', () => {
  const result = searchHar(flows, 'needle', false, false);
  assert.equal(result.error, null);
  assert.equal(result.requestCount, 2);
  assert.deepEqual(new Set(result.matches.map(m => m.field)), new Set(['URL', 'Request header', 'Query parameter', 'Request body', 'Response header', 'Response body', 'WebSocket message 1']));
  assert.equal(result.matches.find(m => m.field === 'Response body').tab, 'response');
  assert.equal(result.matches.find(m => m.field === 'Request body').tab, 'request');
  assert.equal(result.matches.find(m => m.field.startsWith('WebSocket')).tab, 'websocket');
  assert.ok(result.matches.every(m => flows.some(f => f.id === m.flowId)));
});
test('literal search escapes regex punctuation and preserves case and Unicode in snippets', () => {
  assert.equal(searchHar(flows, '[a+b].*', false, false).matches.length, 2);
  assert.equal(searchHar(flows, 'Needle request', false, true).matches.length, 2);
  assert.equal(searchHar(flows, 'needle request', false, true).matches.length, 0);
  assert.equal(searchHar(flows, 'café', false, false).matches[0].match, 'café');
});
test('regex supports alternatives, invalid patterns, and zero-width matches', () => {
  assert.equal(searchHar(flows, 'needle (request|café)', true, false).matches.length, 4);
  assert.match(searchHar(flows, '[', true, false).error, /Invalid regular expression/);
  assert.ok(searchHar(flows, '^', true, false).matches.length > 0);
  assert.equal(searchHar(flows, '', true, false).matches.length, 0);
});
test('host exclusions combine with inclusion and limit global search', () => {
  const visible = filterFlows(flows, { ...filters, excludedHosts: ['ads.test'] });
  assert.deepEqual(visible.map(f => f.host), ['api.test']);
  assert.equal(searchHar(flows, 'needle', false, false, new Set(visible.map(f => f.id))).requestCount, 1);
  assert.equal(filterFlows(flows, { ...filters, host: 'ads.test', excludedHosts: ['ads.test'] }).length, 0);
  assert.equal(filterFlows(flows, { ...filters, excludedHosts: ['ads.test', 'api.test'] }).length, 0);
  assert.equal(filterFlows(flows, filters).length, 2);
});
test('handles absent, binary and malformed bodies without throwing', () => {
  const flow = structuredClone(flows[0]);
  flow.request = null;
  flow.response.body.data = 'not valid base64!';
  assert.equal(searchHar([flow], 'café', false, false).matches.length, 0);
  flow.response.body.data = Buffer.from([0, 1, 2, 3]).toString('base64');
  assert.equal(searchHar([flow], 'AAEC', false, false).matches.length, 0);
  flow.response = null;
  assert.equal(searchHar([flow], 'needle', false, false).requestCount, 1);
});

test('current-session snapshots load full bodies with bounded concurrency and searchable original ids', async (t) => {
  const ids = Array.from({ length: 20 }, (_, i) => `live-${i}`);
  let active = 0;
  let peak = 0;
  t.mock.method(globalThis, 'fetch', async (url, init) => {
    active++;
    peak = Math.max(peak, active);
    assert.equal(init.cache, 'no-store');
    assert.ok(init.signal instanceof AbortSignal);
    await new Promise(resolve => setTimeout(resolve, 1));
    active--;
    return Response.json({ ...flows[0], id: url.split('/').at(-1) });
  });
  const snapshot = await loadSearchSnapshot(ids, new AbortController().signal);
  assert.deepEqual(snapshot.map(flow => flow.id), ids);
  assert.ok(peak > 1 && peak <= 6);
  const results = searchHar(snapshot, '(?<=needle )caf[é]', true, false, new Set(['live-3']));
  assert.equal(results.requestCount, 1);
  assert.equal(results.matches[0].flowId, 'live-3');
  assert.equal(results.matches[0].tab, 'response');
});

test('refresh loads updated bodies and WebSocket messages instead of reusing stale details', async (t) => {
  const flow = structuredClone(flows[0]);
  t.mock.method(globalThis, 'fetch', async () => Response.json(flow));
  const signal = new AbortController().signal;
  const first = await loadSearchSnapshot([flow.id], signal);
  flow.response.body = { kind: 'text', encoding: null, data: 'latest response', size: 15, truncated: false };
  flow.wsMessages.push({ direction: 'recv', opcode: 'text', timestamp: 2, data: 'latest socket', size: 13 });
  const refreshed = await loadSearchSnapshot([flow.id], signal);
  assert.equal(searchHar(first, 'latest', false, false).matches.length, 0);
  assert.deepEqual(searchHar(refreshed, 'latest (response|socket)', true, false).matches.map(m => m.tab), ['response', 'websocket']);
});

test('skips evicted flows but fails the snapshot on server errors', async (t) => {
  let status = 404;
  t.mock.method(globalThis, 'fetch', async (url) => url.endsWith('/gone')
    ? new Response('', { status }) : Response.json(flows[0]));
  const signal = new AbortController().signal;
  assert.deepEqual(await loadSearchSnapshot(['gone', flows[0].id], signal), [flows[0]]);
  status = 500;
  await assert.rejects(loadSearchSnapshot(['gone', flows[0].id], signal), /500/);
});

test('cancelling a snapshot aborts in-flight fetches and prevents remaining requests', async (t) => {
  const controller = new AbortController();
  const fetch = t.mock.method(globalThis, 'fetch', (_url, { signal }) => new Promise((_resolve, reject) => {
    signal.addEventListener('abort', () => reject(signal.reason), { once: true });
  }));
  const pending = loadSearchSnapshot(Array.from({ length: 20 }, (_, i) => String(i)), controller.signal);
  controller.abort();
  await assert.rejects(pending, { name: 'AbortError' });
  assert.equal(fetch.mock.calls.length, 6);
  await assert.rejects(loadSearchSnapshot(['never'], controller.signal), { name: 'AbortError' });
  assert.equal(fetch.mock.calls.length, 6);
});
