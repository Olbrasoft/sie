#!/usr/bin/env python3
"""Dedicated manual dictation target; records only text inserted into this window."""
import json
from pathlib import Path
import sys

import gi
gi.require_version('Gtk', '4.0')
from gi.repository import Gtk, Gdk


class Probe(Gtk.Application):
    def __init__(self, result):
        super().__init__(application_id='cz.olbrasoft.SieDictationProbe')
        self.result = result
        self.enters = 0

    def do_activate(self):
        window = Gtk.ApplicationWindow(application=self, title='SIE Dictation Probe')
        window.set_default_size(850, 300)
        view = Gtk.TextView()
        view.set_wrap_mode(Gtk.WrapMode.WORD_CHAR)
        view.set_top_margin(20)
        view.set_left_margin(20)
        window.set_child(view)
        view.get_buffer().connect('changed', self.changed)
        keys = Gtk.EventControllerKey()
        keys.set_propagation_phase(Gtk.PropagationPhase.CAPTURE)
        keys.connect('key-pressed', self.key_pressed, view.get_buffer())
        view.add_controller(keys)
        self.changed(view.get_buffer())
        window.present()
        view.grab_focus()

    def changed(self, buffer):
        start, end = buffer.get_bounds()
        self.result.write_text(json.dumps({'text': buffer.get_text(start, end, True), 'enters': self.enters}, ensure_ascii=False))

    def key_pressed(self, controller, keyval, keycode, state, buffer):
        if keyval in (Gdk.KEY_Return, Gdk.KEY_KP_Enter):
            self.enters += 1
            self.changed(buffer)
        return False


if __name__ == '__main__':
    app = Probe(Path(sys.argv[1]))
    app.run([sys.argv[0]])
