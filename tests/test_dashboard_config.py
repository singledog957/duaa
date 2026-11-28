import json
from pathlib import Path
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'deploy/dashboard'))
from config_api import public_config, validate_config, write_config


class DashboardConfigTests(unittest.TestCase):
    def test_legacy_targets_and_saved_password_survive_config_roundtrip(self):
        target = json.dumps({'course_id': '91999', 'name': '高等数学，进阶'}, ensure_ascii=False)
        original = {
            'poll_interval_minutes': 5,
            'auto_window_minutes': 15,
            'log_file': 'logs/duaa.log',
            'students': [{'student_id': 's1', 'sso_password': 'existing-password',
                          'course_ids': [target], 'auto_include_new_courses': True}],
        }
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'config.json'
            path.write_text(json.dumps(original), encoding='utf-8')
            payload = public_config(path)
            self.assertEqual(payload['students'][0]['password'], '')
            cleaned = validate_config(payload, path)
            write_config(cleaned, path)
            saved = json.loads(path.read_text())
            self.assertEqual(saved['students'][0]['course_ids'], [target])
            self.assertEqual(saved['students'][0]['sso_password'], 'existing-password')
            self.assertTrue(saved['students'][0]['auto_include_new_courses'])
            payload = public_config(path)
            payload['students'][0]['password'] = '  new "password" & <secret>  '
            cleaned = validate_config(payload, path)
            self.assertEqual(cleaned['students'][0]['sso_password'], '  new "password" & <secret>  ')


if __name__ == '__main__':
    unittest.main()
