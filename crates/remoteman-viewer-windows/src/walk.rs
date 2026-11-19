//! `--ui STEPS --shots DIR`: the app put into each of its screens and states in turn, with a
//! made-up list of computers and no network, so that a picture of every one can be taken with
//! nobody there and looked at beside the design.

use std::time::{Duration, Instant};

use remoteman_core::{Display, Placement};

use crate::app::{App, Attempt, Choice, Dialog, Figures, Menu, MenuKind, Msg, Screen, Session, Target};
use crate::status::{Problem, Reach};
use crate::store::{Quality, now};
use crate::ui::{Field, Id, id, idn, rect};

const ALL: &str = "empty,list,list-tip,card-menu,pair,pair-waiting,pair-failed,code,paired,connecting,asleep,noanswer,notpaired,changed,refused,ended,loggingin,locked,locked-typed,settings,settings-end,choice,computer,computer-end,address,forget,about,session,session-keys,session-display,session-picture,session-share,session-more,session-narrow,hover,hover-keys,session-settings";

pub fn steps(named: &str) -> Vec<String> {
    let named = if named == "all" { ALL } else { named };
    named.split(',').map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned).collect()
}

const WORKSHOP: &str = "workshop.local:7791";
const STUDIO: &str = "studio-mac.local:7791";
const ATTIC: &str = "attic.local:7791";

fn pretend(app: &mut App, fill: bool) {
    let s = &mut app.store;
    if !s.pretend {
        s.pretend = true;
        s.computers.clear();
    }
    if !fill {
        s.computers.clear();
    } else if s.computers.is_empty() {
        let day = 86400;
        for (name, address, os, used) in [("Workshop PC", WORKSHOP, "linux", 0), ("Studio Mac", STUDIO, "macos", 1), ("Attic PC", ATTIC, "windows", 9)] {
            s.used(address, name, os);
            s.update(address, |c| {
                c.paired = now() - 30 * day;
                c.used = now() - used * day;
            });
        }
        s.update(WORKSHOP, |c| c.name = "Workshop PC".into());
    }
    s.settings.hover_bar = false;
    s.settings.figures = false;
}

fn target(address: &str, name: &str) -> Target {
    Target { address: address.into(), name: name.into() }
}

/// The rectangle of a control of the screen as it now is.
fn anchor(app: &mut App, control: Id) -> crate::ui::Rect {
    app.build();
    app.regions.iter().find(|r| r.id == control).map_or(rect(100.0, 100.0, 100.0, 32.0), |r| r.rect)
}

fn open(app: &mut App, kind: MenuKind, control: Id) {
    let at = anchor(app, control);
    app.menu = Some(Menu { kind, anchor: at });
}

/// A session with nobody at the other end: the window as it is while connected, with no
/// picture in it.
fn session(app: &mut App) {
    if app.session.is_some() {
        return;
    }
    let Ok((shared, _feed)) = crate::stream::start(&app.gpu, &app.scene) else { return };
    shared.stream_size.store(crate::stream::pack(2560, 1440), std::sync::atomic::Ordering::Relaxed);
    shared.refresh.store(144, std::sync::atomic::Ordering::Relaxed);
    let display = Display { width: 2560, height: 1440, scale: 1.0, refresh: 144, placement: Placement::Extend, hdr: false, headroom: 0.0 };
    let features = ["placement", "mirror", "configure", "sound", "clipboard", "files"].map(str::to_owned).to_vec();
    app.session = Some(Session::new(None, shared, target(WORKSHOP, "Workshop PC"), features, display, Quality::Automatic, 0, "linux".into()));
    app.enter();
    app.figures = Figures { shown: 144, decoded: 144, decode_ms: 1.9, age_ms: 14.0, round_trip_ms: 0.6, megabits: 22.0, lost: 0, replaced: 2, decoder: "av1 (d3d11va) 2560x1440".into() };
}

pub fn apply(app: &mut App, step: &str) {
    pretend(app, step != "empty");
    let problem = |app: &mut App, address: &str, name: &str, problem: Problem| app.screen = Screen::Problem(target(address, name), problem);
    // Everything of the step before goes, except a session that the next step also wants.
    app.dialog = None;
    app.menu = None;
    app.tip = None;
    app.toast = None;
    app.attempt = None;
    app.retry_at = None;
    app.scroll = 0.0;
    app.input.focus = 0;
    app.reach.clear();
    app.reach.insert(ATTIC.into(), Reach::Unreachable);
    let wants_session = step.starts_with("session") || step.starts_with("hover");
    if !wants_session && app.session.is_some() {
        app.stop(None);
    }
    app.screen = Screen::Computers;
    match step {
        "empty" | "list" => {}
        "list-tip" => {
            let at = anchor(app, id("list.settings"));
            app.tip = Some(("Settings".into(), at));
        }
        "card-menu" => open(app, MenuKind::Card(WORKSHOP.into()), idn("card.more", 0)),
        "pair" => app.update(Msg::Pair(None)),
        "pair-waiting" => {
            app.update(Msg::Pair(Some(WORKSHOP.into())));
            app.dialog = Some(Dialog::Pair { waiting: Some("workshop.local".into()), problem: None });
        }
        "pair-failed" => {
            app.update(Msg::Pair(Some(WORKSHOP.into())));
            app.dialog = Some(Dialog::Pair { waiting: None, problem: Some("workshop.local is not in pairing mode. Start pairing there and try again.".into()) });
        }
        "code" => app.dialog = Some(Dialog::Code { name: "workshop.local".into(), code: "493 027".into(), until: Instant::now() + Duration::from_secs(52) }),
        "paired" => app.dialog = Some(Dialog::Paired { target: target(WORKSHOP, "Workshop PC"), key: "7C1F 22A9 04DE B36A".into() }),
        "connecting" => {
            if let Ok((shared, _feed)) = crate::stream::start(&app.gpu, &app.scene) {
                shared.stage.store(3, std::sync::atomic::Ordering::Relaxed);
                app.attempt = Some(Attempt { target: target(WORKSHOP, "Workshop PC"), shared, display: Display { width: 2560, height: 1440, scale: 1.0, refresh: 144, placement: Placement::Extend, hdr: false, headroom: 0.0 }, pairing: false });
                app.screen = Screen::Connecting;
            }
        }
        "asleep" => problem(app, STUDIO, "Studio Mac", Problem::Asleep),
        "noanswer" => problem(app, ATTIC, "Attic PC", Problem::Unreachable("No answer after 10 seconds".into())),
        "notpaired" => problem(app, WORKSHOP, "Workshop PC", Problem::NotPaired),
        "changed" => problem(app, WORKSHOP, "Workshop PC", Problem::Changed),
        "refused" => problem(app, WORKSHOP, "Workshop PC", Problem::Refused("sharing is paused on this computer".into())),
        "ended" => problem(app, WORKSHOP, "Workshop PC", Problem::Ended("The computer stopped answering.".into())),
        "loggingin" => {
            problem(app, WORKSHOP, "Workshop PC", Problem::LoggingIn("logging anna in".into()));
            app.retry_at = Some(Instant::now() + Duration::from_secs(3600));
        }
        "locked" | "locked-typed" => {
            problem(app, WORKSHOP, "Workshop PC", Problem::Locked("Enter anna's password to unlock it.".into()));
            app.fields.insert(id("locked.password"), if step == "locked" { Field::default() } else { Field::with("not a real one") });
            app.input.focus = id("locked.password");
        }
        "settings" | "settings-end" | "choice" => {
            app.update(Msg::OpenSettings);
            if step == "settings-end" {
                app.build();
                app.scroll = app.scroll_room.get();
            }
            if step == "choice" {
                open(app, MenuKind::Choice(Choice::DefaultQuality), id("set.quality"));
            }
        }
        "computer" | "computer-end" => {
            app.update(Msg::OpenComputer(WORKSHOP.into()));
            if step == "computer-end" {
                app.build();
                app.scroll = app.scroll_room.get();
            }
        }
        "address" => app.update(Msg::EditAddress(ATTIC.into())),
        "forget" => app.update(Msg::AskForget(STUDIO.into())),
        "about" => app.update(Msg::About),
        _ if wants_session => {
            session(app);
            app.screen = Screen::Session;
            let hover = step.starts_with("hover");
            app.store.settings.hover_bar = hover;
            app.store.settings.figures = matches!(step, "session" | "hover");
            (app.bar_out, app.bar_slide) = (hover, if hover { 1.0 } else { 0.0 });
            match step {
                "session-keys" | "hover-keys" => open(app, MenuKind::Keys, id("bar.keys")),
                "session-display" => open(app, MenuKind::Display, id("bar.display")),
                "session-picture" => open(app, MenuKind::Picture, id("bar.picture")),
                "session-share" => open(app, MenuKind::Share, id("bar.share")),
                "session-more" => open(app, MenuKind::More, id("bar.more")),
                "session-narrow" => app.effects.push(crate::app::Effect::Size(640.0, 480.0)),
                "session-settings" => app.update(Msg::OpenComputer(WORKSHOP.into())),
                _ => {}
            }
            if step == "session" {
                app.toast = Some(("Sending notes.txt (5120 bytes)…".into(), Instant::now() + Duration::from_secs(3600)));
            }
        }
        other => say!("walk: no step called {other}"),
    }
    app.dirty = true;
}
