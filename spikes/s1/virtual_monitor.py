#!/usr/bin/env python3
"""Create a Mutter virtual monitor stream on the current session bus, print its PipeWire node
id on stdout, and hold the session open until killed."""
import sys
from gi.repository import Gio, GLib

bus = Gio.bus_get_sync(Gio.BusType.SESSION)
def call(path, iface, method, args=None, reply=None):
    return bus.call_sync("org.gnome.Mutter.ScreenCast", path, iface, method, args,
                         GLib.VariantType(reply) if reply else None, Gio.DBusCallFlags.NONE, 5000, None)
session = call("/org/gnome/Mutter/ScreenCast", "org.gnome.Mutter.ScreenCast", "CreateSession",
               GLib.Variant("(a{sv})", ({},)), "(o)").unpack()[0]
stream = call(session, "org.gnome.Mutter.ScreenCast.Session", "RecordVirtual",
              GLib.Variant("(a{sv})", ({"is-platform": GLib.Variant("b", True), "cursor-mode": GLib.Variant("u", 2)},)),
              "(o)").unpack()[0]
loop = GLib.MainLoop()
def on_added(conn, sender, path, iface, signal, params):
    print(params.unpack()[0], flush=True)
bus.signal_subscribe("org.gnome.Mutter.ScreenCast", "org.gnome.Mutter.ScreenCast.Stream", "PipeWireStreamAdded",
                     stream, None, Gio.DBusSignalFlags.NONE, on_added)
call(session, "org.gnome.Mutter.ScreenCast.Session", "Start")
loop.run()
