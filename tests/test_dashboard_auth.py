import base64
import http.client
import os
from pathlib import Path
import subprocess
import sys
import threading
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'deploy/dashboard'))
import server


class DashboardAuthTests(unittest.TestCase):
    def setUp(self):
        self.token = patch.object(server, 'TOKEN', 'test-access-token')
        self.token.start()
        self.httpd = server.Server(('127.0.0.1', 0), server.Handler)
        self.thread = threading.Thread(target=self.httpd.serve_forever, daemon=True)
        self.thread.start()

    def tearDown(self):
        self.httpd.shutdown()
        self.httpd.server_close()
        self.thread.join()
        self.token.stop()

    def request(self, method, path, authorization=None, **headers):
        if authorization is not None:
            headers['Authorization'] = authorization
        connection = http.client.HTTPConnection('127.0.0.1', self.httpd.server_address[1], timeout=3)
        try:
            connection.request(method, path, headers=headers)
            response = connection.getresponse()
            return response.status, dict(response.getheaders()), response.read()
        finally:
            connection.close()

    def basic(self, password='test-access-token'):
        return 'Basic ' + base64.b64encode(('admin:' + password).encode()).decode()

    def test_all_pages_and_api_methods_require_authentication(self):
        with patch.object(server, 'restart_duaa') as restart:
            for method, path in [('GET', '/'), ('GET', '/app.js'), ('GET', '/api/config'),
                                 ('GET', '/qr/stu1.png'), ('GET', '/healthz'),
                                 ('POST', '/api/duaa/restart'), ('PUT', '/api/config'),
                                 ('DELETE', '/api/accounts/stu1?remove_volume=1')]:
                with self.subTest(method=method, path=path):
                    status, headers, _ = self.request(method, path)
                    self.assertEqual(status, 401)
                    self.assertIn('WWW-Authenticate', headers)
            restart.assert_not_called()

    def test_malformed_or_incorrect_credentials_are_rejected(self):
        for authorization in ['Basic ???', 'Basic /w==', self.basic('wrong'), 'Bearer test-access-token']:
            with self.subTest(authorization=authorization):
                self.assertEqual(self.request('GET', '/', authorization)[0], 401)

    def test_authenticated_browser_can_read_and_mutate(self):
        self.assertEqual(self.request('GET', '/', self.basic())[0], 200)
        with patch.object(server, 'restart_duaa', return_value={'ok': True}) as restart:
            self.assertEqual(self.request('POST', '/api/duaa/restart', self.basic())[0], 200)
            restart.assert_called_once()
            self.assertEqual(self.request('POST', '/api/duaa/restart', self.basic(),
                                         Origin='https://other.example')[0], 403)
            restart.assert_called_once()

    def test_local_access_without_configured_token(self):
        with patch.object(server, 'TOKEN', ''):
            self.assertEqual(self.request('GET', '/')[0], 200)

    def test_nonlocal_bind_without_token_is_rejected_before_startup(self):
        for bind in ['0.0.0.0', '192.168.1.1', 'localhost']:
            with self.subTest(bind=bind), self.assertRaises(ValueError):
                server.validate_bind(bind, '')
        server.validate_bind('127.0.0.1', '')
        server.validate_bind('0.0.0.0', 'configured-token')
        result = subprocess.run([sys.executable, str(ROOT / 'deploy/dashboard/server.py')],
                                env={**os.environ, 'DASHBOARD_BIND': '0.0.0.0', 'DASHBOARD_TOKEN': ''},
                                capture_output=True, text=True, timeout=5)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('DASHBOARD_TOKEN', result.stderr)


if __name__ == '__main__':
    unittest.main()
