import json
import sys
from pathlib import Path

import gi

gi.require_version("Gtk", "4.0")
gi.require_version("Adw", "1")
gi.require_version("GdkPixbuf", "2.0")
import ctrpass  # noqa: E402
from gi.repository import Adw, Gdk, Gio, GLib, Gtk  # noqa: E402

from . import APP_ID, autostart, emulators  # noqa: E402
from .exchange_page import ExchangePage, plural  # noqa: E402
from .fields import summary  # noqa: E402
from .helper import Helper, file_pass  # noqa: E402
from .messages_page import MessagesPage, message_filters  # noqa: E402
from .preferences import PreferencesDialog  # noqa: E402
from .settings import Settings  # noqa: E402
from .titles_page import TitlesPage  # noqa: E402

# pkexec exits with these when the password prompt is dismissed or refused.
DENIED = {126, 127}


class Window(Adw.ApplicationWindow):
    def __init__(self, app):
        super().__init__(application=app, title="ctrpass", default_width=720, default_height=680)
        self.app = app
        self.done = False
        self.settings = Settings.load()
        self.helper = Helper(lambda e: GLib.idle_add(self.on_event, e))
        self.helper_interface = None
        self.state = "idle"
        self.store = None
        self.store_error = None
        self.peer = None
        self.summaries = {}
        self.quitting = False
        self.clear_asked = False

        self.toasts = Adw.ToastOverlay()
        stack = Adw.ViewStack()
        self.exchange_page = ExchangePage(self)
        self.messages_page = MessagesPage(self)
        self.titles_page = TitlesPage(self)
        stack.add_titled_with_icon(self.exchange_page, "streetpass", "StreetPass", "network-wireless-symbolic")
        stack.add_titled_with_icon(self.messages_page, "messages", "Messages", "ctrpass-messages-symbolic")
        stack.add_titled_with_icon(self.titles_page, "games", "Games", "applications-games-symbolic")
        self.stack = stack

        header = Adw.HeaderBar(title_widget=Adw.ViewSwitcher(stack=stack, policy=Adw.ViewSwitcherPolicy.WIDE))
        menu = Gio.Menu()
        menu.append("Preferences", "app.preferences")
        menu.append("About ctrpass", "app.about")
        header.pack_end(Gtk.MenuButton(icon_name="open-menu-symbolic", menu_model=menu, primary=True))

        view = Adw.ToolbarView()
        view.add_top_bar(header)
        view.set_content(stack)
        bar = Adw.ViewSwitcherBar(stack=stack)
        view.add_bottom_bar(bar)
        self.toasts.set_child(view)
        self.set_content(self.toasts)

        narrow = Adw.Breakpoint.new(Adw.BreakpointCondition.parse("max-width: 500sp"))
        narrow.add_setter(bar, "reveal", True)
        self.add_breakpoint(narrow)

        for name, callback in (
            ("new-message", lambda *_: self.messages_page.new_message()),
            ("import-file", lambda *_: self.import_dialog(folder=False)),
            ("import-folder", lambda *_: self.import_dialog(folder=True)),
            ("to-emulator", lambda *_: self.to_emulator()),
        ):
            action = Gio.SimpleAction.new(name, None)
            action.connect("activate", callback)
            self.add_action(action)

        self.open_store()
        self.connect("close-request", self.on_close)

    def toast(self, text, action=None):
        t = Adw.Toast(title=GLib.markup_escape_text(str(text)), timeout=4)
        if action:
            t.set_button_label(action[0])
            t.set_action_name(action[1])
        self.toasts.add_toast(t)

    # Store

    def open_store(self):
        if self.store:
            self.store.close()
            self.store = None
        try:
            self.store = ctrpass.Store(self.settings.store)
            self.store_error = None
        except (ValueError, OSError) as e:
            self.store_error = str(e)
            self.toast(f"Can't open the messages folder: {e}")
        self.refresh()

    def refresh(self):
        self.messages_page.refresh()
        self.titles_page.refresh()

    def title_names(self):
        names = {t["title"]: t["name"] for t in ctrpass.titles()}
        if self.store:
            for t in self.store.titles():
                if t["name"]:
                    names[t["title"]] = t["name"]
        return names

    def summary(self, message):
        """A short name for a message, such as the Mii name, or None."""
        key = (message.title, message.message_id, len(message))
        if key not in self.summaries:
            title = next((t for t in ctrpass.titles() if t["title"] == message.title), None)
            keys = self.settings.keys_text()
            value = None
            if title and keys:
                try:
                    value = summary(title, ctrpass.read_fields(message, keys)["values"])
                except (ValueError, KeyError):
                    value = None
            self.summaries[key] = value
        return self.summaries[key]

    def keys_changed(self):
        self.summaries = {}
        if self.helper.running:
            keys = self.settings.keys_text()
            if keys is not None:
                self.helper.send_keys(keys)
        self.refresh()

    def can_sign(self, title):
        keys = self.settings.keys_text()
        if keys is None:
            self.toast("Choose your key file first", ("Preferences", "app.preferences"))
            return False
        try:
            if ctrpass.title_key_of(keys, title) is None:
                self.toast("The key file has no key for this game")
                return False
        except ValueError as e:
            self.toast(str(e))
            return False
        return True

    def add_to_outbox(self, message):
        if self.store is None:
            self.toast("The messages folder can't be opened")
            return False
        held = self.store.messages(message.title, "outbox")

        def add(replace):
            try:
                if replace:
                    self.store.replace_outbox(message)
                else:
                    self.store.add_message(message, "outbox")
            except (ValueError, RuntimeError) as e:
                self.toast(str(e))
                return
            self.toast("Added to the outbox")
            self.refresh()
            self.messages_page.show_box("outbox")

        if not held:
            add(False)
            return True
        alert = Adw.AlertDialog(heading="Replace the Outbox Message?",
                                body="This game already has a message to send.")
        alert.add_response("cancel", "Cancel")
        alert.add_response("both", "Keep Both")
        alert.add_response("replace", "Replace")
        alert.set_response_appearance("replace", Adw.ResponseAppearance.SUGGESTED)
        alert.set_default_response("replace")
        alert.connect("response", lambda _a, r: r != "cancel" and add(r == "replace"))
        alert.present(self)
        return True

    def import_dialog(self, folder):
        if self.store is None:
            self.toast("The messages folder can't be opened")
            return

        def done(d, result):
            try:
                f = d.select_folder_finish(result) if folder else d.open_finish(result)
            except GLib.Error:
                return
            try:
                got = self.store.import_from(f.get_path())
            except (ValueError, RuntimeError, OSError) as e:
                self.toast(f"Can't import: {e}")
                return
            self.refresh()
            n = got["added"]
            if n:
                self.toast(f"Imported {plural(n, 'message')}")
            elif got["duplicates"]:
                self.toast("Those messages are already here")
            elif got["titles"]:
                self.toast(f"Added {plural(len(got['titles']), 'game')}")
            else:
                self.toast("No messages found")

        if folder:
            Gtk.FileDialog(title="Import Folder").select_folder(self, None, done)
        else:
            Gtk.FileDialog(title="Import File", filters=message_filters()).open(self, None, done)

    def to_emulator(self):
        if self.store is None:
            self.toast("The messages folder can't be opened")
            return
        found = emulators.find()
        if not found:
            self.toast("No emulator found")
            return
        running = emulators.running()
        if running:
            self.toast(f"Close {running} first")
            return
        if len(found) == 1:
            self.copy_to(*found[0])
            return
        alert = Adw.AlertDialog(heading="Copy Inbox to Emulator", body="Choose an emulator.")
        alert.add_response("cancel", "Cancel")
        for i, (name, _folder) in enumerate(found):
            alert.add_response(str(i), name)
        alert.connect("response", lambda _a, r: r != "cancel" and self.copy_to(*found[int(r)]))
        alert.present(self)

    def copy_to(self, name, folder):
        try:
            out = self.store.export_to(str(folder), "inbox")
        except (ValueError, RuntimeError) as e:
            self.toast(str(e))
            return
        results = out["messages"]
        copied = sum(1 for m in results if m["delivered"])
        missing = sum(1 for m in results if not m["delivered"] and "no inbox" in m["result"])
        if not results:
            self.toast("The inbox is empty")
        elif missing:
            self.toast(f"Copied {copied} to {name}. Start the other games once with StreetPass on.")
        elif copied:
            self.toast(f"Copied {plural(copied, 'message')} to {name}")
        else:
            self.toast(f"Nothing new to copy to {name}")

    # Helper

    def ensure_helper(self):
        interface = self.exchange_page.interface()
        if not interface:
            self.toast("No wireless card found")
            return False
        if self.helper.running and self.helper_interface != interface:
            self.helper.close()
        if not self.helper.running:
            try:
                self.helper.start(interface)
            except OSError as e:
                self.toast(f"Can't start StreetPass: {e}")
                return False
            self.helper_interface = interface
            self.settings.interface = interface
            self.settings.save()
            keys = self.settings.keys_text()
            if keys is not None:
                self.helper.send_keys(keys)
        return True

    def start(self):
        if self.settings.keys_text() is None:
            self.toast("Choose your key file first", ("Preferences", "app.preferences"))
            return
        if self.store is None:
            self.toast("The messages folder can't be opened")
            return
        if not self.store.titles():
            self.toast("Add a game first")
            self.stack.set_visible_child_name("games")
            return
        if not self.ensure_helper():
            return
        try:
            self.helper.load_store(self.store.snapshot())
            self.helper.exchange(role=self.settings.mode, radio=self.settings.radio,
                                 rotate_every=self.settings.clear_hours * 3600)
        except (RuntimeError, OSError, ValueError) as e:
            self.toast(str(e))

    def clear_passed(self):
        """Lets passed consoles pass again and changes the MAC, through the helper while it exchanges."""
        if self.store is None:
            self.toast("The messages folder can't be opened")
            return
        try:
            if self.helper.running and self.state == "exchanging":
                self.clear_asked = True
                self.helper.rotate()
            else:
                self.store.rotate()
                self.toast("Passed consoles cleared")
        except (RuntimeError, OSError, ValueError) as e:
            self.toast(str(e))

    def scan(self):
        if self.ensure_helper():
            try:
                self.helper.scan()
            except (RuntimeError, OSError) as e:
                self.toast(str(e))

    def stop(self):
        if self.helper.running:
            self.helper.stop()

    def running(self):
        return self.helper.running and self.state != "idle"

    def can_start_quietly(self):
        """Whether StreetPass can start without anything to fix first."""
        return (self.settings.keys_text() is not None and self.store is not None
                and bool(self.store.titles()) and bool(self.exchange_page.interface()))

    def on_close(self, _window):
        if self.settings.background and self.running() and not self.quitting:
            self.set_visible(False)
            self.app.hide_window()
            return True
        self.clean_up()
        return False

    def clean_up(self):
        if self.done:
            return
        self.done = True
        self.helper.close()
        if self.store:
            self.store.close()
            self.store = None
        self.app.release_window()

    def finish(self):
        """Closes the window, or ends a hidden one that close requests never reach."""
        if self.done:
            return
        if self.get_visible():
            self.close()
        else:
            self.clean_up()
            self.destroy()

    def stop_and_quit(self):
        """Stops StreetPass and closes once the helper is idle."""
        self.quitting = True
        if self.running():
            self.helper.stop()
        else:
            self.finish()

    def on_event(self, e):
        if self.done:
            return GLib.SOURCE_REMOVE
        kind = e["event"]
        page = self.exchange_page
        if kind == "state":
            self.state = e["state"]
            page.set_state(self.state)
            if self.state == "idle" and self.quitting:
                self.finish()
        elif kind == "radio":
            if e.get("mixed"):
                page.status("Looking for consoles", "This card uses channel 1 only.")
        elif kind == "engaged":
            self.peer = e.get("peer")
            page.status("Exchanging", self.peer or "")
        elif kind in ("abandoned", "finished"):
            page.status("Looking for consoles")
        elif kind == "pass":
            self.on_pass(e)
        elif kind == "rotated":
            if self.store and e.get("peers") is not None:
                self.store.set_peers(json.dumps(e["peers"]))
            if self.clear_asked:
                self.clear_asked = False
                self.toast("Passed consoles cleared")
        elif kind == "found":
            page.found(e["console"])
        elif kind == "error":
            self.toast(e.get("message", "Something went wrong"))
        elif kind == "exit":
            self.state = "idle"
            page.set_state("idle")
            code = e.get("code")
            if code in DENIED:
                self.toast("ctrpass was not allowed to use the wireless card")
            elif code:
                lines = [line for line in e.get("stderr", "").splitlines() if line.strip()]
                self.toast(lines[-1] if lines else f"StreetPass stopped (code {code})")
            if not self.get_visible():
                if code:
                    self.app.notify("StreetPass stopped", "Open ctrpass to start again.")
                self.quitting = True
                self.finish()
        return GLib.SOURCE_REMOVE

    def on_pass(self, e):
        try:
            filed = file_pass(self.store, e) if self.store else {"filed": [], "refused": []}
        except (ValueError, RuntimeError) as err:
            self.toast(f"Can't save the new messages: {err}")
            filed = {"filed": [], "refused": []}
        received = len(filed["filed"])
        problems = len(e.get("not_filed", [])) + len(filed["refused"])
        self.exchange_page.add_exchange(e.get("peer") or "Console", len(e.get("sent", [])), received, problems)
        if received:
            self.toast(f"Received {plural(received, 'message')}")
            if not self.get_visible():
                self.app.notify(f"Received {plural(received, 'message')}", "Open ctrpass to see them.")
        self.refresh()


class App(Adw.Application):
    def __init__(self):
        super().__init__(application_id=APP_ID)
        self.background = False
        self.held = False
        self.add_main_option(autostart.FLAG[2:], 0, GLib.OptionFlags.NONE, GLib.OptionArg.NONE,
                             "Start StreetPass without showing the window", None)
        for name, callback in (("preferences", self.preferences), ("about", self.about), ("quit", self.quit_app),
                               ("show", self.show_window), ("stop", self.stop_streetpass)):
            action = Gio.SimpleAction.new(name, None)
            action.connect("activate", callback)
            self.add_action(action)
        self.set_accels_for_action("app.preferences", ["<Control>comma"])
        self.set_accels_for_action("app.quit", ["<Control>q"])

    def do_startup(self):
        Adw.Application.do_startup(self)
        display = Gdk.Display.get_default()
        if display:
            Gtk.IconTheme.get_for_display(display).add_search_path(str(Path(__file__).with_name("icons")))

    def do_handle_local_options(self, options):
        if options.contains(autostart.FLAG[2:]):
            self.background = True
        return -1

    def window(self):
        return next(iter(self.get_windows()), None)

    def do_activate(self):
        window = self.window()
        if window is None and self.background:
            self.background = False
            window = Window(self)
            if window.can_start_quietly():
                window.settings.background = True
                window.start()
                self.hide_window()
                return
        (window or Window(self)).present()
        self.release_window()

    def show_window(self, *_):
        window = self.window()
        if window:
            window.present()
            self.release_window()

    def stop_streetpass(self, *_):
        window = self.window()
        if window:
            window.stop_and_quit()

    def hide_window(self):
        """Keeps running with the window hidden, and says so."""
        if not self.held:
            self.hold()
            self.held = True
        n = Gio.Notification.new("StreetPass is on")
        n.set_body("ctrpass keeps running in the background.")
        n.set_default_action("app.show")
        n.add_button("Stop", "app.stop")
        self.send_notification("running", n)

    def release_window(self):
        self.withdraw_notification("running")
        if self.held:
            self.held = False
            self.release()

    def notify(self, title, body):
        n = Gio.Notification.new(title)
        n.set_body(body)
        n.set_default_action("app.show")
        self.send_notification(None, n)

    def preferences(self, *_):
        window = self.props.active_window
        if window:
            PreferencesDialog(window).present(window)

    def about(self, *_):
        Adw.AboutDialog(
            application_name="ctrpass",
            application_icon="network-wireless-symbolic",
            version=ctrpass.version,
            license_type=Gtk.License.MIT_X11,
            comments="StreetPass with Nintendo 3DS consoles.",
        ).present(self.props.active_window)

    def quit_app(self, *_):
        window = self.window()
        if window:
            window.quitting = True
            window.close()
        self.quit()


def main():
    return App().run(sys.argv)


if __name__ == "__main__":
    sys.exit(main())
