"""Recorded experiment, not a general runtime API. Requires pyte 0.8.2.
Only creates/operates its own new named session under a unique temporary HOME.
"""
import argparse
import codecs
import fcntl
import json
import os
from pathlib import Path
import pty
import struct
import subprocess
import tempfile
import termios
import time

import pyte

args = argparse.ArgumentParser()
args.add_argument('--binary', required=True)
args.add_argument('--output', required=True)
opts = args.parse_args()
binary = str(Path(opts.binary).resolve())
output = Path(opts.output)
output.mkdir(parents=True, exist_ok=False)
root = Path(tempfile.mkdtemp(prefix='h190ui-', dir='/tmp'))
for name in ('home', 'config/herdr-dev', 'state', 'data', 'work', 'tmp'):
    (root / name).mkdir(parents=True, exist_ok=True)
(root / 'config/herdr-dev/config.toml').write_text(
    'onboarding = false\n[terminal]\ndefault_shell = "/bin/sh"\n'
    'shell_mode = "non_login"\nnew_cwd = "' + str(root / 'work') + '"\n'
    '[update]\nversion_check = false\nmanifest_check = false\n'
    '[experimental]\nallow_nested = true\n')
env = {'HOME': str(root/'home'), 'XDG_CONFIG_HOME': str(root/'config'),
       'XDG_STATE_HOME': str(root/'state'), 'XDG_DATA_HOME': str(root/'data'),
       'TMPDIR': str(root/'tmp'), 'PATH': '/usr/bin:/bin:/usr/sbin:/sbin',
       'SHELL': '/bin/sh', 'TERM': 'xterm-256color', 'COLORTERM': 'truecolor',
       'LANG': 'en_US.UTF-8', 'HERDR_DISABLE_SOUND': '1'}
if os.environ.get('HERDR_ENV'):
    env['HERDR_ENV'] = os.environ['HERDR_ENV']
(output/'environment.json').write_text(json.dumps(env, indent=2)+'\n')
clients, events = [], []

class HostScreen(pyte.Screen):
    def report_device_status(self, mode, private=False):
        # Pyte 0.8.2's parser passes private=True but Screen lacks that keyword.
        # This host does not advertise private-status support. Not a Herdr patch.
        if not private:
            super().report_device_status(mode)

def pump(seconds):
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        for index, (process, fd, screen, stream, decoder) in enumerate(clients):
            try:
                data = os.read(fd, 65536)
            except (BlockingIOError, OSError):
                continue
            if not data:
                continue
            with (output/f'client{index}.ansi').open('ab') as raw:
                raw.write(data)
            stream.feed(decoder.decode(data))
            if b'\x1b[6n' in data:
                os.write(fd, f'\x1b[{screen.cursor.y+1};{screen.cursor.x+1}R'.encode())
            if b'\x1b[c' in data or b'\x1b[0c' in data:
                os.write(fd, b'\x1b[?1;2c')
            if b'\x1b[>c' in data:
                os.write(fd, b'\x1b[>0;276;0c')
            for code, color in ((10, b'cccc/cccc/cccc'), (11, b'1818/1818/2828')):
                if f'\x1b]{code};?'.encode() in data:
                    os.write(fd, f'\x1b]{code};rgb:'.encode()+color+b'\x1b\\')
        time.sleep(.02)

def launch():
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 36, 120, 0, 0))
    def child_setup():
        os.setsid()
        fcntl.ioctl(0, termios.TIOCSCTTY, 0)
    process = subprocess.Popen([binary, '--session', 'qa190'], stdin=slave,
        stdout=slave, stderr=slave, env=env, cwd=root/'work', preexec_fn=child_setup)
    os.close(slave)
    os.set_blocking(master, False)
    screen = HostScreen(120, 36)
    clients.append((process, master, screen, pyte.Stream(screen),
                    codecs.getincrementaldecoder('utf-8')('replace')))
    pump(3)
    return len(clients)-1

def cli(label, *command):
    result = subprocess.run([binary, '--session', 'qa190', *command], env=env,
        cwd=root/'work', capture_output=True, text=True, timeout=10)
    (output/(label+'.json')).write_text(json.dumps({'args': command,
        'returncode': result.returncode, 'stdout': result.stdout,
        'stderr': result.stderr}, indent=2)+'\n')
    if result.returncode:
        raise RuntimeError(label+': '+result.stderr)
    try:
        return json.loads(result.stdout)
    except json.JSONDecodeError:
        return result.stdout

def send(index, data, label):
    os.write(clients[index][1], data)
    events.append({'time': time.time(), 'client': index, 'label': label, 'hex': data.hex()})
    pump(.7)

def capture(label):
    for i, (_, _, screen, _, _) in enumerate(clients):
        # Text projection is supporting evidence, NOT a physical screenshot.
        (output/f'{label}-client{i}.txt').write_text('\n'.join(screen.display)+'\n')
    (output/'events.json').write_text(json.dumps(events, indent=2)+'\n')

checks = {}
try:
    first = launch()
    panes = cli('startup', 'pane', 'list')['result']['panes']
    assert len(panes) == 1
    left = panes[0]['pane_id']
    capture('startup')
    send(first, b"printf '__H190_INPUT__\\n'\r", 'shell input via actual client PTY')
    assert '__H190_INPUT__' in cli('input', 'pane', 'read', left).splitlines()
    checks['startup_and_terminal_input'] = True
    send(first, "printf '__UNICODE_中文_🙂__\\n'\r".encode(), 'UTF-8 terminal input')
    assert '__UNICODE_中文_🙂__' in cli('unicode-input', 'pane', 'read', left).splitlines()
    checks['unicode_terminal_input'] = True
    send(first, b'\x02v', 'vertical split via prefix+v')
    panes = cli('vertical-split', 'pane', 'list')['result']['panes']
    assert len(panes) == 2
    right = next(p['pane_id'] for p in panes if p['pane_id'] != left)
    send(first, b'i=1; while [ $i -le 100 ]; do printf "__ROW_%03d__\\n" "$i"; i=$((i+1)); done\r', 'scrollback output')
    assert '__ROW_100__' in cli('before-scroll', 'pane', 'read', right, '--source', 'visible').splitlines()
    send(first, b'\x1b[<0;40;5M\x1b[<0;40;5m', 'SGR click left pane')
    send(first, b"printf '__MOUSE_LEFT__\\n'\r", 'keyboard after mouse focus')
    assert '__MOUSE_LEFT__' in cli('mouse-input', 'pane', 'read', left).splitlines()
    checks['mouse_focus_routes_keyboard'] = True
    send(first, b'\x1b[<64;90;15M'*8, 'SGR wheel up right pane')
    assert '__ROW_100__' not in cli('after-scroll', 'pane', 'read', right, '--source', 'visible').splitlines()
    scroll = cli('scroll-state', 'pane', 'list')['result']['panes']
    assert next(p for p in scroll if p['pane_id'] == right)['scroll']['offset_from_bottom'] > 0
    checks['mouse_scrollback'] = True
    capture('split-and-scroll')
    old = cli('layout-before', 'pane', 'layout', '--pane', left)['result']['layout']['area']
    fcntl.ioctl(clients[first][1], termios.TIOCSWINSZ, struct.pack('HHHH', 42, 140, 0, 0))
    clients[first][2].resize(42, 140)
    pump(1)
    new = cli('layout-resized', 'pane', 'layout', '--pane', left)['result']['layout']['area']
    assert new['height'] > old['height'] and new['width'] > old['width']
    checks['resize_reaches_layout'] = True
    capture('resized')
    before = cli('process-before', 'pane', 'process-info', '--pane', right)['result']['process_info']
    send(first, b'\x02q', 'detach first client')
    assert clients[first][0].wait(timeout=5) == 0
    detached = cli('detached', 'pane', 'list')['result']['panes']
    assert len(detached) == 2
    second = launch()
    after = cli('process-after', 'pane', 'process-info', '--pane', right)['result']['process_info']
    assert before['shell_pid'] == after['shell_pid']
    reattached = cli('reattached', 'pane', 'list')['result']['panes']
    assert {p['terminal_id'] for p in detached} == {p['terminal_id'] for p in reattached}
    send(second, b"printf '__REATTACHED__\\n'\r", 'input after attach')
    assert '__REATTACHED__' in cli('attach-input', 'pane', 'read', right).splitlines()
    checks['detach_attach_same_shell_and_terminals'] = True
    capture('reattached')
    send(second, b'\x02-', 'horizontal split via prefix+minus')
    assert len(cli('horizontal-split', 'pane', 'list')['result']['panes']) == 3
    checks['vertical_and_horizontal_split'] = True
    capture('three-panes')
    send(second, b'\x02q', 'detach second client')
    assert clients[second][0].wait(timeout=5) == 0
finally:
    # Only this invocation's unique sandbox/session; no default/existing server.
    cli('owned-server-stop', 'server', 'stop')
    # A successful stop request alone is not a shutdown observation. Query only
    # this invocation's named session until its status reports not running.
    deadline = time.monotonic() + 5
    while True:
        status = cli('server-status-after-stop', 'status', 'server')
        if isinstance(status, str) and 'status: not running' in status.splitlines():
            break
        if time.monotonic() >= deadline:
            raise RuntimeError('Owned server did not report not running after stop')
        pump(.1)
    capture('final')
    (output/'result.json').write_text(json.dumps({'sandbox': str(root), 'binary': binary,
        'checks': checks, 'human_visual_review': 'PENDING',
        'server_status_evidence': 'server-status-after-stop.json',
        'client_exits': [p.poll() for p, *_ in clients]}, indent=2)+'\n')
    for process, fd, *_ in clients:
        os.close(fd)
