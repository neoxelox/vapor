//! The surfaces of the installed app: the menu bar item, the main
//! window, Settings, and Quit. These scenarios share one guest; each
//! starts from a state the harness sets, so their order does not
//! matter.

use std::time::Duration;

use vapor_e2e::wait::{hold_for, wait_until};
use vapor_e2e::{Failure, ensure};
use vapor_shared::constants::service::{DAEMON_LABEL, HEALTH_TICK_INTERVAL_SECONDS};

use crate::scenario::{
    Ctx, DASHBOARD_AUTO_LAUNCH, DASHBOARD_STATUS, DIAGNOSTICS_VERSION, GuestKind, MAIN_WINDOW,
    MENU_AUTO_LAUNCH, MENU_BAR_ITEM, MENU_OPEN, MENU_QUIT, MENU_RESTART, MENU_SYNC_NOW,
    PROCESS_TIMEOUT, SETTINGS_AUTO_LAUNCH, SETTINGS_USE_GITIGNORE, SIDEBAR_DASHBOARD,
    SIDEBAR_DIAGNOSTICS, Scenario, Start,
};
use crate::ui::Node;

/// The Settings item of the app menu. SwiftUI adds it and gives it no
/// identifier; its title is the system's own.
const APP_MENU_SETTINGS: &str = "title=Settings…";
/// The close button of a window, a control of macOS.
const CLOSE_BUTTON: &str = "subrole=AXCloseButton";

pub fn scenarios() -> Vec<Scenario> {
    vec![
        Scenario {
            id: "V10",
            name: "menu",
            proves: "a click on the menu bar item opens a menu with a status line and every action, and a second click closes it",
            guest: GuestKind::Shared(Start::Unconfigured),
            run: menu,
        },
        Scenario {
            id: "V11",
            name: "main-window",
            proves: "Open Vapor shows the main window, a second Open Vapor focuses the same window, and closing it leaves the menu bar item and the daemon",
            guest: GuestKind::Shared(Start::Unconfigured),
            run: main_window,
        },
        Scenario {
            id: "V12",
            name: "settings",
            proves: "a setting changed in the Settings window reaches vapor.json",
            guest: GuestKind::Shared(Start::Unconfigured),
            run: settings,
        },
        Scenario {
            id: "V13",
            name: "quit",
            proves: "Quit Vapor stops the daemon and ends the app, and nothing starts them again",
            guest: GuestKind::Shared(Start::Unconfigured),
            run: quit,
        },
    ]
}

fn menu(ctx: &mut Ctx) -> Result<(), Failure> {
    // A started app with auto-launch on offers every action. The
    // wait only makes the screenshot show a daemon that is up.
    wait_until(
        PROCESS_TIMEOUT,
        "the daemon to answer `vapor status`",
        || ctx.status().is_some(),
    )?;
    ctx.open_menu()?;
    for key in [
        MENU_OPEN,
        MENU_AUTO_LAUNCH,
        MENU_SYNC_NOW,
        MENU_RESTART,
        MENU_QUIT,
    ] {
        let selector = ctx.menu_item(key)?;
        let node = ctx.app().wait_for(&selector, Some(MENU_BAR_ITEM))?;
        ensure!(
            node.is_on_screen() && node.is_enabled(),
            "the menu item `{}` must be on the screen and enabled",
            node.text()
        );
    }
    // The status line joins the state and the provider with the
    // separator of the catalog.
    let separator = ctx.copy.get("status_line_format")?.replace("%@", "");
    let status = ctx
        .app()
        .wait_for(&format!("title~={separator}"), Some(MENU_BAR_ITEM))?;
    ensure!(
        !status.is_enabled(),
        "the status line `{}` must not be an action",
        status.text()
    );
    ctx.note(format!("the menu's status line reads: {}", status.text()));
    ctx.shot("menu")?;

    ctx.close_menu()
}

fn main_window_count(ctx: &Ctx) -> Result<usize, Failure> {
    let identifier = MAIN_WINDOW.trim_start_matches("id=");
    Ok(ctx
        .app()
        .tree()?
        .iter()
        .filter(|node| node.role == "AXWindow" && node.id.as_deref() == Some(identifier))
        .count())
}

fn main_window(ctx: &mut Ctx) -> Result<(), Failure> {
    ensure!(
        ctx.main_window_absent(),
        "the app must start with no main window"
    );
    ctx.open_main_window()?;
    let app = ctx.app();
    app.wait_for(DASHBOARD_STATUS, Some(MAIN_WINDOW))?;
    app.wait_for(DASHBOARD_AUTO_LAUNCH, Some(MAIN_WINDOW))?;
    ctx.shot("dashboard")?;

    // A second Open Vapor must not make a second window.
    ctx.open_main_window()?;
    ensure!(
        main_window_count(ctx)? == 1,
        "Open Vapor made a second main window"
    );

    ctx.app().click(SIDEBAR_DIAGNOSTICS, Some(MAIN_WINDOW))?;
    let version = ctx.app().wait_for(DIAGNOSTICS_VERSION, Some(MAIN_WINDOW))?;
    ensure!(
        version.text().contains(&ctx.build.version),
        "Diagnostics shows `{}`, the build is {}",
        version.text(),
        ctx.build.version
    );
    ctx.shot("diagnostics")?;
    ctx.app().click(SIDEBAR_DASHBOARD, Some(MAIN_WINDOW))?;
    ctx.app().wait_for(DASHBOARD_STATUS, Some(MAIN_WINDOW))?;

    // Closing the window is a UI action only.
    ctx.app().click(CLOSE_BUTTON, Some(MAIN_WINDOW))?;
    ctx.app().wait_gone(MAIN_WINDOW, None)?;
    // A stop of the daemon goes through the CLI and takes a moment,
    // so one look right after the click would miss it.
    hold_for(
        Duration::from_secs(8),
        "the app and the daemon keep running after the window closes",
        || ctx.process_runs("Vapor") && ctx.job_runs(DAEMON_LABEL),
    )?;
    ctx.wait_menu_bar_item()?;
    ctx.shot("window-closed")
}

fn settings(ctx: &mut Ctx) -> Result<(), Failure> {
    // The app menu is on the screen only while the app has a window.
    ctx.open_main_window()?;
    ctx.app().press(APP_MENU_SETTINGS, None)?;
    let toggle = ctx.app().wait_for(SETTINGS_USE_GITIGNORE, None)?;
    ctx.app().wait_for(SETTINGS_AUTO_LAUNCH, None)?;
    ctx.shot("settings")?;

    let key = "useGitignore";
    let before = ctx.vapor_ok(&format!("config get {key}"))?;
    ensure!(
        toggle.is_on() == (before.trim() == "true"),
        "the toggle shows {} and vapor.json says {}",
        toggle.is_on(),
        before.trim()
    );
    ctx.app().click(SETTINGS_USE_GITIGNORE, None)?;
    let expected = if toggle.is_on() { "false" } else { "true" };
    wait_until(
        PROCESS_TIMEOUT,
        "the changed setting to reach vapor.json",
        || {
            ctx.vapor_ok(&format!("config get {key}"))
                .is_ok_and(|value| value.trim() == expected)
        },
    )?;
    ctx.app().wait(
        SETTINGS_USE_GITIGNORE,
        None,
        PROCESS_TIMEOUT,
        "the toggle to show the new value",
        |node: &Node| node.is_on() != toggle.is_on(),
    )?;
    ctx.shot("settings-changed")
}

fn quit(ctx: &mut Ctx) -> Result<(), Failure> {
    ctx.wait_service_running()?;
    ctx.open_menu()?;
    ctx.shot("before-quit")?;
    ctx.click_menu_item(MENU_QUIT)?;

    wait_until(PROCESS_TIMEOUT, "the app to end", || {
        !ctx.process_runs("Vapor")
    })?;
    wait_until(PROCESS_TIMEOUT, "the daemon to stop", || {
        !ctx.process_runs("vapord")
    })?;
    ensure!(
        ctx.status().is_none(),
        "`vapor status` still gets an answer after Quit Vapor"
    );
    // The supervisor restarts a daemon that crashed, never one the
    // user stopped. It looks once in each health interval, so the hold
    // is longer than one interval.
    hold_for(
        Duration::from_secs(HEALTH_TICK_INTERVAL_SECONDS + 10),
        "neither the app nor the daemon starts again after Quit Vapor",
        || !ctx.process_runs("Vapor") && !ctx.process_runs("vapord"),
    )?;
    ctx.shot("after-quit")
}
