"""A dialog for making or changing a message from its title's fields."""

import ctrpass
from gi.repository import Adw, GLib, Gtk

from .fields import changed, check_text, number_text, parse_bytes, parse_number
from .images import field_picture, load_file, picture_filters

# Numbers up to this fit a spin button; larger ones are typed.
SPIN_MAX = 1 << 31


def esc(text):
    return GLib.markup_escape_text(str(text))


class FieldRow:
    """One field's row, reading back a checked value."""

    def __init__(self, dialog, field, value):
        self.field = field
        self.kind = field["kind"]
        self.value = value
        kind = self.kind
        label = esc(field["label"])
        help_text = field.get("help") or None
        t = kind["type"]
        if t == "text":
            self.row = Adw.EntryRow(title=label, text=value or "", tooltip_text=help_text)
            self.row.connect("changed", lambda _r: self.check())
        elif t == "number" and not kind["hex"] and kind["max"] < SPIN_MAX and not kind.get("exclude"):
            self.row = Adw.SpinRow.new_with_range(kind["min"], kind["max"], 1)
            self.row.set_title(label)
            if help_text:
                self.row.set_subtitle(esc(help_text))
            self.row.set_value(value or kind["min"])
        elif t == "number":
            self.row = Adw.EntryRow(title=label, text=number_text(kind, value), tooltip_text=help_text)
            self.row.connect("changed", lambda _r: self.check())
        elif t == "image":
            self.row = Adw.ActionRow(title=label, subtitle=esc(help_text or ""))
            self.preview = Gtk.Box(valign=Gtk.Align.CENTER)
            self.row.add_suffix(self.preview)
            choose = Gtk.Button(label="Choose…", valign=Gtk.Align.CENTER)
            choose.connect("clicked", lambda _b: dialog.choose_picture(self))
            self.row.add_suffix(choose)
            self.show_picture()
        else:
            self.row = Adw.EntryRow(title=label, text=value or "", tooltip_text=help_text)
            load = Gtk.Button(icon_name="document-open-symbolic", tooltip_text="Load from File", valign=Gtk.Align.CENTER)
            load.add_css_class("flat")
            load.connect("clicked", lambda _b: dialog.choose_bytes(self))
            self.row.add_suffix(load)
            self.row.connect("changed", lambda _r: self.check())

    def show_picture(self):
        child = self.preview.get_first_child()
        if child:
            self.preview.remove(child)
        if self.value:
            size = self.kind["size"]
            self.preview.append(field_picture(self.value, size, max(size, 32)))

    def read(self):
        """Returns the row's value, or raises ValueError."""
        t = self.kind["type"]
        if t == "text":
            return check_text(self.kind, self.row.get_text())
        if t == "number":
            if isinstance(self.row, Adw.SpinRow):
                return int(self.row.get_value())
            return parse_number(self.kind, self.row.get_text())
        if t == "image":
            return self.value
        return parse_bytes(self.kind, self.row.get_text())

    def check(self):
        try:
            self.read()
        except ValueError as e:
            self.row.add_css_class("error")
            self.row.set_tooltip_text(str(e))
            return False
        self.row.remove_css_class("error")
        self.row.set_tooltip_text(self.field.get("help") or None)
        return True


class EditorDialog(Adw.Dialog):
    """Edits `template`'s fields and adds the result to the outbox."""

    def __init__(self, window, title, template, values):
        super().__init__(title=f"Edit {title['name']} Message", content_width=480, content_height=680)
        self.window = window
        self.title = title
        self.template = template
        self.values = values
        self.toasts = Adw.ToastOverlay()
        view = Adw.ToolbarView()
        header = Adw.HeaderBar(show_end_title_buttons=False, show_start_title_buttons=False)
        cancel = Gtk.Button(label="Cancel")
        cancel.connect("clicked", lambda _b: self.close())
        header.pack_start(cancel)
        save = Gtk.Button(label="Add to Outbox")
        save.add_css_class("suggested-action")
        save.connect("clicked", lambda _b: self.save())
        header.pack_end(save)
        view.add_top_bar(header)
        page = Adw.PreferencesPage()
        view.set_content(page)
        self.toasts.set_child(view)
        self.set_child(self.toasts)

        message = Adw.PreferencesGroup(title="Message")
        self.text_row = Adw.EntryRow(title="Text", text=template.text or "")
        message.add(self.text_row)
        page.add(message)

        main = Adw.PreferencesGroup(title="Details")
        more_group = Adw.PreferencesGroup()
        more = Adw.ExpanderRow(title="More")
        more_group.add(more)
        self.rows = []
        for field in title["fields"]:
            r = FieldRow(self, field, values.get(field["key"]))
            self.rows.append(r)
            if field["advanced"]:
                more.add_row(r.row)
            else:
                main.add(r.row)
        page.add(main)
        page.add(more_group)

    def toast(self, text):
        self.toasts.add_toast(Adw.Toast(title=esc(text), timeout=4))

    def choose_picture(self, row):
        def done(d, result):
            try:
                path = d.open_finish(result).get_path()
            except GLib.Error:
                return
            try:
                row.value = load_file(path, row.kind["size"])
            except (GLib.Error, ValueError) as e:
                self.toast(f"Can't open the picture: {e}")
                return
            row.show_picture()

        Gtk.FileDialog(title="Choose Picture", filters=picture_filters()).open(self.window, None, done)

    def choose_bytes(self, row):
        def done(d, result):
            try:
                path = d.open_finish(result).get_path()
            except GLib.Error:
                return
            try:
                with open(path, "rb") as f:
                    data = f.read(row.kind["len"] + 1)
            except OSError as e:
                self.toast(str(e))
                return
            if len(data) != row.kind["len"]:
                self.toast(f"The file must be {row.kind['len']} bytes")
                return
            row.row.set_text(data.hex())

        Gtk.FileDialog(title=f"Choose {row.field['label']}").open(self.window, None, done)

    def collect(self):
        values = {}
        for r in self.rows:
            if r.value is None and r.kind["type"] == "image":
                continue
            try:
                values[r.field["key"]] = r.read()
            except ValueError as e:
                r.check()
                raise ValueError(f"{r.field['label']}: {e}") from None
        return values

    def save(self):
        try:
            values = changed(self.values, self.collect())
        except ValueError as e:
            self.toast(str(e))
            return
        text = self.text_row.get_text()
        try:
            msg = ctrpass.build_message(
                self.title["title"],
                values,
                self.window.settings.keys_text() or "",
                template=self.template,
                text=None if text == (self.template.text or "") else text,
            )
        except (ValueError, KeyError) as e:
            self.toast(str(e))
            return
        if self.window.add_to_outbox(msg):
            self.close()
