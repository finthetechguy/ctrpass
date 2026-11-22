"""Settings kept as JSON in the user's config folder."""

import json
import os
from dataclasses import asdict, dataclass, field
from pathlib import Path

MODES = ("alternate", "lead", "wait")
RADIOS = ("auto", "active", "mixed")
CLEAR_HOURS_MAX = 168


def config_dir():
    return Path(os.environ.get("XDG_CONFIG_HOME") or Path.home() / ".config") / "ctrpass"


def data_dir():
    return Path(os.environ.get("XDG_DATA_HOME") or Path.home() / ".local" / "share")


def _default_keys():
    env = os.environ.get("CTRPASS_KEYS")
    if env:
        return env
    path = config_dir() / "keys"
    return str(path) if path.is_file() else ""


def _default_store():
    return os.environ.get("CTRPASS_STORE") or str(data_dir() / "ctrpass")


@dataclass
class Settings:
    keys_path: str = field(default_factory=_default_keys)
    store: str = field(default_factory=_default_store)
    interface: str = ""
    mode: str = "alternate"
    radio: str = "auto"
    background: bool = False
    clear_hours: int = 8

    @classmethod
    def load(cls):
        try:
            data = json.loads((config_dir() / "settings.json").read_text(encoding="utf-8"))
        except (OSError, ValueError):
            return cls()
        if not isinstance(data, dict):
            return cls()
        defaults = asdict(cls())
        known = {k: v for k, v in data.items() if k in defaults and type(v) is type(defaults[k])}
        s = cls(**known)
        if s.mode not in MODES:
            s.mode = "alternate"
        if s.radio not in RADIOS:
            s.radio = "auto"
        if not 1 <= s.clear_hours <= CLEAR_HOURS_MAX:
            s.clear_hours = 8
        return s

    def save(self):
        d = config_dir()
        d.mkdir(parents=True, exist_ok=True)
        tmp = d / "settings.json.tmp"
        tmp.write_text(json.dumps(asdict(self), indent=2), encoding="utf-8")
        tmp.replace(d / "settings.json")

    def keys_text(self):
        """Returns the key file's text, or None if it is unset or unreadable."""
        if not self.keys_path:
            return None
        try:
            return Path(self.keys_path).read_text(encoding="utf-8")
        except OSError:
            return None
