#!/bin/sh
# Viva development-channel installer. Requires macOS, Python 3 and curl.
# #201: derived from deployed install.sh SHA-256 d35de885721ee4e237d3a670b116aaee2397ab68cd974ba191d68c1de3b68dca.
# Runtime lifecycle belongs to Viva, never to a PID/socket guessing shell script.
set -eu
command -v python3 >/dev/null 2>&1 || { printf '%s\n' 'viva install: python3 is required' >&2; exit 1; }
exec python3 - <<'PY'
import fcntl, hashlib, json, os, platform, re, shlex, shutil, signal, stat, subprocess, sys, tarfile, tempfile, unicodedata
from pathlib import Path

MARKER = '# viva-tailnet-dev-wrapper'
MAX_ARCHIVE = 512 * 1024 * 1024
MAX_TOTAL = 1024 * 1024 * 1024
MAX_FILE = 512 * 1024 * 1024
MAX_ENTRIES = 10000

def fail(message):
    raise RuntimeError(message)

def no_symlink_components(path):
    path = Path(os.path.abspath(path))
    for p in reversed((path, *path.parents)):
        if p.is_symlink(): fail('symbolic link in path: ' + str(p))
    return path

def exists(p): return os.path.lexists(p)

def fetch(url, destination):
    subprocess.run(['curl', '--proto', '=https', '--tlsv1.2', '--noproxy', '*', '-fsSL', '--connect-timeout', '10', '--max-time', '120', '--max-filesize', str(MAX_ARCHIVE), url, '-o', str(destination)], check=True)
    if destination.stat().st_size > MAX_ARCHIVE: fail('download exceeds archive size limit')

def extract_checked(archive, output, top):
    seen, total, count = set(), 0, 0
    with tarfile.open(archive, 'r:gz') as tf:
        for m in tf:
            count += 1
            if count > MAX_ENTRIES: fail('tar has too many members')
            name = m.name
            if not name or name.startswith('/') or '\\' in name or '\x00' in name: fail('unsafe tar path')
            parts = name.rstrip('/').split('/')
            if any(x in ('', '.', '..') for x in parts) or parts[0] != top: fail('unsafe tar path: ' + repr(name))
            key = unicodedata.normalize('NFD', '/'.join(parts)).casefold()
            if key in seen: fail('duplicate tar member: ' + repr(name))
            seen.add(key)
            if not (m.isdir() or m.isfile()) or getattr(m, 'sparse', None) is not None:
                fail('tar contains a link, sparse or special file: ' + repr(name))
            if m.size < 0 or m.size > MAX_FILE: fail('tar member exceeds size limit')
            total += m.size
            if total > MAX_TOTAL: fail('tar exceeds expanded size limit')
            dest = output.joinpath(*parts)
            if m.isdir():
                dest.mkdir(parents=True, exist_ok=True)
            else:
                dest.parent.mkdir(parents=True, exist_ok=True)
                source = tf.extractfile(m)
                if source is None: fail('cannot read tar member')
                with source, dest.open('xb') as out:
                    shutil.copyfileobj(source, out, 1024 * 1024)
                os.chmod(dest, 0o755 if m.mode & 0o111 else 0o644)
    package = output / top
    for path in ('viva', 'extensions/pi/viva.ts', 'extensions/pi/memory/memory.ts'):
        if not (package / path).is_file(): fail('tar missing required file: ' + path)
    os.chmod(package / 'viva', 0o755)
    return package

def main():
    if platform.system() != 'Darwin': fail('this channel publishes macOS binaries only')
    if os.geteuid() == 0: fail('run as your user, not root')
    arch = {'arm64': 'macos-arm64', 'x86_64': 'macos-intel'}.get(platform.machine())
    if not arch: fail('unsupported architecture: ' + platform.machine())
    prefix = no_symlink_components(os.environ.get('VIVA_PREFIX') or os.path.join(os.environ['HOME'], '.local'))
    state = no_symlink_components(os.environ.get('VIVA_HOME') or os.path.join(os.environ['HOME'], '.viva'))
    if prefix == state or prefix in state.parents or state in prefix.parents:
        fail('VIVA_HOME and install prefix must not overlap')
    prefix.mkdir(parents=True, exist_ok=True)
    no_symlink_components(prefix)
    lock = prefix / '.viva-install.lock'
    try: lock.mkdir(mode=0o700)
    except FileExistsError: fail('install lock exists: %s; after verifying no installer is running, remove it manually' % lock)
    stage = None
    try:
        stage = Path(tempfile.mkdtemp(prefix='.viva-stage-', dir=prefix))
        base = os.environ.get('VIVA_INSTALL_BASE', 'https://macair-server.tail4414f.ts.net/viva').rstrip('/')
        if not base.startswith('https://'): fail('install base must use HTTPS')
        fetch(base + '/channel.json', stage / 'channel.json')
        if (stage / 'channel.json').stat().st_size > 1024 * 1024: fail('channel metadata too large')
        with (stage / 'channel.json').open() as f: data = json.load(f)
        info = data.get('assets', {}).get(arch, {})
        name, digest = info.get('file', ''), info.get('sha256', '')
        if not name or not digest:
            print('viva install: channel has no %s build yet' % arch, file=sys.stderr)
            raise SystemExit(2)
        if not isinstance(name, str) or not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9._-]*\.tar\.gz', name): fail('invalid channel filename')
        if not isinstance(digest, str) or not re.fullmatch(r'[0-9a-fA-F]{64}', digest): fail('invalid channel sha256')
        archive = stage / 'package.tar.gz'
        fetch(base + '/' + name, archive)
        h = hashlib.sha256()
        with archive.open('rb') as f:
            for chunk in iter(lambda: f.read(1024 * 1024), b''): h.update(chunk)
        if h.hexdigest() != digest.lower(): fail('checksum mismatch')
        unpack = stage / 'unpack'; unpack.mkdir()
        package = extract_checked(archive, unpack, 'viva-' + arch)
        # Version probe must not touch the caller's VIVA_HOME or its default home.
        probehome = stage / 'probe-home'; probehome.mkdir()
        probeenv = os.environ.copy(); probeenv['HOME'] = str(probehome); probeenv['VIVA_HOME'] = str(probehome / 'viva-home')
        subprocess.run([str(package / 'viva'), '--version'], check=True, env=probeenv, timeout=30)
        dest, bindir = prefix / 'viva', prefix / 'bin'
        no_symlink_components(dest); no_symlink_components(bindir)
        if exists(dest) and not dest.is_dir(): fail('existing installation is not a directory: ' + str(dest))
        if dest.is_dir():
            def walk_error(err): raise err
            for root, dirs, files in os.walk(dest, followlinks=False, onerror=walk_error):
                for entry in dirs + files:
                    if (Path(root) / entry).is_symlink(): fail('symbolic link in existing installation: ' + str(Path(root) / entry))
        bindir.mkdir(exist_ok=True)
        wrapper = bindir / 'viva'; no_symlink_components(wrapper)
        if exists(dest) and not exists(wrapper):
            fail('refusing to replace existing installation without an owned wrapper: ' + str(dest))
        if exists(wrapper):
            if not wrapper.is_file(): fail('wrapper is not a regular file: ' + str(wrapper))
            with wrapper.open('rb') as f: lines = f.read(8192).splitlines()
            if len(lines) < 2 or lines[0] != b'#!/bin/sh' or lines[1] != MARKER.encode(): fail('refusing to replace non-owned wrapper: ' + str(wrapper))
        # Keep an immutable control entry even if a later rename fails.
        control_entry = stage / 'control-viva'
        os.link(package / 'viva', control_entry)
        # Runtime admission and exact installation identity are checked BEFORE
        # replacing any files. 0.3.10 and live member credentials fail closed.
        state.mkdir(mode=0o700, parents=True, exist_ok=True)
        info = state.stat()
        if info.st_uid != os.geteuid() or info.st_mode & 0o077:
            fail('VIVA_HOME must be private and owned by the current user')
        lockfd = os.open(str(state / 'native-install.lock'), os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
        installation_guard = os.fdopen(lockfd, 'r+')
        if not stat.S_ISREG(os.fstat(lockfd).st_mode) or os.fstat(lockfd).st_uid != os.geteuid():
            fail('unsafe runtime installation lock')
        try: fcntl.flock(lockfd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError: fail('another installer/updater owns this VIVA_HOME')
        controlenv = os.environ.copy(); controlenv['VIVA_HOME'] = str(state)
        def control(binary, *args):
            result = subprocess.run([str(binary), 'runtime', 'upgrade', *args],
                                    env=controlenv, capture_output=True, text=True, timeout=130)
            if result.returncode:
                fail('runtime phase failed (files-installed is not upgrade-success): ' + result.stderr[-4096:])
            return json.loads(result.stdout)
        preflight = control(control_entry, '--check', '--entry', str(dest / 'viva' if exists(dest) else package / 'viva'))
        print('viva install: phase=prepared runtime=' + preflight['state'], flush=True)
        # Preserve each previous installation and owned wrapper in a fresh unique directory.
        backup = None
        if exists(dest) or exists(wrapper): backup = Path(tempfile.mkdtemp(prefix='viva-backup-', dir=prefix))
        wrapper_temp = stage / 'wrapper'
        wrapper_temp.write_text('#!/bin/sh\n' + MARKER + '\nexec ' + shlex.quote(str(dest / 'viva')) + ' "$@"\n')
        wrapper_temp.chmod(0o755)
        moved_dest = moved_wrapper = installed_dest = installed_wrapper = False
        try:
            if exists(dest): os.rename(dest, backup / 'viva'); moved_dest = True
            if exists(wrapper): os.rename(wrapper, backup / 'wrapper'); moved_wrapper = True
            os.rename(package, dest); installed_dest = True
            os.rename(wrapper_temp, wrapper); installed_wrapper = True
            subprocess.run([str(wrapper), '--version'], check=True, env=probeenv, timeout=30)
            print('viva install: phase=activate', flush=True)
            activation = control(dest / 'viva', '--entry', str(dest / 'viva'))
        except BaseException:
            # File rollback is safe only when no runtime existed, or the SAME
            # old generation is positively serving. Never restore an old binary
            # behind a committed new host; that is an ambiguous-outcome failure.
            safe_rollback = preflight['state'] == 'no_runtime'
            if not safe_rollback:
                try:
                    current = control(control_entry, 'status')
                    safe_rollback = all(current.get(key) == preflight.get(key) for key in
                        ('runtime_host_id', 'runtime_binary_sha256', 'runtime_start_marker'))
                except BaseException: pass
            if not safe_rollback:
                print('viva install: activation outcome unknown; installed files retained, previous files at %s; no database rollback attempted' % backup, file=sys.stderr)
                raise
            errors = []
            for action in (lambda: wrapper.unlink() if installed_wrapper else None,
                           lambda: shutil.rmtree(dest) if installed_dest else None,
                           lambda: os.rename(backup / 'viva', dest) if moved_dest else None,
                           lambda: os.rename(backup / 'wrapper', wrapper) if moved_wrapper else None):
                try: action()
                except BaseException as e: errors.append(str(e))
            if errors: print('viva install: rollback incomplete; inspect backup %s: %s' % (backup, '; '.join(errors)), file=sys.stderr)
            raise
        print('runtime result: ' + json.dumps(activation, sort_keys=True))
        print('installed channel=%s version=%s git_sha=%s' % (data.get('channel', ''), data.get('version', ''), data.get('git_sha', '')))
        print('binary: %s\ncommand: %s' % (dest / 'viva', wrapper))
        if backup: print('previous files retained: ' + str(backup))
    finally:
        if stage: shutil.rmtree(stage, ignore_errors=True)
        lock.rmdir()

def interrupted(signum, frame): raise RuntimeError('interrupted by signal %s' % signum)
for sig in (signal.SIGTERM, signal.SIGHUP): signal.signal(sig, interrupted)
try: main()
except (Exception, KeyboardInterrupt) as e:
    print('viva install: %s' % e, file=sys.stderr)
    sys.exit(1)
PY
