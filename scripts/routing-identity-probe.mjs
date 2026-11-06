import { spawn } from 'node:child_process';
import { mkdtemp, writeFile, mkdir } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { createServer } from 'node:http';
import { createInterface } from 'node:readline';
import { createHash, randomUUID } from 'node:crypto';

const args = process.argv.slice(2);
const option = name => args.includes(name) ? args[args.indexOf(name) + 1] : null;
const root = await mkdtemp(join(tmpdir(), 'sr-routing-identity-'));
const captures = [];
const digest = value => createHash('sha256').update(String(value)).digest('hex').slice(0, 16);
let active = '';
const server = createServer(async (req, res) => {
  const chunks = [];
  for await (const chunk of req) chunks.push(chunk);
  let body;
  try { body = JSON.parse(Buffer.concat(chunks).toString()); } catch { body = {}; }
  if (/\/(messages|responses)(?:\?|$)/.test(req.url)) {
    const headers = Object.fromEntries(Object.entries(req.headers).filter(([name]) => /session|conversation|thread|turn|agent/i.test(name)).map(([name, value]) => [name, { hash: digest(value), shape: typeof value, matches: String(value) === active }]));
    let metadata = body.metadata;
    if (typeof metadata?.user_id === 'string') {
      try { metadata = { ...metadata, user_id: JSON.parse(metadata.user_id) }; } catch {}
    }
    captures.push({ path: req.url, headers, turnMetadata: req.headers['x-codex-turn-metadata'] ? Object.fromEntries(Object.entries(JSON.parse(req.headers['x-codex-turn-metadata'])).map(([key, value]) => [key, { type: typeof value, hash: digest(value) }])) : null, metadata: metadata && Object.fromEntries(Object.entries(metadata).map(([k, v]) => [k, typeof v === 'object' && v ? Object.fromEntries(Object.entries(v).map(([a, b]) => [a, { hash: digest(b), matches: b === active }])) : { hash: digest(v), matches: v === active, sessionSuffix: typeof v === 'string' && v.endsWith(active) }])), topKeys: Object.keys(body), active: digest(active) });
  }
  res.writeHead(200, { 'content-type': 'text/event-stream' });
  if (req.url.includes('/responses')) {
    const response = { id: `resp_${randomUUID()}`, object: 'response', status: 'completed', model: body.model, output: [{ id: `msg_${randomUUID()}`, type: 'message', role: 'assistant', status: 'completed', content: [{ type: 'output_text', text: 'fixture ok', annotations: [] }] }], usage: { input_tokens: 1, output_tokens: 1, total_tokens: 2 } };
    const item = response.output[0];
    const events = [{ type: 'response.created', response: { ...response, status: 'in_progress', output: [] } },
      { type: 'response.output_item.added', output_index: 0, item: { ...item, content: [], status: 'in_progress' } },
      { type: 'response.content_part.added', item_id: item.id, output_index: 0, content_index: 0, part: { type: 'output_text', text: '', annotations: [] } },
      { type: 'response.output_text.delta', item_id: item.id, output_index: 0, content_index: 0, delta: 'fixture ok' },
      { type: 'response.output_text.done', item_id: item.id, output_index: 0, content_index: 0, text: 'fixture ok' },
      { type: 'response.content_part.done', item_id: item.id, output_index: 0, content_index: 0, part: item.content[0] },
      { type: 'response.output_item.done', output_index: 0, item }, { type: 'response.completed', response }];
    res.end(events.map((e, sequence_number) => `event: ${e.type}\ndata: ${JSON.stringify({ ...e, sequence_number })}\n\n`).join(''));
  } else if (req.url.includes('/messages') && !req.url.includes('count_tokens')) {
    const events = [{ type: 'message_start', message: { id: `msg_${randomUUID()}`, type: 'message', role: 'assistant', model: body.model, content: [], stop_reason: null, usage: { input_tokens: 1, output_tokens: 1 } } },
      { type: 'content_block_start', index: 0, content_block: { type: 'text', text: '' } }, { type: 'content_block_delta', index: 0, delta: { type: 'text_delta', text: 'fixture ok' } },
      { type: 'content_block_stop', index: 0 }, { type: 'message_delta', delta: { stop_reason: 'end_turn', stop_sequence: null }, usage: { output_tokens: 1 } }, { type: 'message_stop' }];
    res.end(events.map(e => `event: ${e.type}\ndata: ${JSON.stringify(e)}\n\n`).join(''));
  } else res.end('{}');
});
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
const endpoint = `http://127.0.0.1:${server.address().port}`;
const env = { ...process.env };
for (const key of Object.keys(env)) if (/^(OPENAI|ANTHROPIC|CLAUDE|CODEX|AZURE|AWS|GOOGLE|HTTP_PROXY|HTTPS_PROXY|ALL_PROXY)/i.test(key)) delete env[key];
Object.assign(env, { CODEX_HOME: root, CLAUDE_CONFIG_DIR: join(root, 'claude'), ANTHROPIC_API_KEY: 'fixture-not-a-real-key', ANTHROPIC_BASE_URL: endpoint, SYNAROUTE_FIXTURE_KEY: 'fixture-not-a-real-key', CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC: '1', NO_PROXY: '*' });
await mkdir(env.CLAUDE_CONFIG_DIR, { recursive: true });
const clients = [];
function client(exe, argv, kind) {
  const child = spawn(resolve(exe), argv, { cwd: root, env, windowsHide: true, stdio: ['pipe', 'pipe', 'pipe'] });
  clients.push(child);
  let seq = 0;
  const pending = new Map();
  const waiting = new Set();
  const events = [];
  child.stderr.resume();
  createInterface({ input: child.stdout }).on('line', line => {
    let msg; try { msg = JSON.parse(line); } catch { return; }
    const id = kind === 'codex' ? msg.id : msg.response?.request_id;
    if (pending.has(id)) { pending.get(id)(msg); pending.delete(id); }
    else { events.push(msg); for (const notify of waiting) notify(); }
  });
  const send = value => child.stdin.write(JSON.stringify(value) + '\n');
  function wait(predicate) {
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => { waiting.delete(check); reject(new Error('fixture response timeout')); }, 20000);
      const check = () => { const found = events.find(predicate); if (found) { clearTimeout(timer); waiting.delete(check); resolve(found); } };
      waiting.add(check); check();
    });
  }
  return { events, wait, send, async call(method, params) {
    const id = kind === 'codex' ? ++seq : `probe-${++seq}`;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => { pending.delete(id); reject(new Error(`${kind} ${method} timeout`)); }, 20000);
      pending.set(id, msg => { clearTimeout(timer); resolve(msg); });
      send(kind === 'codex' ? { id, method, params } : { type: 'control_request', request_id: id, request: { subtype: method, ...params } });
    });
  } };
}
const results = {};
try {
  if (option('--claude')) {
    active = randomUUID();
    const cc = client(option('--claude'), ['--bare', '-p', '--session-id', active, '--input-format', 'stream-json', '--output-format', 'stream-json', '--verbose', '--tools', '', '--model', 'claude-sonnet-4-6'], 'claude');
    await cc.call('initialize', {});
    for (let n = 0; n < 2; n++) {
      const before = cc.events.length;
      cc.send({ type: 'user', message: { role: 'user', content: `Fixture routing turn ${n}.` } });
      await cc.wait(e => cc.events.indexOf(e) >= before && e.type === 'result');
    }
    results.claude = 'two turns complete';
  }
  if (option('--codex')) {
    const config = ['model_provider="fixture"', 'model_providers.fixture.name="Fixture"', `model_providers.fixture.base_url="${endpoint}/v1"`, 'model_providers.fixture.wire_api="responses"', 'model_providers.fixture.env_key="SYNAROUTE_FIXTURE_KEY"', 'model_providers.fixture.requires_openai_auth=false', 'model_providers.fixture.supports_websockets=false', 'analytics.enabled=false'].flatMap(c => ['-c', c]);
    const cx = client(option('--codex'), ['app-server', '--listen', 'stdio://', ...config], 'codex');
    await cx.call('initialize', { clientInfo: { name: 'routing_fixture', version: '1' }, capabilities: { experimentalApi: true } });
    cx.send({ method: 'initialized' });
    const models = await cx.call('model/list', {});
    const model = models.result.data[0].model ?? models.result.data[0].id;
    const thread = await cx.call('thread/start', { model, cwd: root, approvalPolicy: 'never', sandbox: 'read-only', ephemeral: true });
    active = thread.result.thread.id;
    for (let n = 0; n < 2; n++) {
      const turn = await cx.call('turn/start', { threadId: active, input: [{ type: 'text', text: `Fixture routing turn ${n}.`, text_elements: [] }] });
      await cx.wait(e => e.method === 'turn/completed' && e.params?.turn?.id === turn.result.turn.id);
    }
    results.codex = 'two turns complete';
  }
  if (!captures.length) throw new Error('No model requests captured');
  await writeFile(join(root, 'identity.json'), JSON.stringify({ results, captures }, null, 2));
  console.log(JSON.stringify({ results, captures, report: join(root, 'identity.json') }, null, 2));
} finally {
  for (const child of clients) { child.stdin.end(); child.kill(); }
  server.closeAllConnections();
  server.close();
}
