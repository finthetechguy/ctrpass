"""The page listing the games in the store and whether each receives messages."""

import ctrpass
from gi.repository import Adw, GLib, Gtk

from .fields import game_name, title_name


def esc(text):
    return GLib.markup_escape_text(str(text))


def counts(row):
    parts = []
    if row["inbox"]:
        parts.append(f"{row['inbox']} received")
    if row["outbox"]:
        parts.append(f"{row['outbox']} to send")
    return parts


class TitlesPage(Gtk.Stack):
    def __init__(self, window):
        super().__init__(vexpand=True)
        self.window = window
        empty = Adw.StatusPage(
            title="No games yet",
            description="Add a game or import messages from a console.",
            icon_name="applications-games-symbolic",
        )
        add = Gtk.Button(label="Add Game…", halign=Gtk.Align.CENTER)
        add.add_css_class("pill")
        add.add_css_class("suggested-action")
        add.connect("clicked", lambda _b: self.add_dialog())
        empty.set_child(add)
        self.add_named(empty, "empty")

        page = Adw.PreferencesPage()
        self.group = Adw.PreferencesGroup(title="Games", description="Only games set to receive get new messages.")
        plus = Gtk.Button(icon_name="list-add-symbolic", tooltip_text="Add Game", valign=Gtk.Align.CENTER)
        plus.add_css_class("flat")
        plus.connect("clicked", lambda _b: self.add_dialog())
        self.group.set_header_suffix(plus)
        page.add(self.group)
        self.add_named(page, "list")
        self.rows = []

    def refresh(self):
        for row in self.rows:
            self.group.remove(row)
        self.rows = []
        store = self.window.store
        titles = store.titles() if store else []
        names = self.window.title_names()
        for t in sorted(titles, key=lambda t: title_name(t["title"], names).casefold()):
            row = Adw.ActionRow(title=esc(title_name(t["title"], names)))
            row.set_subtitle(esc(" · ".join([f"{t['title']:08X}"] + counts(t))))
            if not t["key"]:
                row.add_suffix(Gtk.Image(icon_name="dialog-warning-symbolic", tooltip_text="No key for this game"))
            switch = Gtk.Switch(active=t["receive"], valign=Gtk.Align.CENTER, tooltip_text="Receive messages")
            switch.connect("notify::active", lambda s, _p, tid=t["title"]: self.set_receive(tid, s.get_active()))
            row.add_suffix(switch)
            remove = Gtk.Button(icon_name="user-trash-symbolic", tooltip_text="Remove", valign=Gtk.Align.CENTER)
            remove.add_css_class("flat")
            remove.connect("clicked", lambda _b, tid=t["title"]: self.confirm_remove(tid))
            row.add_suffix(remove)
            self.group.add(row)
            self.rows.append(row)
        self.set_visible_child_name("list" if titles else "empty")

    def set_receive(self, title, on):
        try:
            self.window.store.set_receive(title, on)
        except (ValueError, RuntimeError) as e:
            self.window.toast(str(e))

    def addable(self):
        """Returns (id, name, key) for each game the key file signs for that is not in the store yet."""
        keys = self.window.settings.keys_text()
        if keys is None or self.window.store is None:
            return []
        try:
            ids = ctrpass.check_keys(keys)["titles"]
        except ValueError:
            return []
        held = {t["title"] for t in self.window.store.titles()}
        names = self.window.title_names()
        return [(t, names.get(t) or game_name(t), ctrpass.title_key_of(keys, t)) for t in ids if t not in held]

    def add_dialog(self):
        if self.window.store is None:
            self.window.toast("The store can't be opened")
            return
        choices = self.addable()
        if not choices:
            if self.window.settings.keys_text() is None:
                self.window.toast("Choose your key file first", ("Preferences", "app.preferences"))
            else:
                self.window.toast("Every game in the key file is already added")
            return
        dialog = Adw.Dialog(title="Add Game", content_width=380, content_height=480)
        view = Adw.ToolbarView()
        view.add_top_bar(Adw.HeaderBar())
        page = Adw.PreferencesPage()
        group = Adw.PreferencesGroup(description="Games from the key file.")
        names = self.window.title_names()
        for tid, name, key in sorted(choices, key=lambda c: title_name(c[0], names).casefold()):
            row = Adw.ActionRow(title=esc(title_name(tid, names)), subtitle=f"{tid:08X}", activatable=True)
            row.add_suffix(Gtk.Image(icon_name="list-add-symbolic"))
            row.connect("activated", lambda _r, c=(tid, name, key): (dialog.close(), self.add(*c)))
            group.add(row)
        page.add(group)
        view.set_content(page)
        dialog.set_child(view)
        dialog.present(self.window)

    def add(self, tid, name, key):
        try:
            self.window.store.add_title(tid, name, key)
            self.window.store.set_receive(tid, True)
        except (ValueError, RuntimeError) as e:
            self.window.toast(str(e))
            return
        self.window.refresh()

    def confirm_remove(self, tid):
        name = title_name(tid, self.window.title_names())
        alert = Adw.AlertDialog(heading=f"Remove {name}?", body="Messages for this game are removed too.")
        alert.add_response("cancel", "Cancel")
        alert.add_response("remove", "Remove")
        alert.set_response_appearance("remove", Adw.ResponseAppearance.DESTRUCTIVE)

        def chosen(_a, response):
            if response != "remove":
                return
            try:
                self.window.store.remove_title(tid)
            except (ValueError, RuntimeError) as e:
                self.window.toast(str(e))
                return
            self.window.refresh()

        alert.connect("response", chosen)
        alert.present(self.window)
