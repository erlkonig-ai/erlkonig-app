"""Synthetic X11/software-rendering smoke, NOT KDE/Wayland acceptance."""
import argparse, importlib.util, json, os, signal, socket, subprocess, sys, time, shutil
from pathlib import Path

parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('candidate',type=Path)
parser.add_argument('--fixture-binary',type=Path,required=True)
parser.add_argument('--root',type=Path,required=True)
args=parser.parse_args()
assert sys.platform.startswith('linux'), 'Linux native probe required'
assert args.root.is_absolute() and not args.root.exists(), 'new absolute fixture root required'
assert all(os.environ.get(k) for k in ('DISPLAY','DBUS_SESSION_BUS_ADDRESS')), 'owned Xvfb and D-Bus session required'
# Match the retained isolated Docker recipe: no real client CLIs in probe PATH.
assert not any(shutil.which(name,path='/usr/bin:/bin') for name in ('codex','claude')), 'real client CLI in fixture PATH'
os.umask(0o077)
args.root.mkdir(mode=0o700)
source=Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('protocol', source / 'scripts/accept/isolated_app.py')
protocol = importlib.util.module_from_spec(spec)
spec.loader.exec_module(protocol)
binary = args.candidate.resolve(strict=True)
fixture_binary=args.fixture_binary.resolve(strict=True)
assert os.access(binary,os.X_OK) and os.access(fixture_binary,os.X_OK)
children = []
checks = []

def listener(pid):
    result = subprocess.run(['lsof','-nP','-a','-p',str(pid),'-iTCP','-sTCP:LISTEN','-Fn'], capture_output=True, text=True, timeout=5)
    assert result.returncode in (0, 1)
    return [line[1:] for line in result.stdout.splitlines() if line.startswith('n')]

def stop(process, url):
    process.send_signal(signal.SIGTERM)
    assert process.wait(timeout=20) == 0
    port = int(url.split(':')[-1].rstrip('/'))
    with socket.socket() as sock:
        assert sock.connect_ex(('127.0.0.1',port)) != 0

def exercise(name, configured):
    root = args.root / name
    root.mkdir(mode=0o700)
    for part in ['home','codex','claude','tmp']:
        (root / part).mkdir(mode=0o700)
    env = {key:os.environ[key] for key in ['DISPLAY','XAUTHORITY','DBUS_SESSION_BUS_ADDRESS'] if key in os.environ}
    env.update(HOME=str(root/'home'), CODEX_HOME=str(root/'codex'), CLAUDE_CONFIG_DIR=str(root/'claude'), TMPDIR=str(root/'tmp'), PATH='/usr/bin:/bin', LANG='C.UTF-8', LIBGL_ALWAYS_SOFTWARE='1')
    # No real client executables are installed/mounted; no client auto-install.
    data = root/'home/.local/share/erlkonig'
    if configured:
        prepared = subprocess.run([str(fixture_binary),str(root)], env=env, cwd=env["HOME"], capture_output=True, text=True, timeout=60, check=True)
        fixture = json.loads(prepared.stdout)
        # Existing fixture helper emits a Mac-only app-data label. Identity and
        # settings are in canonical HOME custody; use actual Linux app-data here.
        fixture['app_data'] = str(data)
    else:
        fixture = {'app_data':str(data)}
    log = (root/'stderr.log').open('wb')
    def launch():
        process = subprocess.Popen([str(binary)],env=env,cwd=env["HOME"],stdin=subprocess.DEVNULL,stdout=log,stderr=log)
        children.append(process)
        deadline = time.monotonic()+90
        while time.monotonic()<deadline:
            assert process.poll() is None, 'owner exited before readiness'
            addresses = listener(process.pid)
            if addresses and (data/'mcp.token').exists():
                assert len(addresses)==1 and addresses[0].startswith('127.0.0.1:')
                client = protocol.HttpMcp('http://'+addresses[0]+'/',(data/'mcp.token').read_text().strip())
                client.initialize()
                return process,client
            time.sleep(.1)
        raise AssertionError('readiness timeout')
    first, client = launch()
    inspected = json.loads(protocol.text_result(client.call('config_inspect',{})))
    node = inspected['node_id']
    key = root/'home/.local/share/triblespace/custody/self.key'
    assert key.is_file() and key.stat().st_mode & 0o777 == 0o600
    if not configured:
        created = json.loads(protocol.text_result(client.call('config_create_workspace',{'observation':inspected['observation'],'label':'Fresh Linux smoke'})))
        assert 'workspace' in created
        protocol.text_result(client.call('goals_list',{'workspace':created['workspace']}))
    if configured:
        assert node == fixture['node_id']
        assert not inspected['settings']['sync_enabled'] and not inspected['settings']['maintenance_enabled']
        protocol.check_public_catalog(client,fixture['workspace'])
        listing = json.loads(protocol.text_result(client.call('list_workspaces',{})))
        assert any(row['workspace']==fixture['workspace'] for row in listing['workspaces'])
        selection = {'workspace':fixture['workspace']}
        protocol.text_result(client.call('goals_list',selection))
        forwarding = protocol.Forwarder(binary,fixture,client.url,env,log,children)
        forwarding.send(protocol.initialize())
        assert 'result' in forwarding.response(0)
        forwarding.send({'jsonrpc':'2.0','method':'notifications/initialized'})
        forwarding.send({'jsonrpc':'2.0','id':1,'method':'tools/call','params':{'name':'goals_list','arguments':selection}})
        protocol.text_result(forwarding.response(1))
        assert not listener(forwarding.process.pid)
    duplicate = subprocess.Popen([str(binary)],env=env,cwd=env["HOME"],stdin=subprocess.DEVNULL,stdout=log,stderr=log)
    children.append(duplicate)
    assert duplicate.wait(timeout=15)==0, 'Linux second launch should activate owner and exit'
    token = client.token
    stop(first,client.url)
    if configured:
        forwarding.process.wait(timeout=10)
    second,client = launch()
    assert json.loads(protocol.text_result(client.call('config_inspect',{})))['node_id']==node
    assert client.token==token
    stop(second,client.url)
    log.close()
    checks.append({'case':name,'checks':['private fresh identity/token','authenticated HTTP','second launch exits','SIGTERM closes listener','stable identity/token after relaunch'] + (['explicit workspace tool','native stdio forwarding without listener'] if configured else ['create workspace over HTTP','explicit workspace tool'])})

try:
    exercise('first-run',False)
    exercise('configured',True)
    (args.root/'result.json').write_text(json.dumps({'status':'passed','checks':checks,'limits':['Xvfb X11/software Mesa only','no KDE/Wayland/manual GUI validation','no real Codex/Claude plugin lifecycle','no native Secrets UI','no claim that wiki window/file opening was exercised']},indent=2)+'\n')
finally:
    for child in reversed(children):
        if child.poll() is None:
            child.terminate()
            try: child.wait(timeout=20)
            except subprocess.TimeoutExpired: child.kill(); child.wait()
