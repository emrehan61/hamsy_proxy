import assert from 'node:assert/strict';
import { test } from 'node:test';
import { spawn, execFile } from 'node:child_process';
import { randomUUID } from 'node:crypto';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { createServer } from 'node:net';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';
import { chromium } from 'playwright';

const exec = promisify(execFile);
const root = resolve(dirname(fileURLToPath(import.meta.url)), '../../..');
const binary = resolve(process.env.HAMSY_BIN ?? join(root, 'target/debug/hamsy'));
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));

async function port() {
  const listener = createServer();
  await new Promise(resolve => listener.listen(0, '127.0.0.1', resolve));
  const value = listener.address().port;
  await new Promise(resolve => listener.close(resolve));
  return value;
}

async function until(check, label, timeout = 10_000) {
  const end = Date.now() + timeout;
  while (Date.now() < end) {
    const value = await check();
    if (value) return value;
    await sleep(50);
  }
  assert.fail(`Timed out waiting for ${label}`);
}

test('real browser advertises, reads, restores and withdraws imported HAR sessions', { timeout: 90_000 }, async t => {
  const directory = await mkdtemp(join(tmpdir(), 'hamsy-browser-'));
  const profile = join(directory, 'profile');
  const uiPort = await port();
  let proxyPort = await port();
  while (proxyPort === uiPort) proxyPort = await port();
  const origin = `http://127.0.0.1:${uiPort}`;
  const env = { ...process.env, HAMSY_HOME: profile };
  delete env.HAMSY_UI_DIR; // Exercise the release's embedded UI, not another checkout.
  const server = spawn(binary, ['run', '--manual', '--no-open', '--no-https',
    '--bind', '127.0.0.1', '--proxy-port', String(proxyPort), '--ui-port', String(uiPort), '--data-dir', profile],
  { cwd: directory, env, stdio: ['ignore', 'pipe', 'pipe'] });
  let logs = '', startupError;
  server.stdout.on('data', value => { logs = (logs + value).slice(-32768); });
  server.stderr.on('data', value => { logs = (logs + value).slice(-32768); });
  server.on('error', error => { startupError = error; });
  let browser;
  t.after(async () => {
    try {
      await browser?.close();
    } finally {
      if (server.exitCode === null) {
        server.kill('SIGINT');
        await until(async () => server.exitCode !== null || server.signalCode !== null, 'test server shutdown', 5000)
          .catch(() => server.kill('SIGKILL'));
      }
      await rm(directory, { recursive: true, force: true });
    }
  });
  await until(async () => {
    if (startupError) throw startupError;
    if (server.exitCode !== null) assert.fail(`Hamsy exited before startup: ${logs}`);
    try { return (await fetch(`${origin}/api/state`, { signal: AbortSignal.timeout(1000) })).ok; }
    catch { return false; }
  }, 'isolated Hamsy API');

  const agent = async (tool, args, success = true) => {
    let output;
    try {
      output = await exec(binary, ['agent', '--api-url', origin, '--viewer-data-dir', profile,
        'call', tool, '--arguments', JSON.stringify(args)], { cwd: directory, env, timeout: 20_000 });
      assert.equal(success, true, 'agent command unexpectedly succeeded');
    } catch (error) {
      if (success || !error.stdout) throw error;
      output = error;
    }
    const value = JSON.parse(output.stdout);
    assert.equal(Boolean(value.error), !success, JSON.stringify(value));
    return value;
  };
  const archives = async () => {
    const response = await fetch(`${origin}/api/sessions`);
    assert.equal(response.status, 200);
    return (await response.json()).sessions.filter(session => session.kind === 'har');
  };

  const id = randomUUID();
  const har = { log: { version: '1.2', creator: { name: 'browser integration', version: '1' }, entries: [{
    startedDateTime: '2026-01-01T00:00:00Z', time: 1,
    request: { method: 'GET', url: 'https://example.test/inspection?token=URL_SECRET', httpVersion: 'HTTP/1.1',
      headers: [{ name: 'Authorization', value: 'Bearer HEADER_SECRET' }], queryString: [{ name: 'token', value: 'URL_SECRET' }] },
    response: { status: 200, statusText: 'OK', httpVersion: 'HTTP/1.1', headers: [],
      content: { mimeType: 'application/json', text: JSON.stringify({ padding: 'x'.repeat(32_000), note: 'late body marker', token: 'BODY_SECRET' }) } },
    _hamsy: { flowId: id },
    _webSocketMessages: [{ type: 'receive', time: 1, opcode: 1, data: JSON.stringify({ note: 'socket marker', token: 'WS_SECRET' }) }],
  }] } };
  const file = join(directory, 'browser-session.har');
  await writeFile(file, JSON.stringify(har));
  browser = await chromium.launch({ headless: true,
    ...(process.env.HAMSY_BROWSER_EXECUTABLE ? { executablePath: process.env.HAMSY_BROWSER_EXECUTABLE } : {}) });
  const context = await browser.newContext();
  let page = await context.newPage();
  await page.goto(origin);
  await page.locator('input.traffic-page__file-input').setInputFiles(file);
  const registered = await until(async () => (await archives()).find(session => session.flowCount === 1), 'browser HAR advertisement');
  const sessionId = `app:${registered.id}`;
  await until(() => page.evaluate(session => new Promise((resolve, reject) => {
    const opened = indexedDB.open('hamsy-har');
    opened.onerror = () => reject(opened.error);
    opened.onsuccess = () => {
      const db = opened.result;
      const transaction = db.transaction('sessions', 'readonly');
      const request = transaction.objectStore('sessions').get(session);
      let persisted = false;
      request.onsuccess = () => { persisted = request.result?.flows?.length === 1; };
      transaction.oncomplete = () => { db.close(); resolve(persisted); };
      transaction.onerror = () => { db.close(); reject(transaction.error); };
    };
  }), registered.id), 'committed HAR persistence in IndexedDB');
  assert.equal((await fetch(`${origin}/api/flows`).then(response => response.json())).flows.length, 0, 'HAR must stay outside live capture');

  const verify = async () => {
    const search = await agent('search_flows', { sessionId, query: 'late body marker' });
    assert.deepEqual(search.matches.map(match => match.flowId), [id]);
    assert(search.matches[0].fields.includes('Response body'));
    assert(!JSON.stringify(search).includes('BODY_SECRET'));
    const body = await agent('get_match_context', { sessionId, id, query: 'late body marker', field: 'Response body', maxContextBytes: 512 });
    assert.equal(body.matches.length, 1);
    assert.equal(body.matches[0].match, 'late body marker');
    assert(JSON.stringify(body).includes('[REDACTED]'));
    const socket = await agent('get_match_context', { sessionId, id, query: 'socket marker', field: 'WebSocket message', messageIndex: 0 });
    assert.equal(socket.matches[0].messageIndex, 0);
    assert.equal(socket.matches[0].match, 'socket marker');
    assert(JSON.stringify(socket).includes('[REDACTED]'));
    const url = await agent('get_match_context', { sessionId, id, query: '/inspection', field: 'URL' });
    const header = await agent('get_match_context', { sessionId, id, query: 'Authorization', field: 'Request header' });
    const evidence = JSON.stringify({ body, socket, url, header });
    for (const secret of ['BODY_SECRET', 'WS_SECRET', 'URL_SECRET', 'HEADER_SECRET']) assert(!evidence.includes(secret), secret);
  };
  await verify();

  const invalid = await agent('search_flows', { sessionId, query: '[', regex: true }, false);
  assert.equal(invalid.error.code, 'invalid_arguments');
  assert.equal(invalid.error.retryable, false);

  // Controlled worker faults exercise the browser bridge's recovery contract.
  // Normal import/search/context above and below still use real Web Workers.
  await page.evaluate(() => {
    window.__hamsyOriginalWorker = window.Worker;
    window.__hamsyWorkerCount = 0;
    window.Worker = class {
      constructor() { window.__hamsyWorkerCount++; }
      postMessage() {}
      terminate() {}
    };
  });
  try {
    const pending = [0, 1].map(() => agent('search_flows', { sessionId, query: 'late body marker' }, false)
      .then(value => ({ value }), error => ({ error })));
    await until(() => page.evaluate(() => window.__hamsyWorkerCount === 2), 'two pending browser workers');
    const busy = await agent('search_flows', { sessionId, query: 'late body marker' }, false);
    assert.equal(busy.error.code, 'busy');
    assert.equal(busy.error.retryable, true);
    for (const completed of await Promise.all(pending)) {
      if (completed.error) throw completed.error;
      assert.equal(completed.value.error.code, 'timeout');
      assert.equal(completed.value.error.retryable, true);
    }
    await page.evaluate(() => {
      window.Worker = class {
        postMessage() { queueMicrotask(() => this.onerror?.(new Event('error'))); }
        terminate() {}
      };
    });
    const failed = await agent('get_match_context', { sessionId, id, query: 'late body marker', field: 'Response body' }, false);
    assert.equal(failed.error.code, 'session_unavailable');
    assert.equal(failed.error.retryable, true);
  } finally {
    await page.evaluate(() => {
      window.Worker = window.__hamsyOriginalWorker;
      delete window.__hamsyOriginalWorker;
      delete window.__hamsyWorkerCount;
    });
  }

  // A real reload restores the archive from IndexedDB; no test fixture mocks
  // the worker, session store, websocket, HTTP API, or agent dispatcher.
  await page.reload();
  await until(async () => (await archives()).find(session => session.id === registered.id), 'restored HAR advertisement');
  await verify();
  await page.close();
  await until(async () => (await archives()).length === 0, 'closed browser session withdrawal');
  const unavailable = await agent('search_flows', { sessionId, query: 'late body marker' }, false);
  assert.equal(unavailable.error.code, 'session_unavailable');
  assert.equal(unavailable.error.retryable, false);

  page = await context.newPage();
  await page.goto(origin);
  await until(async () => (await archives()).find(session => session.id === registered.id), 'reopened persisted HAR session');
  await verify();
});
