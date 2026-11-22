"""Finding 3DS emulators whose StreetPass data is a plain folder."""

import os
from pathlib import Path

from .settings import data_dir

FOLDERS = (
    ("azahar-emu", "Azahar"),
    ("azaharplus-emu", "AzaharPlus"),
    ("citra-emu", "Citra"),
    ("lime3ds-emu", "Lime3DS"),
)
PROCESSES = {"azahar", "azahar-qt", "azaharplus", "citra", "citra-qt", "lime3ds", "lime3ds-qt", "azahar-room"}
CEC = Path("sysdata/00010026/00000000/CEC")


def cec_folder(base):
    """Returns the CEC folder inside an emulator's data folder, or None."""
    base = Path(base)
    for candidate in [base, base / "CEC"] + sorted((base / "nand" / "data").glob("*/" + str(CEC))):
        if (candidate / "MBoxList____").is_file():
            return candidate
    return None


def find(home=None):
    """Returns (name, folder) for each installed emulator with StreetPass data."""
    home = Path(home) if home else Path.home()
    bases = [(data_dir() if home == Path.home() else home / ".local" / "share", "")]
    bases += [(app / "data", " (Flatpak)") for app in sorted((home / ".var" / "app").glob("*"))]
    out = []
    for base, how in bases:
        for folder, name in FOLDERS:
            found = cec_folder(base / folder)
            if found and all(found != f for _n, f in out):
                out.append((name + how, found))
    return out


def running(proc="/proc"):
    """Returns the name of a running emulator, or None."""
    try:
        pids = [p for p in os.listdir(proc) if p.isdigit()]
    except OSError:
        return None
    for pid in pids:
        try:
            comm = Path(proc, pid, "comm").read_text().strip().lower()
        except OSError:
            continue
        if comm in PROCESSES:
            return comm
    return None
