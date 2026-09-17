import assert from 'node:assert/strict';
import { test, after } from 'node:test';
import { execFileSync } from 'node:child_process';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);
const output = mkdtempSync(join(tmpdir(), 'hamsy-session-tests-'));
execFileSync(process.execPath, [require.resolve('typescript/bin/tsc'), '--outDir', output,
  '--noEmit', 'false', '--module', 'commonjs', '--moduleResolution', 'node', '--target', 'es2022',
  '--skipLibCheck', 'src/lib/sessionReads.ts'], { cwd: new URL('..', import.meta.url) });
writeFileSync(join(output, 'package.json'), '{"type":"commonjs"}');
after(() => rmSync(output, { recursive: true, force: true }));
const { readSession } = require(join(output, 'sessionReads.js'));
const { parseHar } = require(join(output, 'har.js'));
const flows = parseHar({ log: { entries: Array.from({ length: 7 }, (_, i) => ({
  request: { method: i % 2 ? 'POST' : 'GET', url: `https://example.test/${i}`,
    headers: [{ name: 'X-Test', value: 'find-header' }],
    postData: { mimeType: 'text/plain', text: 'original request' } },
  response: { status: i % 2 ? 500 : 200, headers: [], content: { size: 17, mimeType: 'text/plain', text: 'original response' } },
})) } }).flows;
const read = (operation, query = {}, input = flows) => readSession(input, { type: 'read', requestId: 'request', sessionId: 'session', operation, query });

test('forward pagination includes sequence zero and every entry exactly once', () => {
  const collected = [];
  let query = { limit: '2' };
  while (true) {
    const page = read('list_flows', query).flows;
    collected.push(...page.map(f => f.id));
    if (!page.length) break;
    query = { ...query, afterSeq: String(page.at(-1).seq) };
  }
  assert.deepEqual(collected, flows.map(f => f.id));
  assert.equal(read('list_flows').flows[0].seq, 0);
  assert.equal(read('list_flows').flows[0].request, undefined);
});
test('filters combine consistently and ranges are bounded', () => {
  assert.equal(read('list_flows', { host: 'EXAMPLE.TEST', methods: 'post', statusClass: '5', q: 'FIND-HEADER' }).flows.length, 3);
  assert.equal(read('list_flows', { host: 'missing.test' }).flows.length, 0);
  for (const query of [{ limit: '202' }, { afterSeq: '-1' }, { statusClass: '0' }, { limit: 'nan' }]) {
    assert.throws(() => read('list_flows', query));
  }
});
test('body omission leaves original records intact and respects explicit text opt-in', () => {
  const snapshot = structuredClone(flows);
  const omitted = read('get_flow', { id: flows[0].id });
  assert.equal(omitted.request.body.data, '');
  assert.equal(omitted.response.body.size, 17);
  assert.deepEqual(omitted.wsMessages, []);
  assert.equal(read('get_flow', { id: flows[0].id, includeBodies: 'true' }).response.body.data, 'original response');
  assert.deepEqual(flows, snapshot);
  const binary = structuredClone(flows[0]);
  binary.response.body.kind = 'base64';
  assert.equal(read('get_flow', { id: binary.id, includeBodies: 'true' }, [binary]).response.body.data, '');
});
test('selection never escapes the supplied session and exported bodies are empty', () => {
  assert.throws(() => read('get_flow', { id: flows[0].id }, flows.slice(1)));
  const har = read('export_har', { ids: flows[0].id });
  assert.equal(har.log.entries.length, 1);
  assert.ok(!JSON.stringify(har).includes('original response'));
  assert.equal(read('export_har', { ids: flows[0].id }, flows.slice(1)).log.entries.length, 0);
  assert.throws(() => read('export_har', {}));
  assert.throws(() => read('replay_request', {}));
  assert.throws(() => read('read_file', { path: '/anything' }));
});
