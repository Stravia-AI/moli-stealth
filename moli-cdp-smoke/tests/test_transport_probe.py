from __future__ import annotations

import json
import unittest

from moli_cdp_smoke.transport_probe import compare


class TransportProbeTests(unittest.TestCase):
    def test_persisted_reference_preserves_live_cipher_comparison(self) -> None:
        connections = [
            {
                "requests": [{"path": path, "headers": []}],
                "frames": [],
                "alpn": "http/1.1",
                "cipher": ("TLS_AES_256_GCM_SHA384", "TLSv1.3", 256),
                "session_reused": False,
                "client_hello": {
                    "legacy_version": 771,
                    "session_id_length": 32,
                    "ciphers": [4866],
                    "compression": [0],
                    "extensions": [],
                },
            }
            for path in ("/page", "/socket")
        ]
        live = {
            "observations": [
                {"repetition": 0, "identity": {}, "connections": connections}
            ]
        }
        reference = json.loads(json.dumps(live))
        self.assertEqual(compare(reference, live), [])

        connections[0]["cipher"] = ("TLS_AES_128_GCM_SHA256", "TLSv1.3", 128)
        self.assertEqual(
            [(item["purpose"], item["field"]) for item in compare(reference, live)],
            [("http", "tls_cipher")],
        )
