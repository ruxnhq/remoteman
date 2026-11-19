//! For looking at the window without a person at it: `--ui STEPS` runs a few steps one after
//! another and leaves. It is how pictures of each screen are taken in a headless compositor.
//!
//!     --ui pair,shot=/tmp/pair.png
//!     --ui connect=127.0.0.1:7811,wait=2,menu=display,shot=/tmp/menu.png
//!
//! Steps, separated by commas:
//! - `wait=SECONDS`, `dark`, `light`, `size=WxH`
//! - `pair[=ADDRESS]`, `pair-go=ADDRESS` (open the dialog and press Pair), `connect=ADDRESS`,
//!   `disconnect`, `back`
//! - `prefs`, `computer=ADDRESS`, `addresses=ADDRESS`
//! - `menu=keys|display|picture|main|none`, `bar` (bring a hidden bar out), `figures`,
//!   `fullscreen`, `send=ctrl-alt-delete` (an entry of the Send Keys menu)
//! - `click=BUTTON LABEL`, `row=ROW TITLE`, `entry=ROW TITLE:TEXT` (type and apply)
//! - `shot=FILE.png` (the window's contents, with any open menu), `page=FILE.png` (the whole
//!   scrolling page of the open dialog)
//! - `keys?` and `size?` print whether keys go to the other computer, and the window's size
//! - `share` (the clipboard), `copy=TEXT` (as if copied in this window), `clipboard?` (print
//!   what this clipboard holds), `sendfile=PATH`, `offers?` (print which lines of the sharing
//!   menu can be chosen), `unlock=PASSWORD` (type it into a locked computer's page), `reach?=ADDRESS` (print what is known of a computer), `second` (add or remove the second display), `second?` (print how it stands), `pointer?` (print whether the pointer is captured and how far it has
//!   been sent)
//! - things that can't happen yet, to see how they are laid out: `pair-code=NAME` (the code
//!   dialog, with a made-up code), `asleep=ADDRESS` (the asleep page), `state=asleep:ADDRESS`
//!   or `state=online:ADDRESS` (shown in the list only with `Features::presence`)
//!
//! While `--ui` is given a session stays up although the stand-in host's frames can't be
//! decoded, and nothing is animated.

use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::{gdk, glib, graphene};

use crate::status::{self, Problem, Reach};
use crate::window::App;
use crate::{computers, pair, prefs};

/// Every widget under `root`, depth first.
fn each(root: &gtk::Widget, visit: &mut dyn FnMut(&gtk::Widget)) {
    visit(root);
    let mut child = root.first_child();
    while let Some(c) = child {
        each(&c, visit);
        child = c.next_sibling();
    }
}

fn find<T: IsA<gtk::Widget>>(root: &gtk::Widget, wanted: impl Fn(&T) -> bool) -> Option<T> {
    let mut found = None;
    each(root, &mut |w| {
        if found.is_none() && w.is_mapped() {
            if let Some(t) = w.downcast_ref::<T>() {
                if wanted(t) {
                    found = Some(t.clone());
                }
            }
        }
    });
    found
}

/// The words on a button, whether it has a label or a child that is one.
fn button_text(button: &gtk::Button) -> String {
    if let Some(label) = button.label() {
        return label.to_string();
    }
    let mut text = String::new();
    each(button.upcast_ref(), &mut |w| {
        if let Some(l) = w.downcast_ref::<gtk::Label>() {
            if text.is_empty() {
                text = l.text().to_string();
            }
        }
    });
    text
}

/// A widget's contents as a picture.
fn render(widget: &gtk::Widget, background: Option<&gdk::RGBA>) -> Option<gdk::Texture> {
    let (w, h) = (widget.width() as f32, widget.height() as f32);
    let snapshot = gtk::Snapshot::new();
    let area = graphene::Rect::new(0.0, 0.0, w, h);
    if let Some(colour) = background {
        snapshot.append_color(colour, &area);
    }
    gtk::WidgetPaintable::new(Some(widget)).snapshot(&snapshot, w as f64, h as f64);
    let node = snapshot.to_node()?;
    Some(widget.native()?.renderer()?.render_texture(node, Some(&area)))
}

/// Save the window's contents, with any open menu drawn where it is.
fn shoot(app: &App, path: &str) {
    // The window itself, so that its own background is in the picture.
    let content: gtk::Widget = app.window.clone().upcast();
    let Some(base) = render(&content, None) else {
        println!("couldn't draw the window");
        return;
    };
    let (w, h) = (content.width() as f32, content.height() as f32);
    let snapshot = gtk::Snapshot::new();
    snapshot.append_texture(&base, &graphene::Rect::new(0.0, 0.0, w, h));
    // Menus are surfaces of their own: put each where the compositor has it.
    let (wx, wy) = app.window.surface_transform();
    let mut popovers = Vec::new();
    each(&content, &mut |widget| {
        if let Some(p) = widget.downcast_ref::<gtk::Popover>() {
            if p.is_mapped() {
                popovers.push(p.clone());
            }
        }
    });
    for p in popovers {
        let (Some(texture), Some(popup)) = (render(p.upcast_ref(), None), p.surface().and_downcast::<gdk::Popup>()) else { continue };
        let (px, py) = p.surface_transform();
        let x = popup.position_x() as f64 + px - wx;
        let y = popup.position_y() as f64 + py - wy;
        snapshot.append_texture(&texture, &graphene::Rect::new(x as f32, y as f32, p.width() as f32, p.height() as f32));
    }
    let saved = snapshot.to_node().and_then(|node| Some(app.window.renderer()?.render_texture(node, Some(&graphene::Rect::new(0.0, 0.0, w, h))))).map(|t| t.save_to_png(path));
    match saved {
        Some(Ok(())) => println!("saved {path}"),
        _ => println!("couldn't save {path}"),
    }
}

/// Save the whole scrolling page of the open dialog, however tall.
fn shoot_page(app: &App, path: &str) {
    let Some(dialog) = app.window.visible_dialog() else {
        println!("no dialog is open");
        return;
    };
    let Some(scrolled) = find::<gtk::ScrolledWindow>(dialog.upcast_ref(), |_| true) else { return };
    let Some(inner) = scrolled.child().and_then(|v| v.first_child()) else { return };
    let dark = adw::StyleManager::default().is_dark();
    let background = if dark { gdk::RGBA::new(0.212, 0.212, 0.227, 1.0) } else { gdk::RGBA::new(0.98, 0.98, 0.984, 1.0) };
    match render(&inner, Some(&background)).map(|t| t.save_to_png(path)) {
        Some(Ok(())) => println!("saved {path}"),
        _ => println!("couldn't save {path}"),
    }
}

async fn pause(seconds: f64) {
    glib::timeout_future(Duration::from_millis((seconds * 1000.0) as u64)).await;
}

pub fn run(app: &Rc<App>, script: String) {
    let app = app.clone();
    glib::spawn_future_local(async move {
        pause(0.8).await;
        for step in script.split(',') {
            let (name, value) = step.split_once('=').unwrap_or((step, ""));
            let root: gtk::Widget = app.window.clone().upcast();
            match name {
                "wait" => pause(value.parse().unwrap_or(1.0)).await,
                "dark" => adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceDark),
                "light" => adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceLight),
                "pair" => pair::open(&app, (!value.is_empty()).then_some(value), false),
                "pair-go" => pair::open(&app, Some(value), true),
                "pair-code" => pair::confirm_code(&app, value, "418205", 107, 120, |_| {}),
                "connect" => app.connect(app.target_of(value), false),
                "asleep" => app.show_page(&status::problem(&app, &app.target_of(value), Problem::Asleep)),
                // What the list shows once the core can tell: `state=asleep:ADDRESS`.
                "state" => {
                    let (state, address) = value.split_once(':').unwrap_or((value, ""));
                    let reach = if state == "asleep" { Reach::Asleep } else { Reach::Online };
                    app.reach.borrow_mut().insert(address.to_owned(), reach);
                    computers::refresh(&app);
                }
                "prefs" => prefs::preferences(&app),
                "computer" => prefs::computer(&app, value, false),
                "addresses" => prefs::computer(&app, value, true),
                "menu" => app.view.open_menu(value),
                "bar" => {
                    app.bar_out.set(true);
                    app.place_bar();
                }
                "figures" => app.set_figures(true),
                "fullscreen" => app.toggle_fullscreen(),
                "disconnect" => app.stop(None),
                "back" => {
                    app.nav.pop();
                }
                "click" => match find::<gtk::Button>(&root, |b| button_text(b) == value || b.tooltip_text().is_some_and(|t| t == value)) {
                    Some(button) => button.emit_clicked(),
                    None => println!("no button {value}"),
                },
                "row" => match find::<adw::PreferencesRow>(&root, |r| r.title() == value) {
                    Some(row) => {
                        row.activate();
                    }
                    None => println!("no row {value}"),
                },
                // `entry=TITLE:TEXT`: type into a row and press its tick.
                "entry" => {
                    let (title, text) = value.split_once(':').unwrap_or((value, ""));
                    match find::<adw::EntryRow>(&root, |r| r.title() == title) {
                        Some(row) => {
                            row.set_text(text);
                            row.emit_by_name::<()>("apply", &[]);
                        }
                        None => println!("no entry {title}"),
                    }
                }
                "size" => {
                    if let Some((w, h)) = value.split_once('x') {
                        app.window.set_default_size(w.parse().unwrap_or(900), h.parse().unwrap_or(680));
                    }
                }
                "size?" => println!("~ window {} x {}, default {:?}", app.window.width(), app.window.height(), app.window.default_size()),
                "keys?" => println!("~ keys go to the other computer: {}", app.forwarding()),
                // Sharing: the clipboard on, a copy made here, what this clipboard holds, a file.
                "share" => app.set_clipboard(true),
                "pointer?" => {
                    let held = app.capture.borrow_mut().as_mut().map(|c| c.take().1);
                    let (dx, dy) = app.moved.get();
                    println!("~ pointer captured: {}, held now: {}, sent {dx:.0},{dy:.0}", held.is_some(), held == Some(true));
                }
                "reach?" => {
                    let reach = match app.reach_of(value) {
                        Some(Reach::Online) => "online",
                        Some(Reach::Asleep) => "asleep",
                        Some(Reach::Unreachable) => "not reachable",
                        None => "not known",
                    };
                    let cards = app.store.borrow().find(value).map_or(0, |c| c.cards.split(',').filter(|c| !c.is_empty()).count());
                    println!("~ {value} is {reach}; {cards} network cards remembered");
                }
                // A locked computer's password, typed into its page and sent.
                "unlock" => {
                    let mut found = None;
                    each(app.window.upcast_ref(), &mut |w| {
                        if let Some(entry) = w.downcast_ref::<gtk::PasswordEntry>() {
                            found = Some(entry.clone());
                        }
                    });
                    match found {
                        Some(entry) => {
                            entry.set_text(value);
                            entry.emit_by_name::<()>("activate", &[]);
                        }
                        None => println!("no password field"),
                    }
                }
                // The second display: add or remove it, and say how it stands.
                "second" => app.toggle_second(),
                "second?" => match app.second.borrow().as_ref() {
                    Some(second) => println!("~ second display: a window, {} pictures shown, {} x {}", second.video.shown_total.get(), second.video.size.get().0, second.video.size.get().1),
                    None => println!("~ second display: none"),
                },
                "offers?" => println!("~ the sharing menu: clipboard {}, files {}, shared {}", app.view.offers().0, app.view.offers().1, app.view.clipboard_shown()),
                "copy" => app.window.clipboard().set_text(value),
                "clipboard?" => app.window.clipboard().read_text_async(gtk::gio::Cancellable::NONE, |text| {
                    println!("~ this clipboard holds: {}", text.ok().flatten().map(|t| t.to_string()).unwrap_or_default());
                }),
                "sendfile" => app.send_file(std::path::Path::new(value)),
                // An entry of the Send Keys menu: `send=ctrl-alt-delete`.
                "send" => {
                    let _ = gtk::prelude::WidgetExt::activate_action(&app.window, "win.send-keys", Some(&value.to_variant()));
                }
                "shot" => shoot(&app, value),
                "page" => shoot_page(&app, value),
                other => println!("unknown step {other}"),
            }
            pause(0.5).await;
        }
        app.stop(None);
        app.app.quit();
    });
}
