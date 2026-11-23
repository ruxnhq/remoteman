#!/usr/bin/env python3
# A full-screen window that writes down the input reaching it, one line each, for checking
# that a client's pointer, clicks, keys, wheel and touches land where they should.
#   scripts/input-log.py [--monitor CONNECTOR]
# Positions are fractions of the window, so they compare with the test client's script.
import sys

import gi

gi.require_version("Gtk", "4.0")
gi.require_version("Gdk", "4.0")
from gi.repository import Gdk, Gtk  # noqa: E402

want = sys.argv[sys.argv.index("--monitor") + 1] if "--monitor" in sys.argv else None


def say(*words):
    print(*words, flush=True)


class Log(Gtk.ApplicationWindow):
    def __init__(self, app):
        super().__init__(application=app, title="input-log")
        area = Gtk.DrawingArea(hexpand=True, vexpand=True, focusable=True)
        self.set_child(area)
        self.area = area

        def frac(x, y):
            w, h = area.get_width() or 1, area.get_height() or 1
            return f"{x / w:.3f} {y / h:.3f}"

        motion = Gtk.EventControllerMotion()
        motion.connect("motion", lambda c, x, y: say("move", frac(x, y)))
        area.add_controller(motion)
        click = Gtk.GestureClick(button=0)
        click.connect("pressed", lambda g, n, x, y: say("press", g.get_current_button(), frac(x, y)))
        click.connect("released", lambda g, n, x, y: say("release", g.get_current_button(), frac(x, y)))
        area.add_controller(click)
        keys = Gtk.EventControllerKey()
        keys.connect("key-pressed", lambda c, val, code, state: say("key", Gdk.keyval_name(val), code) or False)
        self.add_controller(keys)
        scroll = Gtk.EventControllerScroll(flags=Gtk.EventControllerScrollFlags.BOTH_AXES)
        scroll.connect("scroll", lambda c, dx, dy: say("scroll", f"{dx:.2f} {dy:.2f}") or True)
        area.add_controller(scroll)
        legacy = Gtk.EventControllerLegacy()

        def touch(c, ev=None):
            ev = ev or c.get_current_event()
            if ev is None:
                return False
            t = ev.get_event_type()
            if t in (Gdk.EventType.TOUCH_BEGIN, Gdk.EventType.TOUCH_UPDATE, Gdk.EventType.TOUCH_END):
                ok, x, y = ev.get_position()
                # Window coordinates: close enough to the area's, which fills it.
                say("touch", t.value_nick, frac(x, y))
            return False

        legacy.connect("event", touch)
        area.add_controller(legacy)


def start(app):
    win = Log(app)
    monitor = None
    if want:
        mons = Gdk.Display.get_default().get_monitors()
        for i in range(mons.get_n_items()):
            if mons.get_item(i).get_connector() == want:
                monitor = mons.get_item(i)
    if monitor:
        win.fullscreen_on_monitor(monitor)
    else:
        win.fullscreen()
    win.present()
    win.area.grab_focus()
    say("ready", want if monitor else "(any monitor)")


app = Gtk.Application(application_id="dev.remoteman.InputLog")
app.connect("activate", start)
app.run([])
