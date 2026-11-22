"""Starting the app at login through the desktop's autostart folder."""

import os
import shlex
import shutil
import sys
from pathlib import Path

from . import APP_ID
from .settings import config_dir

FLAG = "--background"


def path():
    return config_dir().parent / "autostart" / f"{APP_ID}.desktop"


def command():
    """The command that starts the app hidden, preferring the installed launcher."""
    launcher = shutil.which("ctrpass-app")
    if launcher:
        return [launcher, FLAG]
    return [sys.executable, "-m", "ctrpass_app.main", FLAG]


def enabled():
    return path().is_file()


def set_enabled(on):
    p = path()
    if not on:
        p.unlink(missing_ok=True)
        return
    p.parent.mkdir(parents=True, exist_ok=True)
    env = ""
    if not shutil.which("ctrpass-app"):
        # Run from a source folder, so the package and module must be found the same way.
        here = str(Path(__file__).resolve().parents[1])
        extra = os.environ.get("PYTHONPATH")
        env = "env " + shlex.quote("PYTHONPATH=" + (here + ":" + extra if extra else here)) + " "
    text = "\n".join([
        "[Desktop Entry]",
        "Type=Application",
        "Name=ctrpass",
        "Comment=StreetPass in the background",
        "Exec=" + env + " ".join(shlex.quote(a) for a in command()),
        "Icon=network-wireless-symbolic",
        "Terminal=false",
        "X-GNOME-Autostart-enabled=true",
        "",
    ])
    tmp = p.with_suffix(".tmp")
    tmp.write_text(text, encoding="utf-8")
    tmp.replace(p)
