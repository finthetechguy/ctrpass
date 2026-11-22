"""The page for turning StreetPass on and seeing who was met."""

import time

from gi.repository import Adw, GLib, Gtk

from . import autostart
from .fields import title_name
from .interfaces import wireless_interfaces
from .settings import MODES

MODE_NAMES = {"alternate": "Search and wait", "lead": "Search", "wait": "Wait"}


def esc(text):
    return GLib.markup_escape_text(str(text))


def button(label, callback, style=None):
    b = Gtk.Button(label=label, valign=Gtk.Align.CENTER)
    if style:
        b.add_css_class(style)
    b.connect("clicked", lambda _b: callback())
    return b


def plural(n, word):
    return f"{n} {word}" + ("" if n == 1 else "s")


class ExchangePage(Gtk.Stack):
    def __init__(self, window):
        super().__init__(transition_type=Gtk.StackTransitionType.CROSSFADE, vexpand=True)
        self.window = window
        self.add_named(self._setup(), "setup")
        self.add_named(self._running(), "running")
        self.found_rows = {}
        self.history_rows = []
        self.refresh_interfaces()

    # Pages

    def _setup(self):
        page = Adw.PreferencesPage()
        card = Adw.PreferencesGroup(title="Wireless card", description="Wi-Fi on this card stops while StreetPass is on.")
        self.interfaces = Gtk.StringList()
        self.iface_row = Adw.ComboRow(title="Card", model=self.interfaces)
        card.add(self.iface_row)
        refresh = Gtk.Button(icon_name="view-refresh-symbolic", tooltip_text="Refresh", valign=Gtk.Align.CENTER)
        refresh.add_css_class("flat")
        refresh.connect("clicked", lambda _b: self.refresh_interfaces())
        card.set_header_suffix(refresh)
        page.add(card)

        street = Adw.PreferencesGroup(title="StreetPass")
        self.mode_row = Adw.ComboRow(title="Mode", model=Gtk.StringList.new([MODE_NAMES[m] for m in MODES]))
        self.mode_row.set_selected(MODES.index(self.window.settings.mode))
        self.mode_row.connect("notify::selected", self.mode_changed)
        street.add(self.mode_row)
        self.start_row = Adw.ActionRow(title="Start StreetPass")
        self.start_button = button("Start", lambda: self.window.start(), "suggested-action")
        self.start_row.add_suffix(self.start_button)
        street.add(self.start_row)
        page.add(street)

        self.nearby = Adw.PreferencesGroup(title="Nearby consoles", description="Consoles with StreetPass on.")
        self.scan_button = button("Scan", self.toggle_scan)
        self.scan_button.add_css_class("flat")
        self.nearby.set_header_suffix(self.scan_button)
        page.add(self.nearby)

        self.met = Adw.PreferencesGroup(title="This session")
        self.met.set_visible(False)
        page.add(self.met)
        return page

    def _running(self):
        box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=24, margin_bottom=24)
        self.status_page = Adw.StatusPage(paintable=Adw.SpinnerPaintable(widget=self), title="StreetPass is on")
        stop = Gtk.Button(label="Stop", halign=Gtk.Align.CENTER)
        stop.add_css_class("pill")
        stop.connect("clicked", lambda _b: self.window.stop())
        self.status_page.set_child(stop)
        box.append(self.status_page)
        clamp = Adw.Clamp(maximum_size=600, margin_start=12, margin_end=12)
        lists = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=24)
        options = Gtk.ListBox(selection_mode=Gtk.SelectionMode.NONE)
        options.add_css_class("boxed-list")
        self.background_row = Adw.SwitchRow(title="Run in background", subtitle="Keep StreetPass on after closing the window",
                                            active=self.window.settings.background)
        self.background_row.connect("notify::active", self.background_changed)
        options.append(self.background_row)
        lists.append(options)
        self.live = Gtk.ListBox(selection_mode=Gtk.SelectionMode.NONE)
        self.live.add_css_class("boxed-list")
        self.live.set_visible(False)
        lists.append(self.live)
        clamp.set_child(lists)
        box.append(clamp)
        return Gtk.ScrolledWindow(child=box, hscrollbar_policy=Gtk.PolicyType.NEVER)

    # Card list

    def refresh_interfaces(self):
        cards = wireless_interfaces()
        self.interfaces.splice(0, self.interfaces.get_n_items(), [n + (" (connected)" if c else "") for n, c in cards])
        self.cards = [n for n, _c in cards]
        saved = self.window.settings.interface
        if saved in self.cards:
            self.iface_row.set_selected(self.cards.index(saved))
        sensitive = bool(self.cards)
        self.start_button.set_sensitive(sensitive)
        self.scan_button.set_sensitive(sensitive)
        self.iface_row.set_subtitle("" if sensitive else "No wireless card found")

    def interface(self):
        i = self.iface_row.get_selected()
        return self.cards[i] if 0 <= i < len(self.cards) else None

    def mode(self):
        return MODES[self.mode_row.get_selected()]

    def mode_changed(self, *_):
        self.window.settings.mode = self.mode()
        self.window.settings.save()

    def background_changed(self, *_):
        settings = self.window.settings
        settings.background = self.background_row.get_active()
        if not settings.background and autostart.enabled():
            try:
                autostart.set_enabled(False)
            except OSError as e:
                self.window.toast(f"Can't change login startup: {e}")
        settings.save()

    def sync_background(self):
        self.background_row.set_active(self.window.settings.background)

    # State from the helper

    def set_state(self, state):
        scanning = state == "scanning"
        self.scan_button.set_label("Stop" if scanning else "Scan")
        self.start_button.set_sensitive(not scanning and bool(self.cards))
        self.iface_row.set_sensitive(not scanning)
        self.mode_row.set_sensitive(not scanning)
        if state == "exchanging":
            self.sync_background()
            self.status("Looking for consoles")
            self.set_visible_child_name("running")
        elif state == "idle":
            self.set_visible_child_name("setup")

    def status(self, text, description=""):
        self.status_page.set_title(text)
        self.status_page.set_description(esc(description))

    def toggle_scan(self):
        if self.scan_button.get_label() == "Stop":
            self.window.stop()
        else:
            for row in self.found_rows.values():
                self.nearby.remove(row)
            self.found_rows = {}
            self.window.scan()

    def found(self, console):
        mac = console["mac"]
        titles = console.get("titles") or []
        names = self.window.title_names()
        shown = [title_name(t, names) for t in titles[:3]]
        if len(titles) > 3:
            shown.append(f"{len(titles) - 3} more")
        parts = []
        if console.get("channel"):
            parts.append(f"Channel {console['channel']}")
        if console.get("signal") is not None:
            parts.append(f"{console['signal']} dBm")
        row = self.found_rows.get(mac)
        if row is None:
            row = Adw.ActionRow(title=esc(mac))
            self.nearby.add(row)
            self.found_rows[mac] = row
        row.set_title(esc(", ".join(shown) or "No games"))
        row.set_subtitle(esc(" · ".join([mac] + parts)))

    # Exchanges

    def add_exchange(self, peer, sent, received, problems):
        text = time.strftime("%H:%M")
        parts = [f"Sent {sent}", f"received {received}"]
        subtitle = f"{text} · " + ", ".join(parts)
        if problems:
            subtitle += f" · {plural(problems, 'message')} not kept"
        for group in (self.met, self.live):
            row = Adw.ActionRow(title=esc(peer), subtitle=esc(subtitle))
            row.add_prefix(Gtk.Image(icon_name="emblem-ok-symbolic" if sent or received else "dialog-warning-symbolic"))
            if group is self.met:
                group.add(row)
                group.set_visible(True)
            else:
                group.prepend(row)
                group.set_visible(True)
