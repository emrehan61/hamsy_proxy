import assert from 'node:assert/strict';
import { test, after } from 'node:test';
import { execFileSync } from 'node:child_process';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createRequire } from 'node:module';
const require = createRequire(import.meta.url);
const output = mkdtempSync(join(tmpdir(), 'hamsy-context-tests-'));
execFileSync(process.execPath, [require.resolve('typescript/bin/tsc'), '--outDir', output, '--noEmit', 'false', '--module', 'commonjs', '--moduleResolution', 'node', '--target', 'es2022', '--skipLibCheck', 'src/lib/agentContext.ts', 'src/lib/har.ts'], { cwd: new URL('..', import.meta.url) });
writeFileSync(join(output, 'package.json'), '{"type":"commonjs"}');
after(() => rmSync(output, { recursive: true, force: true }));
const { getMatchContext, prepareContext } = require(join(output, 'agentContext.js'));
const { parseHar } = require(join(output, 'har.js'));
function flow() {
  return parseHar({ log: { entries: [{
    request: { method: 'POST', url: 'https://user:PRIVATE@example.test/?token=PRIVATE&public=needle', headers: [{ name: 'Authorization', value: 'PRIVATE' }] },
    response: { status: 200, headers: [], content: { mimeType: 'application/json', text: JSON.stringify({ password: 'PRIVATE', padding: 'x'.repeat(20000), result: 'café 🍋 needle result' }) } },
  }] } }).flows[0];
}
test('late structured context masks before matching and respects UTF8 byte limits', () => {
  const f = flow(), q = { query: 'needle', field: 'Response body', maxContextBytes: 31 };
  const result = getMatchContext(f, q), excerpt = result.matches[0];
  assert.equal(excerpt.match, 'needle');
  assert.equal(excerpt.truncatedBefore, true);
  const text = excerpt.before + excerpt.match + excerpt.after;
  assert.ok(Buffer.byteLength(text) <= 31);
  assert.ok(!text.includes('\uFFFD'));
  for (const field of ['Response body', 'Request header', 'URL']) assert.deepEqual(getMatchContext(f, { ...q, field, query: 'PRIVATE' }).matches, []);
  assert.ok(f.response.body.data.includes('PRIVATE'));
});
test('WebSocket evidence is bounded, redacted and continues from returned indices', () => {
  const f = flow();
  f.wsMessages = Array.from({ length: 5 }, (_, i) => ({ opcode: 'text', data: JSON.stringify({ token: 'PRIVATE', padding: 'x'.repeat(18000), event: `needle ${i}` }), size: 0 }));
  const q = { query: 'needle', field: 'WebSocket message' }, first = getMatchContext(f, q);
  assert.equal(first.matches.length, 3); assert.equal(first.nextMessageIndex, 3);
  const last = getMatchContext(f, { ...q, messageIndex: first.nextMessageIndex });
  assert.equal(last.matches.length, 2); assert.equal(last.matches[0].messageIndex, 3); assert.equal(last.hasMore, false);
  assert.deepEqual(getMatchContext(f, { ...q, query: 'PRIVATE' }).matches, []);
  const prepared = prepareContext(f, { query: 'needle', field: 'Response body' });
  assert.equal(prepared.sources.length, 1); assert.ok(!JSON.stringify(prepared).includes('wsMessages'));
});
test('base64 text, forms and invalid/truncated/binary sources fail safely', () => {
  const f = flow(), q = { query: 'needle', field: 'Response body' };
  f.response.body = { kind: 'base64', data: Buffer.from('token=PRIVATE&public=needle').toString('base64'), size: 32, truncated: false };
  assert.equal(getMatchContext(f, q).matches[0].match, 'needle');
  assert.ok(!JSON.stringify(getMatchContext(f, q)).includes('PRIVATE'));
  f.response.body.data = Buffer.from('\0needle').toString('base64');
  assert.deepEqual(getMatchContext(f, q).matches, []);
  f.response.body.truncated = true; assert.throws(() => getMatchContext(f, q));
  f.response.body = { kind: 'text', data: '{"password":"PRIVATE","result":"needle', size: 45, truncated: false };
  assert.throws(() => getMatchContext(f, q));
  assert.throws(() => getMatchContext(flow(), { ...q, query: '[', regex: true }));
  assert.throws(() => getMatchContext(flow(), { ...q, maxContextBytes: 4097 }));
});
test('oversized selected metadata fails before URL parsing or pair copying', () => {
  const f = flow(), oversized = 'x'.repeat(16 * 1024 * 1024 + 1);
  f.url = oversized;
  assert.throws(() => prepareContext(f, { query: 'x', field: 'URL' }), /16 MiB/);
  f.request.headers[0].value = oversized;
  assert.throws(() => prepareContext(f, { query: 'x', field: 'Request header' }), /16 MiB/);
});
