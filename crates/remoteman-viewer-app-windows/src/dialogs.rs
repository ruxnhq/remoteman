//! The dialogs the state calls for, as ContentDialogs, one at a time: pairing (the address, then
//! the six-digit code to compare, then the key), forgetting a computer, editing its address, and
//! About. Pressing a dialog's button closes it (Reactor's dialogs close on any button), so a
//! primary button hands its words to the engine and the window keeps the dialog away until the
//! engine's dialog has changed (`Viewer::dialog_closed`); when the state takes a dialog away,
//! nothing is reported.

use serde_json::Value;
use windows_reactor::*;

use crate::app::{Msg, Viewer};
use crate::kit::{J, caption};

/// What makes a dialog a new one to show: its kind and what it is waiting for, its problem and
/// its code.
pub fn key(state: &Value) -> String {
    let d = state.o("dialog");
    if !d.is_object() {
        return String::new();
    }
    format!("{}|{}|{}|{}", d.s("kind"), d.s("waiting"), d.s("problem"), d.s("code"))
}

pub fn kind_of(key: &str) -> &str {
    key.split('|').next().unwrap_or("")
}

fn problem(d: &Value) -> Option<View> {
    let p = d.s("problem");
    (!p.is_empty()).then(|| InfoBar::new().is_open(true).is_closable(false).severity(InfoBarSeverity::Error).message(p).into())
}

pub fn dialog(v: &Viewer, context: &ViewContext<Viewer>) -> Option<ContentDialog> {
    let d = v.state.o("dialog");
    if !d.is_object() {
        return None;
    }
    let name = d.s("name");
    let base = ContentDialog::new().is_open(true).on_closed(context.callback(Msg::Dialog));
    Some(match d.s("kind") {
        "pair" => {
            let waiting = d.s("waiting");
            let text = v.pair_text.clone().unwrap_or_else(|| d.s("address").to_owned());
            let mut col: Vec<View> = vec![
                TextBlock::new().text("Put the other computer in pairing mode, then enter its address here.").text_wrapping(TextWrapping::Wrap).into(),
                TextBox::new()
                    .header("Address")
                    .placeholder_text("workshop.local or 192.168.1.20")
                    .width(360.0)
                    .text(text)
                    .is_enabled(waiting.is_empty())
                    .automation_name("Address")
                    .on_text_changed(context.callback(Msg::PairText))
                    .into(),
                caption("A name or an IP address. Add a colon and the port if it is not 7791.").into(),
            ];
            if !waiting.is_empty() {
                col.push(StackPanel::new().orientation(Orientation::Horizontal).spacing(8.0).children((ProgressRing::new().is_active(true).width(16.0).height(16.0), TextBlock::new().text(format!("Waiting for {waiting}…")))).into());
            }
            col.extend(problem(d));
            base.title("Pair a computer")
                .content(StackPanel::new().spacing(12.0).children(col))
                .primary_button_text("Pair")
                .is_primary_button_enabled(waiting.is_empty())
                .close_button_text("Cancel")
                .default_button(ContentDialogButton::Primary)
        }
        "code" => {
            let code = d.s("code");
            let shown = if code.len() == 6 { format!("{} {}", &code[..3], &code[3..]) } else { code.to_owned() };
            let left = d.n("seconds") as i64;
            base.title(format!("Pair with {name}?"))
                .content(StackPanel::new().spacing(12.0).children((
                    TextBlock::new().text(format!("Check that {name} shows the same code. If it shows a different one, cancel: something else answered at that address.")).text_wrapping(TextWrapping::Wrap),
                    TextBlock::new().text(shown).font_size(40.0).font_weight(FontWeight::SEMI_BOLD).character_spacing(100).horizontal_alignment(HorizontalAlignment::Center),
                    TextBlock::new().text(format!("Pairing closes in {}:{:02}", left / 60, left % 60)).foreground(ThemeBrush::SecondaryText).horizontal_alignment(HorizontalAlignment::Center),
                    caption(&format!("Confirm it on {name}. After pairing, this PC and {name} keep each other's key. Nothing else has to be typed again.")),
                )))
                .close_button_text("Cancel")
        }
        // The six digits go through `Msg::Password` and "Don't ask again" through `Msg::Remember`, as the locked page's.
        "second-factor" => {
            let mut col: Vec<View> = vec![
                TextBlock::new().text(format!("Open your authenticator app and type the six digits it shows for {name}.")).text_wrapping(TextWrapping::Wrap).into(),
                TextBox::new().header("Code").placeholder_text("123456").width(200.0).text(v.password.clone()).automation_name("Code from the authenticator app").on_text_changed(context.callback(Msg::Password)).into(),
                CheckBox::new().content("Don't ask again on this device for 30 days").is_checked(v.remember.unwrap_or(false)).on_click(context.message(Msg::Remember)).into(),
            ];
            col.extend(problem(d));
            base.title("This computer asks for a code from your authenticator app")
                .content(StackPanel::new().spacing(12.0).children(col))
                .primary_button_text("Connect")
                .close_button_text("Cancel")
                .default_button(ContentDialogButton::Primary)
        }
        "paired" => base
            .title(format!("Paired with {name}"))
            .content(StackPanel::new().spacing(8.0).children((
                TextBlock::new().text(format!("This PC and {name} keep each other's key. Its key:")).text_wrapping(TextWrapping::Wrap),
                TextBlock::new().text(d.s("key")).font_family("Consolas").font_size(16.0),
            )))
            .primary_button_text("Connect")
            .close_button_text("Not now")
            .default_button(ContentDialogButton::Primary),
        "forget" => base
            .title(format!("Forget {name}?"))
            .content(TextBlock::new().text("It has to be paired again before this PC can connect.").text_wrapping(TextWrapping::Wrap))
            .primary_button_text("Forget")
            .close_button_text("Cancel"),
        "address" => {
            let address = d.s("address");
            let text = v.address_text.clone().unwrap_or_else(|| address.strip_suffix(":7791").unwrap_or(address).to_owned());
            let mut col: Vec<View> = vec![TextBox::new().header("Address").text(text).width(360.0).automation_name("Address").on_text_changed(context.callback(Msg::AddressText)).into()];
            col.extend(problem(d));
            base.title("Edit address")
                .content(StackPanel::new().spacing(12.0).children(col))
                .primary_button_text("Save")
                .close_button_text("Cancel")
                .default_button(ContentDialogButton::Primary)
        }
        "about" => base
            .title("remoteman")
            .content(TextBlock::new().text(format!("Version {}\nSee and use another computer's screen from this one.", d.s("version"))).text_wrapping(TextWrapping::Wrap))
            .close_button_text("Close"),
        _ => return None,
    })
}
