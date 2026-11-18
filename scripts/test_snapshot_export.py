import hashlib
import json
from pathlib import Path
import tempfile
import unittest

from export_snapshot import merge, seal, unseal


class ExportIntegrityTests(unittest.TestCase):
    def test_corrupt_payload_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'snapshot.json'
            path.write_bytes(seal({'quantity': '0x1234'}).replace(b'0x1234', b'0x1235'))
            with self.assertRaises(ValueError):
                unseal(path)

    def test_digest_uses_exact_payload_bytes(self):
        payload = '{ "amount" : "0x1234", "memo" : "雪" }'.encode()
        envelope = b'{"sha256":"' + hashlib.sha256(payload).hexdigest().encode() + b'","payload":' + payload + b'}'
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'snapshot.json'
            path.write_bytes(envelope)
            self.assertEqual(unseal(path), {'amount': '0x1234', 'memo': '雪'})

    def test_merge_preserves_identity_and_immutable_data(self):
        original = {'identity': {'chain_id': '0x539'}, 'blocks': [{'hash': 'block', 'value': 1}], 'receipts': []}
        changed = json.loads(json.dumps(original))
        changed['blocks'][0]['value'] = 2
        with self.assertRaises(ValueError):
            merge(original, changed)
        changed = json.loads(json.dumps(original))
        changed['identity']['chain_id'] = '0x538'
        with self.assertRaises(ValueError):
            merge(original, changed)


if __name__ == '__main__':
    unittest.main()
