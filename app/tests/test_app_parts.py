import json
import os
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from ctrpass_app import emulators, fields  # noqa: E402
from ctrpass_app.interfaces import wireless_interfaces  # noqa: E402
from ctrpass_app.settings import Settings  # noqa: E402


class SettingsTest(unittest.TestCase):
    def test_round_trip_and_bad_values(self):
        with tempfile.TemporaryDirectory() as d, mock.patch.dict(os.environ, {"XDG_CONFIG_HOME": d}):
            s = Settings.load()
            s.mode = "wait"
            s.interface = "wlan1"
            s.save()
            self.assertEqual(Settings.load().mode, "wait")
            (Path(d) / "ctrpass" / "settings.json").write_text(json.dumps({"mode": "fly", "radio": 3}))
            s = Settings.load()
            self.assertEqual((s.mode, s.radio), ("alternate", "auto"))
            (Path(d) / "ctrpass" / "settings.json").write_text("[]")
            self.assertEqual(Settings.load().mode, "alternate")

    def test_keys_text(self):
        with tempfile.NamedTemporaryFile("w", delete=False) as f:
            f.write("a=1\n")
        try:
            self.assertEqual(Settings(keys_path=f.name).keys_text(), "a=1\n")
        finally:
            os.unlink(f.name)
        self.assertIsNone(Settings(keys_path="").keys_text())
        self.assertIsNone(Settings(keys_path="/nonexistent/keys").keys_text())


class InterfacesTest(unittest.TestCase):
    def test_lists_wireless_cards(self):
        with tempfile.TemporaryDirectory() as d:
            for name, wireless, carrier in (("wlan0", True, "1"), ("wlan1", True, "0"), ("eth0", False, "1"), ("ctrpass0", True, "0")):
                dev = Path(d, name)
                dev.mkdir()
                if wireless:
                    (dev / "phy80211").mkdir()
                (dev / "carrier").write_text(carrier)
            self.assertEqual(wireless_interfaces(d), [("wlan1", False), ("wlan0", True)])


class EmulatorsTest(unittest.TestCase):
    def test_finds_cec_folders(self):
        with tempfile.TemporaryDirectory() as home:
            cec = Path(home, ".local/share/azahar-emu/nand/data/0000/sysdata/00010026/00000000/CEC")
            cec.mkdir(parents=True)
            (cec / "MBoxList____").write_bytes(b"hh")
            flat = Path(home, ".var/app/org.example/data/citra-emu/CEC")
            flat.mkdir(parents=True)
            (flat / "MBoxList____").write_bytes(b"hh")
            with mock.patch.dict(os.environ, {"XDG_DATA_HOME": ""}):
                found = emulators.find(home)
            self.assertEqual(found, [("Azahar", cec), ("Citra (Flatpak)", flat)])

    def test_running(self):
        with tempfile.TemporaryDirectory() as proc:
            Path(proc, "12").mkdir()
            Path(proc, "12", "comm").write_text("bash\n")
            self.assertIsNone(emulators.running(proc))
            Path(proc, "13").mkdir()
            Path(proc, "13", "comm").write_text("azahar-qt\n")
            self.assertEqual(emulators.running(proc), "azahar-qt")


class FieldsTest(unittest.TestCase):
    NUM = {"type": "number", "min": 0, "max": 203, "exclude": [5], "hex": False}
    HEX = {"type": "number", "min": 1, "max": 0xFFFF, "exclude": [], "hex": True}

    def test_numbers(self):
        self.assertEqual(fields.parse_number(self.NUM, " 12 "), 12)
        self.assertEqual(fields.parse_number(self.NUM, "0x10"), 16)
        self.assertEqual(fields.parse_number(self.HEX, "ff"), 255)
        self.assertEqual(fields.number_text(self.HEX, 255), "FF")
        for bad in ("", "abc", "204", "5"):
            with self.assertRaises(ValueError):
                fields.parse_number(self.NUM, bad)

    def test_text_and_bytes(self):
        kind = {"type": "text", "max": 3, "forbid": "%"}
        self.assertEqual(fields.check_text(kind, "abc"), "abc")
        for bad in ("abcd", "a%"):
            with self.assertRaises(ValueError):
                fields.check_text(kind, bad)
        self.assertEqual(fields.parse_bytes({"len": 2}, "0a:0B"), "0a0b")
        with self.assertRaises(ValueError):
            fields.parse_bytes({"len": 2}, "0a")

    def test_names_and_summary(self):
        self.assertEqual(fields.title_name(0x30600), "Mario Kart 7")
        self.assertEqual(fields.title_name(0x30600, {0x30600: "MARIO KART 7"}), "MARIO KART 7")
        self.assertEqual(fields.title_name(0x123400), "Game 00123400")
        title = {"fields": [
            {"key": "mii", "kind": {"type": "text"}, "advanced": True},
            {"key": "name", "kind": {"type": "text"}, "advanced": False},
        ]}
        self.assertEqual(fields.summary(title, {"mii": "x", "name": "Ada"}), "Ada")
        self.assertIsNone(fields.summary(title, {}))
        self.assertEqual(fields.changed({"a": 1, "b": 2}, {"a": 1, "b": 3}), {"b": 3})



class AutostartTest(unittest.TestCase):
    def test_enable_and_disable(self):
        from ctrpass_app import autostart

        with tempfile.TemporaryDirectory() as d, mock.patch.dict(os.environ, {"XDG_CONFIG_HOME": d}):
            self.assertFalse(autostart.enabled())
            autostart.set_enabled(True)
            self.assertTrue(autostart.enabled())
            text = autostart.path().read_text()
            self.assertIn("--background", text)
            self.assertTrue(autostart.path().parent.samefile(Path(d) / "autostart"))
            autostart.set_enabled(False)
            self.assertFalse(autostart.enabled())
            autostart.set_enabled(False)


if __name__ == "__main__":
    unittest.main()
