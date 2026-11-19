import { spawn } from 'node:child_process';
import { mkdtemp, mkdir, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { createInterface } from 'node:readline';
import { join, resolve } from 'node:path';

const executable = process.argv[2];
if (!executable) throw new Error('Provide an absolute Claude executable path');
const root = await mkdtemp(resolve('output/claude-restore-'));
const home = join(root, 'home');
await mkdir(home);
const requests = [];
const server = createServer(async (request, response) => {
  const chunks = [];
  for await (const chunk of request) chunks.push(chunk);
  if (!request.url.includes('/messages') || request.url.includes('count_tokens')) {
    response.writeHead(200, { 'content-type': 'application/json' }); response.end('{"input_tokens":1,"data":[]}'); return;
  }
  requests.push({ path: request.url, header: request.headers['x-synaroute-fixture'] ?? null });
  const body = JSON.parse(Buffer.concat(chunks));
  response.writeHead(200, { 'content-type': 'text/event-stream' });
  const events = [
    { type: 'message_start', message: { id: 'msg_fixture', type: 'message', role: 'assistant', model: body.model, content: [], stop_reason: null, usage: { input_tokens: 1, output_tokens: 1 } } },
    { type: 'content_block_start', index: 0, content_block: { type: 'text', text: '' } },
    { type: 'content_block_delta', index: 0, delta: { type: 'text_delta', text: 'fixture ok' } },
    { type: 'content_block_stop', index: 0 },
    { type: 'message_delta', delta: { stop_reason: 'end_turn', stop_sequence: null }, usage: { output_tokens: 1 } },
    { type: 'message_stop' },
  ];
  response.end(events.map(event => `event: ${event.type}\ndata: ${JSON.stringify(event)}\n\n`).join(''));
});
await new Promise(resolveReady => server.listen(0, '127.0.0.1', resolveReady));
const environment = { ...process.env };
for (const key of Object.keys(environment)) if (/^(ANTHROPIC|CLAUDE|CODEX|OPENAI|AWS|AZURE|GOOGLE|HTTP_PROXY|HTTPS_PROXY|ALL_PROXY)/i.test(key)) delete environment[key];
Object.assign(environment, { CLAUDE_CONFIG_DIR: home, ANTHROPIC_API_KEY: 'fixture-not-a-key', ANTHROPIC_BASE_URL: `http://127.0.0.1:${server.address().port}`,
  CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC: '1', NO_PROXY: '*' });
await writeFile(join(home, '.claude.json'), JSON.stringify({ hasCompletedOnboarding: true }));
const settings = join(home, 'settings.json');
await writeFile(settings, JSON.stringify({ env: { ANTHROPIC_CUSTOM_HEADERS: 'X-SynaRoute-Fixture: original' } }));
const child = spawn(resolve(executable), ['-p', '--input-format', 'stream-json', '--output-format', 'stream-json', '--verbose', '--tools', '', '--model', 'claude-sonnet-4-6', '--setting-sources', 'user'], {
  cwd: root, env: environment, windowsHide: true, stdio: ['pipe', 'pipe', 'pipe'],
});
const events = []; let errors = '';
child.stderr.on('data', chunk => { errors = (errors + chunk.toString()).slice(-6000); });
child.on('error', error => { errors += error.message; });
createInterface({ input: child.stdout }).on('line', line => { try { events.push(JSON.parse(line)); } catch {} });
const send = value => child.stdin.write(`${JSON.stringify(value)}\n`);
async function waitFor(predicate) {
  const deadline = Date.now() + 18000;
  while (Date.now() < deadline) {
    const match = events.find(predicate);
    if (match) return match;
    if (child.exitCode !== null) throw new Error(`Client exited ${child.exitCode}: ${errors}`);
    await new Promise(resolveWait => setTimeout(resolveWait, 25));
  }
  throw new Error(`Client timeout: ${errors}`);
}
const report = { root, requests, turns: [] };
try {
  send({ type: 'control_request', request_id: 'initialize', request: { subtype: 'initialize' } });
  await waitFor(event => event.response?.request_id === 'initialize');
  for (const phase of ['initial', 'removed', 'empty']) {
    if (phase !== 'initial') {
      await writeFile(settings, JSON.stringify({ env: phase === 'removed' ? {} : { ANTHROPIC_CUSTOM_HEADERS: '' } }));
      await new Promise(resolveWait => setTimeout(resolveWait, 1500));
    }
    const previous = events.length;
    send({ type: 'user', message: { role: 'user', content: `Reply fixture ok for ${phase}.` } });
    const result = await waitFor(event => events.indexOf(event) >= previous && event.type === 'result');
    report.turns.push({ phase, result: result.subtype, header: requests.at(-1)?.header });
  }
  report.confirmed = report.turns[0].header === 'original' && report.turns[1].header === 'original' && report.turns[2].header === null;
  if (!report.confirmed) process.exitCode = 1;
} catch (error) { report.error = error.message; process.exitCode = 1; }
finally {
  child.stdin.end(); child.kill(); server.closeAllConnections(); server.close();
  await writeFile(join(root, 'report.json'), JSON.stringify(report, null, 2));
  console.log(JSON.stringify(report, null, 2));
}
