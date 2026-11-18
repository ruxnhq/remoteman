//! Preferences, and the settings of one computer.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use remoteman_core::Placement;

use crate::computers::{self, about, computer_icon, state};
use crate::features::{FEATURES, gate};
use crate::status::Reach;
use crate::store::{Quality, host_part, key_words, when, with_port};
use crate::window::App;

fn switch(title: &str, subtitle: &str, active: bool) -> adw::SwitchRow {
    adw::SwitchRow::builder().title(title).subtitle(subtitle).active(active).build()
}

fn combo(title: &str, subtitle: &str, choices: &[&str], selected: u32) -> adw::ComboRow {
    let row = adw::ComboRow::builder().title(title).subtitle(subtitle).model(&gtk::StringList::new(choices)).selected(selected).build();
    // The row's own way of showing the choice cuts it short when the words beside it are
    // long; this shows it whole and lets those wrap instead.
    let whole = gtk::SignalListItemFactory::new();
    whole.connect_setup(|_, item| {
        if let Some(item) = item.downcast_ref::<gtk::ListItem>() {
            item.set_child(Some(&gtk::Label::builder().xalign(0.0).build()));
        }
    });
    whole.connect_bind(|_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else { return };
        if let (Some(label), Some(text)) = (item.child().and_downcast::<gtk::Label>(), item.item().and_downcast::<gtk::StringObject>()) {
            label.set_label(&text.string());
        }
    });
    row.set_factory(Some(&whole));
    row
}

const QUALITIES: [(Quality, &str); 2] = [(Quality::Automatic, "Automatic"), (Quality::Sharp, "Sharp Text (4:4:4)")];

fn quality_row(title: &str, subtitle: &str, now: Quality) -> adw::ComboRow {
    let names: Vec<&str> = QUALITIES.iter().map(|q| q.1).collect();
    combo(title, subtitle, &names, QUALITIES.iter().position(|q| q.0 == now).unwrap_or(0) as u32)
}

/// Ask, then forget a computer: out of the list, and its key deleted. `after` runs if it was.
fn forget(app: &Rc<App>, over: &impl IsA<gtk::Widget>, address: &str, after: impl Fn() + 'static) {
    let name = app.name_of(address);
    let alert = adw::AlertDialog::builder().heading(format!("Forget {name}?")).body(format!("This device will have to be paired with {name} again before it can connect to it.")).close_response("cancel").default_response("cancel").build();
    alert.add_responses(&[("cancel", "Cancel"), ("forget", "Forget")]);
    alert.set_response_appearance("forget", adw::ResponseAppearance::Destructive);
    alert.connect_response(Some("forget"), {
        let (app, address) = (app.clone(), address.to_owned());
        move |_, _| {
            app.store.borrow_mut().forget(&address);
            app.reach.borrow_mut().remove(&address);
            computers::refresh(&app);
            after();
            app.toast(&format!("{name} was forgotten"));
        }
    });
    alert.present(Some(over));
}

/// The Preferences dialog.
pub fn preferences(app: &Rc<App>) {
    let dialog = adw::PreferencesDialog::builder().title("Preferences").search_enabled(true).build();
    let page = adw::PreferencesPage::new();
    let (hover, figures, fullscreen, shortcuts, quality) = {
        let store = app.store.borrow();
        let s = &store.settings;
        (app.hover_bar.get(), s.figures, s.fullscreen, s.shortcuts, s.quality)
    };
    // Change the remembered preferences and save them.
    let set = |app: &Rc<App>, change: &dyn Fn(&mut crate::store::Settings)| {
        let mut store = app.store.borrow_mut();
        change(&mut store.settings);
        store.save_settings();
    };

    let keyboard = adw::PreferencesGroup::builder().title("Keyboard").description("Keys are sent by their position, so the computer's own layout decides what they type").build();
    keyboard.add(&gate(combo("Key that acts as Super", "Opens the overview on a Linux computer", &["Super"], 0), FEATURES.key_mapping));
    keyboard.add(&gate(combo("Key that acts as Ctrl", "", &["Ctrl"], 0), FEATURES.key_mapping));
    keyboard.add(&gate(switch("Use Ctrl as Command on a Mac", "Ctrl+C on this keyboard copies on a Mac, as it does here", false), FEATURES.key_mapping));
    let send_shortcuts = switch("Send system shortcuts to the computer", "Super, Alt+Tab and others go to the session while its window has the keyboard", shortcuts);
    send_shortcuts.connect_active_notify({
        let app = app.clone();
        move |row| {
            set(&app, &|s| s.shortcuts = row.is_active());
            app.grab_shortcuts();
        }
    });
    keyboard.add(&send_shortcuts);
    let release = adw::ActionRow::builder().title("Release the keyboard").subtitle("Gives shortcuts back to this computer").build();
    let caps = gtk::Box::builder().spacing(4).valign(gtk::Align::Center).build();
    caps.append(&gtk::Label::builder().label("Ctrl").css_classes(["rm-keycap"]).build());
    caps.append(&gtk::Label::builder().label("+").css_classes(["dim-label"]).build());
    caps.append(&gtk::Label::builder().label("Alt").css_classes(["rm-keycap"]).build());
    release.add_suffix(&caps);
    keyboard.add(&gate(release, FEATURES.release_shortcut));
    page.add(&keyboard);

    let pointer = adw::PreferencesGroup::builder().title("Pointer and touch").build();
    pointer.add(&gate(combo("Pointer", "Captured sends raw movement, which games and 3D apps need", &["Follows this pointer", "Captured"], 0), FEATURES.pointer_capture));
    pointer.add(&gate(switch("Draw the pointer here", "It moves at this display's refresh rate instead of waiting for the picture", true), FEATURES.pointer_choice));
    pointer.add(&gate(combo("Scroll direction", "", &["Same as this computer", "Reversed"], 0), FEATURES.scroll_direction));
    pointer.add(&gate(switch("Send touchscreen touches as touch", "Fingers arrive as fingers, so gestures on the computer work", false), FEATURES.touch));
    page.add(&pointer);

    let window = adw::PreferencesGroup::builder().title("Session window").build();
    let controls = combo("Window controls", "Show on hover leaves only the picture in the window until the pointer reaches the top edge", &["Header bar", "Show on hover"], hover as u32);
    controls.connect_selected_notify({
        let app = app.clone();
        move |row| {
            let hover = row.selected() == 1;
            set(&app, &|s| s.hover_bar = hover);
            app.hover_bar.set(hover);
            app.place_bar();
        }
    });
    window.add(&controls);
    let start_full = switch("Start in full screen", "", fullscreen);
    start_full.connect_active_notify({
        let app = app.clone();
        move |row| set(&app, &|s| s.fullscreen = row.is_active())
    });
    window.add(&start_full);
    page.add(&window);

    let picture = adw::PreferencesGroup::builder().title("Picture").build();
    let default_quality = quality_row("Default quality", "Automatic picks the best stream this computer decodes in hardware", quality);
    default_quality.connect_selected_notify({
        let app = app.clone();
        move |row| set(&app, &|s| s.quality = QUALITIES[(row.selected() as usize).min(1)].0)
    });
    picture.add(&default_quality);
    let hz = app.display(Placement::Extend).refresh;
    picture.add(&gate(switch("Match this display's refresh rate", &format!("Up to {hz} Hz on this display"), true), FEATURES.refresh_choice));
    let show_figures = switch("Show figures", "Frames per second, picture age, bitrate, codec, lost frames and round trip", figures);
    show_figures.connect_active_notify({
        let app = app.clone();
        move |row| app.set_figures(row.is_active())
    });
    picture.add(&show_figures);
    page.add(&picture);

    let media = adw::PreferencesGroup::builder().title("Sound, microphone and camera").description("What a newly paired computer may use. Each computer has its own setting, and every session has its own switches").build();
    media.add(&gate(switch("Play the computer's sound here", "", false), FEATURES.sound_setting));
    media.add(&gate(switch("Let the computer use this microphone", "", false), FEATURES.microphone));
    media.add(&gate(switch("Let the computer use this camera", "For calls made on the computer. An indicator shows on both sides while it is in use", false), FEATURES.camera));
    page.add(&media);

    let sharing = adw::PreferencesGroup::builder().title("Sharing").build();
    sharing.add(&gate(switch("Share the clipboard", "Text and images copied on either side can be pasted on the other", false), FEATURES.clipboard));
    let folder = adw::ActionRow::builder().title("Save received files to").build();
    folder.add_suffix(&gtk::Label::builder().label("Downloads").css_classes(["dim-label"]).build());
    folder.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
    sharing.add(&gate(folder, FEATURES.files));
    page.add(&sharing);

    let paired = adw::PreferencesGroup::builder().title("Paired computers").description("A forgotten computer has to be paired again before this device can connect to it").build();
    let computers = app.store.borrow().computers.clone();
    for c in &computers {
        let key = app.store.borrow().key(&c.address).map_or_else(|| "no key".to_owned(), |k| format!("key {}", key_words(&k, 2)));
        let since = if c.paired > 0 { format!("Paired {} · ", when(c.paired)) } else { String::new() };
        let row = adw::ActionRow::builder().title(&c.name).subtitle(format!("{since}{key}")).use_markup(false).build();
        row.add_prefix(&computer_icon());
        let button = gtk::Button::builder().label("Forget…").valign(gtk::Align::Center).build();
        button.connect_clicked({
            let (app, address, dialog, paired, row) = (app.clone(), c.address.clone(), dialog.clone(), paired.clone(), row.clone());
            move |_| {
                let (paired, row) = (paired.clone(), row.clone());
                forget(&app, &dialog, &address, move || paired.remove(&row));
            }
        });
        row.add_suffix(&button);
        paired.add(&row);
    }
    paired.set_visible(!computers.is_empty());
    page.add(&paired);

    dialog.add(&page);
    dialog.present(Some(&app.window));
}

/// The settings of one computer, from its gear button in the list. `addresses`: open at its
/// addresses.
pub fn computer(app: &Rc<App>, address: &str, addresses: bool) {
    let Some(c) = app.store.borrow().find(address).cloned() else { return };
    // The address can be changed here, and everything below must then mean the new one.
    let address = Rc::new(RefCell::new(c.address.clone()));
    let dialog = adw::PreferencesDialog::builder().title(&c.name).build();
    let page = adw::PreferencesPage::new();

    let top = adw::PreferencesGroup::new();
    let summary = adw::ActionRow::builder().title(&c.name).subtitle(about(&c)).use_markup(false).build();
    summary.add_prefix(&computer_icon());
    match app.reach_of(&c.address) {
        Some(Reach::Online) => summary.add_suffix(&state("Online", "ok")),
        Some(Reach::Asleep) => summary.add_suffix(&state("Asleep", "warn")),
        Some(Reach::Unreachable) => summary.add_suffix(&state("Not reachable", "dim")),
        None => {}
    }
    let connect = gtk::Button::builder().label("Connect").valign(gtk::Align::Center).css_classes(["suggested-action"]).build();
    connect.connect_clicked({
        let (app, dialog, address) = (app.clone(), dialog.clone(), address.clone());
        move |_| {
            dialog.force_close();
            app.connect(app.target_of(&address.borrow()), false);
        }
    });
    summary.add_suffix(&connect);
    top.add(&summary);
    let name = adw::EntryRow::builder().title("Name").text(&c.name).show_apply_button(true).build();
    name.connect_apply({
        let (app, dialog, address, summary) = (app.clone(), dialog.clone(), address.clone(), summary.clone());
        move |row| {
            let new = row.text().trim().to_owned();
            if new.is_empty() {
                return;
            }
            app.store.borrow_mut().update(&address.borrow(), |c| c.name = new.clone());
            computers::refresh(&app);
            summary.set_title(&new);
            dialog.set_title(&new);
        }
    });
    top.add(&name);
    page.add(&top);

    let display = adw::PreferencesGroup::builder().title("Use this device's display as").description("Where it sits among the computer's own displays. It can be changed during a session").build();
    let mut first: Option<gtk::CheckButton> = None;
    for (placement, title, subtitle) in [
        (Placement::Extend, "Extra display", "Beside the computer's own displays, which stay on"),
        (Placement::Main, "Main display", "The computer's own displays stay on as secondary ones"),
        (Placement::Only, "Only display", "The computer's own screens are switched off while connected"),
    ] {
        let radio = gtk::CheckButton::builder().valign(gtk::Align::Center).build();
        match &first {
            Some(group) => radio.set_group(Some(group)),
            None => first = Some(radio.clone()),
        }
        radio.set_active(c.placement == placement);
        radio.connect_toggled({
            let (app, address) = (app.clone(), address.clone());
            move |radio| {
                if radio.is_active() {
                    app.store.borrow_mut().update(&address.borrow(), |c| c.placement = placement);
                    // What the command line said was for the first connection.
                    app.said_placement.set(None);
                }
            }
        });
        let row = adw::ActionRow::builder().title(title).subtitle(subtitle).activatable_widget(&radio).build();
        row.add_prefix(&radio);
        display.add(&row);
    }
    page.add(&display);

    let media = adw::PreferencesGroup::builder().title("Sound, microphone and camera").description("What this computer may use from this device when a session starts").build();
    media.add(&gate(switch("Sound", "The computer's sound plays on this device", false), FEATURES.sound_setting));
    media.add(&gate(switch("Microphone", "The computer can use this device's microphone", false), FEATURES.microphone));
    media.add(&gate(switch("Camera", "The computer can use this device's camera, for calls made there", false), FEATURES.camera));
    page.add(&media);

    let connection = adw::PreferencesGroup::builder().title("Connection").build();
    connection.add(&gate(combo("Way in", "Automatic attaches when you are logged in at the computer, and uses remote login when nobody is", &["Automatic"], 0), FEATURES.way_in));
    let quality = quality_row("Picture quality", "", c.quality);
    quality.connect_selected_notify({
        let (app, address) = (app.clone(), address.clone());
        move |row| {
            let quality = QUALITIES[(row.selected() as usize).min(1)].0;
            app.store.borrow_mut().update(&address.borrow(), |c| c.quality = quality);
        }
    });
    connection.add(&quality);
    let addresses_row = adw::ActionRow::builder().title("Addresses").subtitle(&c.address).use_markup(false).activatable(true).build();
    addresses_row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
    connection.add(&addresses_row);
    let key = app.store.borrow().key(&c.address).map_or_else(|| "Not paired".to_owned(), |k| key_words(&k, 4));
    connection.add(&adw::ActionRow::builder().title("Key").subtitle(key).subtitle_selectable(true).css_classes(["property", "rm-mono"]).build());
    page.add(&connection);

    let last = adw::PreferencesGroup::new();
    let forget_row = adw::ButtonRow::builder().title("Forget This Computer…").build();
    forget_row.add_css_class("destructive-action");
    forget_row.connect_activated({
        let (app, dialog, address) = (app.clone(), dialog.clone(), address.clone());
        move |_| {
            let closing = dialog.clone();
            forget(&app, &dialog, &address.borrow(), move || {
                closing.force_close();
            });
        }
    });
    last.add(&forget_row);
    page.add(&last);
    dialog.add(&page);

    // Addresses: one saved address for now.
    let open_addresses = {
        let (app, dialog, address, addresses_row) = (app.clone(), dialog.clone(), address.clone(), addresses_row.clone());
        move || {
            let entry = adw::EntryRow::builder().title("Saved address").text(address.borrow().as_str()).show_apply_button(true).build();
            let group = adw::PreferencesGroup::builder().description(format!("Where this device looks for the computer: a name or an IP address, then a colon and the port ({} unless it was changed). The computer's key stays with it.", remoteman_core::PORT)).build();
            group.add(&entry);
            let sub = adw::PreferencesPage::new();
            sub.add(&group);
            let toolbar = adw::ToolbarView::new();
            toolbar.add_top_bar(&adw::HeaderBar::new());
            toolbar.set_content(Some(&sub));
            entry.connect_apply({
                let (app, dialog, address, addresses_row) = (app.clone(), dialog.clone(), address.clone(), addresses_row.clone());
                move |entry| {
                    let (old, new) = (address.borrow().clone(), with_port(&entry.text()));
                    if host_part(&new).is_empty() {
                        return;
                    }
                    let moved = app.store.borrow_mut().readdress(&old, &new);
                    match moved {
                        Ok(()) => {
                            // Nothing is known about the new address.
                            app.reach.borrow_mut().remove(&old);
                            *address.borrow_mut() = new.clone();
                            entry.set_text(&new);
                            addresses_row.set_subtitle(&new);
                            computers::refresh(&app);
                            dialog.pop_subpage();
                        }
                        Err(why) => dialog.add_toast(adw::Toast::new(&why)),
                    }
                }
            });
            dialog.push_subpage(&adw::NavigationPage::builder().title("Addresses").child(&toolbar).build());
        }
    };
    addresses_row.connect_activated({
        let open = open_addresses.clone();
        move |_| open()
    });

    dialog.present(Some(&app.window));
    if addresses {
        open_addresses();
    }
}
