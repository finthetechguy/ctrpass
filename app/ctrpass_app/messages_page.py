"""The page listing the inbox and outbox."""

import ctrpass
from gi.repository import Adw, Gio, GLib, Gtk

from .editor import EditorDialog
from .fields import short_time, title_name
from .images import picture
from .message_dialog import MessageDialog

EMPTY = {
    "inbox": ("Inbox is empty", "Messages from other consoles appear here."),
    "outbox": ("Outbox is empty", "Messages to send appear here."),
}


def esc(text):
    return GLib.markup_escape_text(str(text))


def message_filters():
    f = Gtk.FileFilter(name="StreetPass messages")
    f.add_pattern("_*")
    f.add_pattern("*.bin")
    f.add_pattern("*.cec")
    everything = Gtk.FileFilter(name="All files")
    everything.add_pattern("*")
    filters = Gio.ListStore.new(Gtk.FileFilter)
    filters.append(everything)
    filters.append(f)
    return filters


class MessagesPage(Gtk.Box):
    def __init__(self, window):
        super().__init__(orientation=Gtk.Orientation.VERTICAL)
        self.window = window
        bar = Gtk.Box(spacing=6, margin_top=12, margin_start=12, margin_end=12, halign=Gtk.Align.CENTER)
        self.boxes = Adw.ToggleGroup()
        self.boxes.add(Adw.Toggle(name="inbox", label="Inbox"))
        self.boxes.add(Adw.Toggle(name="outbox", label="Outbox"))
        self.boxes.set_active_name("inbox")
        self.boxes.connect("notify::active-name", lambda *_: self.refresh())
        bar.append(self.boxes)
        menu = Gio.Menu()
        menu.append("New Message…", "win.new-message")
        menu.append("Import File…", "win.import-file")
        menu.append("Import Folder…", "win.import-folder")
        menu.append("Copy Inbox to Emulator", "win.to-emulator")
        bar.append(Gtk.MenuButton(icon_name="view-more-symbolic", menu_model=menu, tooltip_text="More"))
        self.append(bar)

        self.stack = Gtk.Stack(vexpand=True)
        self.empty = Adw.StatusPage(icon_name="ctrpass-messages-symbolic")
        self.stack.add_named(self.empty, "empty")
        self.page = Adw.PreferencesPage()
        self.stack.add_named(self.page, "list")
        self.append(self.stack)
        self.groups = []

    def box(self):
        return self.boxes.get_active_name() or "inbox"

    def show_box(self, name):
        self.boxes.set_active_name(name)

    def refresh(self):
        for g in self.groups:
            self.page.remove(g)
        self.groups = []
        box = self.box()
        store = self.window.store
        messages = store.messages(None, box) if store else []
        names = self.window.title_names()
        by_title = {}
        for _box, m in messages:
            by_title.setdefault(m.title, []).append(m)
        for title in sorted(by_title, key=lambda t: title_name(t, names).casefold()):
            group = Adw.PreferencesGroup(title=esc(title_name(title, names)))
            for m in sorted(by_title[title], key=lambda m: m.created, reverse=True):
                name = self.window.summary(m)
                text = m.text or "Message"
                subtitle = " · ".join(p for p in (text if name else None, short_time(m.created)) if p)
                row = Adw.ActionRow(title=esc(name or text), subtitle=esc(subtitle), activatable=True)
                if m.icon:
                    row.add_prefix(picture(m.icon, 40, 32))
                else:
                    row.add_prefix(Gtk.Image(icon_name="ctrpass-messages-symbolic", pixel_size=32))
                row.add_suffix(Gtk.Image(icon_name="go-next-symbolic"))
                row.connect("activated", lambda _r, msg=m: self.show(box, msg))
                group.add(row)
            self.page.add(group)
            self.groups.append(group)
        title, description = EMPTY[box]
        self.empty.set_title(title)
        self.empty.set_description(description)
        self.stack.set_visible_child_name("list" if messages else "empty")

    def show(self, box, message):
        dialog = MessageDialog(self.window, box, message)
        title = next((t for t in ctrpass.titles() if t["title"] == message.title), None)
        if title and dialog.fields is not None:
            dialog.add_action("Edit", lambda: (dialog.close(), self.edit(title, message, dialog.fields)), "suggested-action")
        if box == "inbox":
            dialog.add_action("Add to Outbox", lambda: (dialog.close(), self.window.add_to_outbox(message)))
        dialog.add_action("Save As…", lambda: self.save_as(message))
        dialog.add_action("Delete", lambda: self.confirm_delete(dialog, box, message), "destructive-action")
        dialog.present(self.window)

    def edit(self, title, message, values):
        if not self.window.can_sign(title["title"]):
            return
        EditorDialog(self.window, title, message, values).present(self.window)

    def new_message(self):
        titles = ctrpass.titles()
        if len(titles) == 1:
            self.create(titles[0])
            return
        dialog = Adw.Dialog(title="New Message", content_width=360, content_height=360)
        view = Adw.ToolbarView()
        view.add_top_bar(Adw.HeaderBar())
        page = Adw.PreferencesPage()
        group = Adw.PreferencesGroup()
        for t in titles:
            row = Adw.ActionRow(title=esc(t["name"]), activatable=True)
            row.connect("activated", lambda _r, t=t: (dialog.close(), self.create(t)))
            group.add(row)
        page.add(group)
        view.set_content(page)
        dialog.set_child(view)
        dialog.present(self.window)

    def create(self, title):
        if not self.window.can_sign(title["title"]):
            return
        keys = self.window.settings.keys_text()
        try:
            message = ctrpass.build_message(title["title"], {}, keys, seed=self.window.store.seed)
            values = ctrpass.read_fields(message, keys)["values"]
        except (ValueError, KeyError) as e:
            self.window.toast(str(e))
            return
        EditorDialog(self.window, title, message, values).present(self.window)

    def save_as(self, message):
        name = f"{message.title:08X}-{message.message_id:016x}.bin"
        chooser = Gtk.FileDialog(title="Save Message", initial_name=name)

        def done(d, result):
            try:
                target = d.save_finish(result)
            except GLib.Error:
                return
            try:
                with open(target.get_path(), "wb") as f:
                    f.write(bytes(message))
                self.window.toast("Message saved")
            except OSError as e:
                self.window.toast(str(e))

        chooser.save(self.window, None, done)

    def confirm_delete(self, dialog, box, message):
        alert = Adw.AlertDialog(heading="Delete Message?", body="The message is removed from the store.")
        alert.add_response("cancel", "Cancel")
        alert.add_response("delete", "Delete")
        alert.set_response_appearance("delete", Adw.ResponseAppearance.DESTRUCTIVE)

        def chosen(_a, response):
            if response != "delete":
                return
            try:
                self.window.store.remove_message(message.title, box, message.message_id)
            except (ValueError, RuntimeError) as e:
                self.window.toast(str(e))
                return
            dialog.close()
            self.window.refresh()

        alert.connect("response", chosen)
        alert.present(self.window)
