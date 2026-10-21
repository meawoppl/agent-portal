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
        self.assertIn('pull/123/head', calls[0])
        self.assertTrue(any('--detach' in call for call in calls))
        self.assertFalse(any(call[0] == 'cargo' for call in calls))
        cleaned = cleanup.call_args_list[-1].args[0]
        self.assertIn('worktree', cleaned)
        self.assertIn('remove', cleaned)
        self.assertIn('portal-preview-123-', cleaned[-1])


if __name__ == '__main__':
    unittest.main()
