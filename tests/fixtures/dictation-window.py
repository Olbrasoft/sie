#!/usr/bin/env python3
"""Dedicated manual dictation target; records only text inserted into this window."""
import json
from pathlib import Path
import sys

import gi
gi.require_version('Gtk', '4.0')
from gi.repository import Gtk


class Probe(Gtk.Application):
    def __init__(self, result):
        super().__init__(application_id='cz.olbrasoft.SieDictationProbe')
        self.result = result

    def do_activate(self):
        window = Gtk.ApplicationWindow(application=self, title='SIE Dictation Probe')
        window.set_default_size(850, 300)
        view = Gtk.TextView()
        view.set_wrap_mode(Gtk.WrapMode.WORD_CHAR)
        view.set_top_margin(20)
        view.set_left_margin(20)
        window.set_child(view)
        view.get_buffer().connect('changed', self.changed)
        self.changed(view.get_buffer())
        window.present()
        view.grab_focus()

    def changed(self, buffer):
        start, end = buffer.get_bounds()
        self.result.write_text(json.dumps({'text': buffer.get_text(start, end, True)}, ensure_ascii=False))


if __name__ == '__main__':
    app = Probe(Path(sys.argv[1]))
    app.run([sys.argv[0]])
