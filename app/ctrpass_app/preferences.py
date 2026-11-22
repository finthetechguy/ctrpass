"""The preferences dialog: key file, store folder and how the card is used."""

import ctrpass
from gi.repository import Adw, GLib, Gtk

from . import autostart
from .settings import RADIOS

RADIO_NAMES = {"auto": "Automatic", "active": "Standard", "mixed": "Compatibility"}
RADIO_HELP = {
    "auto": "Picks the best mode for the card",
    "active": "Uses every channel",
    "mixed": "Uses channel 1 only, for more cards",
}


class PreferencesDialog(Adw.PreferencesDialog):
    def __init__(self, window):
        super().__init__(title="Preferences")
        self.window = window
        self.settings = window.settings
        page = Adw.PreferencesPage()
        self.add(page)

        keys = Adw.PreferencesGroup(title="Keys", description="A key file made from your own console.")
        self.keys_row = Adw.ActionRow(title="Key file")
        self.keys_row.add_suffix(self._choose_button(self.choose_keys))
        keys.add(self.keys_row)
        page.add(keys)

        store = Adw.PreferencesGroup(title="Messages")
        self.store_row = Adw.ActionRow(title="Folder")
        self.store_row.add_suffix(self._choose_button(self.choose_store))
        store.add(self.store_row)
        page.add(store)

        card = Adw.PreferencesGroup(title="Wireless card")
        self.radio_row = Adw.ComboRow(title="Mode", model=Gtk.StringList.new([RADIO_NAMES[r] for r in RADIOS]))
        self.radio_row.set_selected(RADIOS.index(self.settings.radio))
        self.radio_row.connect("notify::selected", self.radio_changed)
        card.add(self.radio_row)
        page.add(card)

        background = Adw.PreferencesGroup(title="Background")
        self.background_row = Adw.SwitchRow(title="Run in background", subtitle="Keep StreetPass on after closing the window",
                                            active=self.settings.background)
        self.background_row.connect("notify::active", self.background_changed)
        background.add(self.background_row)
        self.login_row = Adw.SwitchRow(title="Start at login", subtitle="Turn StreetPass on when you log in",
                                       active=autostart.enabled())
        self.login_row.connect("notify::active", self.login_changed)
        background.add(self.login_row)
        page.add(background)
        self.update()

    @staticmethod
    def _choose_button(callback):
        button = Gtk.Button(label="Choose…", valign=Gtk.Align.CENTER)
        button.connect("clicked", lambda _b: callback())
        return button

    def update(self):
        text = self.settings.keys_text()
        if not self.settings.keys_path:
            status = "Not set"
        elif text is None:
            status = "Can't be read"
        else:
            try:
                info = ctrpass.check_keys(text)
                n = len(info["titles"])
                status = f"Keys for {n} game" + ("" if n == 1 else "s")
                if not info["record"]:
                    status += ", Miis can't be read"
            except ValueError as e:
                status = str(e)
        self.keys_row.set_subtitle(GLib.markup_escape_text(f"{self.settings.keys_path or '—'}\n{status}"))
        self.store_row.set_subtitle(GLib.markup_escape_text(self.settings.store))
        self.radio_row.set_subtitle(RADIO_HELP[self.settings.radio])

    def choose_keys(self):
        def done(d, result):
            try:
                self.settings.keys_path = d.open_finish(result).get_path()
            except GLib.Error:
                return
            self.saved()
            self.window.keys_changed()

        Gtk.FileDialog(title="Choose Key File").open(self.window, None, done)

    def choose_store(self):
        def done(d, result):
            try:
                self.settings.store = d.select_folder_finish(result).get_path()
            except GLib.Error:
                return
            self.saved()
            self.window.open_store()

        Gtk.FileDialog(title="Choose Messages Folder").select_folder(self.window, None, done)

    def radio_changed(self, *_):
        self.settings.radio = RADIOS[self.radio_row.get_selected()]
        self.saved()

    def background_changed(self, *_):
        self.settings.background = self.background_row.get_active()
        if not self.settings.background and self.login_row.get_active():
            self.login_row.set_active(False)
        self.saved()

    def login_changed(self, *_):
        on = self.login_row.get_active()
        try:
            autostart.set_enabled(on)
        except OSError as e:
            self.window.toast(f"Can't change login startup: {e}")
            return
        if on and not self.background_row.get_active():
            self.background_row.set_active(True)

    def saved(self):
        self.settings.save()
        self.update()
