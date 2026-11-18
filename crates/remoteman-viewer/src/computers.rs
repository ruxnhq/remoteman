//! The first screen: the paired computers.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;

use crate::features::{FEATURES, NOT_YET, gate};
use crate::status::Reach;
use crate::store::{Computer, os_name, when};
use crate::window::App;
use crate::{pair, prefs};

pub struct List {
    pub page: adw::NavigationPage,
    header: adw::HeaderBar,
    /// `empty` before anything is paired, `list` after.
    stack: gtk::Stack,
    group: adw::PreferencesGroup,
    rows: RefCell<Vec<adw::ActionRow>>,
    /// The bar's "+" and the empty page's button: both pair a computer.
    add: gtk::Button,
    first: gtk::Button,
}

/// A computer's picture at the start of its row.
pub fn computer_icon() -> gtk::Image {
    gtk::Image::builder().icon_name("computer-symbolic").pixel_size(32).css_classes(["dim-label"]).build()
}

/// A small coloured word with a dot before it: `ok`, `warn` or `dim`.
pub fn state(text: &str, kind: &str) -> gtk::Box {
    let class = match kind {
        "ok" => "success",
        "warn" => "warning",
        _ => "dim-label",
    };
    let state = gtk::Box::builder().spacing(6).valign(gtk::Align::Center).css_classes(["rm-state", class]).build();
    state.append(&gtk::Box::builder().valign(gtk::Align::Center).css_classes(["rm-dot"]).build());
    state.append(&gtk::Label::new(Some(text)));
    state
}

/// The line under a computer's name: what it runs and when it was last used.
pub fn about(c: &Computer) -> String {
    let os = os_name(&c.os);
    let used = if c.used > 0 {
        format!("Used {}", when(c.used))
    } else if c.paired > 0 {
        format!("Paired {}", when(c.paired))
    } else {
        String::new()
    };
    [os, used].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ")
}

pub fn build() -> List {
    let header = adw::HeaderBar::new();
    let add = gtk::Button::builder().icon_name("list-add-symbolic").tooltip_text("Pair a Computer").build();
    header.pack_start(&add);

    let group = adw::PreferencesGroup::builder().title("Computers").build();
    let nearby = adw::PreferencesGroup::builder().title("Nearby").description("Computers running remoteman on this network that are not paired with this device").build();
    if !FEATURES.discovery {
        // Nothing looks for them yet, and nothing is made up: say so where they will be listed.
        let row = adw::ActionRow::builder().title("Looking for computers on this network").subtitle("Not available yet. Pair a computer by its address with the + button.").sensitive(false).tooltip_text(NOT_YET).build();
        nearby.add(&row);
    }
    let page = adw::PreferencesPage::new();
    page.add(&group);
    page.add(&nearby);

    let first = gtk::Button::builder().label("Pair a Computer…").halign(gtk::Align::Center).css_classes(["pill", "suggested-action"]).build();
    let empty = adw::StatusPage::builder()
        .icon_name("computer-symbolic")
        .title("No Computers Yet")
        .description("Pair this device with a computer that runs remoteman to see and use its screen from here.")
        .child(&first)
        .build();

    let stack = gtk::Stack::new();
    stack.add_named(&empty, Some("empty"));
    stack.add_named(&page, Some("list"));
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&stack));
    let page = adw::NavigationPage::builder().title("remoteman").tag("computers").child(&toolbar).build();
    List { page, header, stack, group, rows: RefCell::new(Vec::new()), add, first }
}

pub fn wire(app: &Rc<App>) {
    app.list.header.pack_end(&app.main_menu());
    for button in [&app.list.add, &app.list.first] {
        let app = app.clone();
        button.connect_clicked(move |_| pair::open(&app, None, false));
    }
}

/// Fill the list from what is remembered.
pub fn refresh(app: &Rc<App>) {
    let list = &app.list;
    for row in list.rows.borrow_mut().drain(..) {
        list.group.remove(&row);
    }
    let store = app.store.borrow();
    for c in &store.computers {
        let row = adw::ActionRow::builder().title(&c.name).subtitle(about(c)).use_markup(false).activatable(true).build();
        row.add_prefix(&computer_icon());
        // Only what an attempt in this run found: nothing here knows a computer is online
        // or asleep before it is tried.
        match app.reach_of(&c.address) {
            Some(Reach::Online) => row.add_suffix(&state("Online", "ok")),
            Some(Reach::Asleep) => {
                row.add_suffix(&state("Asleep", "warn"));
                row.add_suffix(&gate(gtk::Button::builder().label("Wake").valign(gtk::Align::Center).build(), FEATURES.wake));
            }
            Some(Reach::Unreachable) => row.add_suffix(&state("Not reachable", "dim")),
            None => {}
        }
        let settings = gtk::Button::builder().icon_name("emblem-system-symbolic").tooltip_text("Settings for This Computer").valign(gtk::Align::Center).css_classes(["flat"]).build();
        settings.connect_clicked({
            let (app, address) = (app.clone(), c.address.clone());
            move |_| prefs::computer(&app, &address, false)
        });
        row.add_suffix(&settings);
        row.connect_activated({
            let (app, address) = (app.clone(), c.address.clone());
            move |_| app.connect(app.target_of(&address), false)
        });
        list.group.add(&row);
        list.rows.borrow_mut().push(row);
    }
    list.stack.set_visible_child_name(if store.computers.is_empty() { "empty" } else { "list" });
}
