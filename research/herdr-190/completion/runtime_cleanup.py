"""Experiment-only cleanup glue over the existing CLI, not a product runtime.

Importing this module starts no process. CLI callbacks retain per-attempt raw
records; this module only retries observations and preserves exception priority.
"""
from contextlib import contextmanager
import sys
import time
import traceback


def stop_and_observe(cli, pump, report, timeout=5, clock=time.monotonic):
    """Observe endpoint status, never infer OS process reaping from stop success."""
    report.update(not_running=False, status_attempts=[])
    try:
        cli('owned-server-stop', 'server', 'stop')
    except Exception as error:
        # The request may have completed before the connection closed. Record
        # the failed request and still independently inspect this named session.
        report['stop_error'] = f'{type(error).__name__}: {error}'
    deadline = clock() + timeout
    while True:
        label = f'server-status-after-stop-attempt-{len(report["status_attempts"])+1}'
        attempt = {'evidence': label + '.json'}
        try:
            status = cli(label, 'status', 'server')
            attempt['status'] = status
        except Exception as error:
            attempt['error'] = f'{type(error).__name__}: {error}'
            status = None
        report['status_attempts'].append(attempt)
        if isinstance(status, str) and 'status: not running' in status.splitlines():
            report.update(not_running=True, successful_status_evidence=label + '.json')
            return
        if clock() >= deadline:
            raise TimeoutError('Owned server status was not observed as not running')
        pump(.1)


@contextmanager
def preserving_primary_error(cleanup):
    """Always clean up; a secondary failure must not replace the original error."""
    try:
        yield
    except BaseException:
        try:
            cleanup()
        except BaseException:
            try:
                print('Cleanup also failed; preserving the original exception:', file=sys.stderr)
                traceback.print_exc()
            except BaseException:
                # Best-effort diagnostics (e.g. a closed stderr) must not mask
                # the primary failure either. File evidence is attempted by caller.
                pass
        raise
    else:
        # With no original failure, cleanup failure must remain a failing run.
        cleanup()
