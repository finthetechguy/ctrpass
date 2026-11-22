"""A dialog showing one message and its fields."""

import ctrpass
from gi.repository import Adw, GLib, Gtk

from .fields import number_text, title_name
from .images import field_picture, picture


def esc(text):
    return GLib.markup_escape_text(str(text))


def info_row(title, value):
    row = Adw.ActionRow(title=esc(title), subtitle=esc(value), subtitle_selectable=True)
    row.add_css_class("property")
    return row


def field_row(field, value):
    kind = field["kind"]
    if kind["type"] == "image":
        row = Adw.ActionRow(title=esc(field["label"]))
        row.add_suffix(field_picture(value, kind["size"], max(kind["size"], 32)))
        return row
    if kind["type"] == "number":
        return info_row(field["label"], number_text(kind, value))
    if kind["type"] == "text":
        return info_row(field["label"], value or "—")
    return info_row(field["label"], value)


def size_text(n):
    return f"{n / 1024:.1f} KB" if n >= 1024 else f"{n} bytes"


class MessageDialog(Adw.Dialog):
    """Shows a message, with buttons the caller adds for what can be done with it."""

    def __init__(self, window, box, message):
        names = window.title_names()
        super().__init__(title=title_name(message.title, names), content_width=460, content_height=640)
        self.message = message
        view = Adw.ToolbarView()
        view.add_top_bar(Adw.HeaderBar())
        page = Adw.PreferencesPage()
        view.set_content(page)
        self.actions = Gtk.Box(spacing=12, halign=Gtk.Align.CENTER, margin_top=12, margin_bottom=12)
        view.add_bottom_bar(self.actions)
        self.set_child(view)

        top = Adw.PreferencesGroup()
        if message.icon:
            icon = picture(message.icon, 40, 80)
            icon.set_halign(Gtk.Align.CENTER)
            top.add(icon)
            page.add(top)

        about = Adw.PreferencesGroup()
        about.add(info_row("Text", message.text or "—"))
        about.add(info_row("Game", title_name(message.title, names)))
        about.add(info_row("Box", "Inbox" if box == "inbox" else "Outbox"))
        about.add(info_row("Created", message.created[:19]))
        about.add(info_row("Size", size_text(len(message))))
        page.add(about)

        self.fields = None
        title = next((t for t in ctrpass.titles() if t["title"] == message.title), None)
        if title is None:
            return
        details = Adw.PreferencesGroup(title="Details")
        page.add(details)
        try:
            read = ctrpass.read_fields(message, window.settings.keys_text())
        except (ValueError, KeyError) as e:
            details.add(Adw.ActionRow(title="Can't show details", subtitle=esc(e)))
            return
        self.fields = read["values"]
        more = Adw.ExpanderRow(title="More")
        for field in title["fields"]:
            if field["key"] not in self.fields:
                continue
            row = field_row(field, self.fields[field["key"]])
            if field["advanced"]:
                more.add_row(row)
            else:
                details.add(row)
        details.add(more)
        if read["problems"]:
            check = Adw.PreferencesGroup(title="Problems")
            for p in read["problems"]:
                row = Adw.ActionRow(title=esc(p))
                row.add_prefix(Gtk.Image(icon_name="dialog-warning-symbolic"))
                check.add(row)
            page.add(check)

    def add_action(self, text, callback, style=None):
        button = Gtk.Button(label=text)
        button.add_css_class("pill")
        if style:
            button.add_css_class(style)
        button.connect("clicked", lambda _b: callback())
        self.actions.append(button)
        return button
