//! What a scenario sees and does on the screen of a guest, through the
//! driver. A control is named by a selector, `<attribute>=<text>`; a
//! scenario uses the accessibility identifiers of the app
//! (`id=dashboard.sync-now`) so that a change of copy or of language
//! does not break it.

use std::time::Duration;

use serde::Deserialize;
use vapor_e2e::wait::wait_for;
use vapor_e2e::{Failure, ensure};

use crate::guest::{Guest, guest_stage};

/// The file name of the driver inside the stage share.
pub const DRIVER: &str = "vapor-vm-driver";

/// How long a control gets to appear after the action that causes it.
pub const APPEAR_TIMEOUT: Duration = Duration::from_secs(15);

/// The driver's exit code for "no control matches".
const EXIT_NOT_FOUND: i32 = 4;
/// The driver's exit code for "the app does not run".
const EXIT_APP_NOT_RUNNING: i32 = 3;

/// One control, as accessibility describes it.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
pub struct Node {
    #[serde(default)]
    pub depth: u32,
    #[serde(default)]
    pub role: String,
    pub subrole: Option<String>,
    pub id: Option<String>,
    pub title: Option<String>,
    pub desc: Option<String>,
    pub value: Option<String>,
    pub enabled: Option<bool>,
    /// `x, y, width, height` in screen points.
    pub frame: Option<[i64; 4]>,
}

impl Node {
    /// The text a user reads on the control: the value of a text, or
    /// the title or the label of a button.
    pub fn text(&self) -> &str {
        self.value
            .as_deref()
            .or(self.title.as_deref())
            .or(self.desc.as_deref())
            .unwrap_or_default()
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled.unwrap_or(true)
    }

    /// Whether a checkbox or a toggle is on.
    pub fn is_on(&self) -> bool {
        self.value.as_deref() == Some("1")
    }

    /// Whether the control takes space on the screen right now. An
    /// item of a closed menu exists but has no area.
    pub fn is_on_screen(&self) -> bool {
        self.frame.is_some_and(|frame| frame[2] > 0 && frame[3] > 0)
    }
}

/// A notification that macOS shows on the screen.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
pub struct Notification {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub body: String,
}

/// The driver, bound to one app of one guest.
pub struct Ui<'a> {
    guest: &'a Guest,
    app: String,
}

fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

impl<'a> Ui<'a> {
    pub fn new(guest: &'a Guest, app: &str) -> Self {
        Self {
            guest,
            app: app.to_string(),
        }
    }

    fn command(&self, verb: &str, selector: Option<&str>, within: Option<&str>) -> String {
        let mut command = format!("'{}/{DRIVER}' {verb}", guest_stage());
        if !self.app.is_empty() {
            command.push(' ');
            command.push_str(&quote(&self.app));
        }
        if let Some(selector) = selector {
            command.push(' ');
            command.push_str(&quote(selector));
        }
        if let Some(within) = within {
            command.push_str(" --within ");
            command.push_str(&quote(within));
        }
        command
    }

    /// Every control of the app, in the order accessibility lists them.
    pub fn tree(&self) -> Result<Vec<Node>, Failure> {
        let text = self.guest.sh_ok(&self.command("tree", None, None))?;
        Ok(serde_json::from_str(&text)?)
    }

    /// The first control that matches, or `None` when the app runs and
    /// has no such control. `within` narrows the search to the controls
    /// under another one, for a control that more than one window
    /// shows.
    pub fn find(&self, selector: &str, within: Option<&str>) -> Result<Option<Node>, Failure> {
        let command = self.command("find", Some(selector), within);
        let output = self.guest.sh(&command)?;
        match output.status.code() {
            Some(0) => Ok(Some(serde_json::from_slice(&output.stdout)?)),
            Some(EXIT_NOT_FOUND) => Ok(None),
            // Not "no control": a scenario that asserts that a window
            // is absent must not pass because the app crashed.
            Some(EXIT_APP_NOT_RUNNING) => Err(Failure::new(format!(
                "the app `{}` does not run in the guest",
                self.app
            ))),
            _ => Err(Failure::new(format!(
                "the driver failed on `{command}`: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ))),
        }
    }

    pub fn exists(&self, selector: &str, within: Option<&str>) -> bool {
        matches!(self.find(selector, within), Ok(Some(_)))
    }

    /// Whether the app runs and has no control that matches. An app
    /// that does not run, or a driver that fails, is not an answer.
    pub fn lacks(&self, selector: &str, within: Option<&str>) -> bool {
        matches!(self.find(selector, within), Ok(None))
    }

    /// Waits until a control matches and `accept` takes it.
    pub fn wait(
        &self,
        selector: &str,
        within: Option<&str>,
        timeout: Duration,
        what: &str,
        accept: impl Fn(&Node) -> bool,
    ) -> Result<Node, Failure> {
        let mut last = None;
        wait_for(timeout, what, || {
            let node = self.find(selector, within).ok().flatten();
            last = node.clone();
            node.filter(|node| accept(node))
        })
        .map_err(|failure| {
            Failure::new(match &last {
                Some(node) => format!("{failure}; `{selector}` last read \"{}\"", node.text()),
                None => format!("{failure}; no control matched `{selector}`"),
            })
        })
    }

    /// Waits until a control matches.
    pub fn wait_for(&self, selector: &str, within: Option<&str>) -> Result<Node, Failure> {
        self.wait(
            selector,
            within,
            APPEAR_TIMEOUT,
            &format!("the control `{selector}` to appear"),
            |_| true,
        )
    }

    /// Waits until no control matches.
    pub fn wait_gone(&self, selector: &str, within: Option<&str>) -> Result<(), Failure> {
        vapor_e2e::wait::wait_until(
            APPEAR_TIMEOUT,
            &format!("the control `{selector}` to go away"),
            || matches!(self.find(selector, within), Ok(None)),
        )
    }

    /// Sends a pointer click to the middle of a control, as a user does.
    pub fn click(&self, selector: &str, within: Option<&str>) -> Result<Node, Failure> {
        let node = self.wait(
            selector,
            within,
            APPEAR_TIMEOUT,
            &format!("the control `{selector}` to be on the screen"),
            Node::is_on_screen,
        )?;
        ensure!(node.is_enabled(), "the control `{selector}` is disabled");
        let text = self
            .guest
            .sh_ok(&self.command("click", Some(selector), within))?;
        Ok(serde_json::from_str(&text)?)
    }

    /// Presses a control through accessibility, with no pointer. For a
    /// control the pointer cannot reach, such as a dialog of macOS.
    pub fn press(&self, selector: &str, within: Option<&str>) -> Result<Node, Failure> {
        let text = self
            .guest
            .sh_ok(&self.command("press", Some(selector), within))?;
        Ok(serde_json::from_str(&text)?)
    }
}

/// A window on the screen of the guest, of any app.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
pub struct Window {
    #[serde(default)]
    pub owner: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub layer: i64,
}

/// Every window that is on the screen of the guest.
pub fn windows(guest: &Guest) -> Result<Vec<Window>, Failure> {
    let text = guest.sh_ok(&format!("'{}/{DRIVER}' windows", guest_stage()))?;
    Ok(serde_json::from_str(&text)?)
}

/// The notifications of macOS that are on the screen of the guest.
pub fn notifications(guest: &Guest) -> Result<Vec<Notification>, Failure> {
    let text = guest.sh_ok(&format!("'{}/{DRIVER}' notifications", guest_stage()))?;
    Ok(serde_json::from_str(&text)?)
}

/// Closes the notifications of macOS, so that they do not cover the
/// app in a screenshot. Best effort: a guest with none is fine.
pub fn close_notifications(guest: &Guest) {
    let _ = guest.sh(&format!("'{}/{DRIVER}' close-notifications", guest_stage()));
}

/// Accepts the dialog in which macOS asks whether the guest agent may
/// capture the screen. Returns whether the dialog was there.
pub fn allow_screen_capture(guest: &Guest) -> bool {
    guest
        .sh(&format!(
            "'{}/{DRIVER}' allow-screen-capture",
            guest_stage()
        ))
        .ok()
        .and_then(|output| serde_json::from_slice::<serde_json::Value>(&output.stdout).ok())
        .is_some_and(|value| value["pressed"] == true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_node_reads_what_the_user_reads() {
        let nodes: Vec<Node> = serde_json::from_str(
            r#"[
              {"depth":2,"role":"AXStaticText","id":"dashboard.status","value":"Idle · filesystem","frame":[509,368,91,19]},
              {"depth":2,"role":"AXCheckBox","id":"dashboard.autolaunch","desc":"Start Vapor at login","value":"1","enabled":true},
              {"depth":3,"role":"AXMenuItem","title":"Sync now","enabled":false,"frame":[0,900,0,0]}
            ]"#,
        )
        .expect("the driver output parses");
        assert_eq!(nodes[0].text(), "Idle · filesystem");
        assert!(nodes[0].is_on_screen());
        assert!(nodes[1].is_on() && nodes[1].is_enabled());
        assert_eq!(nodes[2].text(), "Sync now");
        assert!(!nodes[2].is_enabled() && !nodes[2].is_on_screen());
    }

    #[test]
    fn a_selector_with_a_quote_stays_one_argument() {
        assert_eq!(quote("title=Don't"), r"'title=Don'\''t'");
    }
}
