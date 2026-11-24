import json
import tempfile
import unittest

try:
    import ctrpass
except ImportError:
    ctrpass = None

PLAZA = 0x00020800
KEYS = "\n".join([
    "slot0x2EKeyN=" + "11" * 16,
    "slot0x31KeyN=" + "22" * 16,
    "streetpassHmacKey=" + "33" * 16,
    "titleKey00020800=" + "07" * 32,
])


@unittest.skipIf(ctrpass is None, "the ctrpass module is not built")
class BindingsTest(unittest.TestCase):
    def setUp(self):
        self.dirs = [tempfile.TemporaryDirectory() for _ in range(2)]
        self.a = ctrpass.Store(self.dirs[0].name)
        self.b = ctrpass.Store(self.dirs[1].name)

    def tearDown(self):
        self.a.close()
        self.b.close()
        for d in self.dirs:
            d.cleanup()

    def test_keys_and_titles(self):
        self.assertEqual(ctrpass.check_keys(KEYS), {"titles": [PLAZA], "record": True})
        with self.assertRaises(ValueError):
            ctrpass.check_keys("nonsense")
        plaza = [t for t in ctrpass.titles() if t["title"] == PLAZA][0]
        self.assertTrue(any(f["key"] == "name" for f in plaza["fields"]))
        self.assertIsNone(plaza["plugin"])
        self.assertIn("errors", ctrpass.plugin_issues())

    def test_build_store_and_file(self):
        msg = ctrpass.build_message(PLAZA, {"name": "Tester"}, KEYS, seed=self.a.seed)
        self.assertEqual(msg.title, PLAZA)
        self.assertTrue(msg.verify(bytes([7] * 32)))
        self.assertEqual(ctrpass.read_fields(msg, KEYS)["values"]["name"], "Tester")
        self.assertEqual(ctrpass.Container(bytes(msg)).message_id, msg.message_id)
        self.assertEqual(len(ctrpass.read_messages(ctrpass.envelope_of([msg]))), 1)

        self.a.add_title(PLAZA, "Mii Plaza", bytes([7] * 32))
        self.a.add_message(msg)
        self.a.set_receive(PLAZA, True)
        [row] = self.a.titles()
        self.assertEqual((row["outbox"], row["key"], row["receive"]), (1, True, True))
        snap = json.loads(self.a.snapshot())
        self.assertEqual(len(snap["messages"]), 1)

        self.b.add_title(PLAZA, None, bytes([7] * 32))
        got = self.b.file_received([bytes(msg)], "96:58:cb:00:00:01")
        self.assertEqual(got["filed"], [msg.message_id])
        [(box, held)] = self.b.messages(PLAZA)
        self.assertEqual(box, "inbox")
        self.b.set_peers(json.dumps({"filter": ["96:58:cb:00:00:01"]}))
        with self.assertRaises(ValueError):
            self.b.set_peers('{"filter": 3}')

    def test_rotate_clears_filter(self):
        self.a.set_peers(json.dumps({"filter": ["96:58:cb:00:00:01"]}))
        before = self.a.identity()
        self.assertIsNone(self.a.rotated())
        mac = self.a.rotate()
        self.assertNotEqual(mac, before["mac"])
        self.assertEqual(self.a.identity(), {"mac": mac, "mii_id": before["mii_id"]})
        self.assertIsNotNone(self.a.rotated())
        self.assertEqual(json.loads(self.a.snapshot())["peers"]["filter"], [])

    def test_export_into_a_cec_folder(self):
        msg = ctrpass.build_message(PLAZA, {}, KEYS, seed=self.a.seed)
        self.a.add_title(PLAZA, None, bytes([7] * 32))
        self.a.add_message(msg, "inbox")
        self.b.add_title(PLAZA, None, bytes([7] * 32))
        out = self.a.export_to(self.dirs[1].name)
        self.assertEqual([m["result"] for m in out["messages"]], ["delivered"])


if __name__ == "__main__":
    unittest.main()
