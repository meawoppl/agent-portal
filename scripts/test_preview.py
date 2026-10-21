"""Lifecycle regression tests; no real builds, forwards or databases are touched."""
import importlib.util
import os
from pathlib import Path
import subprocess
import unittest
from unittest.mock import patch, Mock

spec = importlib.util.spec_from_file_location('preview', Path(__file__).with_name('preview.py'))
preview = importlib.util.module_from_spec(spec)
spec.loader.exec_module(preview)


class PreviewTests(unittest.TestCase):
    def test_interrupted_build_terminates_its_process_group(self):
        process = Mock(pid=123)
        process.communicate.side_effect = [KeyboardInterrupt(), ('', '')]
        with patch.object(preview.subprocess, 'Popen', return_value=process) as start, \
             patch.object(preview.os, 'killpg') as kill:
            with self.assertRaises(KeyboardInterrupt):
                preview.run('cargo', 'build')
        self.assertTrue(start.call_args.kwargs['start_new_session'])
        kill.assert_called_once_with(123, preview.signal.SIGTERM)
        self.assertEqual(process.communicate.call_count, 2)

    def test_cleanup_preserves_a_changed_forward(self):
        with patch.object(preview.subprocess, 'run', return_value=Mock(returncode=0, stdout=':4000  https://other  ')) as run:
            preview.close_forward(':3000  https://preview  ')
        self.assertEqual(run.call_count, 1)

    def test_runtime_cannot_inherit_production_integrations(self):
        with patch.dict(os.environ, {'DATABASE_URL': 'production', 'PORTAL_SESSION_ARCHIVE_BACKEND': 's3',
                                     'GOOGLE_CLIENT_SECRET': 'secret', 'PORTAL_VAPID_PRIVATE_KEY': 'secret'}):
            env = preview.runtime_env(4321, 'isolated')
        self.assertEqual(env['DATABASE_URL'], 'isolated')
        self.assertEqual(env['HOST'], '127.0.0.1')
        self.assertFalse(any(key.startswith(('GOOGLE_', 'PORTAL_')) for key in env))

    def test_backend_exit_is_not_mistaken_for_other_listeners_health(self):
        process = Mock()
        process.poll.return_value = 1
        healthy = Mock(return_value=True)
        with self.assertRaisesRegex(RuntimeError, 'exited'):
            preview.await_ready(healthy, process)
        healthy.assert_not_called()

    def test_stop_escalates_and_waits(self):
        process = Mock()
        process.poll.return_value = None
        process.wait.side_effect = [subprocess.TimeoutExpired('backend', 10), 0]
        preview.stop(process)
        process.terminate.assert_called_once()
        process.kill.assert_called_once()
        self.assertEqual(process.wait.call_count, 2)

    def test_existing_forward_is_not_overwritten(self):
        with patch('sys.argv', ['preview.py', '123', '--forward']), \
             patch.object(preview.shutil, 'which', return_value='/bin/tool'), \
             patch.object(preview, 'run', return_value=Mock(stdout=':3000 https://existing')) as run:
            with self.assertRaises(SystemExit):
                preview.main()
        run.assert_called_once_with('agent-portal', 'forward', 'list', capture_output=True)

    def test_success_builds_frontend_first_and_cleans_private_forward(self):
        calls = []
        def run(*args, **kwargs):
            calls.append(args)
            if args[:3] == ('agent-portal', 'forward', 'list'):
                return Mock(stdout='No active forwards.')
            if args[:2] == ('docker', 'port'):
                return Mock(stdout='127.0.0.1:5555')
            return Mock(stdout='https://preview.example')
        process = Mock()
        process.wait.return_value = 0
        process.poll.return_value = 0
        with patch('sys.argv', ['preview.py', '123', '--forward']), \
             patch.object(preview.shutil, 'which', return_value='/bin/tool'), \
             patch.object(preview, 'run', side_effect=run), \
             patch.object(preview, 'await_ready'), \
             patch.object(preview.subprocess, 'Popen', return_value=process) as start, \
             patch.object(preview.subprocess, 'run', return_value=Mock(returncode=0, stdout=':4321  https://preview.example  (0 / 8 connections)')) as cleanup, \
             patch.object(preview, 'free_port', return_value=4321):
            preview.main()
        build_tools = [call[0] for call in calls if call[0] in ('trunk', 'cargo')]
        self.assertEqual(build_tools, ['trunk', 'cargo'])
        self.assertIn('127.0.0.1::5432', next(call for call in calls if call[:2] == ('docker', 'run')))
        self.assertEqual(start.call_args.kwargs['env']['HOST'], '127.0.0.1')
        self.assertIn(':5555/preview', start.call_args.kwargs['env']['DATABASE_URL'])
        self.assertNotEqual(start.call_args.kwargs['cwd'].name, 'source')
        cleanup.assert_any_call(['agent-portal', 'forward', 'close'], check=False)

    def test_build_failure_cleans_only_own_worktree(self):
        calls = []
        def run(*args, **kwargs):
            calls.append(args)
            if args[0] == 'trunk':
                raise subprocess.CalledProcessError(1, args)
            return Mock(stdout='abc123\n')
        with patch('sys.argv', ['preview.py', '123']), \
             patch.object(preview.shutil, 'which', return_value='/bin/tool'), \
             patch.object(preview, 'run', side_effect=run), \
             patch.object(preview.subprocess, 'run') as cleanup:
            with self.assertRaises(subprocess.CalledProcessError):
                preview.main()
        self.assertTrue(calls[0][-1].startswith('pull/123/head:refs/portal-preview/'))
        self.assertTrue(any('--detach' in call for call in calls))
        self.assertFalse(any(call[0] == 'cargo' for call in calls))
        cleaned = next(c.args[0] for c in cleanup.call_args_list if 'worktree' in c.args[0])
        self.assertIn('worktree', cleaned)
        self.assertIn('remove', cleaned)
        self.assertIn('portal-preview-123-', cleaned[-1])


if __name__ == '__main__':
    unittest.main()
