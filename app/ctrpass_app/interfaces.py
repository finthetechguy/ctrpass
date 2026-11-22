"""Finding the wireless cards ctrpass can use."""

from pathlib import Path


def wireless_interfaces(root="/sys/class/net"):
    """Returns (name, connected) for each wireless interface, unconnected ones first."""
    out = []
    for dev in sorted(Path(root).glob("*")):
        if not (dev / "phy80211").exists() or dev.name.startswith("ctrpass"):
            continue
        try:
            carrier = (dev / "carrier").read_text().strip() == "1"
        except OSError:
            carrier = False
        out.append((dev.name, carrier))
    out.sort(key=lambda e: (e[1], e[0]))
    return out
