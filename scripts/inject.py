#!/usr/bin/env python3
"""Input for tests, put into a throwaway Mutter the way a remote-desktop program would: through
org.gnome.Mutter.RemoteDesktop on the session bus this runs on. Steps are read from the command
line, one after another:
  wait=SECONDS   move=DX,DY (relative, in one go)   moves=N,DX,DY (N small ones)
  key=EVDEV_CODE (down and up)   down=CODE   up=CODE   wheel=STEPS (vertical notches)
  click (the left button, down and up)
Never run it on a real desktop's bus: it would move the real pointer."""
import sys, time
import gi
from gi.repository import Gio, GLib

bus = Gio.bus_get_sync(Gio.BusType.SESSION)
call = lambda path, iface, method, args=None, reply=None: bus.call_sync("org.gnome.Mutter.RemoteDesktop", path, iface, method, args, reply, Gio.DBusCallFlags.NONE, 5000, None)
session = call("/org/gnome/Mutter/RemoteDesktop", "org.gnome.Mutter.RemoteDesktop", "CreateSession", None, GLib.VariantType("(o)")).unpack()[0]
S = "org.gnome.Mutter.RemoteDesktop.Session"
call(session, S, "Start")
key = lambda code, down: call(session, S, "NotifyKeyboardKeycode", GLib.Variant("(ub)", (code, down)))
move = lambda dx, dy: call(session, S, "NotifyPointerMotionRelative", GLib.Variant("(dd)", (dx, dy)))
# Mutter 51.1 loses the first key of a new virtual keyboard and the first click of a new
# virtual pointer: spend a key that does nothing and a move by nothing on them.
key(240, True); key(240, False); move(0.0, 0.0)
for step in sys.argv[1:]:
    name, _, value = step.partition("=")
    if name == "wait":
        time.sleep(float(value))
    elif name == "move":
        dx, dy = (float(v) for v in value.split(","))
        move(dx, dy)
    elif name == "moves":
        n, dx, dy = value.split(",")
        for _ in range(int(n)):
            move(float(dx), float(dy)); time.sleep(0.01)
    elif name == "key":
        key(int(value), True); time.sleep(0.05); key(int(value), False)
    elif name in ("down", "up"):
        key(int(value), name == "down")
    elif name == "wheel":
        call(session, S, "NotifyPointerAxisDiscrete", GLib.Variant("(ui)", (0, int(value))))
    elif name == "click":
        for down in (True, False):
            call(session, S, "NotifyPointerButton", GLib.Variant("(ib)", (272, down))); time.sleep(0.05)
    time.sleep(0.1)
call(session, S, "Stop")
