"""Showing image fields and loading pictures into them."""

import ctrpass
from gi.repository import Gdk, GdkPixbuf, GLib, Gtk


def texture(rgb, size):
    """Returns a texture for square RGB pixels."""
    pixbuf = GdkPixbuf.Pixbuf.new_from_bytes(GLib.Bytes.new(rgb), GdkPixbuf.Colorspace.RGB, False, 8, size, size, size * 3)
    return Gdk.Texture.new_for_pixbuf(pixbuf)


def picture(rgb, size, shown=None):
    """Returns a picture widget for square RGB pixels, drawn `shown` pixels wide."""
    shown = shown or size
    p = Gtk.Picture(paintable=texture(rgb, size), can_shrink=False, content_fit=Gtk.ContentFit.FILL)
    p.set_size_request(shown, shown)
    p.set_valign(Gtk.Align.CENTER)
    return p


def field_picture(hex_value, size, shown=None):
    return picture(ctrpass.icon_rgb(size, bytes.fromhex(hex_value)), size, shown)


def load_file(path, size):
    """Reads a picture file into an image field value of `size` pixels, as hex."""
    pixbuf = GdkPixbuf.Pixbuf.new_from_file(path)
    if pixbuf.get_has_alpha():
        flat = GdkPixbuf.Pixbuf.new(GdkPixbuf.Colorspace.RGB, False, 8, pixbuf.get_width(), pixbuf.get_height())
        flat.fill(0xFFFFFFFF)
        pixbuf.composite(flat, 0, 0, pixbuf.get_width(), pixbuf.get_height(), 0, 0, 1, 1,
                         GdkPixbuf.InterpType.NEAREST, 255)
        pixbuf = flat
    w, h, stride = pixbuf.get_width(), pixbuf.get_height(), pixbuf.get_rowstride()
    data = pixbuf.get_pixels()
    rgb = b"".join(data[y * stride:y * stride + w * 3] for y in range(h))
    return ctrpass.icon_from_rgb(size, w, h, rgb).hex()


def picture_filters():
    from gi.repository import Gio

    f = Gtk.FileFilter(name="Pictures")
    for t in ("image/png", "image/jpeg", "image/bmp", "image/gif"):
        f.add_mime_type(t)
    filters = Gio.ListStore.new(Gtk.FileFilter)
    filters.append(f)
    return filters
