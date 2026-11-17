#!/usr/bin/env python3
"""Moving content for tests: a fullscreen GTK 4 window, drawn on the GPU (render nodes, no
Cairo), that redraws on every frame the compositor asks for and prints frames drawn per second. It also
prints the input it receives (keys, text, clicks, pointer motion, scrolling, touches), so a test
can check that a client's input arrived."""
import math, time
import gi
gi.require_version("Gtk", "4.0")
from gi.repository import Gtk, Gdk, Graphene, Pango

class Anim(Gtk.Widget):
    def __init__(self):
        super().__init__()
        self.n = 0; self.last = time.monotonic(); self.count = 0
        self.set_hexpand(True); self.set_vexpand(True)
        self.layout = self.create_pango_layout("")
        self.layout.set_font_description(Pango.FontDescription("monospace 22"))
        self.add_tick_callback(self.tick)
        self.set_focusable(True)
        self.moves = 0
        keys = Gtk.EventControllerKey()
        keys.connect("key-pressed", lambda c, val, code, state: print(f"input: key {Gdk.keyval_name(val)} unicode {Gdk.keyval_to_unicode(val):#x}", flush=True))
        self.add_controller(keys)
        motion = Gtk.EventControllerMotion()
        motion.connect("motion", self.on_motion)
        self.add_controller(motion)
        click = Gtk.GestureClick()
        click.connect("pressed", lambda g, n, x, y: print(f"input: {'touch' if g.get_current_sequence() else 'click'} at {x:.0f},{y:.0f}", flush=True))
        self.add_controller(click)
        scroll = Gtk.EventControllerScroll(flags=Gtk.EventControllerScrollFlags.BOTH_AXES)
        scroll.connect("scroll", lambda c, dx, dy: print(f"input: scroll {dx:.1f},{dy:.1f}", flush=True))
        self.add_controller(scroll)
    def on_motion(self, controller, x, y):
        self.moves += 1
        if self.moves % 10 == 1:
            print(f"input: pointer at {x:.0f},{y:.0f} ({self.moves} moves)", flush=True)
    def tick(self, widget, clock):
        self.n += 1; self.count += 1
        now = time.monotonic()
        if now - self.last >= 1:
            print(f"client drew {self.count} frames", flush=True)
            self.count = 0; self.last = now
        self.queue_draw()
        return True
    def do_snapshot(self, snap):
        w, h = self.get_width(), self.get_height()
        t = self.n / 120.0
        bg = Gdk.RGBA(); bg.parse("#14171f")
        snap.append_color(bg, Graphene.Rect().init(0, 0, w, h))
        for i in range(60):
            a = t * (0.5 + i * 0.07) + i
            x, y = w / 2 + math.cos(a) * w * 0.42, h / 2 + math.sin(a * 1.3) * h * 0.42
            c = Gdk.RGBA(); c.red = 0.5 + 0.5 * math.sin(i); c.green = 0.5 + 0.5 * math.cos(i * 2); c.blue = 0.8; c.alpha = 1
            snap.append_color(c, Graphene.Rect().init(x - 40, y - 40, 80, 80))
        white = Gdk.RGBA(); white.parse("white")
        self.layout.set_text("\n".join(f"frame {self.n + r} the quick brown fox jumps over the lazy dog 0123456789" for r in range(24)))
        snap.save(); snap.translate(Graphene.Point().init(30 + (self.n * 2) % 300, 30))
        snap.append_layout(self.layout, white); snap.restore()

def on_activate(app):
    win = Gtk.ApplicationWindow(application=app)
    anim = Anim()
    win.set_child(anim); win.fullscreen(); win.present(); anim.grab_focus()
app = Gtk.Application(application_id="dev.remoteman.spike.animate")
app.connect("activate", on_activate)
app.run(None)
