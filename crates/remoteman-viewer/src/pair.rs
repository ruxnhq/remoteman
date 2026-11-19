//! Pairing a computer with this device.
//!
//! What works today: the other computer is put in pairing mode, and this device connects to its
//! address; each then keeps the other's key. The design's way (pick the computer from those
//! found nearby, compare a six-digit code) needs the core to find computers and to produce the
//! code; the code dialog is here for then, behind `Features::pairing_code`.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;

use crate::computers;
use crate::features::FEATURES;
use crate::status::{self, Problem};
use crate::store::{host_part, key_words, with_port};
use crate::stream::Session;
use crate::window::{App, Target};

/// The code both sides show while pairing, and the seconds left to compare it. The core has no
/// such thing yet, so there is never one.
fn code_of(_session: &Session) -> Option<(String, u32)> {
    None
}

/// This device's name, as the other computer is told it.
fn device_name() -> String {
    std::fs::read_to_string("/etc/hostname").map(|s| s.trim().to_owned()).unwrap_or_else(|_| "Linux".into())
}

/// The dialog behind "+": the computer's address, and Pair. `start`: begin at once.
pub fn open(app: &Rc<App>, address: Option<&str>, start: bool) {
    let dialog = adw::Dialog::builder().title("Pair a Computer").content_width(440).build();
    let header = adw::HeaderBar::new();
    let column = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(18).margin_top(6).margin_bottom(24).margin_start(24).margin_end(24).build();
    column.append(&gtk::Label::builder().label("Put the other computer in pairing mode, then enter its address here.").wrap(true).justify(gtk::Justification::Center).build());

    let entry = adw::EntryRow::builder().title("Address").text(address.map(|a| a.strip_suffix(&format!(":{}", remoteman_core::PORT)).unwrap_or(a)).unwrap_or_default()).build();
    let field = gtk::Box::new(gtk::Orientation::Vertical, 8);
    let list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).css_classes(["boxed-list"]).build();
    list.append(&entry);
    field.append(&list);
    field.append(&gtk::Label::builder().label(format!("A name or an IP address. Add a colon and the port if it is not {}.", remoteman_core::PORT)).wrap(true).xalign(0.0).margin_start(2).css_classes(["rm-hint-below", "dim-label"]).build());
    column.append(&field);

    // Under the address: nothing, the wait, or what went wrong.
    let spinner = adw::Spinner::builder().width_request(16).height_request(16).build();
    let waiting = gtk::Label::builder().wrap(true).build();
    let wait = gtk::Box::builder().spacing(8).halign(gtk::Align::Center).visible(false).build();
    wait.append(&spinner);
    wait.append(&waiting);
    column.append(&wait);
    let problem = gtk::Label::builder().wrap(true).justify(gtk::Justification::Center).visible(false).css_classes(["error"]).build();
    column.append(&problem);

    let pair = gtk::Button::builder().label("Pair").halign(gtk::Align::Center).css_classes(["pill", "suggested-action"]).build();
    column.append(&pair);

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&column));
    dialog.set_child(Some(&toolbar));
    dialog.set_default_widget(Some(&pair));

    // The attempt under way, to give up if the dialog is closed.
    let attempt = Rc::new(Cell::new(None::<u32>));
    let go = {
        let (app, dialog, entry, pair, wait, waiting, problem, attempt) = (app.clone(), dialog.clone(), entry.clone(), pair.clone(), wait.clone(), waiting.clone(), problem.clone(), attempt.clone());
        move || {
            let typed = entry.text().trim().to_owned();
            if typed.is_empty() {
                problem.set_text("Enter the computer's address first.");
                problem.set_visible(true);
                return;
            }
            let address = with_port(&typed);
            let target = app.target_of(&address);
            problem.set_visible(false);
            waiting.set_text(&format!("Waiting for {}…", host_part(&address)));
            wait.set_visible(true);
            entry.set_sensitive(false);
            pair.set_sensitive(false);
            let (dialog, entry, pair, wait, problem, slot, to) = (dialog.clone(), entry.clone(), pair.clone(), wait.clone(), problem.clone(), attempt.clone(), target.clone());
            // If the computer asks its owner about this device, the code to compare is shown
            // while this waits.
            gtk::glib::timeout_add_local(std::time::Duration::from_millis(250), {
                let (app, waiting, slot, host) = (app.clone(), waiting.clone(), attempt.clone(), host_part(&address).to_owned());
                let mut said = false;
                move || {
                    if slot.get().is_none() {
                        return gtk::glib::ControlFlow::Break;
                    }
                    if let (false, Some((code, seconds))) = (said, app.pairing.lock().unwrap().clone()) {
                        said = true;
                        waiting.set_text(&format!("{host} is asking its owner about this device.\nCheck that it shows this code, and confirm there within {seconds} seconds:\n\n{code}"));
                        if app.args.ui.is_some() {
                            println!("~ pairing code {code}");
                        }
                    }
                    gtk::glib::ControlFlow::Continue
                }
            });
            let id = app.attempt(&target, true, move |app, result| {
                slot.set(None);
                match result {
                    Ok(session) => {
                        app.store.borrow_mut().paired(&to.address, &session.client.host_name, &session.client.host_os);
                        computers::refresh(app);
                        dialog.force_close();
                        let to = app.target_of(&to.address);
                        match code_of(&session).filter(|_| FEATURES.pairing_code) {
                            Some((code, seconds)) => confirm_code(app, &to.name, &code, seconds, seconds, {
                                let (app, to) = (app.clone(), to.clone());
                                let session = RefCell::new(Some(session));
                                move |same| match (same, session.borrow_mut().take()) {
                                    (true, Some(session)) => paired(&app, session, &to),
                                    (false, Some(session)) => {
                                        session.client.close();
                                        app.store.borrow_mut().forget(&to.address);
                                        computers::refresh(&app);
                                    }
                                    _ => {}
                                }
                            }),
                            None => paired(app, session, &to),
                        }
                    }
                    Err(e) => {
                        let host = host_part(&to.address);
                        problem.set_text(&match status::classify(&e) {
                            Problem::Unreachable(detail) => format!("{host} did not answer. {detail}."),
                            Problem::NotPaired => format!("{host} is not in pairing mode. Start pairing there and try again."),
                            Problem::Changed => format!("{host} is not the computer this device was paired with at that address. Forget that one first."),
                            Problem::Refused(said) => format!("{host} said: {said}"),
                            _ => e,
                        });
                        problem.set_visible(true);
                        wait.set_visible(false);
                        entry.set_sensitive(true);
                        pair.set_sensitive(true);
                    }
                }
            });
            attempt.set(Some(id));
        }
    };
    pair.connect_clicked({
        let go = go.clone();
        move |_| go()
    });
    entry.connect_entry_activated({
        let go = go.clone();
        move |_| go()
    });
    dialog.connect_closed({
        let app = app.clone();
        move |_| {
            if let Some(id) = attempt.take() {
                app.give_up(id);
            }
        }
    });
    dialog.present(Some(&app.window));
    if start {
        go();
    }
}

/// "Pair with …?": both sides show a code, and the user says whether they are the same.
/// `left` of `total` seconds remain before pairing closes.
pub fn confirm_code(app: &Rc<App>, name: &str, code: &str, left: u32, total: u32, done: impl Fn(bool) + 'static) {
    // Two groups of three, as it is read out.
    let (a, b) = code.split_at(code.len() / 2);
    let digits = gtk::Box::builder().spacing(22).halign(gtk::Align::Center).margin_top(2).margin_bottom(6).build();
    digits.append(&gtk::Label::builder().label(a).css_classes(["rm-code"]).build());
    digits.append(&gtk::Label::builder().label(b).css_classes(["rm-code"]).build());
    let bar = gtk::ProgressBar::builder().fraction(left as f64 / total.max(1) as f64).build();
    let time = gtk::Label::builder().label(format!("Pairing closes in {}:{:02}", left / 60, left % 60)).css_classes(["caption", "dim-label", "numeric"]).build();
    let extra = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(8).build();
    extra.append(&digits);
    extra.append(&bar);
    extra.append(&time);

    let dialog = adw::AlertDialog::builder().heading(format!("Pair with {name}?")).body(format!("{name} is showing a code too. Confirm only if both codes are the same.")).extra_child(&extra).prefer_wide_layout(true).close_response("cancel").default_response("match").build();
    dialog.add_responses(&[("cancel", "Cancel"), ("match", "Codes Match")]);
    dialog.set_response_appearance("match", adw::ResponseAppearance::Suggested);
    dialog.connect_response(None, move |_, response| done(response == "match"));
    dialog.present(Some(&app.window));
}

/// "… Is Paired": the two now know each other by key. The connection that paired them is
/// kept, so Connect shows it at once; closing the dialog lets it go.
fn paired(app: &Rc<App>, session: Session, target: &Target) {
    let name = &target.name;
    let dialog = adw::Dialog::builder().content_width(440).build();
    let header = adw::HeaderBar::builder().show_title(false).build();
    let column = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(12).margin_bottom(28).margin_start(24).margin_end(24).build();
    column.append(&gtk::Image::builder().icon_name("selection-mode-symbolic").pixel_size(88).css_classes(["success"]).build());
    column.append(&gtk::Label::builder().label(format!("{name} Is Paired")).wrap(true).justify(gtk::Justification::Center).css_classes(["title-1"]).build());
    column.append(&gtk::Label::builder().label(format!("This device and {name} now recognise each other by key. It does not have to be in pairing mode again.")).wrap(true).justify(gtk::Justification::Center).build());

    let list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).margin_top(10).css_classes(["boxed-list"]).build();
    let key = adw::ActionRow::builder().title(format!("{name}'s key")).subtitle(key_words(&session.client.host_fingerprint, 4)).use_markup(false).subtitle_selectable(true).css_classes(["property", "rm-mono"]).build();
    list.append(&key);
    list.append(&adw::ActionRow::builder().title("This device is known there as").subtitle(device_name()).use_markup(false).css_classes(["property"]).build());
    column.append(&list);

    let connect = gtk::Button::builder().label("Connect").halign(gtk::Align::Center).margin_top(14).css_classes(["pill", "suggested-action"]).build();
    column.append(&connect);

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&column));
    dialog.set_child(Some(&toolbar));
    dialog.set_default_widget(Some(&connect));

    let session = Rc::new(RefCell::new(Some(session)));
    connect.connect_clicked({
        let (app, dialog, session, target) = (app.clone(), dialog.clone(), session.clone(), target.clone());
        move |_| {
            let held = session.borrow_mut().take();
            dialog.force_close();
            if let Some(session) = held {
                // If the connection has gone meanwhile, the session page says so.
                app.enter(session, &target);
            }
        }
    });
    dialog.connect_closed({
        let (app, name) = (app.clone(), name.clone());
        move |_| {
            if let Some(session) = session.borrow_mut().take() {
                session.client.close();
                app.toast(&format!("{name} is paired"));
            }
        }
    });
    dialog.present(Some(&app.window));
}
