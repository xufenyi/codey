"""Black-box HTTP + mock Codex tests. Only Python's standard library is needed."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time
import unittest
import urllib.error
import urllib.parse
import urllib.request

ROOT = Path(__file__).resolve().parents[1]
SUFFIX = '.exe' if os.name == 'nt' else ''
BIN = ROOT / 'target' / 'debug' / ('codey' + SUFFIX)
MOCK = ROOT / 'target' / 'debug' / ('codey-mock' + SUFFIX)


class CodeyTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='codey-e2e-')
        self.dir = Path(self.temp.name)
        self.env = dict(os.environ, CODEY_MOCK_DIR=str(self.dir))
        self.process = None
        self.start()
        self.api('settings', {'codexPath': str(MOCK), 'approvalTimeoutSecs': 30, 'historyRetentionDays': 0})

    def start(self):
        self.process = subprocess.Popen([str(BIN), '--headless', '--no-browser', '--data-dir', str(self.dir / 'data')], env=self.env, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
        session = self.dir / 'data' / 'session.json'
        deadline = time.monotonic() + 15
        while time.monotonic() < deadline:
            if self.process.poll() is not None:
                raise AssertionError(self.process.stderr.read().decode(errors='replace'))
            if session.exists():
                data = json.loads(session.read_text(encoding='utf-8'))
                parts = urllib.parse.urlsplit(data['url'])
                self.base = f'{parts.scheme}://{parts.netloc}'
                self.token = urllib.parse.parse_qs(parts.fragment)['token'][0]
                try:
                    self.api('state')
                    return
                except Exception:
                    pass
            time.sleep(.05)
        raise AssertionError('Server did not start')

    def tearDown(self):
        if self.process and self.process.poll() is None:
            try:
                self.api('shutdown', {'mode': 'interrupt'})
                self.process.wait(timeout=30)
            except Exception:
                self.process.kill()
                self.process.wait()
        if self.process and self.process.stderr:
            self.process.stderr.close()
        self.temp.cleanup()

    def api(self, path, body=None, headers=None):
        h = {'Authorization': 'Bearer ' + self.token, 'Content-Type': 'application/json'}
        h.update(headers or {})
        req = urllib.request.Request(self.base + '/api/' + path, data=json.dumps(body).encode() if body is not None else None, headers=h)
        with urllib.request.urlopen(req, timeout=25) as response:
            return json.load(response)

    def create(self, prompt='Do the test.', **kwargs):
        t = int(time.time()*1000)
        body = dict(prompt=prompt, cwd=str(self.dir), title='E2E task', threadMode='new', sendMode='time', sendAt=t+800, stalePolicy='send', missedPolicy='send')
        body.update(kwargs)
        return self.api('jobs', body)['job']

    def wait_job(self, job_id, statuses, timeout=15):
        deadline = time.monotonic()+timeout
        while time.monotonic() < deadline:
            state = self.api('state')
            j = next(j for j in state['jobs'] if j['id'] == job_id)
            if j['status'] in statuses:
                return j
            time.sleep(.1)
        raise AssertionError(f'Expected {statuses}, got {j}')

    def test_authentication_and_origin(self):
        for headers in [{'Authorization': 'Bearer wrong'}, {'Origin': 'https://example.com'}, {'Host': 'attacker.invalid'}]:
            with self.assertRaises(urllib.error.HTTPError) as error:
                self.api('state', headers=headers)
            self.assertIn(error.exception.code, [401,403])
            error.exception.close()

    def test_new_and_existing_threads_complete(self):
        for kwargs in [{}, {'threadMode':'existing', 'threadId':'existing-thread', 'stalePolicy':'confirm'}]:
            j=self.create(**kwargs)
            result=self.wait_job(j['id'], ['completed'])
            self.assertTrue(result['turnId'])
            self.assertTrue(result['acceptedAt'])
            self.assertIn('Mock', result['output'])

    def test_uncertain_submission_is_never_retried(self):
        j=self.create('[disconnect]')
        self.wait_job(j['id'], ['review'])
        time.sleep(1.5)
        records=(self.dir/'accepted.jsonl').read_text().splitlines()
        self.assertEqual(len(records),1)
        with self.assertRaises(urllib.error.HTTPError) as error:
            self.api('jobs/'+j['id'], {'action':'now'})
        error.exception.close()
        self.api('history/refresh', {})
        self.wait_job(j['id'], ['completed'])

    def test_approval_accept_and_interrupt(self):
        j=self.create('[approval]')
        self.wait_job(j['id'], ['approval'])
        a=self.api('state')['approvals'][0]
        self.api('approvals/'+a['id'], {'allow':True})
        self.wait_job(j['id'], ['completed'])
        j=self.create('[hold]')
        self.wait_job(j['id'], ['running'])
        self.api('jobs/'+j['id'], {'action':'interrupt'})
        self.wait_job(j['id'], ['review'])

    def test_stale_confirmation_and_busy_thread(self):
        j=self.create(threadMode='existing', threadId='existing-thread', stalePolicy='confirm', sendAt=int(time.time()*1000)+2000)
        (self.dir/'history-changed').touch()
        current=self.wait_job(j['id'], ['confirmation'])
        with self.assertRaises(urllib.error.HTTPError) as error:
            self.api('jobs/'+j['id'], {'action':'confirm','confirmationId':'obsolete'})
        error.exception.close()
        self.api('jobs/'+j['id'], {'action':'confirm','confirmationId':current['confirmationId']})
        self.wait_job(j['id'], ['completed'])
        j=self.create(threadMode='existing',threadId='busy-thread')
        result=self.wait_job(j['id'], ['scheduled'])
        time.sleep(1)
        result=self.wait_job(j['id'], ['scheduled'])
        self.assertIsNone(result['submittedAt'])

    def test_quota_account_change_and_expiry(self):
        (self.dir/'quota-blocked').touch()
        j=self.create(sendMode='available', expiresAt=int(time.time()*1000)+10000)
        time.sleep(1)
        result=self.wait_job(j['id'], ['scheduled'])
        self.assertIsNone(result['submittedAt'])
        (self.dir/'quota-blocked').unlink()
        self.api('jobs/'+j['id'], {'action':'now'})
        self.wait_job(j['id'], ['completed'])
        j=self.create(sendAt=int(time.time()*1000)+1500)
        (self.dir/'account-changed').touch()
        self.wait_job(j['id'], ['paused'])

    def test_restart_and_missed_time(self):
        j=self.create(sendAt=int(time.time()*1000)+5000, missedPolicy='confirm')
        self.api('shutdown', {'mode':'wait'})
        self.process.wait(timeout=15)
        self.process.stderr.close()
        time.sleep(5)
        self.start()
        self.wait_job(j['id'], ['confirmation'])

    def test_reject_does_not_claim_acceptance(self):
        j=self.create('[reject]')
        result=self.wait_job(j['id'], ['paused'])
        self.assertIsNone(result['submittedAt'])
        self.assertIsNone(result['acceptedAt'])

    def test_approval_timeout_never_retries(self):
        j=self.create('[approval]')
        self.wait_job(j['id'], ['approval'])
        self.wait_job(j['id'], ['review'], timeout=35)
        self.assertEqual(len((self.dir/'accepted.jsonl').read_text().splitlines()), 1)
        self.assertEqual(self.api('state')['approvals'], [])

    def test_expired_availability_and_crash_recovery(self):
        (self.dir/'quota-blocked').touch()
        j=self.create(sendMode='available', expiresAt=int(time.time()*1000)+1800)
        self.wait_job(j['id'], ['expired'])
        (self.dir/'quota-blocked').unlink()
        j=self.create('[hold]')
        self.wait_job(j['id'], ['running'])
        self.process.kill()
        self.process.wait()
        self.process.stderr.close()
        self.start()
        self.wait_job(j['id'], ['review'])
        self.assertEqual(len((self.dir/'accepted.jsonl').read_text().splitlines()), 1)


if __name__ == '__main__':
    unittest.main(verbosity=2)
