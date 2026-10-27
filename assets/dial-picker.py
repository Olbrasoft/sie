"""Small GTK view; window selection and activation policy live in Rust."""
import json
import os
import sys

import gi
gi.require_version("Gtk", "3.0")
gi.require_version("Gdk", "3.0")
from gi.repository import Gdk, Gio, GLib, Gtk

GLib.set_prgname("sie-switcher")
window = Gtk.Window(title="Přepnout aplikaci — SIE")
window.set_default_size(660, 460)
window.set_position(Gtk.WindowPosition.CENTER)
window.set_type_hint(Gdk.WindowTypeHint.DIALOG)
window.set_icon_name("view-app-grid")
outer = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=12)
outer.set_border_width(16)
window.add(outer)
outer.pack_start(Gtk.Label(label="Aplikace ze všech ploch"), False, False, 0)
scroll = Gtk.ScrolledWindow()
scroll.set_policy(Gtk.PolicyType.NEVER, Gtk.PolicyType.AUTOMATIC)
outer.pack_start(scroll, True, True, 0)
listing = Gtk.ListBox()
listing.set_selection_mode(Gtk.SelectionMode.SINGLE)
scroll.add(listing)
outer.pack_start(Gtk.Label(label="Otáčením vyberte · stiskem potvrďte · Esc zruší"), False, False, 0)
pending = b""
seen_focus = False
confirming = False
closed = False
revision = 0

def emit(event, **values):
    print(json.dumps(dict(event=event, **values)), flush=True)

def cancel(*_args):
    global closed
    if not closed:
        closed = True
        emit("cancel")
        Gtk.main_quit()
    return True

def confirm(*_args):
    global confirming
    row = listing.get_selected_row()
    if row is not None and not confirming and not closed:
        confirming = True
        emit("confirm", index=row.get_index())
    return True

def focus_changed(*_args):
    global seen_focus
    if window.is_active():
        seen_focus = True
    elif seen_focus and not confirming:
        cancel()

def key(_widget, event):
    if event.keyval == Gdk.KEY_Escape:
        return cancel()
    if event.keyval in (Gdk.KEY_Return, Gdk.KEY_KP_Enter):
        return confirm()
    return False

def selected(_listing, row):
    if row is not None:
        emit("selected", index=row.get_index(), revision=revision)

def apply(message):
    global revision
    revision = message.get("revision", revision)
    if "windows" in message:
        apps = Gio.AppInfo.get_all()
        for item in message["windows"]:
            row = Gtk.ListBoxRow()
            box = Gtk.Box(spacing=12)
            box.set_border_width(10)
            row.add(box)
            klass = item["wm_class"]
            app = next((a for a in apps if (a.get_id() or "").removesuffix(".desktop").lower() == klass.lower()), None)
            icon = Gtk.Image.new_from_gicon(app.get_icon(), Gtk.IconSize.DIALOG) if app and app.get_icon() else Gtk.Image.new_from_icon_name("application-x-executable", Gtk.IconSize.DIALOG)
            box.pack_start(icon, False, False, 0)
            labels = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=3)
            name = app.get_display_name() if app else klass
            heading = Gtk.Label(label=f"{name} · Plocha {item['workspace'] + 1}", xalign=0)
            title = Gtk.Label(label=item["title"] or name, xalign=0)
            title.set_ellipsize(3)
            labels.pack_start(heading, False, False, 0)
            labels.pack_start(title, False, False, 0)
            box.pack_start(labels, True, True, 0)
            listing.add(row)
        window.show_all()
        window.present()
    if "selected" in message:
        row = listing.get_row_at_index(message["selected"])
        if row is not None:
            listing.select_row(row)
            row.grab_focus()
    if message.get("confirm"):
        confirm()

def read_commands(_fd, _condition):
    global pending
    data = os.read(0, 65536)
    if not data:
        cancel()
        return False
    pending += data
    if len(pending) > 262144:
        cancel()
        return False
    while b"\n" in pending:
        line, pending = pending.split(b"\n", 1)
        try:
            apply(json.loads(line))
        except (ValueError, KeyError, TypeError):
            cancel()
            return False
    return True

window.connect("delete-event", cancel)
window.connect("notify::is-active", focus_changed)
window.connect("key-press-event", key)
listing.connect("row-selected", selected)
listing.connect("row-activated", confirm)
GLib.io_add_watch(0, GLib.IO_IN | GLib.IO_HUP, read_commands)
Gtk.main()
