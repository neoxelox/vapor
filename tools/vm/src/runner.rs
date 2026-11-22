//! A run: check the host, stage the build, give every scenario the
//! guest it needs, keep the evidence, and leave no VM behind.
//!
//! Scenarios of the installed app share one guest and run in turn;
//! scenarios of the install each get a fresh clone. Two macOS guests
//! may run at a time, so the two groups run side by side when both
//! places are free.

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use sha2::{Digest, Sha256};
use vapor_e2e::report::Verdict;
use vapor_e2e::wait::wait_until;
use vapor_e2e::{Failure, ensure};

use crate::guest::{Guest, Session, guest_results, guest_stage};
use crate::scenario::{
    APP_PATH, Build, CLI_PATH, CLOUD_ROOT, Copy, Ctx, GuestKind, INSTALLER, LOCAL_ROOT, Scenario,
    Start,
};
use crate::scenarios::{self, install::E2E_BINARIES};
use crate::tart::Vm;
use crate::tart::{self, MAX_MACOS_GUESTS, MIN_TART_VERSION, VM_PREFIX};
use crate::ui::{self, DRIVER};

/// How long a run waits for a place when two macOS guests already run.
const SLOT_TIMEOUT: Duration = Duration::from_secs(600);

/// The tag of a checkout in the names of its guests: Tart's store is
/// shared by every checkout of the repository on a machine, and a
/// checkout only ever stops or deletes the guests that carry its tag.
/// `scripts/clean.sh` computes the same tag from the same path.
pub fn checkout_tag(repo_root: &Path) -> Result<String, Failure> {
    let physical = vapor_shared::paths::canonicalize(repo_root).map_err(|error| {
        Failure::new(format!("cannot resolve {}: {error}", repo_root.display()))
    })?;
    let digest = Sha256::digest(physical.to_string_lossy().as_bytes());
    Ok(digest
        .iter()
        .take(4)
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

/// Where the tier keeps what it stages and what a run produces.
#[derive(Clone, Debug)]
pub struct Paths {
    pub repo_root: PathBuf,
    /// See [`checkout_tag`].
    pub checkout: String,
    /// `<repo>/.vapor/vm`, removed by `./scripts/clean.sh`.
    pub vm_root: PathBuf,
    /// The directory every guest sees read-only: the installer, the
    /// driver, and the e2e binaries.
    pub stage: PathBuf,
}

impl Paths {
    pub fn new(repo_root: &Path) -> Result<Self, Failure> {
        let vm_root = repo_root.join(".vapor").join("vm");
        Ok(Self {
            repo_root: repo_root.to_path_buf(),
            checkout: checkout_tag(repo_root)?,
            stage: vm_root.join("stage"),
            vm_root,
        })
    }

    /// The name of a guest of this checkout: `vapor-vm-<tag>-<rest>`.
    fn guest_name(&self, rest: &str) -> String {
        format!("{VM_PREFIX}{}-{rest}", self.checkout)
    }

    fn target_debug(&self) -> PathBuf {
        std::env::var_os("CARGO_TARGET_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| self.repo_root.join("target"))
            .join("debug")
    }
}

pub struct RunOptions {
    pub repo_root: PathBuf,
    /// Scenario ids or names; empty means all.
    pub only: Vec<String>,
    /// Keep the guest of a failed scenario, stopped, for a look.
    pub keep: bool,
    /// Record a video of each scenario, for a change that moves on
    /// the screen.
    pub video: bool,
    pub json: Option<PathBuf>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ScenarioResult {
    pub id: String,
    pub name: String,
    pub proves: String,
    pub verdict: Verdict,
    pub reasons: Vec<String>,
    pub seconds: f64,
    /// The scenario's directory of screenshots and logs.
    pub evidence: String,
    pub notes: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Report {
    pub run: String,
    pub image: String,
    pub build: String,
    pub passed: usize,
    pub failed: usize,
    pub seconds: f64,
    pub scenarios: Vec<ScenarioResult>,
}

pub fn discover_repo_root() -> Result<PathBuf, Failure> {
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        candidates.extend(exe.ancestors().map(Path::to_path_buf));
    }
    if let Ok(cwd) = std::env::current_dir() {
        candidates.extend(cwd.ancestors().map(Path::to_path_buf));
    }
    candidates
        .into_iter()
        .find(|candidate| {
            candidate.join("Cargo.toml").is_file()
                && candidate.join("scripts").join("vm.sh").is_file()
        })
        .ok_or_else(|| Failure::new("cannot find the repository root; pass --repo-root"))
}

// -- The host ---------------------------------------------------------

/// A macOS guest cannot be newer than its host, and the image is
/// macOS 27.
const MIN_HOST_MACOS: u32 = 27;

/// The major version of the macOS this process runs on.
fn host_macos_major() -> Option<u32> {
    let output = Command::new("sw_vers")
        .arg("-productVersion")
        .output()
        .ok()?;
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .split('.')
        .next()?
        .parse()
        .ok()
}

/// Fails with what to do when this machine cannot run the tier.
pub fn check_host() -> Result<(), Failure> {
    ensure!(
        cfg!(all(target_os = "macos", target_arch = "aarch64")),
        "Tier VM runs macOS guests, which needs a Mac with Apple silicon"
    );
    let host = host_macos_major();
    ensure!(
        host.is_some_and(|major| major >= MIN_HOST_MACOS),
        "the guest runs macOS {MIN_HOST_MACOS}, which needs macOS {MIN_HOST_MACOS} or newer on this Mac; it runs macOS {}",
        host.map_or_else(
            || "of an unknown version".to_string(),
            |major| major.to_string()
        )
    );
    let version = tart::version().map_err(|failure| {
        Failure::new(format!(
            "{failure}\nTier VM needs Tart. Install it by hand, one time:\n  brew tap openai/tools\n  brew trust --formula openai/tools/tart openai/tools/softnet\n  brew install openai/tools/tart"
        ))
    })?;
    ensure!(
        version >= MIN_TART_VERSION,
        "Tart {}.{}.{} is too old for macOS 27 guests; {}.{}.{} or newer is needed",
        version.0,
        version.1,
        version.2,
        MIN_TART_VERSION.0,
        MIN_TART_VERSION.1,
        MIN_TART_VERSION.2
    );
    Ok(())
}

fn check_image() -> Result<(), Failure> {
    ensure!(
        tart::image_present()?,
        "the guest image is not in Tart's store. A run never downloads; pull it one time with:\n  ./scripts/vm.sh --setup"
    );
    Ok(())
}

/// Copies what the guests need into the stage directory.
pub fn stage(paths: &Paths) -> Result<(), Failure> {
    fs::create_dir_all(&paths.stage)?;
    let installer = paths.repo_root.join("dist").join(INSTALLER);
    ensure!(
        installer.is_file(),
        "{} does not exist; build it with ./scripts/build.sh package",
        installer.display()
    );
    fs::copy(&installer, paths.stage.join(INSTALLER))?;
    stage_driver(paths)?;

    // The e2e binaries are staged when they exist; the scenario that
    // needs them says so when they do not.
    let e2e = paths.stage.join("e2e");
    fs::create_dir_all(&e2e)?;
    for name in E2E_BINARIES {
        let binary = paths.target_debug().join(name);
        if binary.is_file() {
            fs::copy(&binary, e2e.join(name))?;
        }
    }
    Ok(())
}

fn stage_driver(paths: &Paths) -> Result<(), Failure> {
    fs::create_dir_all(&paths.stage)?;
    let driver = paths
        .vm_root
        .join("driver-build")
        .join("release")
        .join(DRIVER);
    ensure!(
        driver.is_file(),
        "{} does not exist; ./scripts/vm.sh builds the driver",
        driver.display()
    );
    fs::copy(&driver, paths.stage.join(DRIVER))?;
    Ok(())
}

// -- The prepared guest -----------------------------------------------

/// The two places a run has for guests. A clone resumes with the
/// network address of the prepared guest it came from, and two VMs
/// with one address cannot run together, so each place has a prepared
/// guest of its own and holds one clone at a time.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Place {
    /// The guest that the scenarios of the installed app share, and
    /// the manual guest.
    Shared,
    /// The fresh guests, one after the other.
    Fresh,
}

impl Place {
    const ALL: [Place; 2] = [Place::Shared, Place::Fresh];

    fn label(self) -> &'static str {
        match self {
            Place::Shared => "shared",
            Place::Fresh => "fresh",
        }
    }

    /// The start of the name of a manual guest in this place, after
    /// the tag of its checkout. The name alone has to say the place,
    /// because a run of another checkout reads it with [`Place::of`].
    fn manual_prefix(self) -> &'static str {
        match self {
            Place::Shared => "manual-",
            Place::Fresh => "manual2-",
        }
    }

    /// The place a VM of the tier takes, read from its name.
    fn of(name: &str) -> Option<Place> {
        let rest = name.strip_prefix(VM_PREFIX)?;
        if rest.starts_with("prepared-") {
            None
        } else if rest.contains("-manual-") || rest.ends_with("-shared") {
            Some(Place::Shared)
        } else {
            Some(Place::Fresh)
        }
    }
}

/// The name of the prepared guest of a place. One exists for each
/// image digest and place.
pub fn prepared_name(place: Place) -> String {
    let digest = tart::IMAGE_DIGEST.trim_start_matches("sha256:");
    format!("{VM_PREFIX}prepared-{}-{}", &digest[..12], place.label())
}

/// Makes the prepared guests that do not exist. A prepared guest is a
/// clone of the image that booted once, accepted the screen capture
/// question of macOS, and was saved to disk. A clone of it starts in
/// seconds.
pub fn ensure_prepared(paths: &Paths) -> Result<(), Failure> {
    // Another process that makes the same guests finishes first.
    let _store = StoreLock::take()?;
    let existing = tart::list()?;
    for name in Place::ALL.iter().map(|place| prepared_name(*place)) {
        match existing.iter().find(|vm| vm.name == name) {
            Some(vm) if vm.state == "suspended" => continue,
            // One that a failed setup left half made is in the way. A
            // prepared guest of another image digest is left alone: a
            // checkout that pins that digest uses it.
            Some(_) => {
                let _ = tart::stop(&name);
                tart::delete(&name)?;
            }
            None => {}
        }
        println!("[vm] preparing `{name}`, a guest that runs are cloned from (one time)");
        make_prepared(paths, &name)?;
    }
    Ok(())
}

fn make_prepared(paths: &Paths, name: &str) -> Result<(), Failure> {
    stage_driver(paths)?;
    let results = paths.vm_root.join("prepare");
    fs::create_dir_all(&results)?;

    tart::clone(&tart::image(), name)?;
    if let Err(failure) = tart::configure(name) {
        let _ = tart::delete(name);
        return Err(failure);
    }
    let guest = Guest::adopt(name, &paths.stage, &results)?;
    match prepare(&guest) {
        Ok(()) => guest.suspend(),
        Err(failure) => {
            let _ = guest.destroy();
            Err(failure)
        }
    }
}

fn prepare(guest: &Guest) -> Result<(), Failure> {
    let permissions = guest.sh_ok(&format!("'{}/{DRIVER}' permissions", guest_stage()))?;
    let permissions: serde_json::Value = serde_json::from_str(&permissions)?;
    for permission in ["accessibility", "screenCapture", "postEvent"] {
        ensure!(
            permissions[permission] == true,
            "the guest image does not grant `{permission}` to commands of its guest agent"
        );
    }
    // The first capture makes macOS ask whether the guest agent may
    // capture the screen with no picker. The answer is kept in the
    // guest, so the clones do not ask again for a month.
    guest.sh_ok("screencapture -x /tmp/vapor-vm-prepare.png")?;
    let _ = wait_until(
        Duration::from_secs(20),
        "the screen capture question of macOS",
        || ui::allow_screen_capture(guest),
    );
    ui::close_notifications(guest);
    Ok(())
}

// -- Places for guests ------------------------------------------------

/// Held while a process looks for a free place and starts a guest in
/// it, and while it makes the prepared guests. Tart's store is shared
/// by every checkout on the machine: without the lock two runs that
/// both see a free place both take it. The lock is an empty file in
/// the user's temporary directory, the one thing the tier keeps on the
/// host outside the repository and Tart's store.
struct StoreLock {
    _file: File,
}

impl StoreLock {
    fn take() -> Result<Self, Failure> {
        let path = std::env::temp_dir().join("vapor-vm.lock");
        let file = File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(|error| Failure::new(format!("cannot open {}: {error}", path.display())))?;
        file.lock()
            .map_err(|error| Failure::new(format!("cannot lock {}: {error}", path.display())))?;
        Ok(Self { _file: file })
    }
}

/// Why a guest cannot start in `place` now, or `None` when it can.
fn blocker(place: Place) -> Option<String> {
    let Ok(all) = tart::list() else {
        return Some("`tart list` failed".to_string());
    };
    let running: Vec<&Vm> = all.iter().filter(|vm| vm.is_running()).collect();
    if let Some(vm) = running.iter().find(|vm| Place::of(&vm.name) == Some(place)) {
        return Some(format!("the guest `{}` runs in its place", vm.name));
    }
    // A guest of the tier runs macOS; for another VM Tart is asked.
    let macos = running
        .iter()
        .filter(|vm| vm.is_ours() || tart::is_macos(&vm.name))
        .count();
    (macos >= MAX_MACOS_GUESTS).then(|| format!("{macos} macOS guests already run on this host"))
}

/// Starts a clone of the prepared guest of `place`, after waiting
/// until the place is free and the host runs fewer than two macOS
/// guests.
fn start_guest(
    place: Place,
    name: &str,
    paths: &Paths,
    results: &Path,
    session: Session,
) -> Result<Guest, Failure> {
    let _store = StoreLock::take()?;
    if let Some(reason) = blocker(place) {
        println!(
            "[vm] waiting to start `{name}`: {reason}. If no run is in progress, `./scripts/vm.sh --guest-stop` deletes the guests of this checkout"
        );
    }
    wait_until(
        SLOT_TIMEOUT,
        "a place for a guest: Apple permits two macOS guests on one host, and the tier runs one guest of each kind",
        || blocker(place).is_none(),
    )?;
    Guest::start(&prepared_name(place), name, &paths.stage, results, session)
}

// -- The shared guest -------------------------------------------------

/// Puts Vapor in a shared guest into a start state: no trace of an
/// earlier scenario, then the app started the way a user starts it.
fn reset(ctx: &Ctx, start: Start) -> Result<(), Failure> {
    // End the app first, so it cannot start the service again, then
    // remove the service and everything Vapor wrote.
    ctx.guest.sh_ok(&format!(
        "pkill -x Vapor; '{CLI_PATH}' service uninstall >/dev/null 2>&1; pkill -x vapord; \
         rm -rf ~/.vapor ~/VaporTest; true"
    ))?;
    wait_until(
        Duration::from_secs(30),
        "the app and the daemon of the earlier scenario to end",
        || !ctx.process_runs("Vapor") && !ctx.process_runs("vapord"),
    )?;
    if start == Start::Profile {
        ctx.guest
            .sh_ok(&format!("mkdir -p ~/{LOCAL_ROOT} ~/{CLOUD_ROOT}"))?;
        ctx.vapor_ok(&format!("config set localRoot \"$HOME/{LOCAL_ROOT}\""))?;
        ctx.vapor_ok(&format!("config set cloudRoot \"$HOME/{CLOUD_ROOT}\""))?;
    }
    ctx.launch_app()?;
    ctx.wait_service_running()?;
    ui::close_notifications(ctx.guest);
    Ok(())
}

// -- One scenario -----------------------------------------------------

fn failed(scenario: &Scenario, reason: String, results: &Path) -> ScenarioResult {
    ScenarioResult {
        id: scenario.id.to_string(),
        name: scenario.name.to_string(),
        proves: scenario.proves.to_string(),
        verdict: Verdict::Fail,
        reasons: vec![reason],
        seconds: 0.0,
        evidence: results.join(scenario.slug()).display().to_string(),
        notes: Vec::new(),
    }
}

/// Runs one scenario in `guest`, with a video when the run asked for
/// one and, on a failure, a screenshot and the list of controls.
fn run_scenario(
    scenario: &Scenario,
    guest: &mut Guest,
    copy: &Copy,
    build: &Build,
    video: bool,
) -> ScenarioResult {
    let started = Instant::now();
    let slug = scenario.slug();
    let evidence = guest.results().join(&slug).display().to_string();
    let mut result = ScenarioResult {
        id: scenario.id.to_string(),
        name: scenario.name.to_string(),
        proves: scenario.proves.to_string(),
        verdict: Verdict::Pass,
        reasons: Vec::new(),
        seconds: 0.0,
        evidence,
        notes: Vec::new(),
    };
    let mut ctx = match Ctx::new(guest, copy, build, &slug) {
        Ok(ctx) => ctx,
        Err(failure) => {
            result.verdict = Verdict::Fail;
            result.reasons.push(failure.message);
            return result;
        }
    };
    ctx.record_video = video;
    let outcome = match scenario.guest {
        GuestKind::Shared(start) => reset(&ctx, start),
        GuestKind::Fresh => Ok(()),
    }
    .and_then(|()| {
        ctx.start_video();
        (scenario.run)(&mut ctx)
    });
    if let Err(failure) = outcome {
        result.verdict = Verdict::Fail;
        result.reasons.push(failure.message);
        let _ = ctx.shot_with_notifications("failure");
        ctx.dump_controls();
    }
    ctx.collect_logs();
    ctx.stop_video();
    result.notes = std::mem::take(&mut ctx.notes);
    result.seconds = started.elapsed().as_secs_f64();
    println!(
        "[vm] {} {} {} ({:.0}s){}",
        result.verdict.label(),
        result.id,
        result.name,
        result.seconds,
        result
            .reasons
            .first()
            .map(|reason| format!(": {reason}"))
            .unwrap_or_default()
    );
    result
}

struct Run<'a> {
    id: &'a str,
    dir: &'a Path,
    paths: &'a Paths,
    copy: &'a Copy,
    build: &'a Build,
    keep: bool,
    video: bool,
}

impl Run<'_> {
    fn guest_name(&self, suffix: &str) -> String {
        self.paths
            .guest_name(&format!("run-{}-{}", self.id, suffix.to_lowercase()))
    }

    /// Deletes a guest, or with `--keep` leaves the guest of a failure
    /// running as the tier started it (no window, no route to the
    /// Internet) and says how to look at it and how to delete it.
    fn finish(&self, guest: Guest, any_failed: bool) {
        if self.keep && any_failed {
            println!(
                "[vm] kept the guest `{name}`, which still runs. A command in it: `tart exec {name} /usr/local/bin/vapor status`. It holds its place until `./scripts/vm.sh --guest-stop` deletes it",
                name = guest.name()
            );
            guest.detach();
        } else if let Err(failure) = guest.destroy() {
            println!("[vm] {failure}");
        }
    }

    /// The scenarios of the installed app, in one guest, in turn.
    fn shared(&self, scenarios: &[Scenario]) -> Vec<ScenarioResult> {
        if scenarios.is_empty() {
            return Vec::new();
        }
        let name = self.guest_name("shared");
        let started = start_guest(Place::Shared, &name, self.paths, self.dir, Session::Run)
            .and_then(|guest| {
                guest.sh_ok(&format!(
                    "sudo installer -pkg '{}/{INSTALLER}' -target / && test -d '{APP_PATH}'",
                    guest_stage()
                ))?;
                Ok(guest)
            });
        let mut guest = match started {
            Ok(guest) => guest,
            Err(failure) => {
                return scenarios
                    .iter()
                    .map(|scenario| {
                        failed(
                            scenario,
                            format!("the shared guest did not come up: {failure}"),
                            self.dir,
                        )
                    })
                    .collect();
            }
        };
        let results: Vec<ScenarioResult> = scenarios
            .iter()
            .map(|scenario| run_scenario(scenario, &mut guest, self.copy, self.build, self.video))
            .collect();
        let any_failed = results.iter().any(|result| result.verdict.is_red());
        self.finish(guest, any_failed);
        results
    }

    /// The scenarios of the install, each in a fresh clone.
    fn fresh(&self, scenarios: &[Scenario]) -> Vec<ScenarioResult> {
        scenarios
            .iter()
            .map(|scenario| {
                let name = self.guest_name(scenario.id);
                match start_guest(Place::Fresh, &name, self.paths, self.dir, Session::Run) {
                    Ok(mut guest) => {
                        let result =
                            run_scenario(scenario, &mut guest, self.copy, self.build, self.video);
                        self.finish(guest, result.verdict.is_red());
                        result
                    }
                    Err(failure) => failed(
                        scenario,
                        format!("the fresh guest did not come up: {failure}"),
                        self.dir,
                    ),
                }
            })
            .collect()
    }
}

fn select(only: &[String]) -> Result<Vec<Scenario>, Failure> {
    let all = scenarios::all();
    if only.is_empty() {
        return Ok(all);
    }
    for wanted in only {
        ensure!(
            all.iter()
                .any(|scenario| scenario.id.eq_ignore_ascii_case(wanted) || scenario.name == wanted),
            "no scenario has the id or the name `{wanted}`; see --list"
        );
    }
    Ok(all
        .into_iter()
        .filter(|scenario| {
            only.iter()
                .any(|wanted| scenario.id.eq_ignore_ascii_case(wanted) || scenario.name == wanted)
        })
        .collect())
}

fn run_id() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default();
    format!("{seconds}-{}", std::process::id())
}

/// Runs the selected scenarios. `Ok(true)` is a green run.
pub fn run(options: &RunOptions) -> Result<bool, Failure> {
    let started = Instant::now();
    let paths = Paths::new(&options.repo_root)?;
    check_host()?;
    check_image()?;
    let selected = select(&options.only)?;
    let copy = Copy::load(&options.repo_root)?;
    let build = Build::load(&options.repo_root)?;
    stage(&paths)?;
    remove_stopped_manual_guests(&paths)?;
    ensure_prepared(&paths)?;

    let id = run_id();
    let dir = paths.vm_root.join(format!("run-{id}"));
    fs::create_dir_all(&dir)?;
    println!(
        "[vm] run {id}: {} scenario(s), evidence in {}",
        selected.len(),
        dir.display()
    );

    let (shared, fresh): (Vec<Scenario>, Vec<Scenario>) = selected
        .iter()
        .partition(|scenario| matches!(scenario.guest, GuestKind::Shared(_)));
    let run = Run {
        id: &id,
        dir: &dir,
        paths: &paths,
        copy: &copy,
        build: &build,
        keep: options.keep,
        video: options.video,
    };
    // The two groups run side by side; each waits for its own place.
    let mut results = if !shared.is_empty() && !fresh.is_empty() {
        thread::scope(|scope| {
            let shared_results = scope.spawn(|| run.shared(&shared));
            let mut results = run.fresh(&fresh);
            results.extend(shared_results.join().unwrap_or_else(|_| {
                shared
                    .iter()
                    .map(|scenario| failed(scenario, "the harness panicked".to_string(), &dir))
                    .collect()
            }));
            results
        })
    } else {
        let mut results = run.fresh(&fresh);
        results.extend(run.shared(&shared));
        results
    };
    results.sort_by(|a, b| a.id.cmp(&b.id));

    // A run leaves no VM of its own, except a guest that --keep asked for.
    let leftovers: Vec<String> = tart::list()?
        .into_iter()
        .filter(|vm| {
            vm.name
                .starts_with(&paths.guest_name(&format!("run-{id}-")))
        })
        .map(|vm| vm.name)
        .collect();
    let kept = options.keep && results.iter().any(|result| result.verdict.is_red());
    if !kept && !leftovers.is_empty() {
        results.push(ScenarioResult {
            id: "VM".to_string(),
            name: "no-guest-left".to_string(),
            proves: "a run deletes every guest it made".to_string(),
            verdict: Verdict::Fail,
            reasons: vec![format!("the run left these VMs: {}", leftovers.join(", "))],
            seconds: 0.0,
            evidence: dir.display().to_string(),
            notes: Vec::new(),
        });
    }

    let report = Report {
        run: id.clone(),
        image: tart::image(),
        build: build.version.clone(),
        passed: results
            .iter()
            .filter(|result| result.verdict == Verdict::Pass)
            .count(),
        failed: results
            .iter()
            .filter(|result| result.verdict.is_red())
            .count(),
        seconds: started.elapsed().as_secs_f64(),
        scenarios: results,
    };
    let json = serde_json::to_string_pretty(&report)?;
    let report_path = options
        .json
        .clone()
        .unwrap_or_else(|| dir.join("report.json"));
    fs::write(&report_path, json)?;
    println!(
        "[vm] {} passed, {} failed in {:.0}s; report: {}",
        report.passed,
        report.failed,
        report.seconds,
        report_path.display()
    );
    Ok(report.failed == 0)
}

// -- Setup, the manual guest, and cleaning -----------------------------

/// Pulls the image when it is missing and makes the prepared guest.
/// The one command of the tier that uses the network.
pub fn setup(repo_root: &Path) -> Result<(), Failure> {
    let paths = Paths::new(repo_root)?;
    check_host()?;
    if tart::image_present()? {
        println!("[vm] the guest image is already in Tart's store");
    } else {
        println!(
            "[vm] pulling {} into Tart's store (~/.tart): about 32 GB to download, 36 GB on disk",
            tart::image()
        );
        tart::pull_image()?;
    }
    ensure_prepared(&paths)?;
    println!("[vm] ready: ./scripts/vm.sh runs the scenarios");
    Ok(())
}

/// Deletes the manual guests of this checkout that no longer run: a
/// person closed the window, or the Mac restarted. A manual guest that
/// runs is never touched here; only its owner ends it.
fn remove_stopped_manual_guests(paths: &Paths) -> Result<(), Failure> {
    // Under the lock no guest is between its clone and its start.
    let _store = StoreLock::take()?;
    let prefixes = Place::ALL.map(|place| paths.guest_name(place.manual_prefix()));
    for vm in tart::list()?.iter().filter(|vm| {
        vm.is_ours()
            && !vm.is_running()
            && prefixes.iter().any(|prefix| vm.name.starts_with(prefix))
    }) {
        tart::delete(&vm.name)?;
    }
    Ok(())
}

/// Starts a guest with the build installed and the app started, and
/// leaves it running for manual work. It has no time limit. With
/// `second` it takes the place of the fresh guests, so the scenarios
/// of the installed app and another manual guest still have theirs.
/// With `internet` it has a route to the Internet.
pub fn manual_guest(
    repo_root: &Path,
    window: bool,
    second: bool,
    internet: bool,
) -> Result<(), Failure> {
    let paths = Paths::new(repo_root)?;
    check_host()?;
    check_image()?;
    let copy = Copy::load(repo_root)?;
    let build = Build::load(repo_root)?;
    stage(&paths)?;
    remove_stopped_manual_guests(&paths)?;
    ensure_prepared(&paths)?;

    let id = run_id();
    let dir = paths.vm_root.join(format!("manual-{id}"));
    fs::create_dir_all(&dir)?;
    let place = if second { Place::Fresh } else { Place::Shared };
    let name = paths.guest_name(&format!("{}{id}", place.manual_prefix()));
    let mut guest = start_guest(
        place,
        &name,
        &paths,
        &dir,
        Session::Manual { window, internet },
    )?;
    let ready = (|| {
        let ctx = Ctx::new(&mut guest, &copy, &build, "session")?;
        ctx.install()?;
        ctx.launch_app()?;
        ctx.wait_service_running()
    })();
    if let Err(failure) = ready {
        let _ = guest.destroy();
        return Err(failure);
    }
    println!("[vm] the guest `{name}` runs with this build installed and the app started.");
    if internet {
        println!("  it has a route to the Internet, for a test with a real cloud provider");
    }
    if window {
        println!(
            "  its screen is in a Tart window on this Mac: use the app in it as a user does. The guest user is `admin`"
        );
        println!(
            "  the window has no time limit: it stays until it is closed, or until --guest-stop"
        );
    }
    println!("  a command in the guest:  tart exec {name} /usr/local/bin/vapor status");
    println!(
        "  the controls of the app: tart exec {name} '{}/{DRIVER}' tree sh.arn.vapor",
        guest_stage()
    );
    println!(
        "  a screenshot:            tart exec {name} /usr/sbin/screencapture -x '{}/session/shot.png'",
        guest_results()
    );
    println!("  files from the guest:    {}", dir.display());
    println!("  stop and delete it:      ./scripts/vm.sh --guest-stop");
    guest.detach();
    Ok(())
}

/// Stops and deletes the guests whose names start with `prefix`.
/// Returns how many it deleted.
fn delete_guests(prefix: &str) -> Result<usize, Failure> {
    let mut deleted = 0;
    for vm in tart::list()?
        .iter()
        .filter(|vm| vm.is_ours() && vm.name.starts_with(prefix))
    {
        let _ = tart::stop(&vm.name);
        tart::delete(&vm.name)?;
        deleted += 1;
    }
    Ok(deleted)
}

/// Stops and deletes every guest of this checkout: a manual guest, a
/// guest that `--keep` left, and a guest of a run that was killed.
pub fn stop_guests(repo_root: &Path) -> Result<usize, Failure> {
    delete_guests(&Paths::new(repo_root)?.guest_name(""))
}

/// Stops and deletes the guests of this checkout, then the prepared
/// guests, which every checkout shares. The pulled image stays.
pub fn clean(repo_root: &Path) -> Result<usize, Failure> {
    let mut deleted = stop_guests(repo_root)?;
    // What still runs belongs to a run of another checkout, which
    // clones the prepared guests.
    if tart::list()?
        .iter()
        .any(|vm| vm.is_ours() && vm.is_running())
    {
        println!("[vm] another checkout runs the tier; the prepared guests stay");
    } else {
        deleted += delete_guests(&format!("{VM_PREFIX}prepared-"))?;
    }
    Ok(deleted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_guest_name_tells_its_place() {
        assert_eq!(
            Place::of("vapor-vm-1a2b3c4d-run-17-99-shared"),
            Some(Place::Shared)
        );
        assert_eq!(
            Place::of("vapor-vm-1a2b3c4d-manual-17-99"),
            Some(Place::Shared)
        );
        assert_eq!(
            Place::of("vapor-vm-1a2b3c4d-run-17-99-v03"),
            Some(Place::Fresh)
        );
        // A manual guest says its place in its name.
        for place in Place::ALL {
            let name = format!("vapor-vm-1a2b3c4d-{}17-99", place.manual_prefix());
            assert_eq!(Place::of(&name), Some(place));
        }
        // A prepared guest never runs during a run, and a VM of
        // someone else takes no place of the tier.
        assert_eq!(Place::of(&prepared_name(Place::Shared)), None);
        assert_eq!(Place::of("my-own-vm"), None);
    }

    #[test]
    fn the_two_places_have_prepared_guests_of_their_own() {
        let shared = prepared_name(Place::Shared);
        let fresh = prepared_name(Place::Fresh);
        assert_ne!(shared, fresh);
        assert!(shared.starts_with(VM_PREFIX) && fresh.starts_with(VM_PREFIX));
    }

    #[test]
    fn a_checkout_tag_is_stable_and_differs_between_checkouts() {
        let here = Path::new(env!("CARGO_MANIFEST_DIR"));
        let tag = checkout_tag(here).expect("the crate directory resolves");
        assert_eq!(tag.len(), 8);
        assert_eq!(tag, checkout_tag(here).expect("resolves again"));
        assert_ne!(
            tag,
            checkout_tag(&here.join("src")).expect("the source directory resolves")
        );
    }
}
