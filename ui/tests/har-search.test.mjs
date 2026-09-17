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
  '--skipLibCheck', 'src/lib/harSearch.ts', 'src/lib/har.ts', 'src/lib/filter.ts'], { cwd: new URL('..', import.meta.url) });
writeFileSync(join(output, 'package.json'), '{"type":"commonjs"}');
after(() => rmSync(output, { recursive: true, force: true }));
const { searchHar } = require(join(output, 'harSearch.js'));
const { parseHar } = require(join(output, 'har.js'));
const { filterFlows } = require(join(output, 'filter.js'));

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
