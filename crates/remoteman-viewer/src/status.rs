//! The pages between the list and a session: connecting, and the ways it can go wrong.

use std::rc::Rc;

use adw::prelude::*;
use remoteman_core::{Display, Placement};

use crate::features::{FEATURES, gate};
use crate::store::when;
use crate::window::{App, Target};
use crate::{pair, prefs};

/// What is known of a computer before it is tried.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    Online,
    Asleep,
    /// It didn't answer when it was last tried.
    Unreachable,
}

/// Why there is no session.
pub enum Problem {
    /// Nothing answered; with what was tried and what came of it.
    Unreachable(String),
    /// Known to be asleep and wakeable. Nothing can tell yet (`Features::presence`).
    Asleep,
    /// The computer doesn't trust this device, or this device has no key for it.
    NotPaired,
    /// Something else answered with another key.
    Changed,
    /// The computer answered and said no; with its reason.
    Refused(String),
    /// The computer is locked and takes its user's password to let this device in; with what
    /// it said (who the user is, or that the last password was wrong).
    Locked(String),
    /// A session that was running ended; with the reason.
    Ended(String),
}

/// Sort out the core's error text.
pub fn classify(error: &str) -> Problem {
    if error.contains("isn't the host that was paired") {
        Problem::Changed
    } else if error.contains("isn't paired") || error.contains("the host refused: not_paired") {
        Problem::NotPaired
    } else if let Some((_, said)) = error.split_once("the host refused: locked: ") {
        Problem::Locked(said.to_owned())
    } else if let Some((_, said)) = error.split_once("the host refused: ") {
        // `code: message`; the message is the part for people.
        Problem::Refused(said.split_once(": ").map_or(said, |(_, m)| m).to_owned())
    } else if error.contains("timed out") {
        Problem::Unreachable("No answer after 10 seconds".into())
    } else if error.starts_with("look up") || error.contains("no such host") {
        Problem::Unreachable("No computer was found by that name".into())
    } else {
        Problem::Unreachable(error.to_owned())
    }
}

/// Sort out why a session ended. `shown`: it had shown a picture by then.
pub fn ended(reason: &str, shown: bool) -> Problem {
    // What the computer said as it closed: `closed by peer: WORDS (code N)`.
    let said = reason.split_once("closed by peer: ").map(|(_, m)| m.rsplit_once(" (code ").map_or(m, |(m, _)| m));
    match said {
        Some("bye") | Some("done") => Problem::Ended("The computer closed the connection.".into()),
        // It is locked, and takes its user's password with the next try.
        Some(words) if !shown && words.starts_with("locked: ") => Problem::Locked(words["locked: ".len()..].to_owned()),
        // It let this device in and then gave a reason for not going on.
        Some(words) if !shown => Problem::Refused(words.to_owned()),
        Some(words) => Problem::Ended(format!("The computer closed the connection: {words}")),
        None if reason.contains("timed out") => Problem::Ended("The computer stopped answering.".into()),
        None => Problem::Ended(reason.to_owned()),
    }
}

/// A page over the list: a bar with the computer's name and a way back, and `content`.
fn page(app: &App, title: &str, content: &impl IsA<gtk::Widget>) -> adw::NavigationPage {
    let header = adw::HeaderBar::new();
    header.pack_end(&app.main_menu());
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(content));
    adw::NavigationPage::builder().title(title).child(&toolbar).build()
}

fn pill(label: &str, suggested: bool) -> gtk::Button {
    let button = gtk::Button::builder().label(label).css_classes(["pill"]).build();
    if suggested {
        button.add_css_class("suggested-action");
    }
    button
}

/// What the display will be for the computer, in a sentence and a line of detail.
fn placement_words(placement: Placement, name: &str) -> (String, &'static str) {
    match placement {
        Placement::Extend => (format!("This display will be an extra display for {name}"), "Beside its own displays, which stay on"),
        Placement::Main => (format!("This display will be {name}'s main display"), "Its own displays stay on as secondary ones"),
        Placement::Only => (format!("This display will be {name}'s only display"), "Its own screens turn off while you are connected"),
        Placement::Mirror => (format!("This display will show {name}'s main monitor"), "As it is, at that monitor's size"),
    }
}

/// Waiting for a computer to answer. Going back, or Cancel, gives up attempt `id`.
pub fn connecting(app: &Rc<App>, target: &Target, display: Display, id: u32) -> adw::NavigationPage {
    let column = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(24).margin_top(28).margin_bottom(32).margin_start(12).margin_end(12).build();

    let top = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(12).build();
    top.append(&adw::Spinner::builder().width_request(48).height_request(48).halign(gtk::Align::Center).build());
    top.append(&gtk::Label::builder().label(format!("Connecting to {}", target.name)).wrap(true).justify(gtk::Justification::Center).margin_top(4).css_classes(["title-1"]).build());
    top.append(
        &gtk::Label::builder()
            .label(format!("Asking {} for a {} × {} display at {} Hz…", target.name, display.width, display.height, display.refresh))
            .wrap(true)
            .justify(gtk::Justification::Center)
            .max_width_chars(60)
            .build(),
    );
    column.append(&top);

    let (title, detail) = placement_words(display.placement, &target.name);
    let row = adw::ActionRow::builder().title(title).subtitle(detail).use_markup(false).build();
    row.add_prefix(&gtk::Image::builder().icon_name("video-display-symbolic").pixel_size(32).css_classes(["dim-label"]).build());
    let change = gtk::Button::builder().label("Change…").valign(gtk::Align::Center).build();
    row.add_suffix(&change);
    let list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).css_classes(["boxed-list"]).build();
    list.append(&row);
    column.append(&list);

    let cancel = gtk::Button::builder().label("Cancel").halign(gtk::Align::Center).build();
    column.append(&cancel);

    let clamp = adw::Clamp::builder().maximum_size(600).child(&column).build();
    let scroll = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).child(&clamp).build();
    let page = page(app, &target.name, &scroll);
    // Leaving the page, by the back button or because the answer came, ends the wait.
    page.connect_hidden({
        let app = app.clone();
        move |_| app.give_up(id)
    });
    cancel.connect_clicked({
        let app = app.clone();
        move |_| app.show_list()
    });
    change.connect_clicked({
        let (app, address) = (app.clone(), target.address.clone());
        move |_| {
            app.show_list();
            prefs::computer(&app, &address, false);
        }
    });
    page
}

/// A page saying why there is no session with a computer, and what can be done.
pub fn problem(app: &Rc<App>, target: &Target, problem: Problem) -> adw::NavigationPage {
    let name = &target.name;
    let again = |label: &str| {
        let button = pill(label, true);
        let (app, target) = (app.clone(), target.clone());
        button.connect_clicked(move |_| app.connect(target.clone(), false));
        button
    };
    let buttons = gtk::Box::builder().spacing(12).halign(gtk::Align::Center).build();
    let extra = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(24).build();
    let (icon, title, description) = match problem {
        Problem::Unreachable(detail) => {
            // What was tried. There is one saved address and nothing that searches the network.
            let row = adw::ActionRow::builder().title("Saved address").subtitle(format!("{} · {detail}", target.address)).use_markup(false).build();
            row.add_suffix(&gtk::Image::builder().icon_name("window-close-symbolic").css_classes(["dim-label"]).build());
            let list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).css_classes(["boxed-list"]).build();
            list.append(&row);
            extra.append(&adw::Clamp::builder().maximum_size(480).child(&list).build());
            let edit = pill("Edit Address…", false);
            edit.connect_clicked({
                let (app, address) = (app.clone(), target.address.clone());
                move |_| {
                    app.show_list();
                    prefs::computer(&app, &address, true);
                }
            });
            buttons.append(&edit);
            buttons.append(&again("Try Again"));
            // A computer whose network cards were once seen on this network can be sent the
            // packet that wakes it, which does nothing if it is off or somewhere else.
            let wakeable = crate::features::FEATURES.wake && app.store.borrow().find(&target.address).is_some_and(|c| !c.cards.is_empty());
            if wakeable {
                let wake = pill("Wake Up", false);
                wake.connect_clicked({
                    let (app, address) = (app.clone(), target.address.clone());
                    move |_| {
                        app.show_list();
                        app.wake(&address);
                    }
                });
                buttons.append(&wake);
            }
            let last = if wakeable { "If it is asleep on this network, remoteman can try to wake it." } else { "It can't be woken from here: it has not been seen on this device's network." };
            ("network-wireless-offline-symbolic", format!("Can't Reach {name}"), format!("It did not answer at its saved address. It may be switched off or asleep, or on a network this device can't get to. {last}"))
        }
        Problem::Asleep => {
            let wake = gate(pill("Wake Up", true), FEATURES.wake);
            buttons.append(&wake);
            let used = app.store.borrow().find(&target.address).map_or(0, |c| c.used);
            let seen = if used > 0 { format!("It was last used from this device {}. ", when(used)) } else { String::new() };
            ("weather-clear-night-symbolic", format!("{name} Is Asleep"), format!("{seen}remoteman can wake it over the network, which usually takes about ten seconds, and connects as soon as it answers."))
        }
        Problem::NotPaired => {
            let pair = pill("Pair Again…", true);
            pair.connect_clicked({
                let (app, address) = (app.clone(), target.address.clone());
                move |_| {
                    app.show_list();
                    pair::open(&app, Some(&address), false);
                }
            });
            buttons.append(&pair);
            ("dialog-password-symbolic", format!("{name} Doesn't Know This Device"), "It only lets in devices it has been paired with. Put it in pairing mode, then pair again from here.".to_owned())
        }
        Problem::Changed => {
            let settings = pill("Computer Settings…", false);
            settings.connect_clicked({
                let (app, address) = (app.clone(), target.address.clone());
                move |_| {
                    app.show_list();
                    prefs::computer(&app, &address, false);
                }
            });
            buttons.append(&settings);
            ("dialog-warning-symbolic", format!("{name} Has a Different Key"), format!("What answered at {} is not the computer this device was paired with. If it has been set up again, forget it and pair it anew.", target.address))
        }
        Problem::Locked(said) => {
            // The password goes to the computer with the next try and is kept nowhere here.
            let password = gtk::PasswordEntry::builder().show_peek_icon(true).placeholder_text("Password").width_request(280).activates_default(true).build();
            let unlock = pill("Unlock", true);
            let go = {
                let (app, target, password) = (app.clone(), target.clone(), password.clone());
                move || {
                    let typed = password.text().to_string();
                    if !typed.is_empty() {
                        *app.unlock.borrow_mut() = Some(typed);
                        app.connect(target.clone(), false);
                    }
                }
            };
            unlock.connect_clicked({
                let go = go.clone();
                move |_| go()
            });
            password.connect_activate(move |_| go());
            extra.append(&adw::Clamp::builder().maximum_size(320).child(&password).build());
            buttons.append(&unlock);
            ("system-lock-screen-symbolic", format!("{name} Is Locked"), format!("{said} It locks again when you leave."))
        }
        Problem::Refused(said) => {
            buttons.append(&again("Try Again"));
            ("computer-fail-symbolic", format!("{name} Can't Start a Session"), format!("It answered, and said: {said}"))
        }
        Problem::Ended(reason) => {
            buttons.append(&again("Connect Again"));
            ("network-offline-symbolic", format!("The Session with {name} Ended"), reason)
        }
    };
    extra.append(&buttons);
    let status = adw::StatusPage::builder().icon_name(icon).title(title).description(glib_escape(&description)).child(&extra).build();
    page(app, name, &status)
}

/// A status page's description is markup.
fn glib_escape(text: &str) -> String {
    gtk::glib::markup_escape_text(text).to_string()
}
