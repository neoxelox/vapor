//! What the installer and the first start do to a Mac that never had
//! Vapor. Each scenario gets a fresh guest, because a second install
//! or a second first start proves nothing about the first.

use std::time::Duration;

use vapor_e2e::wait::{hold_for, wait_until};
use vapor_e2e::{Failure, ensure};
use vapor_shared::constants::service::{DAEMON_LABEL, SUPERVISOR_LABEL};

use crate::guest::{guest_results, guest_stage};
use crate::scenario::{APP_PATH, CLI_PATH, Ctx, GuestKind, MAIN_WINDOW, PROCESS_TIMEOUT, Scenario};
use crate::ui;

/// Names of the debug binaries the e2e harness needs, as the runner
/// stages them under `e2e/` in the stage share.
pub const E2E_BINARIES: &[&str] = &["vapor", "vapord", "vapor-e2e"];

pub fn scenarios() -> Vec<Scenario> {
    vec![
        Scenario {
            id: "V01",
            name: "install",
            proves: "the installer puts Vapor.app in /Applications and `vapor` on the PATH at the version of the build, and starts nothing",
            guest: GuestKind::Fresh,
            run: install,
        },
        Scenario {
            id: "V02",
            name: "first-start",
            proves: "the first start of the app registers the daemon, its supervisor, and the login item, shows the menu bar item, and opens no window",
            guest: GuestKind::Fresh,
            run: first_start,
        },
        Scenario {
            id: "V03",
            name: "start-at-login",
            proves: "after a restart of the Mac the app, the daemon, and the supervisor run again without the user, and no window opens",
            guest: GuestKind::Fresh,
            run: start_at_login,
        },
        Scenario {
            id: "V04",
            name: "service-round-trip",
            proves: "the launchd round trip of Tier E2E (R01) passes on a Mac with the real launchd and no Vapor installed",
            guest: GuestKind::Fresh,
            run: service_round_trip,
        },
    ]
}

fn ensure_no_vapor(ctx: &Ctx, when: &str) -> Result<(), Failure> {
    ensure!(
        !ctx.process_runs("Vapor") && !ctx.process_runs("vapord"),
        "{when}: a Vapor process runs"
    );
    ensure!(
        !ctx.job_loaded(DAEMON_LABEL) && !ctx.job_loaded(SUPERVISOR_LABEL),
        "{when}: launchd has a Vapor job"
    );
    ensure!(
        !ctx.guest.succeeds("test -e ~/.vapor"),
        "{when}: the runtime directory ~/.vapor exists"
    );
    Ok(())
}

fn install(ctx: &mut Ctx) -> Result<(), Failure> {
    ensure!(
        !ctx.guest
            .succeeds(&format!("test -e '{APP_PATH}' || test -e '{CLI_PATH}'")),
        "a fresh guest must not have Vapor"
    );
    ensure_no_vapor(ctx, "before the install")?;

    ctx.install()?;

    for binary in [
        "Contents/MacOS/Vapor",
        "Contents/MacOS/vapord",
        "Contents/Helpers/vapor",
    ] {
        ensure!(
            ctx.guest
                .succeeds(&format!("test -x '{APP_PATH}/{binary}'")),
            "the installed app has no executable {binary}"
        );
    }
    let link = ctx.guest.sh_ok(&format!("readlink '{CLI_PATH}'"))?;
    ensure!(
        link.trim() == format!("{APP_PATH}/Contents/Helpers/vapor"),
        "{CLI_PATH} must link to the CLI inside the app, it links to `{}`",
        link.trim()
    );
    // A new shell of the user finds `vapor` with no path.
    let version = ctx.guest.sh_ok("/bin/zsh -lc 'vapor --version'")?;
    ensure!(
        version.contains(&ctx.build.version),
        "`vapor --version` says `{}`, the build is {}",
        version.trim(),
        ctx.build.version
    );
    ensure_no_vapor(ctx, "after the install and before the first start")?;

    // For the reader of the evidence: the app as Finder shows it.
    ctx.guest.sh_ok(&format!("open -R '{APP_PATH}'"))?;
    wait_until(
        PROCESS_TIMEOUT,
        "Finder to show the Applications folder",
        || {
            ui::windows(ctx.guest).is_ok_and(|all| {
                all.iter()
                    .any(|window| window.owner == "Finder" && window.title == "Applications")
            })
        },
    )?;
    ctx.shot("installed")
}

fn first_start(ctx: &mut Ctx) -> Result<(), Failure> {
    ctx.install()?;
    ctx.launch_app()?;
    ctx.wait_service_running()?;

    for label in [DAEMON_LABEL, SUPERVISOR_LABEL] {
        ensure!(
            ctx.guest
                .succeeds(&format!("test -f ~/Library/LaunchAgents/{label}.plist")),
            "the first start wrote no LaunchAgent for {label}"
        );
    }
    ensure!(
        ctx.guest.succeeds("test -f ~/.vapor/vapor.json"),
        "the first start wrote no ~/.vapor/vapor.json"
    );
    wait_until(
        PROCESS_TIMEOUT,
        "the daemon to answer `vapor status`",
        || ctx.status().is_some(),
    )?;

    wait_until(
        PROCESS_TIMEOUT,
        "macOS to record Vapor as a login item that is switched on",
        || ctx.login_item_registered(),
    )?;
    // macOS tells the user about a new login item with a notification
    // that names the app.
    wait_until(
        PROCESS_TIMEOUT,
        "macOS to show the notification for the login item of Vapor",
        || {
            ui::notifications(ctx.guest).is_ok_and(|all| {
                all.iter()
                    .any(|notification| notification.body.contains("“Vapor”"))
            })
        },
    )?;
    ctx.shot_with_notifications("first-start")?;

    // The app starts in the menu bar only.
    ensure!(
        !ctx.app().exists(MAIN_WINDOW, None),
        "the first start must not open the main window"
    );
    ctx.open_menu()?;
    ctx.shot("menu")
}

fn start_at_login(ctx: &mut Ctx) -> Result<(), Failure> {
    ctx.install()?;
    ctx.launch_app()?;
    ctx.wait_service_running()?;
    // The app registers its login item after the service is up, and
    // macOS writes it to disk a few seconds later. A restart before
    // that loses the item, and proves nothing about a start at login.
    wait_until(
        PROCESS_TIMEOUT,
        "macOS to save Vapor as a login item that is switched on",
        || ctx.login_item_saved(),
    )?;
    ctx.shot("before-restart")?;

    ctx.guest.reboot()?;

    // Nothing starts the app here: the login item must.
    wait_until(PROCESS_TIMEOUT, "the app to start at login", || {
        ctx.process_runs("Vapor")
    })?;
    ctx.wait_menu_bar_item()?;
    ctx.wait_service_running()?;
    wait_until(
        PROCESS_TIMEOUT,
        "the daemon to answer `vapor status`",
        || ctx.status().is_some(),
    )?;
    hold_for(
        Duration::from_secs(5),
        "no main window opens after a start at login",
        || !ctx.app().exists(MAIN_WINDOW, None),
    )?;
    ctx.shot("after-restart")
}

/// Runs scenario R01 of the Tier E2E harness inside the guest. On a
/// contributor's Mac that scenario is refused, because it installs a
/// real LaunchAgent; a fresh guest is the disposable machine it needs.
fn service_round_trip(ctx: &mut Ctx) -> Result<(), Failure> {
    let stage = guest_stage();
    ensure!(
        ctx.guest
            .succeeds(&format!("test -x '{stage}/e2e/vapor-e2e'")),
        "the stage has no e2e harness; run the tier through ./scripts/vm.sh, which builds it"
    );
    let report = format!("{}/{}/e2e-report.json", guest_results(), ctx.slug);
    let root = "$HOME/vapor-e2e";
    let copy = "mkdir -p \"$HOME/vapor-e2e/target/debug\" && cp";
    let binaries: Vec<String> = E2E_BINARIES
        .iter()
        .map(|name| format!("'{stage}/e2e/{name}'"))
        .collect();
    ctx.guest.sh_ok(&format!(
        "{copy} {} \"{root}/target/debug/\"",
        binaries.join(" ")
    ))?;

    let output = ctx.guest.sh(&format!(
        "cd \"{root}\" && ./target/debug/vapor-e2e --repo-root \"{root}\" run --only R01 --full --skip-build --json '{report}' > '{}/{}/e2e-output.txt' 2>&1",
        guest_results(),
        ctx.slug
    ))?;
    let text = std::fs::read_to_string(ctx.dir().join("e2e-report.json")).unwrap_or_default();
    let verdict = serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|report| {
            report["scenarios"]
                .as_array()?
                .iter()
                .find(|scenario| scenario["id"] == "R01")
                .map(|scenario| scenario["verdict"].as_str().unwrap_or_default().to_string())
        })
        .unwrap_or_default();
    ensure!(
        output.status.success() && verdict == "pass",
        "R01 did not pass in the guest (exit {}, verdict `{verdict}`); see e2e-output.txt",
        output.status
    );
    ctx.note("R01 of Tier E2E passed inside the guest");
    Ok(())
}
