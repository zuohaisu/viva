#!/usr/bin/env python3
"""#206 QA regressions against REAL installed native binaries and `viva update`.

Only the HTTPS release-metadata transport is replaced by an isolated CONNECT
fixture. Product release URLs/curl and application authentication are unchanged.
No real release, user runtime, credentials, or package version is modified.
"""
import argparse
import json
import os
from pathlib import Path
import shutil
import signal
import socket
import socketserver
import ssl
import subprocess
import tempfile
import threading
import time

from native_upgrade import Fixture, digest
from native_runtime import rpc


class MetadataProxy(socketserver.StreamRequestHandler):
    def handle(self):
        self.connection.settimeout(10)
        request = self.rfile.readline(8192)
        while self.rfile.readline(8192) not in (b'\r\n', b'\n', b''):
            pass
        if request != b'CONNECT api.github.com:443 HTTP/1.1\r\n':
            self.connection.sendall(b'HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\n\r\n')
            return
        self.connection.sendall(b'HTTP/1.1 200 Connection established\r\n\r\n')
        with self.server.tls.wrap_socket(self.connection, server_side=True) as stream:
            source = stream.makefile('rb')
            request = source.readline(8192)
            while source.readline(8192) not in (b'\r\n', b'\n', b''):
                pass
            if request != b'GET /repos/zuohaisu/viva/releases/latest HTTP/1.1\r\n':
                raise AssertionError('unexpected release request')
            body = json.dumps({'tag_name': 'v' + self.server.version, 'draft': False,
                               'prerelease': False, 'assets': []}).encode()
            self.server.requests += 1
            stream.sendall(b'HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: ' + str(len(body)).encode() + b'\r\n\r\n' + body)


def command(fixture, args):
    child = subprocess.Popen([str(fixture.binary), *args], env=fixture.env,
                             stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    deadline = time.monotonic() + 150
    while child.poll() is None:
        if time.monotonic() > deadline:
            child.kill()
            child.wait()
            raise AssertionError('CLI deadline exceeded')
        if fixture.master is not None:
            fixture.drain(.05)
        else:
            time.sleep(.05)
    stdout, stderr = child.communicate(timeout=5)
    return {'exit': child.returncode, 'stdout': stdout.decode(errors='replace'),
            'stderr': stderr.decode(errors='replace')}


def setup(root, binary, environment):
    root.mkdir()
    fixture = Fixture(root)
    fixture.binary.parent.mkdir(parents=True)
    shutil.copy2(binary, fixture.binary)
    fixture.env.update(environment)
    return fixture


def startup_identity(root, candidate, replacement):
    """Stop in a dyld constructor, before Rust main/server_started, then rename.

    A fixed binary must reject an image/path mismatch rather than publish the
    replacement's SHA as the identity of the old loaded executable.
    """
    root.mkdir()
    source = root / 'stop.c'
    source.write_text('#include <signal.h>\n#include <stddef.h>\n#include <sys/proc_info.h>\n'
                      '_Static_assert(PROC_PIDREGIONPATHINFO == 8, "libproc flavor");\n'
                      '_Static_assert(sizeof(struct proc_regioninfo) == 96, "region prefix ABI");\n'
                      '_Static_assert(offsetof(struct proc_regionwithpathinfo, prp_vip) == 96, "vnode ABI offset");\n'
                      '__attribute__((constructor)) static void stop_before_main(void) { raise(SIGSTOP); }\n')
    dylib = root / 'stop.dylib'
    subprocess.run(['clang', '-dynamiclib', str(source), '-o', str(dylib)], check=True,
                   stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    binary = root / 'viva'
    shutil.copy2(candidate, binary)
    home = root / 'state'
    home.mkdir(mode=0o700)
    env = {'PATH': '/usr/bin:/bin', 'HOME': str(root), 'VIVA_HOME': str(home),
           'SHELL': '/bin/sh', 'TERM': 'xterm-256color', 'DYLD_INSERT_LIBRARIES': str(dylib)}
    log = root / 'startup.log'
    with log.open('wb') as stderr:
        host = subprocess.Popen([str(binary), 'runtime', 'server'], env=env,
                                stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=stderr)
    try:
        deadline = time.monotonic() + 15
        while True:
            assert host.poll() is None, 'dyld startup fixture exited before stop'
            state = subprocess.check_output(['ps', '-p', str(host.pid), '-o', 'stat='], text=True).strip()
            if 'T' in state:
                break
            assert time.monotonic() < deadline, 'dyld constructor did not stop before main'
            time.sleep(.02)
        new_file = root / 'replacement'
        shutil.copy2(replacement, new_file)
        os.replace(new_file, binary)
        os.kill(host.pid, signal.SIGCONT)
        deadline = time.monotonic() + 20
        observed = None
        while host.poll() is None and time.monotonic() < deadline:
            try:
                observed = rpc(home, 'viva.snapshot')['result']['snapshot']['runtime_binary_sha256']
                break
            except (OSError, KeyError, ValueError):
                time.sleep(.05)
        return {'passed': host.poll() is not None and host.returncode != 0
                         and 'does not match running image' in log.read_text(errors='replace'),
                'loaded_binary_sha256': digest(candidate), 'replacement_sha256': digest(replacement),
                'reported_runtime_sha256': observed, 'exit': host.poll(),
                'diagnostic': log.read_text(errors='replace')[-1500:]}
    finally:
        if host.poll() is None:
            os.kill(host.pid, signal.SIGCONT)
            host.kill()
        host.wait(timeout=5)


def run(args):
    packet = {'schema': 1, 'passed': False, 'candidate_sha256': digest(args.candidate),
              'previous_sha256': digest(args.previous), 'cases': {},
              'transport': 'private CONNECT metadata fixture; unchanged official release URL and real system curl',
              'application_credential_injection': False}
    with tempfile.TemporaryDirectory(prefix='vq', dir='/private/tmp') as temp:
        root = Path(temp)
        cert, key = root / 'cert.pem', root / 'key.pem'
        subprocess.run(['openssl', 'req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-keyout', str(key),
                        '-out', str(cert), '-days', '1', '-subj', '/CN=api.github.com',
                        '-addext', 'subjectAltName=DNS:api.github.com'], check=True,
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        server = socketserver.ThreadingTCPServer(('127.0.0.1', 0), MetadataProxy)
        server.daemon_threads = True
        server.tls = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        server.tls.load_cert_chain(cert, key)
        server.version = subprocess.check_output([str(args.candidate), '--version'], text=True).strip().split()[-1]
        server.requests = 0
        threading.Thread(target=server.serve_forever, daemon=True).start()
        proxy = 'http://127.0.0.1:' + str(server.server_address[1])
        env = {'HTTPS_PROXY': proxy, 'https_proxy': proxy, 'NO_PROXY': '', 'no_proxy': '',
               'ALL_PROXY': '', 'CURL_CA_BUNDLE': str(cert)}
        try:
            for kind in ('absent', 'stale_socket', 'regular_file'):
                fixture = setup(root / kind, args.candidate, env)
                endpoint = fixture.home / 'runtime/workbench/herdr.sock'
                try:
                    if kind == 'stale_socket':
                        with socket.socket(socket.AF_UNIX) as stale:
                            stale.bind(str(endpoint))
                    elif kind == 'regular_file':
                        endpoint.write_bytes(b'fixture-not-a-listener')
                    check = command(fixture, ['runtime', 'upgrade', '--check', '--entry', str(fixture.binary)])
                    activate = command(fixture, ['runtime', 'upgrade', '--entry', str(fixture.binary)])
                    update = command(fixture, ['update'])
                    passed = all(result['exit'] == 0 for result in (check, activate, update))
                    passed = passed and all('no_runtime' in result['stdout'] for result in (check, activate))
                    passed = passed and not (fixture.home / 'runtime-capability/owner-anchor').exists()
                    packet['cases'][kind] = {'passed': passed, 'check': check, 'activate': activate, 'update': update}
                finally:
                    fixture.cleanup()
            fixture = setup(root / 'retry', args.previous, env)
            try:
                old = fixture.start()
                fixture.output(b"VQ_STATE=kept; printf '\\122\\105\\101\\104\\131\\n'", b'READY')
                replacement = fixture.binary.with_name('next-viva')
                shutil.copy2(args.candidate, replacement)
                os.replace(replacement, fixture.binary)
                deferred = command(fixture, ['update', '--no-restart'])
                assert fixture.observe()['runtime_host_id'] == old['runtime_host_id'], 'opt-out unexpectedly switched host'
                update = command(fixture, ['update'])
                new = fixture.observe()
                passed = update['exit'] == 0 and new['runtime_host_id'] != old['runtime_host_id']
                passed = passed and new.get('runtime_binary_sha256') == digest(args.candidate)
                if passed:
                    # Negotiation is not a license to inject input while the
                    # old frontend is still applying its replacement surface.
                    # Use the same initial-render drain as Fixture.start; no
                    # command resend and no extension of the output deadline.
                    fixture.drain(2)
                    fixture.output(b"printf '\\101\\106\\124\\105\\122_%s\\n' \"$VQ_STATE\"", b'AFTER_kept')
                repeat = command(fixture, ['update'])
                passed = passed and repeat['exit'] == 0 and fixture.observe()['runtime_host_id'] == new['runtime_host_id']
                packet['cases']['files_current_runtime_old'] = {'passed': passed, 'deferred': deferred,
                    'update': update, 'repeat': repeat, 'host_changed': new['runtime_host_id'] != old['runtime_host_id'],
                    'target_runtime_hash': new.get('runtime_binary_sha256')}
            finally:
                fixture.cleanup()
            fixture = setup(root / 'failed-retry', args.previous, env)
            fixture.env['HERDR_TEST_HANDOFF_IMPORT_FAIL'] = 'after_domain_ready'
            try:
                old = fixture.start()
                fixture.output(b"VQ_STATE=kept; printf '\\122\\105\\101\\104\\131\\n'", b'READY')
                replacement = fixture.binary.with_name('next-viva')
                shutil.copy2(args.candidate, replacement)
                os.replace(replacement, fixture.binary)
                first = command(fixture, ['runtime', 'upgrade', '--entry', str(fixture.binary)])
                retry = command(fixture, ['update'])
                same_host = fixture.observe()['runtime_host_id'] == old['runtime_host_id']
                passed = first['exit'] != 0 and retry['exit'] != 0 and same_host
                passed = passed and 'activation' in retry['stderr'] and digest(fixture.binary) == digest(args.candidate)
                if passed:
                    fixture.drain(2)
                    fixture.output(b"printf '\\122\\117\\114\\114\\102\\101\\103\\113_%s\\n' \"$VQ_STATE\"", b'ROLLBACK_kept')
                packet['cases']['failed_activation_noop_retry'] = {'passed': passed, 'first': first,
                    'retry': retry, 'old_host_serving': same_host}
            finally:
                fixture.cleanup()
            fixture = setup(root / 'headless', args.previous, env)
            log = (root / 'headless.log').open('wb')
            host = subprocess.Popen([str(fixture.binary), 'runtime', 'server'], env=fixture.env,
                                    stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=log)
            try:
                deadline = time.monotonic() + 20
                while True:
                    try:
                        old = fixture.observe()
                        if (fixture.home / 'runtime-capability/owner-anchor').exists(): break
                    except (OSError, KeyError, ValueError): pass
                    assert host.poll() is None and time.monotonic() < deadline, 'headless source did not start'
                    time.sleep(.05)
                assert old['raw_clients'] == [], 'unexpected frontend in headless fixture'
                replacement = fixture.binary.with_name('next-viva')
                shutil.copy2(args.candidate, replacement)
                os.replace(replacement, fixture.binary)
                update = command(fixture, ['update'])
                new = fixture.observe()
                recovery = next((json.loads(line.split(': ', 1)[1]) for line in update['stdout'].splitlines()
                                 if line.startswith('Native activation result: ')), {})
                packet['cases']['no_previous_frontends'] = {
                    'passed': update['exit'] == 0 and new['runtime_host_id'] != old['runtime_host_id']
                        and recovery.get('client_recovery') == 'no_previous_clients'
                        and recovery.get('previous_frontend_count') == 0
                        and recovery.get('fresh_controller_verified') is True,
                    'reported_recovery': recovery, 'update': update}
            finally:
                fixture.cleanup()
                host.wait(timeout=5)
                log.close()
            packet['cases']['startup_identity'] = startup_identity(root / 'startup', args.candidate, args.previous)
            packet['metadata_requests'] = server.requests
            packet['passed'] = all(case['passed'] for case in packet['cases'].values())
        finally:
            server.shutdown()
            server.server_close()
    return packet


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--previous', type=Path, required=True)
    parser.add_argument('--candidate', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    result = run(args)
    args.output.write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps(result, indent=2))
    raise SystemExit(0 if result['passed'] else 1)
