#!/usr/bin/env python3
"""#201 real HTTPS/curl installer + live native PTY upgrade fixture.

Only task-created HOME/VIVA_HOME/installations. No release/deployment, credentials,
Keychain, real user data or human visual acceptance. Pass two different compatible
native builds; --baseline optionally reproduces the pre-fix files-only symptom.
"""
import argparse
import fcntl
import functools
import hashlib
import http.server
import json
import os
from pathlib import Path
import platform
import pty
import select
import shlex
import shutil
import signal
import socket
import ssl
import subprocess
import struct
import tarfile
import tempfile
import termios
import threading
import time

from native_runtime import rpc, process_identity

REPO = Path(__file__).resolve().parents[2]


def digest(path):
    h = hashlib.sha256()
    with path.open('rb') as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b''):
            h.update(chunk)
    return h.hexdigest()


class Fixture:
    def __init__(self, root):
        self.root = root
        self.home = root / 'state'
        self.home.mkdir(mode=0o700)
        runtime = self.home / 'runtime/workbench'
        runtime.mkdir(parents=True)
        (runtime / 'config.toml').write_text("[terminal]\ndefault_shell = '/bin/sh'\nshell_mode = 'non_login'\n[ui.sound]\nenabled = false\n")
        user = root / 'user'
        user.mkdir(mode=0o700)
        self.env = {'PATH': os.environ['PATH'], 'HOME': str(user),
                    'VIVA_HOME': str(self.home), 'VIVA_PREFIX': str(root / 'prefix'),
                    'SHELL': '/bin/sh', 'TERM': 'xterm-256color', 'LANG': 'en_US.UTF-8'}
        self.binary = root / 'prefix/viva/viva'
        self.client = None
        self.hosts = {}
        self.master = None
        self.frame = bytearray()

    def observe(self):
        value = rpc(self.home, 'viva.snapshot')['result']['snapshot']
        pid = value['runtime_pid']
        self.hosts[pid] = process_identity(pid)
        return value

    def start(self):
        self.master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 32, 120, 0, 0))
        self.client = subprocess.Popen([str(self.binary)], env=self.env, cwd=self.root, stdin=slave,
                                       stdout=slave, stderr=slave, start_new_session=True)
        os.close(slave)
        end = time.monotonic() + 20
        while time.monotonic() < end:
            self.drain(.1)
            try:
                value = self.observe()
                if (self.home / 'runtime-capability/owner-anchor').exists():
                    self.drain(2)
                    return value
            except (OSError, KeyError, ValueError):
                pass
            assert self.client.poll() is None, self.frame[-1500:].decode(errors='replace')
        raise AssertionError('fixture host never ready')

    def drain(self, duration):
        end = time.monotonic() + duration
        while time.monotonic() < end:
            if select.select([self.master], [], [], .05)[0]:
                try:
                    data = os.read(self.master, 65536)
                    if not data:
                        return
                    self.frame.extend(data)
                except OSError:
                    return

    def output(self, command, marker):
        self.frame.clear()
        os.write(self.master, command + b'\r')
        end = time.monotonic() + 12
        while time.monotonic() < end:
            self.drain(.1)
            if marker in self.frame:
                return
        raise AssertionError('real PTY output absent: ' + repr(marker) + '; frame=' + self.frame[-2500:].decode(errors='replace'))

    def detach(self):
        os.write(self.master, b'\x02')
        time.sleep(.08)
        os.write(self.master, b'q')
        end = time.monotonic() + 10
        while self.client.poll() is None and time.monotonic() < end:
            self.drain(.1)
        assert self.client.poll() == 0, 'frontend did not detach cleanly'
        os.close(self.master)
        self.master = None

    def cleanup(self):
        # Activation can fail after commit; discover the actual task-owned
        # successor before removing any fixture files, never guess a PID.
        try:
            self.observe()
        except (OSError, KeyError, ValueError):
            pass
        if self.client and self.client.poll() is None:
            self.client.kill()
            self.client.wait(timeout=5)
        if self.master is not None:
            os.close(self.master)
            self.master = None
        for pid, identity in self.hosts.items():
            if identity and str(self.binary) + ' runtime server' in identity and process_identity(pid) == identity:
                os.kill(pid, signal.SIGTERM)
                end = time.monotonic() + 5
                while process_identity(pid) == identity and time.monotonic() < end:
                    time.sleep(.05)
                if process_identity(pid) == identity:
                    os.kill(pid, signal.SIGKILL)


def package(web, binary, arch, tag):
    folder = web / ('payload-' + tag)
    top = folder / ('viva-' + arch)
    (top / 'extensions').mkdir(parents=True)
    shutil.copy2(binary, top / 'viva')
    shutil.copytree(REPO / 'extensions/pi', top / 'extensions/pi', ignore=shutil.ignore_patterns('node_modules'))
    for name in ('LICENSE', 'README.md'):
        shutil.copy2(REPO / name, top / name)
    (top / 'licenses').mkdir()
    shutil.copy2(REPO / 'upstream/herdr/LICENSE', top / 'licenses/herdr-LICENSE')
    (top / 'NOTICE').write_text('Isolated #201 validation fixture; modified Viva/Herdr source from the task checkout, not a release.\n')
    archive = web / (tag + '.tar.gz')
    with tarfile.open(archive, 'w:gz') as output:
        output.add(top, arcname=top.name)
    return {'file': archive.name, 'sha256': digest(archive)}


class QuietHandler(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *_):
        pass


def run(args):
    assert platform.system() == 'Darwin', 'macOS process/installer fixture only'
    arch = {'arm64': 'macos-arm64', 'x86_64': 'macos-intel'}[platform.machine()]
    result = {'schema': 1, 'architecture': arch, 'passed': False,
              'human_visual': 'HUMAN_VISUAL_REVIEW_PENDING',
              'active_canonical_member_transfer': 'not_supported',
              'intel' if arch == 'macos-arm64' else 'arm64': 'not_run'}
    with tempfile.TemporaryDirectory(prefix='vu', dir='/private/tmp') as temporary:
        root = Path(temporary).resolve()
        web = root / 'web'
        web.mkdir()
        cert = root / 'cert.pem'
        key = root / 'key.pem'
        subprocess.run(['openssl', 'req', '-x509', '-newkey', 'rsa:2048', '-nodes',
                        '-keyout', str(key), '-out', str(cert), '-days', '1',
                        '-subj', '/CN=localhost', '-addext', 'subjectAltName=DNS:localhost'],
                       check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), functools.partial(QuietHandler, directory=str(web)))
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        context.load_cert_chain(cert, key)
        server.socket = context.wrap_socket(server.socket, server_side=True)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        base = 'https://localhost:' + str(server.server_port)
        assets = {tag: package(web, binary, arch, tag) for tag, binary in
                  [('a', args.previous), ('b', args.candidate)] + ([('baseline', args.baseline)] if args.baseline else [])}
        shutil.copy2(REPO / 'packaging/install.sh', web / 'install.sh')
        fixture = Fixture(root)
        fixture.env.update({'VIVA_INSTALL_BASE': base, 'CURL_CA_BUNDLE': str(cert)})

        def install(tag, expect=True):
            (web / 'channel.json').write_text(json.dumps({'channel': 'isolated-fixture', 'assets': {arch: assets[tag]}}))
            command = subprocess.Popen(['/bin/sh', '-c', 'curl --noproxy "*" -fsSL "$VIVA_INSTALL_BASE/install.sh" | sh'],
                                       env=fixture.env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            out, err = bytearray(), bytearray()
            # Drain the real TUI during install: a blocked renderer is not a
            # valid client-recovery test. subprocess pipes are bounded here.
            end = time.monotonic() + 180
            while command.poll() is None and time.monotonic() < end:
                if fixture.master is not None:
                    fixture.drain(.05)
                else:
                    time.sleep(.05)
            if command.poll() is None:
                command.kill()
                raise AssertionError('installer exceeded deadline')
            stdout, stderr = command.communicate(timeout=5)
            out.extend(stdout); err.extend(stderr)
            if expect:
                assert command.returncode == 0, err.decode(errors='replace')[-4096:]
            else:
                assert command.returncode != 0, 'installer falsely reported success'
            return out.decode(errors='replace'), err.decode(errors='replace')

        try:
            # The old commit is run as a real process. Replacing its disk file
            # and launching a new frontend did not trigger native-to-native
            # activation on that recorded baseline.
            if args.baseline:
                fixture.binary.parent.mkdir(parents=True)
                shutil.copy2(args.baseline, fixture.binary)
                bindir = root / 'prefix/bin'
                bindir.mkdir()
                wrapper = bindir / 'viva'
                wrapper.write_text('#!/bin/sh\n# viva-tailnet-dev-wrapper\nexec ' + shlex.quote(str(fixture.binary)) + ' "$@"\n')
                wrapper.chmod(0o755)
                before = fixture.start()
                replacement = fixture.binary.with_name('new-viva')
                shutil.copy2(args.candidate, replacement)
                os.replace(replacement, fixture.binary)
                after = fixture.observe()
                assert before['runtime_host_id'] == after['runtime_host_id']
                result['baseline_files_replaced_old_runtime_unchanged'] = True
                _, error = install('b', expect=False)
                assert 'does not support cooperative upgrade' in error, error
                assert fixture.observe()['runtime_host_id'] == before['runtime_host_id']
                result['unsupported_generation_refused_old_host_serving'] = True
                fixture.cleanup()
                fixture.client = None
                fixture.hosts.clear()
                # Keep all historical state/backup bytes: a separate private
                # state root is used for the compatible-generation scenario.
                fixture.home = root / 'compatible-state'
                fixture.home.mkdir(mode=0o700)
                runtime = fixture.home / 'runtime/workbench'
                runtime.mkdir(parents=True)
                (runtime / 'config.toml').write_text("[terminal]\ndefault_shell = '/bin/sh'\nshell_mode = 'non_login'\n")
                fixture.env['VIVA_HOME'] = str(fixture.home)
                # Baseline setup did not create a wrapper; retain its files.
                os.rename(fixture.binary.parent, root / 'baseline-files')
            out, _ = install('a')
            assert 'no_runtime' in out and not (fixture.home / 'runtime/workbench/herdr.sock').exists()
            install('a')
            result['fresh_and_repeat_install_no_daemon'] = True
            old = fixture.start()
            fixture.output(b"VU_STATE=kept; sleep 120 & VU_JOB=$(jobs -p); printf '\\122\\105\\101\\104\\131\\n'", b'READY')
            result['previous_sha256'] = digest(fixture.binary)
            assert digest(args.previous) != digest(args.candidate), 'two different actual builds are required'
            out, _ = install('b')
            new = fixture.observe()
            assert old['runtime_host_id'] != new['runtime_host_id'], 'host identity did not change'
            assert new['runtime_binary_sha256'] == digest(args.candidate) == digest(fixture.binary), 'actual runtime build differs from installed target'
            try:
                state = subprocess.check_output(['ps', '-p', str(old['runtime_pid']), '-o', 'stat='], text=True).strip()
            except subprocess.CalledProcessError:
                state = ''
            assert not state or state.startswith('Z'), 'old host is still live: ' + state
            result['old_host_exit'] = 'exited_pending_parent_reap' if state else 'exited_and_reaped'
            assert 'upgrade_complete' in out, 'installer did not report verified activation'
            result['host_changed_target_build_ready'] = True
            fixture.output(b"kill -0 \"$VU_JOB\" && printf '\\101\\106\\124\\105\\122_%s\\n' \"$VU_STATE\"", b'AFTER_kept')
            result['existing_frontend_recovered_live_shell_and_background_job'] = True
            fixture.detach()
            same = fixture.start()
            assert same['runtime_host_id'] == new['runtime_host_id']
            fixture.output(b"kill -0 \"$VU_JOB\" && printf '\\122\\105\\101\\124\\124\\101\\103\\110_%s\\n' \"$VU_STATE\"", b'REATTACH_kept')
            fixture.detach()
            result['fresh_process_reattach_retained_work'] = True
            result['target_sha256'] = digest(args.candidate)
            stable_hash = digest(fixture.binary)
            wrong_platform = root / 'wrong-platform-viva'
            shutil.copy2(args.candidate, wrong_platform)
            with wrong_platform.open('r+b') as binary:
                binary.seek(4)
                binary.write(struct.pack('<I', 0x01000007 if arch == 'macos-arm64' else 0x0100000c))
            assets['wrong-platform'] = package(web, wrong_platform, arch, 'wrong-platform')
            _, error = install('wrong-platform', expect=False)
            assert 'not a native 64-bit Mach-O' in error
            result['wrong_platform_refused_before_execution'] = True
            assets['bad-checksum'] = dict(assets['b'], sha256='0' * 64)
            _, error = install('bad-checksum', expect=False)
            assert 'checksum mismatch' in error
            assets['missing'] = dict(assets['b'], file='missing.tar.gz')
            install('missing', expect=False)
            assert digest(fixture.binary) == stable_hash
            assert fixture.observe()['runtime_host_id'] == new['runtime_host_id']
            result['download_checksum_failures_preserve_files_and_runtime'] = True
            with (fixture.home / 'native-install.lock').open('r+') as guard:
                fcntl.flock(guard, fcntl.LOCK_EX | fcntl.LOCK_NB)
                _, error = install('b', expect=False)
                assert 'another installer/updater owns' in error, error
            result['concurrent_installer_refused'] = True
            # A wrong VIVA_HOME must not upgrade another installation's host.
            neighbor_root = root / 'neighbor'
            neighbor_root.mkdir()
            neighbor = Fixture(neighbor_root)
            neighbor.binary.parent.mkdir(parents=True)
            shutil.copy2(args.previous, neighbor.binary)
            try:
                neighbor_before = neighbor.start()
                fixture.env['VIVA_HOME'] = str(neighbor.home)
                _, error = install('b', expect=False)
                assert 'another installation' in error, error
                assert neighbor.observe()['runtime_host_id'] == neighbor_before['runtime_host_id']
                assert digest(fixture.binary) == stable_hash
                result['home_and_installation_isolation'] = True
            finally:
                fixture.env['VIVA_HOME'] = str(fixture.home)
                neighbor.cleanup()
            # A receiver that bound endpoints and rotated its capability then
            # failed must leave the old runtime AND restored old files usable.
            primary = fixture
            rollback_root = root / 'rollback'
            rollback_root.mkdir()
            fixture = Fixture(rollback_root)
            fixture.env.update({'VIVA_INSTALL_BASE': base, 'CURL_CA_BUNDLE': str(cert),
                                'HERDR_TEST_HANDOFF_IMPORT_FAIL': 'after_domain_ready'})
            try:
                install('a')
                rollback_before = fixture.start()
                _, error = install('b', expect=False)
                assert 'handoff' in error
                rollback_after = fixture.observe()
                assert rollback_after['runtime_host_id'] == rollback_before['runtime_host_id']
                assert digest(fixture.binary) == digest(args.previous)
                status = subprocess.run([str(fixture.binary), 'runtime', 'upgrade', 'status'],
                                        env=fixture.env, capture_output=True, timeout=15)
                assert status.returncode == 0, status.stderr.decode(errors='replace')
                result['readiness_failure_restored_files_and_old_runtime_service'] = True
            finally:
                fixture.cleanup()
                fixture = primary
            result['passed'] = True
        except Exception as error:
            result['error'] = str(error)
        finally:
            fixture.cleanup()
            server.shutdown()
    return result


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--previous', type=Path, required=True)
    parser.add_argument('--candidate', type=Path, required=True)
    parser.add_argument('--baseline', type=Path)
    parser.add_argument('--output', type=Path, required=True)
    arguments = parser.parse_args()
    packet = run(arguments)
    arguments.output.write_text(json.dumps(packet, indent=2) + '\n')
    print(json.dumps(packet, indent=2))
    raise SystemExit(0 if packet['passed'] else 1)
