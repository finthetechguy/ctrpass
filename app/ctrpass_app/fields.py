"""Turning title field values into text for editing and back."""


def number_text(kind, value):
    if value is None:
        return ""
    return f"{value:X}" if kind.get("hex") else str(value)


def parse_number(kind, text):
    """Returns the number in `text`, or raises ValueError with a short reason."""
    t = text.strip()
    if not t:
        raise ValueError("Enter a number")
    try:
        if t.lower().startswith("0x"):
            n = int(t[2:], 16)
        else:
            n = int(t, 16) if kind.get("hex") else int(t)
    except ValueError:
        raise ValueError("Not a number") from None
    if n < kind["min"] or n > kind["max"]:
        raise ValueError(f"Must be from {number_text(kind, kind['min'])} to {number_text(kind, kind['max'])}")
    if n in kind.get("exclude", []):
        raise ValueError("This value is not allowed")
    return n


def check_text(kind, text):
    """Raises ValueError when `text` does not fit a text field."""
    if len(text.encode("utf-16-le")) // 2 > kind["max"]:
        raise ValueError(f"At most {kind['max']} characters")
    bad = [c for c in text if c in kind.get("forbid", "")]
    if bad:
        raise ValueError(f"Can't contain {bad[0]}")
    return text


def parse_bytes(kind, text):
    t = text.strip().replace(":", "").replace(" ", "")
    try:
        data = bytes.fromhex(t)
    except ValueError:
        raise ValueError("Not hex") from None
    if len(data) != kind["len"]:
        raise ValueError(f"Must be {kind['len']} bytes")
    return data.hex()


def changed(before, after):
    """Returns the values in `after` that differ from `before`."""
    return {k: v for k, v in after.items() if before.get(k) != v}


# Names for common StreetPass games, used until a console's own data names them.
KNOWN = {
    0x00020500: "Nintendo 3DS Sound",
    0x00020800: "Mii Plaza",
    0x00030600: "Mario Kart 7",
    0x00030B00: "nintendogs + cats",
    0x00051600: "Nintendo Letter Box",
    0x00053F00: "Super Mario 3D Land",
    0x0007AD00: "New Super Mario Bros. 2",
    0x0007C800: "Mario Tennis Open",
    0x0008C500: "Tomodachi Life",
    0x0012EC00: "Tetris Ultimate",
    0x00141000: "Pokémon Shuffle",
    0x001A0400: "Super Mario Maker",
}


def title_name(title, names=None):
    """A display name for a title ID, from `names` when known."""
    name = (names or {}).get(title) or KNOWN.get(title)
    return name or f"Game {title:08X}"


def short_time(created):
    """Drops the seconds from a message's creation time."""
    return created[:16]


def summary(title, values):
    """The first text value of a message's main fields, to name the message in lists."""
    for field in title["fields"]:
        v = values.get(field["key"])
        if field["kind"]["type"] == "text" and not field["advanced"] and v:
            return v
    return None
