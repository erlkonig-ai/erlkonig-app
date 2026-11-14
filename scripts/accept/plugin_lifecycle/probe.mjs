// Isolated local CLI acceptance. Never reads real host profiles or starts a chat.
// node probe.mjs [--codex /path/codex] [--claude /path/claude]
//                [--binary /path/erlkonig] [--prepare-helper /path/prepare] [--keep]
// Without --binary, a harmless JS executable stands in for mcp-stdio.
// With --binary, a disposable authenticated HTTP owner exercises the real bridge.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { randomBytes } from 'node:crypto';
import { spawn, spawnSync } from 'node:child_process';
import { createInterface } from 'node:readline';
import { createServer } from 'node:http';

const options = new Map();
for (let i = 2; i < process.argv.length; i++) {
  const key = process.argv[i];
  assert(['--codex', '--claude', '--binary', '--prepare-helper', '--keep'].includes(key), `Unknown option: ${key}`);
  options.set(key, key === '--keep' ? true : process.argv[++i]);
}
function executable(name) {
  if (options.has('--' + name)) return path.resolve(options.get('--' + name));
  const found = spawnSync('which', [name], { encoding: 'utf8' });
  assert.equal(found.status, 0, `${name} is required (or provide --${name})`);
  return found.stdout.trim();
}
const codex = executable('codex'), claude = executable('claude');
const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'erlkonig-plugin-accept-')));
let market = path.join(root, 'market');
const exactPackage = options.has('--prepare-helper');
assert(!exactPackage || options.has('--binary'), '--prepare-helper requires --binary');
const marketplace = exactPackage ? 'erlkonig-local' : 'erlkonig-local-proof';
const plugin = exactPackage ? 'erlkonig' : 'harmless', pluginId = `${plugin}@${marketplace}`;
const serverName = exactPackage ? 'faculties' : 'erlkonig-proof';
const env = {
  HOME: path.join(root, 'home'), CODEX_HOME: path.join(root, 'codex'),
  CLAUDE_CONFIG_DIR: path.join(root, 'claude'), TMPDIR: path.join(root, 'tmp'),
  PATH: process.env.PATH || '/usr/bin:/bin',
  DISABLE_TELEMETRY: '1', DISABLE_ERROR_REPORTING: '1',
  DISABLE_AUTOUPDATER: '1',
  CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC: '1',
};
for (const dir of [env.HOME, env.CODEX_HOME, env.CLAUDE_CONFIG_DIR, env.TMPDIR]) {
  fs.mkdirSync(dir, { recursive: true });
}
const clientBin = path.join(root, 'client-bin');
fs.mkdirSync(clientBin);
fs.symlinkSync(codex, path.join(clientBin, 'codex'));
fs.symlinkSync(claude, path.join(clientBin, 'claude'));
env.PATH = clientBin + path.delimiter + env.PATH;
function write(relative, value) {
  const destination = path.join(root, relative);
  fs.mkdirSync(path.dirname(destination), { recursive: true });
  fs.writeFileSync(destination, typeof value === 'string' ? value : JSON.stringify(value, null, 2));
}
write('codex/config.toml', 'cli_auth_credentials_store = "file"\nmcp_oauth_credentials_store = "file"\n[analytics]\nenabled = false\n');
const children = new Set();
function start(binary, args) {
  const child = spawn(binary, args, { cwd: env.HOME, env, stdio: ['pipe', 'pipe', 'pipe'] });
  children.add(child);
  child.once('exit', () => children.delete(child));
  return child;
}
async function stop(child) {
  if (child.exitCode !== null || child.signalCode !== null) return;
  const exited = new Promise(resolve => child.once('exit', resolve));
  child.stdin.end();
  const timer = setTimeout(() => child.kill('SIGKILL'), 2000);
  await exited;
  clearTimeout(timer);
}
async function run(binary, args, timeoutMs = 15000) {
  const child = start(binary, args);
  let stdout = '', stderr = '';
  child.stdout.on('data', data => stdout += data);
  child.stderr.on('data', data => stderr += data);
  child.stdin.end();
  const timer = setTimeout(() => child.kill('SIGKILL'), timeoutMs);
  const code = await new Promise((resolve, reject) => { child.once('error', reject); child.once('exit', resolve); });
  clearTimeout(timer);
  assert.equal(code, 0, `${path.basename(binary)} ${args.join(' ')} failed: ${stderr}`);
  return stdout;
}
let http = null, codexRegistered = false, claudeRegistered = false;
let unrelatedCodex = false, unrelatedClaude = false;
const unrelatedMarket = 'unrelated-local-proof', unrelatedId = 'unrelated@' + unrelatedMarket;
const requests = [];
const lifecycle = [];
let command = process.execPath;
let args = [path.join(path.dirname(new URL(import.meta.url).pathname), 'mock-app.mjs'), 'mcp-stdio'];

try {
  if (options.has('--binary')) {
    command = path.resolve(options.get('--binary'));
    const dataDir = path.join(root, 'data');
    fs.mkdirSync(dataDir, { mode: 0o700 });
    const token = randomBytes(32).toString('hex'); // Fixture credential, never output.
    fs.writeFileSync(path.join(dataDir, 'mcp.token'), token, { mode: 0o600 });
    http = createServer((request, response) => {
      assert.equal(request.headers.authorization, 'Bearer ' + token);
      let body = '';
      request.on('data', data => body += data);
      request.on('end', () => {
        const message = body ? JSON.parse(body) : null;
        requests.push({ method: request.method, rpcMethod: message?.method });
        if (request.method === 'DELETE') { response.writeHead(200); response.end(); return; }
        if (message?.id === undefined) { response.writeHead(202); response.end(); return; }
        const result = message.method === 'initialize' ? {
          protocolVersion: '2025-06-18', capabilities: { tools: {} },
          serverInfo: { name: 'erlkonig-harmless-proof', version: '0.0.1' },
        } : { tools: [{ name: 'probe', description: 'Harmless local probe.', inputSchema: { type: 'object' } }] };
        response.writeHead(200, { 'Content-Type': 'application/json', 'Mcp-Session-Id': 'fixture-session' });
        response.end(JSON.stringify({ jsonrpc: '2.0', id: message.id, result }));
      });
    });
    await new Promise((resolve, reject) => { http.once('error', reject); http.listen(0, '127.0.0.1', resolve); });
    args = ['mcp-stdio', '--data-dir', dataDir, '--url', `http://127.0.0.1:${http.address().port}/`];
    if (exactPackage) {
      market = (await run(path.resolve(options.get('--prepare-helper')), [dataDir, command, args.at(-1)])).trim();
      assert(market.startsWith(dataDir + '/'), 'renderer escaped its scratch data root');
      for (const provider of ['openai', 'claude']) {
        const mcp = JSON.parse(fs.readFileSync(path.join(market, provider, '.mcp.json'), 'utf8'));
        assert.deepEqual(mcp.mcpServers.faculties, { command, args });
        assert(!JSON.stringify(mcp).includes(token), 'fixture credential entered package');
        assert(fs.existsSync(path.join(market, provider, 'skills/faculties/SKILL.md')));
      }
    }
  }
  const server = { command, args };
  if (!exactPackage) {
  write('market/.agents/plugins/marketplace.json', {
    name: marketplace, plugins: [{ name: plugin, source: { source: 'local', path: './openai' },
      policy: { installation: 'AVAILABLE', authentication: 'ON_INSTALL' }, category: 'Productivity' }],
  });
  write('market/.claude-plugin/marketplace.json', {
    name: marketplace, owner: { name: 'Disposable local proof' },
    plugins: [{ name: plugin, source: './claude', version: '0.0.1', description: 'Harmless local proof.' }],
  });
  write('market/openai/.codex-plugin/plugin.json', { name: plugin, version: '0.0.1', mcpServers: './.mcp.json' });
  write('market/openai/.mcp.json', { mcpServers: { 'erlkonig-proof': server } });
  write('market/claude/.claude-plugin/plugin.json', { name: plugin, version: '0.0.1' });
  write('market/claude/.mcp.json', { mcpServers: { 'erlkonig-proof': { type: 'stdio', ...server } } });
  }

  const versions = { codex: (await run(codex, ['--version'])).trim(), claude: (await run(claude, ['--version'])).trim() };
  await run(claude, ['plugin', 'validate', path.join(market, 'claude')]);
  if (exactPackage) {
    // Exercise the production Rust wrappers, not a reimplementation of them.
    write('unrelated/.agents/plugins/marketplace.json', { name: unrelatedMarket,
      plugins: [{ name: 'unrelated', source: { source: 'local', path: './openai' },
        policy: { installation: 'AVAILABLE', authentication: 'ON_INSTALL' }, category: 'Productivity' }] });
    write('unrelated/.claude-plugin/marketplace.json', { name: unrelatedMarket,
      owner: { name: 'Disposable acceptance' }, plugins: [{ name: 'unrelated', source: './claude' }] });
    write('unrelated/openai/.codex-plugin/plugin.json', { name: 'unrelated', version: '0.0.1' });
    write('unrelated/claude/.claude-plugin/plugin.json', { name: 'unrelated', version: '0.0.1' });
    const unrelatedRoot = path.join(root, 'unrelated');
    await run(codex, ['plugin', 'marketplace', 'add', unrelatedRoot, '--json']); unrelatedCodex = true;
    await run(codex, ['plugin', 'add', unrelatedId, '--json']);
    await run(claude, ['plugin', 'marketplace', 'add', unrelatedRoot]); unrelatedClaude = true;
    await run(claude, ['plugin', 'install', unrelatedId, '--scope', 'user']);
    const relocated = path.join(root, 'relocated-erlkonig');
    fs.symlinkSync(command, relocated);
    function cachedMcp(directory) {
      const files = [];
      for (const entry of fs.readdirSync(directory, { withFileTypes: true })) {
        const file = path.join(directory, entry.name);
        if (entry.isDirectory()) files.push(...cachedMcp(file));
        else if (entry.isFile() && entry.name === '.mcp.json') files.push(file);
      }
      return files;
    }
    for (const [host, binary] of [['codex', codex], ['claude', claude]]) {
      const rows = async () => {
        const list = JSON.parse(await run(binary, ['plugin', 'list', '--json']));
        return host === 'codex' ? list.installed : list;
      };
      const id = row => row.pluginId || row.id;
      const unrelatedBefore = (await rows()).find(row => id(row) === unrelatedId);
      assert(unrelatedBefore?.enabled);
      const native = async (operation, executable = command) => JSON.parse(await run(
        path.resolve(options.get('--prepare-helper')),
        [args[2], executable, args.at(-1), operation, host], 120000));
      assert.equal((await native('inventory')).installed, false);
      assert.equal((await native('install')).installed, true);
      if (host === 'codex') codexRegistered = true; else claudeRegistered = true;
      const installedNative = await native('inventory');
      assert(installedNative.installed && installedNative.enabled && !installedNative.load_errors);
      let disabledObserved = false;
      if (host === 'claude') {
        await run(binary, ['plugin', 'disable', pluginId, '--scope', 'user']);
        const disabled = await native('inventory');
        assert(disabled.installed && !disabled.enabled);
        disabledObserved = true;
      }
      assert.equal((await native('install', relocated)).installed, true);
      const row = (await rows()).find(row => id(row) === pluginId);
      assert(row);
      const files = cachedMcp(host === 'codex' ? path.join(env.CODEX_HOME, 'plugins/cache') : row.installPath);
      const wiring = files.map(file => JSON.parse(fs.readFileSync(file, 'utf8')).mcpServers?.faculties).filter(Boolean);
      assert(wiring.length > 0, 'installed client cache must expose generated MCP wiring');
      assert(wiring.every(server => server.command === relocated), 'refresh retained the old executable path');
      assert.equal((await native('remove')).installed, false);
      assert.equal((await native('inventory')).installed, false);
      assert.deepEqual((await rows()).find(row => id(row) === unrelatedId), unrelatedBefore,
        'native lifecycle changed an unrelated plugin');
      lifecycle.push({ host, nativeInstallInventoryRefreshRemove: true, cachePathRefreshed: true,
        disabledObserved, unrelatedPreserved: true });
    }
    // Restore the exact requested executable before direct host discovery.
    await run(path.resolve(options.get('--prepare-helper')), [args[2], command, args.at(-1)]);
  }
  if (!codexRegistered) await run(codex, ['plugin', 'marketplace', 'add', market, '--json']);
  codexRegistered = true;
  const cm = JSON.parse(await run(codex, ['plugin', 'marketplace', 'list', '--json'])).marketplaces;
  assert(cm.some(row => row.name === marketplace && row.marketplaceSource?.source === market));
  const before = JSON.parse(await run(codex, ['plugin', 'list', '--marketplace', marketplace, '--available', '--json']));
  assert(before.available.some(row => row.pluginId === pluginId && !row.installed));
  await run(codex, ['plugin', 'add', pluginId, '--json']);
  const installed = JSON.parse(await run(codex, ['plugin', 'list', '--marketplace', marketplace, '--json']));
  assert(installed.installed.some(row => row.pluginId === pluginId && row.installed && row.enabled));

  const app = start(codex, ['app-server', '--listen', 'stdio://']);
  const input = createInterface({ input: app.stdout });
  const pending = new Map(); let nextId = 1;
  app.stderr.resume();
  input.on('line', line => { const message = JSON.parse(line); pending.get(message.id)?.(message); });
  async function rpc(method, params) {
    const id = nextId++;
    let timer;
    try {
      return await new Promise((resolve, reject) => {
        pending.set(id, resolve);
        timer = setTimeout(() => reject(Error(`${method} timed out`)), 15000);
        app.stdin.write(JSON.stringify({ id, method, params }) + '\n');
      });
    } finally { clearTimeout(timer); pending.delete(id); }
  }
  const init = await rpc('initialize', { clientInfo: { name: 'erlkonig_local_acceptance', version: '0.0.1' } });
  assert(init.result);
  app.stdin.write(JSON.stringify({ method: 'initialized', params: {} }) + '\n');
  const inventory = await rpc('mcpServerStatus/list', { detail: 'toolsAndAuthOnly' });
  assert(inventory.result?.data.some(row => row.pluginId === pluginId && row.tools?.probe),
    `Codex did not discover probe (fixture HTTP requests: ${requests.length})`);
  await stop(app); input.close();

  if (!claudeRegistered) await run(claude, ['plugin', 'marketplace', 'add', market]);
  claudeRegistered = true;
  const am = JSON.parse(await run(claude, ['plugin', 'marketplace', 'list', '--json']));
  assert(am.some(row => row.name === marketplace && row.source === 'directory' && row.path === market));
  await run(claude, ['plugin', 'install', pluginId, '--scope', 'user']);
  const installedClaude = JSON.parse(await run(claude, ['plugin', 'list', '--json']));
  assert(installedClaude.some(row => row.id === pluginId && row.scope === 'user' && row.enabled));
  const health = await run(claude, ['mcp', 'list']);
  assert(health.includes(`plugin:${plugin}:${serverName}:`) && health.includes('Connected'));
  await run(claude, ['plugin', 'uninstall', pluginId, '--scope', 'user']);
  assert(!JSON.parse(await run(claude, ['plugin', 'list', '--json'])).some(row => row.id === pluginId));
  await run(codex, ['plugin', 'remove', pluginId, '--json']);
  assert(!JSON.parse(await run(codex, ['plugin', 'list', '--marketplace', marketplace, '--json'])).installed.some(row => row.pluginId === pluginId));
  console.log(JSON.stringify({ passed: true, versions, mode: exactPackage ? 'exact-generated-packages-real-bridge' : options.has('--binary') ? 'real-bridge-fixture-owner' : 'harmless-stdio-fixture', lifecycle, codexTools: 'probe', claudeHealth: 'Connected', requests, scratch: options.has('--keep') ? root : null }));
} finally {
  for (const child of [...children]) await stop(child);
  if (codexRegistered) await run(codex, ['plugin', 'marketplace', 'remove', marketplace, '--json']).catch(() => {});
  if (claudeRegistered) await run(claude, ['plugin', 'marketplace', 'remove', marketplace]).catch(() => {});
  if (unrelatedCodex) await run(codex, ['plugin', 'marketplace', 'remove', unrelatedMarket, '--json']).catch(() => {});
  if (unrelatedClaude) await run(claude, ['plugin', 'marketplace', 'remove', unrelatedMarket]).catch(() => {});
  if (http) { http.closeAllConnections(); await new Promise(resolve => http.close(resolve)); }
  if (!options.has('--keep')) fs.rmSync(root, { recursive: true, force: true });
}
