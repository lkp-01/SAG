import json
import pathlib
import sys
import threading
import unittest
import urllib.request
from http.server import ThreadingHTTPServer

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
from mock_http_server import H


class WorkloadEvidenceTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.server = ThreadingHTTPServer(("127.0.0.1", 0), H)
        cls.thread = threading.Thread(target=cls.server.serve_forever, daemon=True)
        cls.thread.start()
        cls.base = f"http://127.0.0.1:{cls.server.server_address[1]}"

    @classmethod
    def tearDownClass(cls):
        cls.server.shutdown()
        cls.server.server_close()

    def request(self, path, method="GET", body=None, headers=None):
        raw = None if body is None else json.dumps(body).encode()
        req = urllib.request.Request(
            self.base + path,
            data=raw,
            method=method,
            headers=headers or {},
        )
        with urllib.request.urlopen(req) as response:
            return response.status, json.load(response)

    def test_get_echoes_unique_correlation_and_canonical_identity(self):
        status, body = self.request(
            "/dev/?sag_correlation=corr-get-1",
            headers={"x-sag-user-id": "user-1", "x-sag-user-roles": "reader,admin"},
        )
        self.assertEqual(status, 200)
        self.assertEqual(body["correlation"], "corr-get-1")
        self.assertEqual(body["user_id"], "user-1")
        self.assertEqual(body["roles"], ["reader", "admin"])

    def test_mutation_counts_each_upstream_dispatch(self):
        headers = {
            "Content-Type": "application/json",
            "Idempotency-Key": "idem-count-1",
            "x-sag-user-id": "user-2",
            "x-sag-user-roles": "writer",
        }
        first_status, first = self.request(
            "/dev/", "POST", {"correlation": "corr-post-1"}, headers
        )
        second_status, second = self.request(
            "/dev/", "POST", {"correlation": "corr-post-1"}, headers
        )
        self.assertEqual((first_status, second_status), (200, 200))
        self.assertEqual(first["correlation"], "corr-post-1")
        self.assertEqual(first["side_effect_count"], 1)
        self.assertEqual(second["side_effect_count"], 2)


if __name__ == "__main__":
    unittest.main()
