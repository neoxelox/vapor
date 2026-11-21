//! What the app shows for each state of the daemon. The daemon is put
//! into the state through the CLI and the two sync folders, and the
//! scenario reads the menu and the Dashboard. The app reads the daemon
//! on a timer, so each assertion waits for the next read.

use vapor_e2e::wait::wait_until;
use vapor_e2e::{Failure, ensure};

use crate::scenario::{
    CLOUD_ROOT, Ctx, DASHBOARD_STATUS, DASHBOARD_SYNC_DETAIL, GuestKind, LOCAL_ROOT, MAIN_WINDOW,
    NOTICE_DECISIONS, NOTICE_RESTART_REQUIRED, PROCESS_TIMEOUT, STATUS_TIMEOUT, Scenario, Start,
};

pub fn scenarios() -> Vec<Scenario> {
    vec![
        Scenario {
            id: "V20",
            name: "state-unconfigured",
            proves: "with no sync profile the app shows that nothing syncs yet and the daemon's reason for it",
            guest: GuestKind::Shared(Start::Unconfigured),
            run: unconfigured,
        },
        Scenario {
            id: "V21",
            name: "state-idle",
            proves: "with a profile and nothing to do the app shows Idle in the menu and on the Dashboard",
            guest: GuestKind::Shared(Start::Profile),
            run: idle,
        },
        Scenario {
            id: "V22",
            name: "state-syncing",
            proves: "files that change in the local folder reach the cloud folder, and the app shows the work while it lasts",
            guest: GuestKind::Shared(Start::Profile),
            run: syncing,
        },
        Scenario {
            id: "V23",
            name: "state-paused",
            proves: "after `vapor pause` the app shows Paused",
            guest: GuestKind::Shared(Start::Profile),
            run: paused,
        },
        Scenario {
            id: "V24",
            name: "state-needs-answer",
            proves: "a one-way mode that the user did not confirm shows Error, the daemon's reason, and that one question waits",
            guest: GuestKind::Shared(Start::Profile),
            run: needs_answer,
        },
        Scenario {
            id: "V25",
            name: "state-restart-required",
            proves: "a setting that needs a restart of the daemon shows the restart notice",
            guest: GuestKind::Shared(Start::Profile),
            run: restart_required,
        },
    ]
}

/// The status line of the menu and of the Dashboard both show `key`.
fn expect_state(ctx: &mut Ctx, key: &str, step: &str) -> Result<(), Failure> {
    ctx.open_main_window()?;
    ctx.wait_text(DASHBOARD_STATUS, Some(MAIN_WINDOW), key, STATUS_TIMEOUT)?;
    ctx.shot(step)?;
    // The menu is built when it opens, from the state the Dashboard
    // already shows.
    ctx.wait_menu_text(key)?;
    ctx.shot(&format!("{step}-menu"))?;
    ctx.close_menu()
}

fn run_state(ctx: &Ctx) -> String {
    ctx.status()
        .and_then(|status| status["run"]["state"].as_str().map(str::to_string))
        .unwrap_or_default()
}

fn wait_run_state(ctx: &Ctx, state: &str) -> Result<(), Failure> {
    wait_until(
        PROCESS_TIMEOUT,
        &format!("the daemon to report the run state `{state}`"),
        || run_state(ctx) == state,
    )
}

fn unconfigured(ctx: &mut Ctx) -> Result<(), Failure> {
    wait_run_state(ctx, "unconfigured")?;
    let reason = ctx
        .status()
        .and_then(|status| status["run"]["reason"].as_str().map(str::to_string))
        .unwrap_or_default();
    ensure!(
        !reason.is_empty(),
        "the daemon gives no reason for `unconfigured`"
    );

    expect_state(ctx, "sync_state_paused_label", "unconfigured")?;
    let detail = ctx.app().wait(
        DASHBOARD_SYNC_DETAIL,
        Some(MAIN_WINDOW),
        STATUS_TIMEOUT,
        "the Dashboard to show the daemon's reason",
        |node| node.text() == reason,
    )?;
    ctx.note(format!("the Dashboard shows the reason: {}", detail.text()));
    Ok(())
}

fn idle(ctx: &mut Ctx) -> Result<(), Failure> {
    wait_run_state(ctx, "running")?;
    ctx.open_main_window()?;
    // Before its first read of the daemon the app already says Idle.
    // The line under the status comes from the daemon only, so its
    // text proves that the state on the screen is one the daemon gave.
    let detail = ctx.app().wait(
        DASHBOARD_SYNC_DETAIL,
        Some(MAIN_WINDOW),
        STATUS_TIMEOUT,
        "the Dashboard to show the daemon's own line under the status",
        |node| !node.text().is_empty(),
    )?;
    ctx.note(format!(
        "under the status the Dashboard shows: {}",
        detail.text()
    ));
    expect_state(ctx, "sync_state_idle_label", "idle")
}

/// The size of the file that keeps the daemon at work. The daemon
/// hashes a file at a fixed pace, measured at about 65 MB each second
/// on an idle guest, so this one stays in the queue for about 50 s:
/// longer than the 30 s between two reads of the app. It is sparse in
/// the local folder and takes its full size in the cloud folder.
const LARGE_FILE_MEGABYTES: u32 = 3200;

fn syncing(ctx: &mut Ctx) -> Result<(), Failure> {
    wait_run_state(ctx, "running")?;
    ctx.open_main_window()?;

    ctx.guest.sh_ok(&format!(
        "mkfile -n {LARGE_FILE_MEGABYTES}m ~/{LOCAL_ROOT}/large.bin && echo small > ~/{LOCAL_ROOT}/small.txt"
    ))?;
    wait_until(PROCESS_TIMEOUT, "the daemon to queue the new files", || {
        ctx.status()
            .is_some_and(|status| status["queued"].as_u64().unwrap_or(0) > 0)
    })?;
    // A device under load shows the same work as Throttled.
    let busy = [
        ctx.copy.get("sync_state_syncing_label")?.to_string(),
        ctx.copy.get("sync_state_throttled_label")?.to_string(),
    ];
    let seen = ctx.app().wait(
        DASHBOARD_STATUS,
        Some(MAIN_WINDOW),
        STATUS_TIMEOUT,
        "the Dashboard to show that work is in the queue",
        |node| busy.iter().any(|label| node.text().contains(label)),
    )?;
    ctx.note(format!(
        "while the files synced the Dashboard showed: {}",
        seen.text()
    ));
    ctx.shot("syncing")?;

    // Both files reach the other folder, byte for byte.
    wait_until(
        STATUS_TIMEOUT * 3,
        "the cloud folder to hold the same files as the local folder",
        || {
            ctx.guest.succeeds(&format!(
                "cmp -s ~/{LOCAL_ROOT}/small.txt ~/{CLOUD_ROOT}/small.txt && cmp -s ~/{LOCAL_ROOT}/large.bin ~/{CLOUD_ROOT}/large.bin"
            ))
        },
    )?;
    wait_until(PROCESS_TIMEOUT, "the daemon's queue to be empty", || {
        ctx.status()
            .is_some_and(|status| status["queued"].as_u64() == Some(0))
    })?;
    expect_state(ctx, "sync_state_idle_label", "idle-again")
}

fn paused(ctx: &mut Ctx) -> Result<(), Failure> {
    wait_run_state(ctx, "running")?;
    ctx.vapor_ok("pause")?;
    wait_run_state(ctx, "paused")?;
    expect_state(ctx, "sync_state_paused_label", "paused")
}

fn needs_answer(ctx: &mut Ctx) -> Result<(), Failure> {
    wait_run_state(ctx, "running")?;
    // A one-way mode mirrors one side onto the other, so the daemon
    // holds the profile until the user confirms it.
    ctx.vapor_ok("config set syncMode pull-only")?;
    ctx.vapor_ok("service restart")?;
    wait_run_state(ctx, "error")?;
    wait_until(
        PROCESS_TIMEOUT,
        "the daemon to count one open question",
        || {
            ctx.status()
                .is_some_and(|status| status["decisions"].as_u64() == Some(1))
        },
    )?;

    expect_state(ctx, "sync_state_error_label", "needs-answer")?;
    ctx.open_main_window()?;
    // Under the status the Dashboard gives the daemon's reason, which
    // says what the user must confirm.
    let reason = ctx
        .status()
        .and_then(|status| status["run"]["reason"].as_str().map(str::to_string))
        .unwrap_or_default();
    ensure!(!reason.is_empty(), "the daemon gives no reason for `error`");
    ctx.app().wait(
        DASHBOARD_SYNC_DETAIL,
        Some(MAIN_WINDOW),
        STATUS_TIMEOUT,
        "the Dashboard to show the daemon's reason",
        |node| node.text() == reason,
    )?;
    ctx.wait_text(
        NOTICE_DECISIONS,
        Some(MAIN_WINDOW),
        "attention_decision_one",
        STATUS_TIMEOUT,
    )?;
    ctx.shot("question-waits")
}

fn restart_required(ctx: &mut Ctx) -> Result<(), Failure> {
    wait_run_state(ctx, "running")?;
    // The sync folders are read at the start of the daemon only.
    ctx.guest.sh_ok("mkdir -p ~/VaporTest/other-cloud")?;
    ctx.vapor_ok("config set cloudRoot \"$HOME/VaporTest/other-cloud\"")?;
    wait_until(
        PROCESS_TIMEOUT,
        "the daemon to report that a restart is needed",
        || {
            ctx.status()
                .is_some_and(|status| status["restartRequired"].is_string())
        },
    )?;
    ctx.open_main_window()?;
    let notice = ctx.app().wait(
        NOTICE_RESTART_REQUIRED,
        Some(MAIN_WINDOW),
        STATUS_TIMEOUT,
        "the Dashboard to show the restart notice",
        |node| !node.text().is_empty(),
    )?;
    ctx.note(format!("the notice reads: {}", notice.text()));
    ctx.shot("restart-required")
}
