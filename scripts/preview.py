#!/usr/bin/env python3
"""Build and serve a trusted PR in an isolated, disposable local environment."""
import argparse
import json
import os
from pathlib import Path
import secrets
import shlex
import shutil
import signal
import socket
import subprocess
import tempfile
import time
import urllib.request


def run(*args, **kwargs):
    # Build tools spawn compilers/linkers. Own their process group so a service
    # SIGTERM (not only a terminal's group-wide Ctrl-C) cannot orphan a build.
    if kwargs.pop('capture_output', False):
        kwargs.update(stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    process = subprocess.Popen(args, text=True, start_new_session=True, **kwargs)
    try:
        stdout, stderr = process.communicate()
    except BaseException:
        os.killpg(process.pid, signal.SIGTERM)
        try:
            process.communicate(timeout=10)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            process.communicate()
        raise
    if process.returncode:
        raise subprocess.CalledProcessError(process.returncode, args, stdout, stderr)
    return subprocess.CompletedProcess(args, process.returncode, stdout, stderr)


def free_port():
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        return sock.getsockname()[1]


def runtime_env(port, database, title='Agent Portal PR preview'):
    # Never inherit production OAuth, archive, notification or database settings.
    # Run outside the checkout as well: backend startup loads .env from its cwd.
    return {'PATH': os.environ.get('PATH', '/usr/bin:/bin'),
            'HOST': '127.0.0.1', 'PORT': str(port), 'DATABASE_URL': database,
            'SESSION_SECRET': secrets.token_hex(64),
            'BASE_URL': f'http://127.0.0.1:{port}',
            'SPLASH_TEXT': title, 'RUST_LOG': 'info'}


def await_ready(check, process=None, timeout=90):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if process is not None and process.poll() is not None:
            raise RuntimeError('Preview backend exited; inspect backend.log')
        try:
            if check():
                return
        except (OSError, urllib.error.URLError):
            pass
        time.sleep(0.25)
    raise RuntimeError('Preview readiness timed out')


def stop(process):
    if process is not None and process.poll() is None:
        process.terminate()
        try:
            process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()


def close_forward(identity):
    current = subprocess.run(['agent-portal', 'forward', 'list'], text=True,
                             capture_output=True, check=False)
    if current.returncode == 0 and any(line.startswith(identity) for line in current.stdout.splitlines()):
        subprocess.run(['agent-portal', 'forward', 'close'], check=False)
    else:
        print('Forward changed or unavailable; leaving it untouched.', flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('pr', type=int, help='PR number in this repository (trusted code only)')
    parser.add_argument('--forward', action='store_true',
                        help='Register a private Portal forward; requires a session with no existing forward')
    parser.add_argument('--ssh-host', default=socket.gethostname(), help='SSH destination printed for a remote reviewer')
    args = parser.parse_args()
    if args.pr < 1:
        parser.error('PR must be positive')
    for executable in ['git', 'cargo', 'trunk', 'docker'] + (['agent-portal'] if args.forward else []):
        if shutil.which(executable) is None:
            parser.error(f'{executable} is required; see scripts/README.md')
    if args.forward:
        existing = run('agent-portal', 'forward', 'list', capture_output=True).stdout.strip()
        if existing != 'No active forwards.':
            parser.error('This session already forwards a service. Use a separate session or omit --forward.')
    root = Path(__file__).resolve().parents[1]
    # Rust artifacts are large; /tmp may be a small memory-backed filesystem.
    cache = Path(os.environ.get('XDG_CACHE_HOME', str(Path.home() / '.cache'))) / 'agent-portal/previews'
    cache.mkdir(parents=True, exist_ok=True)
    scratch = Path(tempfile.mkdtemp(prefix=f'portal-preview-{args.pr}-', dir=cache))
    tree = scratch / 'source'
    container = f'portal-preview-{args.pr}-{secrets.token_hex(6)}'
    preview_ref = f'refs/portal-preview/{secrets.token_hex(16)}'
    backend = None
    forward_identity = None
    tree_added = False
    try:
        # FETCH_HEAD is shared across worktrees. A private ref cannot be
        # overwritten by another reviewer's concurrent fetch.
        run('git', '-C', str(root), 'fetch', 'origin', f'pull/{args.pr}/head:{preview_ref}')
        commit = run('git', '-C', str(root), 'rev-parse', preview_ref, capture_output=True).stdout.strip()
        run('git', '-C', str(root), 'worktree', 'add', '--detach', str(tree), commit)
        tree_added = True
        print(f'Building PR #{args.pr} at {commit}; work directory: {scratch}', flush=True)
        build_env = os.environ.copy()
        # A unique output directory prevents another worktree overwriting our executable.
        build_env['CARGO_TARGET_DIR'] = str(scratch / 'target')
        build_env['CARGO_PROFILE_DEV_DEBUG'] = '0'
        build_env['CARGO_PROFILE_DEV_INCREMENTAL'] = 'false'
        build_env.pop('TRUNK_NO_COLOR', None)
        build_env.pop('NO_COLOR', None)
        run('trunk', 'build', '--locked', cwd=tree / 'frontend', env=build_env)
        run('cargo', 'build', '--locked', '-p', 'backend', cwd=tree, env=build_env)
        password = secrets.token_hex(24)
        run('docker', 'run', '--detach', '--rm', '--name', container,
            '--publish', '127.0.0.1::5432', '--env', 'POSTGRES_USER=preview',
            '--env', 'POSTGRES_DB=preview', '--env', f'POSTGRES_PASSWORD={password}', 'postgres:16-alpine')
        await_ready(lambda: subprocess.run(['docker', 'exec', container, 'pg_isready', '-U', 'preview'],
                                         stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode == 0)
        binding = run('docker', 'port', container, '5432/tcp', capture_output=True).stdout.strip()
        db_port = int(binding.rsplit(':', 1)[1])
        port = free_port()
        database = f'postgresql://preview:{password}@127.0.0.1:{db_port}/preview'
        title = f'PR #{args.pr} preview {secrets.token_hex(8)}'
        with (scratch / 'backend.log').open('w') as log:
            backend = subprocess.Popen([str(scratch / 'target/debug/backend'), '--dev-mode'],
                                       cwd=scratch, env=runtime_env(port, database, title), stdout=log, stderr=log)
            def healthy():
                with urllib.request.urlopen(f'http://127.0.0.1:{port}/api/config', timeout=2) as response:
                    return response.status == 200 and json.load(response).get('splash_text') == title
            await_ready(healthy, backend)
            if args.forward:
                opened = run('agent-portal', 'forward', str(port), stdout=subprocess.PIPE)
                url = opened.stdout.strip()
                forward_identity = f':{port}  {url}  '
                print(url, flush=True)
                print('Private forward: sign in to the main Portal in this browser first. Share session access with reviewers.')
            print(f'PR #{args.pr} ({commit[:12]}) ready: http://127.0.0.1:{port}/', flush=True)
            print('Remote fallback (run on the reviewer’s machine):', flush=True)
            print(shlex.join(['ssh', '-N', '-o', 'ExitOnForwardFailure=yes', '-L',
                              f'127.0.0.1:{port}:127.0.0.1:{port}', args.ssh_host]), flush=True)
            print(f'Then open http://127.0.0.1:{port}/ . Ctrl-C here removes this preview and its database.', flush=True)
            code = backend.wait()
            if code:
                raise RuntimeError(f'Preview backend exited ({code})')
    finally:
        if forward_identity:
            close_forward(forward_identity)
        stop(backend)
        subprocess.run(['docker', 'rm', '-f', container], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        if tree_added:
            subprocess.run(['git', '-C', str(root), 'worktree', 'remove', '--force', str(tree)], check=False)
        subprocess.run(['git', '-C', str(root), 'update-ref', '-d', preview_ref], check=False)
        # Keep the small log for diagnosis; build/source and the disposable DB are gone.
        shutil.rmtree(scratch / 'target', ignore_errors=True)
        print(f'Preview stopped. Diagnostic log: {scratch / "backend.log"}', flush=True)


if __name__ == '__main__':
    signal.signal(signal.SIGTERM, lambda *_: (_ for _ in ()).throw(KeyboardInterrupt()))
    try:
        main()
    except KeyboardInterrupt:
        pass
