import json
import os
import queue
import shlex
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from ctrpass_app.helper import Helper, file_pass, helper_command

FAKE = Path(__file__).with_name("fake_helper.py")


class FakeStore:
    def __init__(self):
        self.peers = None

    def file_received(self, messages, peer):
        raise AssertionError("nothing arrived")

    def set_peers(self, text):
        self.peers = json.loads(text)


class HelperTest(unittest.TestCase):
    def setUp(self):
        os.environ["CTRPASS_HELPER"] = f"{shlex.quote(sys.executable)} {shlex.quote(str(FAKE))}"
        self.events = queue.Queue()
        self.helper = Helper(self.events.put)

    def tearDown(self):
        self.helper.close()
        del os.environ["CTRPASS_HELPER"]

    def next(self, kind):
        while True:
            e = self.events.get(timeout=5)
            if e["event"] == kind:
                return e

    def test_command_uses_override(self):
        self.assertEqual(helper_command("wlan1")[-3:], ["serve", "-i", "wlan1"])
        self.assertEqual(helper_command("wlan1", force=True)[-1], "--force")

    def test_exchange(self):
        self.helper.start("wlan1")
        self.assertEqual(self.next("hello")["interface"], "wlan1")
        with tempfile.NamedTemporaryFile("w", delete=False) as f:
            f.write("slot0x31KeyN=00\n")
        try:
            self.helper.load_keys(f.name)
        finally:
            os.unlink(f.name)
        self.assertTrue(self.next("keys")["record"])
        self.helper.exchange(role="wait", passes=1)
        self.assertEqual(self.next("error")["message"], "no store loaded")
        self.helper.load_store(json.dumps({"seed": "00" * 32, "titles": [], "messages": []}))
        self.assertEqual(self.next("loaded")["titles"], 0)
        self.helper.exchange(role="wait", passes=1)
        event = self.next("pass")
        self.assertEqual(event["role"], "wait")
        store = FakeStore()
        self.assertEqual(file_pass(store, event), {"filed": [], "refused": []})
        self.assertEqual(store.peers["filter"], ["96:58:cb:00:00:01"])
        self.helper.close()
        self.assertEqual(self.next("exit")["code"], 0)


if __name__ == "__main__":
    unittest.main()
