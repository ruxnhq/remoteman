//! Why there is no session: the core's words sorted into the pages the design has for them.
//! As the Linux viewer's `status.rs`.

/// What is known of a computer before it is tried.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Reach {
    Online,
    Asleep,
    /// It didn't answer when it was last tried.
    Unreachable,
}

/// Why there is no session.
#[derive(Clone, Debug, PartialEq)]
pub enum Problem {
    /// Nothing answered; with what came of the attempt.
    Unreachable(String),
    /// Known to be asleep and wakeable. Nothing can tell yet (`Features::presence`).
    Asleep,
    /// The computer doesn't trust this PC, or this PC has no key for it.
    NotPaired,
    /// Something else answered with another key.
    Changed,
    /// The computer answered and said no; with its reason.
    Refused(String),
    /// A session that was running ended; with the reason.
    Ended(String),
}

/// Sort out the core's error text.
pub fn classify(error: &str) -> Problem {
    if error.contains("isn't the host that was paired") {
        Problem::Changed
    } else if error.contains("isn't paired") || error.contains("the host refused: not_paired") {
        Problem::NotPaired
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
        // It let this PC in and then gave a reason for not going on.
        Some(words) if !shown => Problem::Refused(words.to_owned()),
        Some(words) => Problem::Ended(format!("The computer closed the connection: {words}")),
        None if reason.contains("timed out") => Problem::Ended("The computer stopped answering.".into()),
        None => Problem::Ended(reason.to_owned()),
    }
}
