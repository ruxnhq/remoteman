#!/usr/bin/env python3
"""An ordinary app for the clipboard test (examples/clipboard.rs): a GTK 4 window that copies
and pastes when told to on its standard input, one command per line, and answers each with one
line on its standard output ("ok ..." or "err ..."). Bytes go through files, never the command
line. A Wayland client may only set or read the clipboard while it has the keyboard focus, so
it says "ready" once its window is the active one.

  copy-text PATH     copy the UTF-8 text in PATH, the way an app's Copy does
  copy MIME PATH     offer the bytes in PATH as MIME, untouched
  copy-big MIME N    offer N bytes of filler as MIME
  paste-text PATH    paste as text, the way an app's Paste does, into PATH
  paste MIME PATH    paste the bytes offered as MIME, untouched, into PATH
  formats            list the types the clipboard offers
  quit
"""
import sys, threading
import gi
gi.require_version("Gtk", "4.0")
from gi.repository import Gtk, Gdk, Gio, GLib

def say(line):
    print(line, flush=True)

class Helper(Gtk.Application):
    def __init__(self):
        super().__init__(application_id=None, flags=Gio.ApplicationFlags.NON_UNIQUE)
        self.ready = False

    def do_activate(self):
        self.window = Gtk.ApplicationWindow(application=self, title="clipboard helper")
        self.window.set_default_size(320, 200)
        self.window.set_child(Gtk.Label(label="clipboard helper"))
        self.window.connect("notify::is-active", self.on_active)
        self.window.present()
        self.clipboard = self.window.get_display().get_clipboard()
        self.on_active()

    def on_active(self, *_):
        if self.window.is_active() and not self.ready:
            self.ready = True
            say("ready")
            threading.Thread(target=self.read_commands, daemon=True).start()

    def read_commands(self):
        for line in sys.stdin:
            GLib.idle_add(self.command, line.strip())
        GLib.idle_add(self.quit)

    def command(self, line):
        try:
            # The path comes last and may have spaces in it.
            verb = line.split(" ")[0]
            args = line.split(" ", 2 if verb in ("copy", "copy-big", "paste") else 1)[1:]
            if verb == "copy-text":
                with open(args[0], encoding="utf-8") as f:
                    self.clipboard.set(f.read())
                say("ok copy-text")
            elif verb == "copy":
                with open(args[1], "rb") as f:
                    self.offer(args[0], f.read())
            elif verb == "copy-big":
                self.offer(args[0], b"x" * int(args[1]))
            elif verb == "paste-text":
                self.clipboard.read_text_async(None, self.pasted_text, args[0])
            elif verb == "paste":
                self.clipboard.read_async([args[0]], GLib.PRIORITY_DEFAULT, None, self.paste_opened, args[1])
            elif verb == "formats":
                say("ok formats " + " ".join(self.clipboard.get_formats().get_mime_types() or []))
            elif verb == "quit":
                self.quit()
            elif verb:
                say(f"err unknown command {verb}")
        except Exception as e:
            say(f"err {e}".replace("\n", " "))
        return False

    def offer(self, mime, data):
        self.clipboard.set_content(Gdk.ContentProvider.new_for_bytes(mime, GLib.Bytes.new(data)))
        say(f"ok copy {len(data)}")

    def pasted_text(self, clipboard, result, path):
        try:
            text = clipboard.read_text_finish(result)
            if text is None:
                raise ValueError("no text")
            with open(path, "w", encoding="utf-8") as f:
                f.write(text)
            say(f"ok paste-text {len(text.encode())}")
        except Exception as e:
            say(f"err {e}".replace("\n", " "))

    def paste_opened(self, clipboard, result, path):
        try:
            stream, mime = clipboard.read_finish(result)
            out = Gio.MemoryOutputStream.new_resizable()
            flags = Gio.OutputStreamSpliceFlags.CLOSE_SOURCE | Gio.OutputStreamSpliceFlags.CLOSE_TARGET
            out.splice_async(stream, flags, GLib.PRIORITY_DEFAULT, None, self.paste_read, (path, mime))
        except Exception as e:
            say(f"err {e}".replace("\n", " "))

    def paste_read(self, out, result, where):
        path, mime = where
        try:
            out.splice_finish(result)
            data = out.steal_as_bytes().get_data() or b""
            with open(path, "wb") as f:
                f.write(data)
            say(f"ok paste {len(data)} {mime}")
        except Exception as e:
            say(f"err {e}".replace("\n", " "))

Helper().run(None)
