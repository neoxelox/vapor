//! The scenario model: what a scenario declares, and the context it
//! runs in. A scenario is one behaviour of the installed product,
//! proven on the screen of a guest and through the CLI in that guest.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::Duration;

use vapor_e2e::wait::wait_until;
use vapor_e2e::{Failure, ensure};
use vapor_shared::constants::service::{DAEMON_LABEL, SUPERVISOR_LABEL};

use crate::guest::{Guest, guest_results, guest_stage};
use crate::ui::{self, Node, Ui};

/// The bundle identifier of the Mac app (`AGENTS.md` §2.2).
pub const APP_BUNDLE_ID: &str = "sh.arn.vapor";
/// Where the installer puts the app and the link to its CLI.
pub const APP_PATH: &str = "/Applications/Vapor.app";
pub const CLI_PATH: &str = "/usr/local/bin/vapor";
/// The installer under test, as `apps/macos/scripts/package.sh` names it.
pub const INSTALLER: &str = "Vapor-macos-arm64.pkg";
/// The sync roots of a scenario's profile, under the guest's home.
pub const LOCAL_ROOT: &str = "VaporTest/local";
pub const CLOUD_ROOT: &str = "VaporTest/cloud";

/// The app reads the daemon's state on a 30-second timer, so a change
/// in the daemon reaches the screen within one tick plus a margin.
pub const STATUS_TIMEOUT: Duration = Duration::from_secs(50);

/// How long the app, the daemon, or a job gets to start or to stop.
pub const PROCESS_TIMEOUT: Duration = Duration::from_secs(30);

/// The state of Vapor that a shared guest is put into before a
/// scenario, so the scenarios that share it do not depend on order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Start {
    /// Installed and started, with no sync profile.
    Unconfigured,
    /// Installed and started, with one two-way profile between two
    /// folders of the guest (the filesystem provider).
    Profile,
}

/// Which guest a scenario needs. It follows from what the scenario
/// tests: an install or a first start needs a guest with no trace of
/// Vapor, and a look at a screen does not.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GuestKind {
    /// A fresh clone with no Vapor on it. The scenario installs.
    Fresh,
    /// The guest that scenarios of the installed app share, in turn.
    Shared(Start),
}

pub type ScenarioFn = fn(&mut Ctx) -> Result<(), Failure>;

#[derive(Clone, Copy, Debug)]
pub struct Scenario {
    /// Stable id (`V03`) used by `--only` and in reports.
    pub id: &'static str,
    /// Kebab-case name.
    pub name: &'static str,
    /// One line stating what a pass proves.
    pub proves: &'static str,
    pub guest: GuestKind,
    pub run: ScenarioFn,
}

impl Scenario {
    /// The directory name of the scenario's evidence.
    pub fn slug(&self) -> String {
        format!("{}-{}", self.id, self.name)
    }
}

/// The English copy of the app, read from the catalog in the
/// repository, so a scenario compares the screen with what the build
/// shows and not with a sentence written into the scenario.
#[derive(Clone, Debug, Default)]
pub struct Copy {
    strings: BTreeMap<String, String>,
}

impl Copy {
    pub fn load(repo_root: &Path) -> Result<Self, Failure> {
        let path = repo_root.join("assets").join("locales").join("en.json");
        let text = fs::read_to_string(&path)
            .map_err(|error| Failure::new(format!("cannot read {}: {error}", path.display())))?;
        Ok(Self {
            strings: serde_json::from_str(&text)?,
        })
    }

    /// The text of `key`. A key the catalog lacks is a failure of the
    /// scenario, not an empty string that matches everything.
    pub fn get(&self, key: &str) -> Result<&str, Failure> {
        self.strings
            .get(key)
            .map(String::as_str)
            .ok_or_else(|| Failure::new(format!("the English catalog has no key `{key}`")))
    }
}

/// The build under test, as the repository describes it.
#[derive(Clone, Debug, Default)]
pub struct Build {
    /// The product version, from the root `VERSION` file.
    pub version: String,
}

impl Build {
    pub fn load(repo_root: &Path) -> Result<Self, Failure> {
        let path = repo_root.join("VERSION");
        let version = fs::read_to_string(&path)
            .map_err(|error| Failure::new(format!("cannot read {}: {error}", path.display())))?
            .trim()
            .to_string();
        ensure!(!version.is_empty(), "{} is empty", path.display());
        Ok(Self { version })
    }
}

/// What a scenario works with: its guest, its evidence directory, and
/// the steps every scenario shares.
pub struct Ctx<'a> {
    pub guest: &'a mut Guest,
    pub copy: &'a Copy,
    pub build: &'a Build,
    /// The name of the scenario's evidence directory.
    pub slug: String,
    /// The scenario's directory inside the results share, as the guest
    /// and the host name it.
    guest_dir: String,
    host_dir: PathBuf,
    step: u32,
    /// How many video files the scenario started.
    videos: u32,
    pub notes: Vec<String>,
}

impl<'a> Ctx<'a> {
    pub fn new(
        guest: &'a mut Guest,
        copy: &'a Copy,
        build: &'a Build,
        slug: &str,
    ) -> Result<Self, Failure> {
        let host_dir = guest.results().join(slug);
        fs::create_dir_all(&host_dir)?;
        Ok(Self {
            guest,
            copy,
            build,
            slug: slug.to_string(),
            guest_dir: format!("{}/{slug}", guest_results()),
            host_dir,
            step: 0,
            videos: 0,
            notes: Vec::new(),
        })
    }

    /// The scenario's evidence directory on the host.
    pub fn dir(&self) -> &Path {
        &self.host_dir
    }

    pub fn note(&mut self, text: impl Into<String>) {
        self.notes.push(text.into());
    }

    /// The driver, bound to the Vapor app.
    pub fn app(&self) -> Ui<'_> {
        Ui::new(self.guest, APP_BUNDLE_ID)
    }

    // -- Evidence ---------------------------------------------------

    /// Starts a video of the screen. The first file of a scenario is
    /// `scenario.mov`; a scenario that restarts the guest gets one
    /// more file for each boot, because a restart ends the recording.
    pub fn start_video(&mut self) {
        self.videos += 1;
        let file = match self.videos {
            1 => "scenario.mov".to_string(),
            part => format!("scenario-{part}.mov"),
        };
        let _ = self.guest.sh(&format!(
            "nohup screencapture -x -v '{}/{file}' >/dev/null 2>&1 &",
            self.guest_dir
        ));
    }

    /// Ends the video. An interrupt makes `screencapture` finish the
    /// file.
    pub fn stop_video(&self) {
        let _ = self.guest.sh("pkill -INT -x screencapture");
        let _ = wait_until(Duration::from_secs(15), "the video to be written", || {
            !self.guest.succeeds("pgrep -x screencapture >/dev/null")
        });
    }

    /// Restarts the guest's operating system, as a user restarts a
    /// Mac, with a video before the restart and one after it.
    pub fn restart_guest(&mut self) -> Result<(), Failure> {
        self.stop_video();
        self.guest.reboot()?;
        self.start_video();
        Ok(())
    }

    /// A screenshot of the whole screen, named after the step. The
    /// notifications of macOS are closed first so they do not cover
    /// the app.
    pub fn shot(&mut self, step: &str) -> Result<(), Failure> {
        ui::close_notifications(self.guest);
        self.capture(step)
    }

    /// A screenshot that keeps the notifications of macOS, for the
    /// scenario that asserts them.
    pub fn shot_with_notifications(&mut self, step: &str) -> Result<(), Failure> {
        self.capture(step)
    }

    fn capture(&mut self, step: &str) -> Result<(), Failure> {
        self.step += 1;
        let file = format!("{}/{:02}-{step}.png", self.guest_dir, self.step);
        // macOS can ask again whether the guest agent may capture the
        // screen; the dialog would be in the picture.
        ui::allow_screen_capture(self.guest);
        self.guest
            .sh_ok(&format!("screencapture -x '{file}'"))
            .map(drop)
    }

    /// Writes every control of the app to `controls.json`, for a
    /// reader of a failure.
    pub fn dump_controls(&self) {
        if let Ok(nodes) = self.app().tree() {
            let lines: Vec<serde_json::Value> = nodes
                .iter()
                .map(|node| {
                    serde_json::json!({
                        "depth": node.depth, "role": node.role, "subrole": node.subrole,
                        "id": node.id, "title": node.title, "desc": node.desc,
                        "value": node.value, "enabled": node.enabled, "frame": node.frame,
                    })
                })
                .collect();
            if let Ok(text) = serde_json::to_string_pretty(&lines) {
                let _ = fs::write(self.host_dir.join("controls.json"), text);
            }
        }
    }

    /// Copies the daemon's logs and the state of its launchd jobs out
    /// of the guest.
    pub fn collect_logs(&self) {
        let dir = &self.guest_dir;
        let _ = self.guest.sh(&format!(
            "mkdir -p '{dir}/logs' && cp -R ~/.vapor/logs/. '{dir}/logs/' 2>/dev/null; \
             launchctl list | grep -i vapor > '{dir}/launchd.txt' 2>/dev/null; \
             '{CLI_PATH}' status > '{dir}/status.txt' 2>&1; true"
        ));
    }

    // -- The product in the guest -----------------------------------

    /// Installs the build under test with the system installer, as a
    /// user's double click on the package does.
    pub fn install(&self) -> Result<(), Failure> {
        self.guest
            .sh_ok(&format!(
                "sudo installer -pkg '{}/{INSTALLER}' -target /",
                guest_stage()
            ))
            .map(drop)
    }

    /// Starts the installed app the way Finder does, and waits for its
    /// menu bar item.
    pub fn launch_app(&self) -> Result<(), Failure> {
        self.guest.sh_ok(&format!("open '{APP_PATH}'"))?;
        self.wait_menu_bar_item()
    }

    pub fn wait_menu_bar_item(&self) -> Result<(), Failure> {
        self.app()
            .wait(
                MENU_BAR_ITEM,
                None,
                PROCESS_TIMEOUT,
                "the menu bar item of Vapor to appear",
                Node::is_on_screen,
            )
            .map(drop)
    }

    /// Runs the installed `vapor` CLI in the guest.
    pub fn vapor(&self, arguments: &str) -> Result<Output, Failure> {
        self.guest.sh(&format!("'{CLI_PATH}' {arguments}"))
    }

    /// Runs the CLI and fails unless it exits with zero.
    pub fn vapor_ok(&self, arguments: &str) -> Result<String, Failure> {
        self.guest.sh_ok(&format!("'{CLI_PATH}' {arguments}"))
    }

    /// `vapor status --json`, or `None` while the daemon does not answer.
    pub fn status(&self) -> Option<serde_json::Value> {
        let output = self.vapor("status --json").ok()?;
        if !output.status.success() {
            return None;
        }
        serde_json::from_slice(&output.stdout).ok()
    }

    /// Whether a process with exactly this name runs in the guest.
    pub fn process_runs(&self, name: &str) -> bool {
        self.guest
            .succeeds(&format!("pgrep -x '{name}' >/dev/null"))
    }

    /// Whether launchd has a running job with this label for the user.
    pub fn job_runs(&self, label: &str) -> bool {
        // `launchctl list` prints `<pid> <status> <label>`, with `-`
        // for a job that is loaded but has no process.
        self.guest.succeeds(&format!(
            "launchctl list | awk -v label='{label}' '$3 == label && $1 != \"-\" {{ found = 1 }} END {{ exit !found }}'"
        ))
    }

    /// Whether launchd knows a job with this label at all.
    pub fn job_loaded(&self, label: &str) -> bool {
        self.guest.succeeds(&format!(
            "launchctl list | awk -v label='{label}' '$3 == label {{ found = 1 }} END {{ exit !found }}'"
        ))
    }

    /// Whether macOS has the app on record as a login item that is
    /// switched on. `sfltool` prints the records of the system's
    /// background task manager; the record of an app ends with its
    /// bundle identifier, a few lines after its disposition.
    pub fn login_item_registered(&self) -> bool {
        self.guest.succeeds(&format!(
            "sudo sfltool dumpbtm 2>/dev/null | grep -B6 'Bundle Identifier: {APP_BUNDLE_ID}$' \
             | grep -q 'Disposition: \\[enabled'"
        ))
    }

    /// Whether macOS has written the login item of the app to its
    /// database on disk. macOS does that a few seconds after the app
    /// registers the item, and a restart before it loses the item.
    pub fn login_item_saved(&self) -> bool {
        self.login_item_registered()
            && self.guest.succeeds(&format!(
                "sudo strings -a /private/var/db/com.apple.backgroundtaskmanagement/*.btm \
                 | grep -q '2\\.{}$'",
                APP_BUNDLE_ID.replace('.', "\\.")
            ))
    }

    /// Waits until the daemon and its supervisor both run under launchd.
    pub fn wait_service_running(&self) -> Result<(), Failure> {
        wait_until(
            PROCESS_TIMEOUT,
            "launchd to run the daemon job and the supervisor job",
            || self.job_runs(DAEMON_LABEL) && self.job_runs(SUPERVISOR_LABEL),
        )
    }

    // -- The screen ------------------------------------------------

    /// The selector of the menu item that shows the text of `key`.
    /// macOS gives the items of this menu no identifier, so the text
    /// of the catalog is what names one.
    pub fn menu_item(&self, key: &str) -> Result<String, Failure> {
        Ok(format!("title={}", self.copy.get(key)?))
    }

    fn menu_is_open(&self) -> Result<bool, Failure> {
        Ok(self
            .app()
            .find(&self.menu_item(MENU_OPEN)?, Some(MENU_BAR_ITEM))?
            .is_some_and(|node| node.is_on_screen()))
    }

    /// Opens the menu of the menu bar item with a click, and waits for
    /// its items.
    pub fn open_menu(&self) -> Result<(), Failure> {
        if self.menu_is_open()? {
            return Ok(());
        }
        self.app().click(MENU_BAR_ITEM, None)?;
        wait_until(ui::APPEAR_TIMEOUT, "the menu of Vapor to open", || {
            self.menu_is_open().unwrap_or(false)
        })
    }

    /// Closes the menu with a second click on the menu bar item.
    pub fn close_menu(&self) -> Result<(), Failure> {
        if !self.menu_is_open()? {
            return Ok(());
        }
        self.app().click(MENU_BAR_ITEM, None)?;
        wait_until(ui::APPEAR_TIMEOUT, "the menu of Vapor to close", || {
            !self.menu_is_open().unwrap_or(true)
        })
    }

    /// Opens the menu and clicks the item that shows the text of `key`.
    pub fn click_menu_item(&self, key: &str) -> Result<Node, Failure> {
        self.open_menu()?;
        self.app().click(&self.menu_item(key)?, Some(MENU_BAR_ITEM))
    }

    /// Opens the menu and waits for an item whose text contains the
    /// text of `key`: the status line carries the state's label.
    pub fn wait_menu_text(&self, key: &str) -> Result<Node, Failure> {
        self.open_menu()?;
        let expected = self.copy.get(key)?;
        self.app().wait(
            &format!("title~={expected}"),
            Some(MENU_BAR_ITEM),
            ui::APPEAR_TIMEOUT,
            &format!("an item of the menu to show \"{expected}\""),
            Node::is_on_screen,
        )
    }

    /// Whether the app runs and shows no main window. An app that
    /// crashed has no window either, and that is not what a scenario
    /// means by "no window".
    pub fn main_window_absent(&self) -> bool {
        self.process_runs("Vapor") && self.app().lacks(MAIN_WINDOW, None)
    }

    /// Opens the main window from the menu, as a user does.
    pub fn open_main_window(&self) -> Result<(), Failure> {
        self.click_menu_item(MENU_OPEN)?;
        self.app().wait_for(MAIN_WINDOW, None).map(drop)
    }

    /// Fails unless `selector` shows the text of `key` in the English
    /// catalog within `timeout`.
    pub fn wait_text(
        &self,
        selector: &str,
        within: Option<&str>,
        key: &str,
        timeout: Duration,
    ) -> Result<Node, Failure> {
        let expected = self.copy.get(key)?.to_string();
        self.app().wait(
            selector,
            within,
            timeout,
            &format!("`{selector}` to show \"{expected}\""),
            |node| node.text().contains(&expected),
        )
    }

    /// Fails unless the control is there and enabled, or there and
    /// disabled.
    pub fn expect_enabled(&self, selector: &str, enabled: bool) -> Result<(), Failure> {
        let node = self.app().wait_for(selector, None)?;
        ensure!(
            node.is_enabled() == enabled,
            "the control `{selector}` is {}, expected {}",
            if node.is_enabled() {
                "enabled"
            } else {
                "disabled"
            },
            if enabled { "enabled" } else { "disabled" }
        );
        Ok(())
    }
}

// How a scenario names a control. A control in a window has an
// accessibility identifier; the source of truth for those is
// `apps/macos/Sources/VaporCore/AccessibilityID.swift`, and a test
// below fails when an `id=` selector anywhere in this crate names an
// identifier that is not in that file.

/// The menu bar item itself. SwiftUI gives it no identifier, and the
/// app has exactly one. Its menu is searched inside it.
pub const MENU_BAR_ITEM: &str = "subrole=AXMenuExtra";
/// The catalog keys of the items of that menu; see [`Ctx::menu_item`].
pub const MENU_OPEN: &str = "menubar_open_vapor";
pub const MENU_AUTO_LAUNCH: &str = "menubar_toggle_auto_launch";
pub const MENU_SYNC_NOW: &str = "menubar_sync_now";
pub const MENU_RESTART: &str = "menubar_restart_daemon";
pub const MENU_QUIT: &str = "menubar_quit_vapor";
pub const MAIN_WINDOW: &str = "id=main-window";
pub const SIDEBAR_DASHBOARD: &str = "id=sidebar.dashboard";
pub const SIDEBAR_DIAGNOSTICS: &str = "id=sidebar.diagnostics";
pub const DASHBOARD_STATUS: &str = "id=dashboard.status";
pub const DASHBOARD_STATUS_DETAIL: &str = "id=dashboard.status-detail";
pub const DASHBOARD_SYNC_DETAIL: &str = "id=dashboard.sync-detail";
pub const DASHBOARD_SYNC_NOW: &str = "id=dashboard.sync-now";
pub const DASHBOARD_AUTO_LAUNCH: &str = "id=dashboard.auto-launch";
pub const DIAGNOSTICS_VERSION: &str = "id=diagnostics.version";
pub const NOTICE_RESTART_REQUIRED: &str = "id=notice.restart-required";
pub const NOTICE_DECISIONS: &str = "id=notice.decisions";
pub const SETTINGS_USE_GITIGNORE: &str = "id=settings.use-gitignore";
pub const SETTINGS_AUTO_LAUNCH: &str = "id=settings.auto-launch";

#[cfg(test)]
mod tests {
    use super::*;

    fn repo_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
    }

    /// Every `"id=<identifier>"` literal in the Rust sources of this
    /// crate, with the file it is in.
    fn id_selectors(directory: &Path, found: &mut Vec<(String, PathBuf)>) {
        for entry in fs::read_dir(directory).expect("a source directory lists") {
            let path = entry.expect("a directory entry reads").path();
            if path.is_dir() {
                id_selectors(&path, found);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                let text = fs::read_to_string(&path).expect("a source file reads");
                for part in text.split("\"id=").skip(1) {
                    let identifier: String = part
                        .chars()
                        .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-'))
                        .collect();
                    // A literal that ends right after the identifier is
                    // a selector; anything else is prose or a format.
                    if !identifier.is_empty() && part[identifier.len()..].starts_with('"') {
                        found.push((identifier, path.clone()));
                    }
                }
            }
        }
    }

    #[test]
    fn every_identifier_a_scenario_uses_is_defined_by_the_app() {
        let swift = repo_root().join("apps/macos/Sources/VaporCore/AccessibilityID.swift");
        let source = fs::read_to_string(&swift).expect("the identifier file of the app exists");
        let mut selectors = Vec::new();
        id_selectors(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
            &mut selectors,
        );
        assert!(
            selectors
                .iter()
                .any(|(identifier, _)| identifier == "main-window"),
            "the scan found no selector; it no longer reads the sources"
        );
        for (identifier, file) in selectors {
            assert!(
                source.contains(&format!("\"{identifier}\"")),
                "{} uses the identifier `{identifier}`, which {} does not define",
                file.display(),
                swift.display()
            );
        }
    }

    #[test]
    fn the_english_catalog_has_every_menu_item_and_refuses_a_missing_key() {
        let copy = Copy::load(&repo_root()).expect("the catalog loads");
        for key in [
            MENU_OPEN,
            MENU_AUTO_LAUNCH,
            MENU_SYNC_NOW,
            MENU_RESTART,
            MENU_QUIT,
        ] {
            assert!(!copy.get(key).expect("a menu item's key").is_empty());
        }
        assert!(copy.get("no_such_key").is_err());
    }
}
