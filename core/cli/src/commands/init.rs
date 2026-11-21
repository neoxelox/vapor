//! `vapor init`: the first run of an install, from a terminal.
//!
//! One command for everything a first use needs: the profile, the
//! sign-in when the provider has an account, and the background
//! service. It asks what the onboarding of an app asks, in the same
//! order, and a change to one changes the other (`AGENTS.md` §2).
//!
//! Each step reads its state from what is true on the device. So a
//! later run does only what is left, and a run on an install that is
//! set up changes nothing and says so. With no terminal nothing is
//! asked: every answer is an option of the command.

use std::fmt::{self, Display};
use std::io;
use std::path::Path;
use std::time::SystemTime;

use serde::Serialize;
use vapor_platform::SecretStore;
use vapor_shared::constants::{self, init as names};

use super::auth::{self, AuthError};
use super::profiles::{self, AddRequest, ProfileRow, ProfileStorage, ProfilesError, Prompter};

/// What `vapor init` was given on the command line.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct InitRequest {
    pub provider: Option<String>,
    pub local: Option<String>,
    pub cloud: Option<String>,
    pub mode: Option<String>,
    /// Whether Vapor starts at login. Left out, a terminal asks and a
    /// run with no terminal takes yes.
    pub auto_launch: Option<bool>,
    /// The user's own answer about lingering, where the platform has
    /// it. Left out, the service step decides.
    pub linger: Option<bool>,
}

impl InitRequest {
    fn names_a_profile(&self) -> bool {
        self.provider.is_some()
            || self.local.is_some()
            || self.cloud.is_some()
            || self.mode.is_some()
    }
}

/// Whether the cloud folder is in the account already.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CloudFolder {
    Exists,
    /// Not there: the daemon creates it at the first sync.
    New,
    /// Nobody looked, and why.
    Unchecked(String),
}

/// What is true of the background service.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ServiceState {
    Installed,
    NotInstalled,
    /// The user turned auto-launch off.
    Off,
    /// The service cannot be installed here, and why.
    Unavailable(String),
}

/// What `vapor init` does outside `vapor.json` and the secret store.
/// The binary gives the real thing; the tests give a script.
pub trait InitHost {
    /// Signs in to `provider` and gives the token to store. `None`
    /// when nothing here can sign in: no terminal for the browser flow
    /// and no token on stdin.
    fn sign_in(&mut self, provider: &str) -> Option<Result<String, String>>;
    /// Looks for the cloud folder in the account of `profile_id`.
    fn cloud_folder(&mut self, provider: &str, profile_id: &str, cloud_root: &str) -> CloudFolder;
    fn service(&mut self) -> ServiceState;
    /// Registers the background service and starts the daemon. `Ok`
    /// may carry a line for the user.
    fn install_service(&mut self, linger: Option<bool>) -> Result<Option<String>, String>;
    /// Records that Vapor does not start at login. Installs nothing
    /// and removes nothing.
    fn decline_service(&mut self) -> Result<(), String>;
    /// A line for the user, shown as the run goes.
    fn note(&mut self, line: &str);
}

#[derive(Debug)]
pub enum InitError {
    Profiles(ProfilesError),
    Auth(AuthError),
    Io(io::Error),
    /// `vapor.json` is not usable as it is; the text says what to fix.
    Config(String),
}

impl Display for InitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Profiles(error) => write!(f, "{error}"),
            Self::Auth(error) => write!(f, "{error}"),
            Self::Io(error) => write!(f, "{error}"),
            Self::Config(problem) => write!(
                f,
                "vapor.json has a problem: {problem}. Fix the file, then run `vapor init` again"
            ),
        }
    }
}

impl std::error::Error for InitError {}

impl From<ProfilesError> for InitError {
    fn from(error: ProfilesError) -> Self {
        Self::Profiles(error)
    }
}

impl From<AuthError> for InitError {
    fn from(error: AuthError) -> Self {
        Self::Auth(error)
    }
}

impl From<io::Error> for InitError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// One step of the first run: its name, its state (`constants::init`),
/// and what the user has to know about it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InitStep {
    pub step: &'static str,
    pub state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl InitStep {
    fn new(step: &'static str, state: &'static str, detail: Option<String>) -> Self {
        Self {
            step,
            state,
            detail,
        }
    }

    fn settled(&self) -> bool {
        names::SETTLED_STATES.contains(&self.state)
    }
}

/// The daemon's run state, as `vapor status` gives it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DaemonLine {
    pub state: String,
    pub reason: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InitReport {
    /// Every step is settled: `vapor init` has nothing left to do.
    pub initialized: bool,
    pub steps: Vec<InitStep>,
    pub profiles: Vec<ProfileRow>,
    /// Left out when no daemon answers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub daemon: Option<DaemonLine>,
}

impl InitReport {
    fn of(steps: Vec<InitStep>, profiles: Vec<ProfileRow>) -> Self {
        Self {
            initialized: steps.iter().all(InitStep::settled),
            steps,
            profiles,
            daemon: None,
        }
    }

    pub fn render_text(&self) -> String {
        let mut out = String::from(if self.initialized {
            "Vapor is initialized.\n"
        } else {
            "Vapor is not initialized yet. Run `vapor init` again when the steps below are settled.\n"
        });
        for step in &self.steps {
            out.push_str(&format!("  {}: {}", step.step, step.state));
            if let Some(detail) = &step.detail {
                out.push_str(&format!(" ({detail})"));
            }
            out.push('\n');
        }
        for profile in &self.profiles {
            out.push_str(&format!(
                "{}  {} -> {}  ({}, {})\n",
                profile.id,
                profile
                    .local_root
                    .as_deref()
                    .unwrap_or("(local root not set)"),
                profile
                    .cloud_root
                    .as_deref()
                    .unwrap_or("(cloud root not set)"),
                profile.provider,
                profile.sync_mode,
            ));
        }
        match &self.daemon {
            Some(daemon) if daemon.reason.is_empty() => {
                out.push_str(&format!("daemon: {}\n", daemon.state));
            }
            Some(daemon) => {
                out.push_str(&format!("daemon: {} ({})\n", daemon.state, daemon.reason));
            }
            None => out.push_str("daemon: not running\n"),
        }
        out
    }
}

/// `vapor init`. `prompter` is given on a terminal.
pub fn run(
    config_path: &Path,
    storage: &ProfileStorage,
    store: &dyn SecretStore,
    request: &InitRequest,
    mut prompter: Option<&mut (dyn Prompter + '_)>,
    host: &mut dyn InitHost,
    now: SystemTime,
) -> Result<InitReport, InitError> {
    let listed = profiles::list(config_path);
    if !listed.problems.is_empty() {
        return Err(InitError::Config(listed.problems.join("; ")));
    }

    let (mut steps, rows) = if listed.profiles.is_empty() {
        let first = first_profile(
            config_path,
            storage,
            store,
            request,
            prompter.as_deref_mut(),
            host,
            now,
        )?;
        match first {
            // A sign-in that failed ends the run before a folder is
            // asked for. Nothing is written, and the next run starts
            // at the sign-in again.
            FirstProfile::Stopped(steps) => return Ok(InitReport::of(steps, Vec::new())),
            FirstProfile::Written(steps, row) => (steps, vec![row]),
        }
    } else {
        if request.names_a_profile() {
            host.note(
                "A profile exists already, so the provider, folder, and mode options were not \
                 used. `vapor profiles list` shows it; `vapor profiles add` adds another.",
            );
        }
        let steps = vec![
            existing_profiles_step(&listed.profiles),
            sign_in_existing(config_path, store, host)?,
        ];
        (steps, listed.profiles)
    };

    steps.push(service_step(request, prompter, host)?);
    Ok(InitReport::of(steps, rows))
}

enum FirstProfile {
    Stopped(Vec<InitStep>),
    Written(Vec<InitStep>, ProfileRow),
}

/// The steps of an install with no profile, in the order of the
/// onboarding: provider, sign-in, the two folders, the mode.
fn first_profile(
    config_path: &Path,
    storage: &ProfileStorage,
    store: &dyn SecretStore,
    request: &InitRequest,
    mut prompter: Option<&mut (dyn Prompter + '_)>,
    host: &mut dyn InitHost,
    now: SystemTime,
) -> Result<FirstProfile, InitError> {
    let provider = match (&request.provider, prompter.as_deref_mut()) {
        (Some(provider), _) => provider.clone(),
        (None, Some(prompter)) => profiles::ask_provider(prompter)?,
        (None, None) => constants::provider::DEFAULT.to_string(),
    };
    let provider = profiles::known_provider(&provider)?;

    // The sign-in comes before the folders, so the folder step can
    // look into the account. The profile does not exist yet: the
    // sign-in is stored under the id the first profile gets.
    let id = constants::profile::DEFAULT_PROFILE_ID;
    let sign_in = sign_in_step(store, host, &[(id.to_string(), provider.clone())])?;
    if sign_in.state == names::STATE_FAILED {
        return Ok(FirstProfile::Stopped(vec![sign_in]));
    }

    let mut add = AddRequest {
        provider: Some(provider.clone()),
        local: request.local.clone(),
        cloud: request.cloud.clone(),
        mode: request.mode.clone(),
        ..AddRequest::default()
    };
    if let Some(prompter) = prompter.as_deref_mut() {
        if add.local.is_none() {
            add.local = Some(profiles::ask_local(prompter)?);
        }
        if add.cloud.is_none() {
            add.cloud = Some(profiles::ask_cloud(prompter, &provider)?);
        }
    }
    // Planned before the mode is asked for: a folder that cannot be a
    // sync root is refused here, and the cloud folder is looked up as
    // the daemon will resolve it.
    let preview = profiles::preview(config_path, &add)?;
    let folder = if sign_in.state == names::STATE_WAITS {
        CloudFolder::Unchecked("not signed in yet".to_string())
    } else {
        host.cloud_folder(&preview.provider, &preview.id, &preview.cloud_root)
    };
    let folder_line = match &folder {
        CloudFolder::Exists => format!(
            "{} exists in the cloud: Vapor syncs with what is in it",
            preview.cloud_root
        ),
        CloudFolder::New => format!("{} is new: Vapor creates it", preview.cloud_root),
        CloudFolder::Unchecked(reason) => {
            format!("{} was not looked up: {reason}", preview.cloud_root)
        }
    };
    // On a terminal the answer comes right after the folder was typed.
    // With no terminal the summary carries it.
    if prompter.is_some() {
        host.note(&folder_line);
    }

    if let Some(prompter) = prompter.as_deref_mut()
        && add.mode.is_none()
    {
        add.mode = Some(profiles::ask_mode(prompter)?);
    }
    let outcome = profiles::add(
        config_path,
        storage,
        &add,
        prompter.map(|prompter| -> &mut dyn Prompter { prompter }),
        now,
    )?;
    auth::forget_other_providers(store, &outcome.profile.id, &outcome.profile.provider)?;

    let profile = if outcome.one_way == Some("asks") {
        InitStep::new(
            names::STEP_PROFILE,
            names::STATE_WAITS,
            Some(
                "the profile syncs nothing until you confirm the one-way mode: vapor decisions list"
                    .to_string(),
            ),
        )
    } else {
        InitStep::new(names::STEP_PROFILE, names::STATE_DONE, Some(folder_line))
    };
    Ok(FirstProfile::Written(
        vec![profile, sign_in],
        outcome.profile,
    ))
}

fn existing_profiles_step(rows: &[ProfileRow]) -> InitStep {
    let problems: Vec<String> = rows
        .iter()
        .filter_map(|row| {
            row.problem
                .as_ref()
                .map(|problem| format!("{}: {problem}", row.id))
        })
        .collect();
    if problems.is_empty() {
        InitStep::new(names::STEP_PROFILE, names::STATE_DONE, None)
    } else {
        InitStep::new(
            names::STEP_PROFILE,
            names::STATE_FAILED,
            Some(problems.join("; ")),
        )
    }
}

/// The sign-in step of an install that has profiles: every profile
/// whose provider has an account and that holds no sign-in.
fn sign_in_existing(
    config_path: &Path,
    store: &dyn SecretStore,
    host: &mut dyn InitHost,
) -> Result<InitStep, InitError> {
    let config = vapor_shared::config::load_from(config_path).config;
    let accounts: Vec<(String, String)> = auth::status_from(store, &config, None)?
        .profiles
        .into_iter()
        .filter(|entry| entry.state != constants::auth::STATE_NOT_NEEDED)
        .map(|entry| (entry.profile_id, entry.provider))
        .collect();
    sign_in_step(store, host, &accounts)
}

/// Signs in each `(profile id, provider)` whose provider has an
/// account and that holds no sign-in yet.
fn sign_in_step(
    store: &dyn SecretStore,
    host: &mut dyn InitHost,
    accounts: &[(String, String)],
) -> Result<InitStep, InitError> {
    let accounts: Vec<&(String, String)> = accounts
        .iter()
        .filter(|(_, provider)| constants::provider::SIGN_IN_REQUIRED.contains(&provider.as_str()))
        .collect();
    if accounts.is_empty() {
        return Ok(InitStep::new(
            names::STEP_SIGN_IN,
            names::STATE_NOT_NEEDED,
            None,
        ));
    }
    let mut waiting = Vec::new();
    for (profile_id, provider) in accounts {
        if auth::signed_in(store, profile_id, provider)? {
            continue;
        }
        match host.sign_in(provider) {
            Some(Ok(token)) => auth::store_sign_in(store, profile_id, provider, &token)?,
            Some(Err(reason)) => {
                return Ok(InitStep::new(
                    names::STEP_SIGN_IN,
                    names::STATE_FAILED,
                    Some(format!("{provider}: {reason}")),
                ));
            }
            None => waiting.push(profile_id.clone()),
        }
    }
    if waiting.is_empty() {
        return Ok(InitStep::new(names::STEP_SIGN_IN, names::STATE_DONE, None));
    }
    Ok(InitStep::new(
        names::STEP_SIGN_IN,
        names::STATE_WAITS,
        Some(format!(
            "sign in on a terminal with `vapor init`, or pipe a token: \
             `<command that prints the token> | vapor init --token -` (profile {})",
            waiting.join(", ")
        )),
    ))
}

fn service_step(
    request: &InitRequest,
    prompter: Option<&mut (dyn Prompter + '_)>,
    host: &mut dyn InitHost,
) -> Result<InitStep, InitError> {
    let step = |state, detail| InitStep::new(names::STEP_SERVICE, state, detail);
    let off = || {
        step(
            names::STATE_OFF,
            Some("Vapor does not start at login; `vapor run` runs it by hand".to_string()),
        )
    };
    // An explicit no is recorded and nothing is looked at: the service
    // stays as it is, installed or not.
    if request.auto_launch == Some(false) {
        return Ok(match host.decline_service() {
            Ok(()) => off(),
            Err(reason) => step(names::STATE_FAILED, Some(reason)),
        });
    }
    let wanted = match host.service() {
        ServiceState::Installed => return Ok(step(names::STATE_DONE, None)),
        ServiceState::Unavailable(reason) => {
            return Ok(step(names::STATE_UNAVAILABLE, Some(reason)));
        }
        // The user said no in an earlier run, and is not asked again.
        // Only an explicit yes changes it.
        ServiceState::Off if request.auto_launch != Some(true) => return Ok(off()),
        ServiceState::Off => true,
        ServiceState::NotInstalled => match (request.auto_launch, prompter) {
            (Some(answer), _) => answer,
            (None, Some(prompter)) => {
                let answer = prompter.ask(
                    "Start Vapor at login and keep it running in the background (yes, no)",
                    "yes",
                )?;
                !matches!(answer.trim().to_ascii_lowercase().as_str(), "no" | "n")
            }
            (None, None) => true,
        },
    };
    if !wanted {
        return Ok(match host.decline_service() {
            Ok(()) => off(),
            Err(reason) => step(names::STATE_FAILED, Some(reason)),
        });
    }
    Ok(match host.install_service(request.linger) {
        Ok(detail) => step(names::STATE_DONE, detail),
        Err(reason) => step(names::STATE_FAILED, Some(reason)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tempfile::TempDir;
    use vapor_platform::InMemorySecretStore;

    /// A host that answers from a script and records what it was asked.
    struct Scripted {
        sign_in: Option<Result<String, String>>,
        folder: CloudFolder,
        service: ServiceState,
        install: Result<Option<String>, String>,
        calls: Vec<String>,
        notes: Vec<String>,
    }

    impl Default for Scripted {
        fn default() -> Self {
            Self {
                sign_in: None,
                folder: CloudFolder::New,
                service: ServiceState::NotInstalled,
                install: Ok(None),
                calls: Vec::new(),
                notes: Vec::new(),
            }
        }
    }

    impl InitHost for Scripted {
        fn sign_in(&mut self, provider: &str) -> Option<Result<String, String>> {
            self.calls.push(format!("sign-in {provider}"));
            self.sign_in.clone()
        }

        fn cloud_folder(&mut self, provider: &str, profile_id: &str, _: &str) -> CloudFolder {
            self.calls.push(format!("folder {provider} {profile_id}"));
            self.folder.clone()
        }

        fn service(&mut self) -> ServiceState {
            self.calls.push("service".to_string());
            self.service.clone()
        }

        fn install_service(&mut self, linger: Option<bool>) -> Result<Option<String>, String> {
            self.calls.push(format!("install {linger:?}"));
            if self.install.is_ok() {
                self.service = ServiceState::Installed;
            }
            self.install.clone()
        }

        fn decline_service(&mut self) -> Result<(), String> {
            self.calls.push("decline".to_string());
            self.service = ServiceState::Off;
            Ok(())
        }

        fn note(&mut self, line: &str) {
            self.notes.push(line.to_string());
        }
    }

    /// Answers the questions in order; an empty answer takes the
    /// suggestion, as Enter does.
    struct Answers {
        answers: Vec<&'static str>,
        asked: Vec<String>,
    }

    impl Answers {
        fn new(answers: &[&'static str]) -> Self {
            Self {
                answers: answers.to_vec(),
                asked: Vec::new(),
            }
        }
    }

    impl Prompter for Answers {
        fn ask(&mut self, question: &str, default: &str) -> io::Result<String> {
            self.asked.push(question.to_string());
            let answer = if self.answers.is_empty() {
                ""
            } else {
                self.answers.remove(0)
            };
            Ok(if answer.is_empty() { default } else { answer }.to_string())
        }

        fn confirm(&mut self, question: &str) -> io::Result<bool> {
            self.asked.push(question.to_string());
            Ok(!self.answers.is_empty() && self.answers.remove(0) == "y")
        }
    }

    struct Install {
        temp: TempDir,
        store: Arc<InMemorySecretStore>,
    }

    impl Install {
        fn fresh() -> Self {
            Self {
                temp: TempDir::new().expect("temp"),
                store: Arc::new(InMemorySecretStore::new()),
            }
        }

        fn config(&self) -> std::path::PathBuf {
            self.temp.path().join("vapor.json")
        }

        fn folder(&self, name: &str) -> String {
            self.temp.path().join(name).display().to_string()
        }

        fn flags(&self) -> InitRequest {
            InitRequest {
                local: Some(self.folder("local")),
                cloud: Some(self.folder("cloud")),
                ..InitRequest::default()
            }
        }

        fn run(
            &self,
            request: &InitRequest,
            prompter: Option<&mut dyn Prompter>,
            host: &mut Scripted,
        ) -> Result<InitReport, InitError> {
            run(
                &self.config(),
                &ProfileStorage {
                    state: self.temp.path().join("state"),
                    trash: self.temp.path().join("trash"),
                },
                self.store.as_ref(),
                request,
                prompter,
                host,
                SystemTime::UNIX_EPOCH,
            )
        }
    }

    fn states(report: &InitReport) -> Vec<(&'static str, &'static str)> {
        report
            .steps
            .iter()
            .map(|step| (step.step, step.state))
            .collect()
    }

    #[test]
    fn with_flags_and_no_terminal_one_run_sets_everything_up() {
        let install = Install::fresh();
        let mut host = Scripted::default();
        let report = install
            .run(&install.flags(), None, &mut host)
            .expect("init");

        assert!(report.initialized, "{report:?}");
        assert_eq!(
            states(&report),
            [
                ("profile", "done"),
                ("sign-in", "not-needed"),
                ("service", "done"),
            ]
        );
        assert_eq!(report.profiles.len(), 1);
        assert_eq!(report.profiles[0].id, "default");
        // No terminal: start at login is taken as yes, and the service
        // step decides about lingering.
        assert_eq!(
            host.calls,
            ["folder filesystem default", "service", "install None"]
        );
        assert_eq!(profiles::list(&install.config()).profiles.len(), 1);
    }

    #[test]
    fn a_second_run_changes_nothing_and_says_initialized() {
        let install = Install::fresh();
        let mut host = Scripted::default();
        install
            .run(&install.flags(), None, &mut host)
            .expect("first run");
        let written = std::fs::read_to_string(install.config()).expect("config");
        host.calls.clear();

        let report = install
            .run(&InitRequest::default(), None, &mut host)
            .expect("second run");
        assert!(report.initialized);
        assert_eq!(
            states(&report),
            [
                ("profile", "done"),
                ("sign-in", "not-needed"),
                ("service", "done"),
            ]
        );
        assert_eq!(host.calls, ["service"], "nothing is installed twice");
        assert_eq!(
            std::fs::read_to_string(install.config()).expect("config"),
            written
        );
        assert!(report.render_text().starts_with("Vapor is initialized.\n"));
    }

    #[test]
    fn with_no_terminal_the_two_folders_have_to_be_given() {
        let install = Install::fresh();
        let mut host = Scripted::default();
        let error = install
            .run(&InitRequest::default(), None, &mut host)
            .expect_err("no folder, nobody to ask");
        assert!(
            error.to_string().contains("--local <path> --cloud <path>"),
            "{error}"
        );
        assert!(!install.config().exists(), "nothing is written");
        assert!(host.calls.is_empty(), "{:?}", host.calls);
    }

    #[test]
    fn a_terminal_asks_in_the_order_of_the_onboarding() {
        let install = Install::fresh();
        let mut host = Scripted {
            sign_in: Some(Ok("ya29.test".to_string())),
            folder: CloudFolder::Exists,
            ..Scripted::default()
        };
        let local = install.folder("local");
        let local: &'static str = Box::leak(local.into_boxed_str());
        // Provider, then the two folders, then the mode, then start at
        // login. Empty answers take the suggestion.
        let mut answers = Answers::new(&["gdrive", local, "/Vapor", "", ""]);
        let report = install
            .run(&InitRequest::default(), Some(&mut answers), &mut host)
            .expect("init");

        assert!(report.initialized, "{report:?}");
        assert_eq!(
            answers.asked,
            [
                "Provider (filesystem, gdrive)",
                "Folder on this device",
                "Folder in the cloud",
                "Sync mode (two-way, pull-only, push-only)",
                "Start Vapor at login and keep it running in the background (yes, no)",
            ]
        );
        // The sign-in comes before the folders, so the cloud folder is
        // looked up in the account.
        assert_eq!(
            host.calls,
            [
                "sign-in gdrive",
                "folder gdrive default",
                "service",
                "install None",
            ]
        );
        assert_eq!(
            host.notes,
            ["/Vapor exists in the cloud: Vapor syncs with what is in it"]
        );
        assert!(auth::signed_in(install.store.as_ref(), "default", "gdrive").expect("store"));
    }

    #[test]
    fn a_sign_in_that_fails_ends_the_run_with_nothing_written() {
        let install = Install::fresh();
        let mut host = Scripted {
            sign_in: Some(Err("the browser gave no answer".to_string())),
            ..Scripted::default()
        };
        let request = InitRequest {
            provider: Some("gdrive".to_string()),
            ..install.flags()
        };
        let report = install.run(&request, None, &mut host).expect("init");
        assert!(!report.initialized);
        assert_eq!(states(&report), [("sign-in", "failed")]);
        assert!(!install.config().exists());
        assert_eq!(host.calls, ["sign-in gdrive"]);
    }

    #[test]
    fn with_no_way_to_sign_in_the_rest_is_set_up_and_the_sign_in_waits() {
        let install = Install::fresh();
        let mut host = Scripted::default();
        let request = InitRequest {
            provider: Some("gdrive".to_string()),
            local: Some(install.folder("local")),
            cloud: Some("/Vapor".to_string()),
            ..InitRequest::default()
        };
        let report = install.run(&request, None, &mut host).expect("init");
        assert!(!report.initialized, "a sign-in that waits is not settled");
        assert_eq!(
            states(&report),
            [
                ("profile", "done"),
                ("sign-in", "waits"),
                ("service", "done"),
            ]
        );
        // Nobody is signed in, so nobody looked into the account.
        assert_eq!(host.calls, ["sign-in gdrive", "service", "install None"]);
        assert_eq!(
            report.steps[0].detail.as_deref(),
            Some("/Vapor was not looked up: not signed in yet")
        );
        assert!(host.notes.is_empty(), "no terminal: the summary says it");

        // The next run does only what is left: the sign-in.
        host.calls.clear();
        host.sign_in = Some(Ok("ya29.test".to_string()));
        let report = install
            .run(&InitRequest::default(), None, &mut host)
            .expect("second run");
        assert!(report.initialized, "{report:?}");
        assert_eq!(host.calls, ["sign-in gdrive", "service"]);
    }

    #[test]
    fn a_sign_in_kept_from_a_run_that_stopped_is_used_and_not_asked_for_again() {
        let install = Install::fresh();
        auth::store_sign_in(install.store.as_ref(), "default", "gdrive", "ya29.test")
            .expect("store");
        let mut host = Scripted::default();
        let request = InitRequest {
            provider: Some("gdrive".to_string()),
            local: Some(install.folder("local")),
            cloud: Some("/Vapor".to_string()),
            ..InitRequest::default()
        };
        let report = install.run(&request, None, &mut host).expect("init");
        assert!(report.initialized, "{report:?}");
        assert_eq!(
            host.calls,
            ["folder gdrive default", "service", "install None"]
        );
    }

    #[test]
    fn a_profile_for_another_provider_drops_the_sign_in_made_before_it() {
        let install = Install::fresh();
        auth::store_sign_in(install.store.as_ref(), "default", "gdrive", "ya29.test")
            .expect("store");
        let mut host = Scripted::default();
        install
            .run(&install.flags(), None, &mut host)
            .expect("init with the filesystem provider");
        assert!(!auth::signed_in(install.store.as_ref(), "default", "gdrive").expect("store"));
    }

    #[test]
    fn a_one_way_mode_with_no_terminal_waits_for_its_answer() {
        let install = Install::fresh();
        let mut host = Scripted::default();
        let request = InitRequest {
            mode: Some("push-only".to_string()),
            ..install.flags()
        };
        let report = install.run(&request, None, &mut host).expect("init");
        assert!(!report.initialized);
        assert_eq!(report.steps[0].state, "waits");
        assert!(
            report.steps[0]
                .detail
                .as_deref()
                .is_some_and(|detail| detail.contains("vapor decisions list")),
            "{report:?}"
        );
    }

    #[test]
    fn start_at_login_follows_the_answer_and_a_no_is_not_asked_again() {
        // The flag says no: nothing is looked at and nothing installed.
        let install = Install::fresh();
        let mut host = Scripted::default();
        let request = InitRequest {
            auto_launch: Some(false),
            ..install.flags()
        };
        let report = install.run(&request, None, &mut host).expect("init");
        assert!(report.initialized, "off is an answer: {report:?}");
        assert_eq!(report.steps[2].state, "off");
        assert_eq!(host.calls, ["folder filesystem default", "decline"]);

        // A later run on a terminal does not ask again.
        host.calls.clear();
        let mut answers = Answers::new(&[]);
        let report = install
            .run(&InitRequest::default(), Some(&mut answers), &mut host)
            .expect("second run");
        assert_eq!(report.steps[2].state, "off");
        assert!(answers.asked.is_empty(), "{:?}", answers.asked);
        assert_eq!(host.calls, ["service"]);

        // An explicit yes installs.
        host.calls.clear();
        let request = InitRequest {
            auto_launch: Some(true),
            linger: Some(false),
            ..InitRequest::default()
        };
        let report = install.run(&request, None, &mut host).expect("third run");
        assert_eq!(report.steps[2].state, "done");
        assert_eq!(host.calls, ["service", "install Some(false)"]);
    }

    #[test]
    fn a_terminal_answer_of_no_records_it() {
        let install = Install::fresh();
        let mut host = Scripted::default();
        // The provider and the mode take the suggestion; the folders
        // came as options.
        let mut answers = Answers::new(&["", "", "no"]);
        let report = install
            .run(&install.flags(), Some(&mut answers), &mut host)
            .expect("init");
        assert_eq!(report.steps[2].state, "off");
        assert_eq!(
            host.calls,
            ["folder filesystem default", "service", "decline"]
        );
    }

    #[test]
    fn a_service_that_cannot_be_installed_is_a_failed_step_and_not_a_lost_profile() {
        let install = Install::fresh();
        let mut host = Scripted {
            install: Err("vapord binary not found".to_string()),
            ..Scripted::default()
        };
        let report = install
            .run(&install.flags(), None, &mut host)
            .expect("init");
        assert!(!report.initialized);
        assert_eq!(
            states(&report),
            [
                ("profile", "done"),
                ("sign-in", "not-needed"),
                ("service", "failed"),
            ]
        );
        assert_eq!(profiles::list(&install.config()).profiles.len(), 1);
    }

    #[test]
    fn a_platform_with_no_service_manager_is_settled_without_the_service() {
        let install = Install::fresh();
        let mut host = Scripted {
            service: ServiceState::Unavailable("no service manager here".to_string()),
            ..Scripted::default()
        };
        let report = install
            .run(&install.flags(), None, &mut host)
            .expect("init");
        assert!(report.initialized);
        assert_eq!(report.steps[2].state, "unavailable");
    }

    #[test]
    fn options_for_a_profile_are_not_used_when_one_exists() {
        let install = Install::fresh();
        let mut host = Scripted::default();
        install
            .run(&install.flags(), None, &mut host)
            .expect("first run");
        let request = InitRequest {
            local: Some(install.folder("another")),
            cloud: Some(install.folder("another-cloud")),
            ..InitRequest::default()
        };
        let report = install.run(&request, None, &mut host).expect("second run");
        assert_eq!(report.profiles.len(), 1, "no second profile");
        assert!(
            host.notes
                .iter()
                .any(|note| note.contains("A profile exists already")),
            "{:?}",
            host.notes
        );
    }

    #[test]
    fn a_vapor_json_the_daemon_refuses_is_refused() {
        let install = Install::fresh();
        std::fs::write(install.config(), "{ not json").expect("write");
        let mut host = Scripted::default();
        let error = install
            .run(&install.flags(), None, &mut host)
            .expect_err("a broken file");
        assert!(error.to_string().contains("vapor.json"), "{error}");
        assert!(host.calls.is_empty());
    }

    #[test]
    fn the_json_shape_is_stable() {
        let report = InitReport {
            initialized: false,
            steps: vec![
                InitStep::new(names::STEP_PROFILE, names::STATE_DONE, None),
                InitStep::new(
                    names::STEP_SIGN_IN,
                    names::STATE_WAITS,
                    Some("sign in".to_string()),
                ),
                InitStep::new(names::STEP_SERVICE, names::STATE_OFF, None),
            ],
            profiles: vec![ProfileRow {
                id: "default".to_string(),
                name: "default".to_string(),
                provider: "gdrive".to_string(),
                local_root: Some("/home/alex/Vapor".to_string()),
                cloud_root: Some("/Vapor".to_string()),
                sync_mode: "two-way".to_string(),
                enabled: true,
                problem: None,
            }],
            daemon: Some(DaemonLine {
                state: "sign-in-required".to_string(),
                reason: "sign in again".to_string(),
            }),
        };
        assert_eq!(
            serde_json::to_string(&report).expect("json"),
            concat!(
                r#"{"initialized":false,"steps":["#,
                r#"{"step":"profile","state":"done"},"#,
                r#"{"step":"sign-in","state":"waits","detail":"sign in"},"#,
                r#"{"step":"service","state":"off"}],"#,
                r#""profiles":[{"id":"default","name":"default","provider":"gdrive","#,
                r#""localRoot":"/home/alex/Vapor","cloudRoot":"/Vapor","syncMode":"two-way","#,
                r#""enabled":true}],"#,
                r#""daemon":{"state":"sign-in-required","reason":"sign in again"}}"#
            )
        );
    }
}
