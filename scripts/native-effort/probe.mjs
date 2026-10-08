// Real client processes, isolated homes, loopback fixture only. No live model credentials.
import { spawn } from 'node:child_process';
import { mkdtemp, writeFile } from 'node:fs/promises';
import { tmpdir, homedir } from 'node:os';
import { join, resolve } from 'node:path';
import { createServer } from 'node:http';
import { createInterface } from 'node:readline';

const args = process.argv.slice(2);
const value = name => args[args.indexOf(name) + 1];
if (!args.includes('--codex') || !args.includes('--claude')) {
  throw new Error('Usage: node scripts/native-effort/probe.mjs --codex <exe> --claude <exe>');
}
const root = await mkdtemp(join(tmpdir(), 'synaroute-native-probe-'));
const requests = [];
const server = createServer(async (req, res) => {
  const chunks = [];
  for await (const chunk of req) chunks.push(chunk);
  let body; try { body = JSON.parse(Buffer.concat(chunks).toString()); } catch { body = {}; }
  requests.push({ path: req.url, model: body.model, effort: body.reasoning?.effort ?? body.output_config?.effort,
    updates: body.input?.filter?.(i => i.type === 'configuration_update') });
  if (req.url === '/judge') {
    await new Promise(r => setTimeout(r, 600));
    res.writeHead(200, { 'content-type': 'application/json' });
    res.end(JSON.stringify({ choices: [{ message: { content: JSON.stringify({ effort: 'high' }) } }] }));
  } else if (req.url.includes('/responses')) {
    const response = { id: 'resp_fixture', object: 'response', status: 'completed', model: body.model,
      output: [{ id: 'msg_fixture', type: 'message', role: 'assistant', status: 'completed', content: [{ type: 'output_text', text: 'fixture ok', annotations: [] }] }],
      usage: { input_tokens: 1, output_tokens: 1, total_tokens: 2, input_tokens_details: { cached_tokens: 0 } } };
    res.writeHead(200, { 'content-type': 'text/event-stream' });
    const item = response.output[0];
    const events = [
      { type: 'response.created', response: { ...response, status: 'in_progress', output: [] } },
      { type: 'response.output_item.added', output_index: 0, item: { ...item, status: 'in_progress', content: [] } },
      { type: 'response.content_part.added', item_id: item.id, output_index: 0, content_index: 0, part: { type: 'output_text', text: '', annotations: [] } },
      { type: 'response.output_text.delta', item_id: item.id, output_index: 0, content_index: 0, delta: 'fixture ok' },
      { type: 'response.output_text.done', item_id: item.id, output_index: 0, content_index: 0, text: 'fixture ok' },
      { type: 'response.content_part.done', item_id: item.id, output_index: 0, content_index: 0, part: item.content[0] },
      { type: 'response.output_item.done', output_index: 0, item },
      { type: 'response.completed', response },
    ];
    res.end(events.map((e, sequence_number) => `event: ${e.type}\ndata: ${JSON.stringify({ ...e, sequence_number })}\n\n`).join(''));
  } else if (req.url.includes('/messages') && !req.url.includes('count_tokens')) {
    const message = { id: 'msg_fixture', type: 'message', role: 'assistant', model: body.model,
      content: [], stop_reason: null, usage: { input_tokens: 1, output_tokens: 1 } };
    res.writeHead(200, { 'content-type': 'text/event-stream' });
    const events = [{ type: 'message_start', message }, { type: 'content_block_start', index: 0, content_block: { type: 'text', text: '' } },
      { type: 'content_block_delta', index: 0, delta: { type: 'text_delta', text: 'fixture ok' } }, { type: 'content_block_stop', index: 0 },
      { type: 'message_delta', delta: { stop_reason: 'end_turn', stop_sequence: null }, usage: { output_tokens: 1 } }, { type: 'message_stop' }];
    res.end(events.map(e => `event: ${e.type}\ndata: ${JSON.stringify(e)}\n\n`).join(''));
  } else {
    res.writeHead(200, { 'content-type': 'application/json' });
    res.end(JSON.stringify({ data: [] }));
  }
});
await new Promise(r => server.listen(0, '127.0.0.1', r));
const endpoint = `http://127.0.0.1:${server.address().port}`;
const env = { ...process.env };
for (const key of Object.keys(env)) {
  if (/^(OPENAI|ANTHROPIC|CLAUDE|CODEX|AZURE|AWS|GOOGLE|HTTP_PROXY|HTTPS_PROXY|ALL_PROXY)/i.test(key)) delete env[key];
}
Object.assign(env, { CODEX_HOME: root, CLAUDE_CONFIG_DIR: join(root, 'claude'), ANTHROPIC_API_KEY: 'fixture-not-a-real-key',
  ANTHROPIC_BASE_URL: endpoint, SYNAROUTE_FIXTURE_KEY: 'fixture-not-a-real-key', CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC: '1', NO_PROXY: '*' });

function client(exe, argv, kind) {
  const process = spawn(resolve(exe), argv, { cwd: root, env, windowsHide: true, stdio: ['pipe', 'pipe', 'pipe'] });
  const pending = new Map(); const events = []; let id = 0; let stderr = '';
  process.stderr.on('data', b => { stderr = (stderr + b).slice(-4000); });
  createInterface({ input: process.stdout }).on('line', line => {
    let msg; try { msg = JSON.parse(line); } catch { return; }
    const key = kind === 'codex' ? msg.id : msg.response?.request_id;
    if (pending.has(key)) { const done = pending.get(key); pending.delete(key); done(msg); }
    else events.push(msg);
  });
  function send(msg) { process.stdin.write(JSON.stringify(msg) + '\n'); }
  return { events, process, stderr: () => stderr, send,
    call(method, params) {
      const requestId = kind === 'codex' ? ++id : `probe-${++id}`;
      return new Promise((resolvePromise, reject) => {
        const timer = setTimeout(() => { pending.delete(requestId); reject(new Error(`${kind} ${method} timed out: ${stderr}`)); }, 12000);
        pending.set(requestId, msg => { clearTimeout(timer); resolvePromise(msg); });
        send(kind === 'codex' ? { id: requestId, method, params } : { type: 'control_request', request_id: requestId, request: { subtype: method, ...params } });
      });
    },
    close() { process.stdin.end(); process.kill(); }
  };
}
const report = { root, requests, codex: {}, claude: {} };
try {
  const config = [
    'model_provider="fixture"', 'model_providers.fixture.name="Local fixture"',
    `model_providers.fixture.base_url="${endpoint}/v1"`, 'model_providers.fixture.wire_api="responses"',
    'model_providers.fixture.env_key="SYNAROUTE_FIXTURE_KEY"', 'model_providers.fixture.requires_openai_auth=false',
    'model_providers.fixture.supports_websockets=false', 'features.multi_agent=false', 'analytics.enabled=false',
  ].flatMap(s => ['-c', s]);
  const codex = client(value('--codex'), ['app-server', '--listen', 'stdio://', ...config], 'codex');
  try {
    report.codex.initialize = await codex.call('initialize', { clientInfo: { name: 'synaroute_probe', version: '1' }, capabilities: { experimentalApi: true } });
    codex.send({ method: 'initialized' });
    const models = await codex.call('model/list', {});
    report.codex.models = models.result?.data?.map(m => ({ id: m.id, model: m.model, supportedReasoningEfforts: m.supportedReasoningEfforts }));
    const model = models.result?.data?.find(m => m.supportedReasoningEfforts?.some(e => e.reasoningEffort === 'high'));
    if (!model) throw new Error('No model with native high effort advertised');
    const started = await codex.call('thread/start', { model: model.model ?? model.id, cwd: root, approvalPolicy: 'never', sandbox: 'read-only', ephemeral: true });
    if (started.error) throw new Error(JSON.stringify(started.error));
    const threadId = started.result.thread.id;
    for (const effort of ['low', 'high']) {
      report.codex[effort] = await codex.call('turn/start', { threadId, input: [{ type: 'text', text: 'Reply fixture ok.', text_elements: [] }], effort });
      const turnId = report.codex[effort].result?.turn?.id;
      const deadline = Date.now() + 12000;
      while (!codex.events.some(e => e.method === 'turn/completed' && e.params?.turn?.id === turnId) && Date.now() < deadline) await new Promise(r => setTimeout(r, 50));
      report.codex[`${effort}Completed`] = codex.events.find(e => e.method === 'turn/completed' && e.params?.turn?.id === turnId);
    }
    report.codex.errors = codex.events.filter(e => e.method === 'error');
  } catch (error) { report.codex.error = error.message; } finally { codex.close(); }
  const claude = client(value('--claude'), ['--bare', '-p', '--input-format', 'stream-json', '--output-format', 'stream-json', '--verbose', '--tools', '', '--model', 'claude-sonnet-4-6'], 'claude');
  try {
    report.claude.initialize = await claude.call('initialize', {});
    for (const effort of ['low', 'high']) {
      report.claude[effort] = await claude.call('apply_flag_settings', { settings: { effortLevel: effort } });
      const before = claude.events.length;
      claude.send({ type: 'user', message: { role: 'user', content: 'Reply fixture ok.' } });
      const deadline = Date.now() + 12000;
      while (!claude.events.slice(before).some(e => e.type === 'result') && Date.now() < deadline) await new Promise(r => setTimeout(r, 50));
      report.claude[`${effort}Completed`] = claude.events.slice(before).find(e => e.type === 'result');
    }
  } catch (error) { report.claude.error = error.message; } finally { claude.close(); }
  if (args.includes('--adapter-tests')) {
    await writeFile(join(root, 'config.toml'), `model_provider = "fixture"\n[model_providers.fixture]\nname = "Local fixture"\nbase_url = "${endpoint}/v1"\nwire_api = "responses"\nenv_key = "SYNAROUTE_FIXTURE_KEY"\nrequires_openai_auth = false\nsupports_websockets = false\n`);
    const before = requests.length;
    const exitCode = await new Promise((done, reject) => {
      const child = spawn(args.includes('--cargo') ? value('--cargo') : join(homedir(), '.cargo', 'bin', process.platform === 'win32' ? 'cargo.exe' : 'cargo'), ['test', '--manifest-path', 'src-tauri/Cargo.toml', 'native_fixture', '--', '--ignored', '--nocapture'], {
        env: { ...env, SYNAROUTE_FIXTURE_ROOT: root, SYNAROUTE_FIXTURE_JUDGE: endpoint + '/judge', SYNAROUTE_FIXTURE_CODEX: resolve(value('--codex')), SYNAROUTE_FIXTURE_CLAUDE: resolve(value('--claude')) },
        windowsHide: true, stdio: ['ignore', 'inherit', 'inherit'],
      });
      child.on('error', reject); child.on('exit', done);
    });
    report.adapter = { exitCode, requests: requests.slice(before) };
    if (report.adapter.requests.filter(r => r.path.includes('/responses') || r.path.includes('/v1/messages?')).length !== 4) process.exitCode = 1;
    if (exitCode !== 0 || ['low', 'high'].some(e => !report.adapter.requests.some(r => r.path.includes('/responses') && r.effort === e)
      || !report.adapter.requests.some(r => r.path.includes('/messages') && r.effort === e))) process.exitCode = 1;
  }
} finally {
  server.closeAllConnections(); server.close();
  await writeFile(join(root, 'report.json'), JSON.stringify(report, null, 2));
  const proof = { report: join(root, 'report.json'), requests, codexError: report.codex.error,
    codexCompleted: ['low', 'high'].map(e => report.codex[`${e}Completed`]?.params?.turn?.status),
    claudeError: report.claude.error, claudeAccepted: ['low', 'high'].map(e => report.claude[e]?.response?.subtype),
    claudeCompleted: ['low', 'high'].map(e => report.claude[`${e}Completed`]?.subtype) };
  console.log(JSON.stringify(proof, null, 2));
  if (proof.codexCompleted.some(s => s !== 'completed') || proof.claudeCompleted.some(s => s !== 'success') || ['low', 'high'].some(e => !requests.some(r => r.path.includes('/responses') && r.effort === e)
    || !requests.some(r => r.path.includes('/messages') && r.effort === e))) process.exitCode = 1;
}
