//! `vapor profiles`: list, add, and remove sync profiles.
//!
//! A profile is one pair of folders that Vapor keeps in sync: a local
//! root, a cloud root, a provider, and a sync mode. `add` is the setup
//! of an install: until the first profile exists the daemon syncs
//! nothing. The apps run these commands underneath their own forms, so
//! the checks live here once: a provider the daemon knows, a sync mode
//! it knows, roots that do not overlap.
//!
//! Every write goes through the configuration lock. The first profile
//! composes in a running daemon without a restart; a profile added next
//! to running ones starts at the next daemon start.

use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use serde::Serialize;
use serde_json::{Map, Value, json};
use vapor_daemon::profiles::{ProfileSet, ResolvedProfile, is_valid_profile_id};
use vapor_shared::config::VaporConfig;
use vapor_shared::{SyncMode, constants};

#[derive(Debug)]
pub enum ProfilesError {
    Io(io::Error),
    /// `vapor.json` is not a JSON object the command can edit.
    UnreadableConfig(String),
    /// What the request asks for cannot be a profile; the text says why.
    Invalid(String),
    UnknownProfile {
        requested: String,
        known: Vec<String>,
    },
    /// The daemon holds the profile's state; it has to stop first.
    DaemonRunning,
    /// The user answered no to a confirmation.
    Declined,
}

impl From<io::Error> for ProfilesError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl std::fmt::Display for ProfilesError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "{error}"),
            Self::UnreadableConfig(reason) => write!(
                f,
                "vapor.json cannot be edited: {reason}. Fix the file, then run the command again"
            ),
            Self::Invalid(reason) => write!(f, "{reason}"),
            Self::UnknownProfile { requested, known } if known.is_empty() => {
                write!(f, "no profile '{requested}': there is no profile yet")
            }
            Self::UnknownProfile { requested, known } => write!(
                f,
                "no profile '{requested}' (profiles: {})",
                known.join(", ")
            ),
            Self::DaemonRunning => write!(
                f,
                "the daemon is running and holds the profile's sync state; stop it first \
                 (vapor service stop), remove the profile, then start it again"
            ),
            Self::Declined => write!(f, "nothing was changed"),
        }
    }
}

/// One profile as the surfaces show it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileRow {
    pub id: String,
    pub name: String,
    pub provider: String,
    pub local_root: Option<String>,
    pub cloud_root: Option<String>,
    pub sync_mode: String,
    pub enabled: bool,
    /// Why the profile cannot run as it is written.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub problem: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfilesListReport {
    pub profiles: Vec<ProfileRow>,
    /// What stops `vapor.json` from giving the profiles it means to.
    pub problems: Vec<String>,
}

impl ProfilesListReport {
    pub fn render_text(&self) -> String {
        let mut out = String::new();
        if self.profiles.is_empty() && self.problems.is_empty() {
            out.push_str("No sync profile yet. Set Vapor up with: vapor init\n");
        }
        for profile in &self.profiles {
            out.push_str(&format!(
                "{}  {} -> {}  ({}, {}{})\n",
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
                if profile.enabled { "" } else { ", disabled" },
            ));
            if let Some(problem) = &profile.problem {
                out.push_str(&format!("    cannot run: {problem}\n"));
            }
        }
        for problem in &self.problems {
            out.push_str(&format!("problem: {problem}\n"));
        }
        out
    }
}

fn row(profile: &ResolvedProfile, sync_mode: &str) -> ProfileRow {
    ProfileRow {
        id: profile.id.clone(),
        name: profile.name.clone(),
        provider: profile.provider_kind.clone(),
        local_root: profile
            .scope
            .local_root
            .as_ref()
            .map(|path| path.display().to_string()),
        cloud_root: Some(profile.scope.cloud_root.clone()).filter(|root| !root.is_empty()),
        sync_mode: sync_mode.to_string(),
        enabled: profile.enabled,
        problem: profile.invalid.clone(),
    }
}

/// The sync mode a profile is written with, as the file spells it.
fn written_sync_mode(config: &VaporConfig, profile_id: &str) -> String {
    config
        .profiles
        .iter()
        .find(|profile| profile.id.trim() == profile_id)
        .and_then(|profile| profile.sync_mode.clone())
        .unwrap_or_else(|| config.sync_mode.clone())
}

/// `vapor profiles list`.
pub fn list(config_path: &Path) -> ProfilesListReport {
    let load = vapor_shared::config::load_from(config_path);
    let ProfileSet { profiles, problems } = vapor_daemon::profiles::resolve_profile_set(&load);
    ProfilesListReport {
        profiles: profiles
            .iter()
            .map(|profile| row(profile, &written_sync_mode(&load.config, &profile.id)))
            .collect(),
        problems,
    }
}

/// Where Vapor keeps what it knows about the profiles of an install.
#[derive(Clone, Debug)]
pub struct ProfileStorage {
    /// `vapor_dir/state`.
    pub state: PathBuf,
    /// `vapor_dir/trash`.
    pub trash: PathBuf,
}

impl ProfileStorage {
    pub fn of_this_install() -> Self {
        Self {
            state: vapor_shared::runtime_paths::state_directory(),
            trash: vapor_shared::runtime_paths::trash_directory(),
        }
    }
}

/// What `vapor profiles add` was given. The two folders are the user's
/// to choose: a terminal asks for them and offers a suggestion
/// ([`ask_for_missing`]), and with no terminal both have to be given.
/// The provider and the mode take their defaults when they are left
/// out.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AddRequest {
    pub id: Option<String>,
    pub name: Option<String>,
    pub provider: Option<String>,
    pub local: Option<String>,
    pub cloud: Option<String>,
    pub mode: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AddOutcome {
    pub profile: ProfileRow,
    /// `live`: a running daemon composes the profile within seconds.
    /// `restart`: other profiles run, so this one starts with the next
    /// daemon start.
    pub applies: &'static str,
    /// The command that signs the profile in, for a provider with an
    /// account.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sign_in: Option<String>,
    /// For a one-way mode: `confirmed` when the user said yes here,
    /// `asks` when the daemon will hold the profile behind a decision.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub one_way: Option<&'static str>,
}

impl AddOutcome {
    pub fn render_text(&self) -> String {
        let profile = &self.profile;
        let mut out = format!(
            "Added profile {}: {} -> {} ({}, {}).\n",
            profile.id,
            profile.local_root.as_deref().unwrap_or_default(),
            profile.cloud_root.as_deref().unwrap_or_default(),
            profile.provider,
            profile.sync_mode,
        );
        out.push_str(match self.applies {
            "live" => "A running daemon starts it within a few seconds.\n",
            _ => "It starts with the next daemon start: vapor service restart\n",
        });
        if let Some(command) = &self.sign_in {
            out.push_str(&format!("Sign in to {}: {command}\n", profile.provider));
        }
        if self.one_way == Some("asks") {
            out.push_str(
                "The profile syncs nothing until you confirm the one-way mode: vapor decisions list\n",
            );
        }
        out
    }
}

/// Asks the questions of the interactive setup. The real one talks to
/// the terminal; tests script the answers.
pub trait Prompter {
    /// Asks for a value; an empty answer takes `default`.
    fn ask(&mut self, question: &str, default: &str) -> io::Result<String>;
    /// Asks a yes/no question; an empty answer is no.
    fn confirm(&mut self, question: &str) -> io::Result<bool>;
}

/// Prompts on stderr and reads stdin, so stdout carries only the result.
pub struct TerminalPrompter;

impl TerminalPrompter {
    fn read_line(prompt: &str) -> io::Result<String> {
        let mut stderr = io::stderr().lock();
        write!(stderr, "{prompt}")?;
        stderr.flush()?;
        let mut answer = String::new();
        io::stdin().lock().read_line(&mut answer)?;
        Ok(answer.trim().to_string())
    }
}

impl Prompter for TerminalPrompter {
    fn ask(&mut self, question: &str, default: &str) -> io::Result<String> {
        let answer = Self::read_line(&format!("{question} [{default}]: "))?;
        Ok(if answer.is_empty() {
            default.to_string()
        } else {
            answer
        })
    }

    fn confirm(&mut self, question: &str) -> io::Result<bool> {
        let answer = Self::read_line(&format!("{question} [y/N]: "))?;
        Ok(matches!(answer.to_ascii_lowercase().as_str(), "y" | "yes"))
    }
}

/// The ids of the profiles that `vapor.json` names now.
pub fn profile_ids(config_path: &Path) -> Vec<String> {
    list(config_path)
        .profiles
        .into_iter()
        .map(|profile| profile.id)
        .collect()
}

/// An id to offer for a profile of the folder `local`: the folder's
/// name when it can be an id, a numbered one otherwise.
fn suggested_id(local: &str, taken: &[String]) -> String {
    let from_folder: String = Path::new(local.trim())
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let from_folder = from_folder.trim_matches('-').to_string();
    if is_valid_profile_id(&from_folder) && !taken.contains(&from_folder) {
        return from_folder;
    }
    (taken.len() + 1..)
        .map(|number| format!("profile-{number}"))
        .find(|id| !taken.contains(id))
        .unwrap_or_default()
}

/// The questions of a setup, one function each: `vapor profiles add`
/// and `vapor init` ask them in their own order, in the same words and
/// with the same suggestion.
pub fn ask_provider(prompter: &mut dyn Prompter) -> io::Result<String> {
    prompter.ask(
        &format!("Provider ({})", constants::provider::ALL.join(", ")),
        constants::provider::DEFAULT,
    )
}

pub fn ask_local(prompter: &mut dyn Prompter) -> io::Result<String> {
    prompter.ask(
        "Folder on this device",
        constants::filtering::DEFAULT_LOCAL_ROOT,
    )
}

pub fn ask_cloud(prompter: &mut dyn Prompter, provider: &str) -> io::Result<String> {
    prompter.ask(
        "Folder in the cloud",
        constants::filtering::default_cloud_root(provider),
    )
}

pub fn ask_mode(prompter: &mut dyn Prompter) -> io::Result<String> {
    prompter.ask(
        &format!("Sync mode ({})", constants::sync_mode::ALL.join(", ")),
        constants::sync_mode::DEFAULT,
    )
}

/// Fills what the request leaves open by asking, with the default that
/// a plain `add` would take as the suggestion. `taken` holds the ids of
/// the profiles that exist: next to one of them the new profile needs
/// an id of its own, so that is asked too.
pub fn ask_for_missing(
    request: AddRequest,
    taken: &[String],
    prompter: &mut dyn Prompter,
) -> Result<AddRequest, ProfilesError> {
    let provider = match request.provider {
        Some(provider) => provider,
        None => ask_provider(prompter)?,
    };
    let local = match request.local {
        Some(local) => local,
        None => ask_local(prompter)?,
    };
    let cloud = match request.cloud {
        Some(cloud) => cloud,
        None => ask_cloud(prompter, &provider)?,
    };
    let mode = match request.mode {
        Some(mode) => mode,
        None => ask_mode(prompter)?,
    };
    let id = match request.id {
        Some(id) => Some(id),
        None if taken.is_empty() => None,
        None => Some(prompter.ask(
            "Id of the new profile (lowercase letters, digits, `-`, `_`)",
            &suggested_id(&local, taken),
        )?),
    };
    Ok(AddRequest {
        id,
        provider: Some(provider),
        local: Some(local),
        cloud: Some(cloud),
        mode: Some(mode),
        ..request
    })
}

/// A profile ready to be written: every field decided and checked.
#[derive(Clone, Debug, Eq, PartialEq)]
struct PlannedProfile {
    id: String,
    name: Option<String>,
    provider: String,
    local: String,
    cloud: String,
    mode: SyncMode,
    /// The roots as the daemon resolves them.
    local_resolved: PathBuf,
    cloud_resolved: String,
}

impl PlannedProfile {
    /// The local root as the daemon holds it: canonical. A one-way
    /// question and its confirmation are about exactly that path.
    fn local_as_the_daemon_holds_it(&self) -> PathBuf {
        vapor_shared::paths::canonicalize_ahead(&self.local_resolved)
    }
}

fn read_document(config_path: &Path) -> Result<Map<String, Value>, ProfilesError> {
    match std::fs::read_to_string(config_path) {
        Ok(contents) if contents.trim().is_empty() => Ok(Map::new()),
        Ok(contents) => match serde_json::from_str::<Value>(&contents) {
            Ok(Value::Object(map)) => Ok(map),
            Ok(_) => Err(ProfilesError::UnreadableConfig(
                "it is not a JSON object".to_string(),
            )),
            Err(error) => Err(ProfilesError::UnreadableConfig(error.to_string())),
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Map::new()),
        Err(error) => Err(ProfilesError::Io(error)),
    }
}

fn plan(
    existing: &[ResolvedProfile],
    top_provider: &str,
    request: &AddRequest,
) -> Result<PlannedProfile, ProfilesError> {
    let invalid = |reason: String| Err(ProfilesError::Invalid(reason));
    let id = match request.id.as_deref().map(str::trim) {
        Some(id) => id.to_string(),
        None if existing.is_empty() => constants::profile::DEFAULT_PROFILE_ID.to_string(),
        None => {
            return invalid(
                "a profile exists already; give the new one an id: --id <name>".to_string(),
            );
        }
    };
    if !is_valid_profile_id(&id) {
        return invalid(format!(
            "'{id}' is not a profile id: use lowercase letters, digits, `-` and `_`"
        ));
    }
    if existing.iter().any(|profile| profile.id == id) {
        return invalid(format!("a profile with the id '{id}' exists already"));
    }
    let provider = request
        .provider
        .clone()
        .unwrap_or_else(|| top_provider.to_string());
    let provider = known_provider(&provider)?;
    let mode_text = request
        .mode
        .clone()
        .unwrap_or_else(|| constants::sync_mode::DEFAULT.to_string());
    let mode = vapor_daemon::sync_directories::parse_sync_mode(&mode_text)
        .map_err(ProfilesError::Invalid)?;
    // A profile exists only when the user chose its two folders. A
    // terminal asked for them before this point. With no terminal there
    // is nobody to ask, and a suggestion that nobody saw is no choice.
    let chosen = |folder: &Option<String>| folder.clone().filter(|path| !path.trim().is_empty());
    let (Some(local), Some(cloud)) = (chosen(&request.local), chosen(&request.cloud)) else {
        return invalid(
            "a profile needs its two folders: --local <path> --cloud <path> \
             (on a terminal the command asks for them)"
                .to_string(),
        );
    };
    // A path typed in a terminal is relative to the terminal's
    // directory. The daemon has no such directory, so the full path is
    // what gets written.
    let local = full_path(&local);
    let cloud = if provider == constants::provider::FILESYSTEM {
        full_path(&cloud)
    } else {
        cloud
    };

    // The same checks, the same resolution, and the same overlap rule
    // the daemon runs.
    let candidate = VaporConfig {
        provider: provider.clone(),
        local_root: Some(local.clone()),
        cloud_root: Some(cloud.clone()),
        sync_mode: mode_text,
        ..VaporConfig::default()
    };
    if let Some(problem) = vapor_daemon::profiles::scope_problem(&candidate) {
        return invalid(problem);
    }
    let scope = vapor_daemon::sync_directories::resolve_with_config(&candidate);
    let Some(local_resolved) = scope.local_root else {
        return invalid(format!(
            "the local folder {local} is not usable: it has to be a folder path"
        ));
    };
    if provider == constants::provider::FILESYSTEM
        && let Some(reason) = vapor_daemon::sync_directories::filesystem_roots_overlap(
            &local_resolved,
            &scope.cloud_root,
        )
    {
        return invalid(format!("invalid sync directories: {reason}"));
    }
    Ok(PlannedProfile {
        id,
        name: request.name.clone().filter(|name| !name.trim().is_empty()),
        provider,
        local,
        cloud,
        mode,
        local_resolved,
        cloud_resolved: scope.cloud_root,
    })
}

/// `provider` trimmed, when it is a provider Vapor has.
pub fn known_provider(provider: &str) -> Result<String, ProfilesError> {
    let provider = provider.trim();
    if constants::provider::ALL.contains(&provider) {
        Ok(provider.to_string())
    } else {
        Err(ProfilesError::Invalid(format!(
            "unknown provider '{provider}'; use one of {}",
            constants::provider::ALL.join(", ")
        )))
    }
}

/// What a request would write, with nothing written: the id the
/// profile gets and its cloud folder as the daemon resolves it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Preview {
    pub id: String,
    pub provider: String,
    pub cloud_root: String,
}

/// Plans a request and writes nothing. It refuses what [`add`] refuses,
/// so a setup that asks more questions after the folders learns of a
/// bad folder before it asks them.
pub fn preview(config_path: &Path, request: &AddRequest) -> Result<Preview, ProfilesError> {
    use vapor_shared::runtime_paths::with_config_lock;
    let planned = with_config_lock(config_path, || plan_on_disk(config_path, request))?.planned;
    Ok(Preview {
        id: planned.id,
        provider: planned.provider,
        cloud_root: planned.cloud_resolved,
    })
}

/// The profile the request gives with `vapor.json` as it is now, and
/// what writing it needs.
struct PlanOnDisk {
    planned: PlannedProfile,
    document: Map<String, Value>,
    config: VaporConfig,
    existing: Vec<ResolvedProfile>,
}

/// Plans the request against the file on disk. The caller holds the
/// configuration lock.
fn plan_on_disk(config_path: &Path, request: &AddRequest) -> Result<PlanOnDisk, ProfilesError> {
    let document = read_document(config_path)?;
    let load = vapor_shared::config::load_from(config_path);
    // A file the daemon refuses stays refused with one more profile in
    // it, and a `profiles` value of the wrong shape would be replaced.
    let problems = vapor_daemon::profiles::unusable_configuration(&load);
    if !problems.is_empty() {
        return Err(ProfilesError::UnreadableConfig(problems.join("; ")));
    }
    let existing = vapor_daemon::profiles::resolve_profiles(&load.config);
    let planned = plan(&existing, &load.config.provider, request)?;
    Ok(PlanOnDisk {
        planned,
        document,
        config: load.config,
        existing,
    })
}

/// `raw` as a full path: a `~` path and an absolute path stay as they
/// are; a relative path is anchored at the current directory.
fn full_path(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed == "~" || trimmed.starts_with("~/") || Path::new(trimmed).is_absolute() {
        return trimmed.to_string();
    }
    std::path::absolute(trimmed)
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| trimmed.to_string())
}

fn profile_entry(profile: &PlannedProfile) -> Value {
    use constants::profile as keys;
    let mut entry = json!({
        keys::KEY_ID: profile.id,
        keys::KEY_PROVIDER: profile.provider,
        keys::KEY_LOCAL_ROOT: profile.local,
        keys::KEY_CLOUD_ROOT: profile.cloud,
        keys::KEY_SYNC_MODE: profile.mode.as_config_value(),
    });
    if let Some(name) = &profile.name {
        entry[keys::KEY_NAME] = json!(name);
    }
    entry
}

/// Writes the planned profile into the document. The first profile of
/// an install goes into the top-level keys (the implicit `default`
/// profile); a profile next to others goes into the `profiles` list,
/// and an implicit profile moves into the list with it, so it stays a
/// profile.
fn write_profile(
    document: &mut Map<String, Value>,
    config: &VaporConfig,
    profile: &PlannedProfile,
) {
    use constants::config as keys;
    let has_list = !config.profiles.is_empty();
    let implicit_exists = !has_list
        && (config.configured_local_root().is_some() || config.configured_cloud_root().is_some());
    if !has_list && !implicit_exists && profile.id == constants::profile::DEFAULT_PROFILE_ID {
        document.insert(keys::KEY_PROVIDER.to_string(), json!(profile.provider));
        document.insert(keys::KEY_LOCAL_ROOT.to_string(), json!(profile.local));
        document.insert(keys::KEY_CLOUD_ROOT.to_string(), json!(profile.cloud));
        document.insert(
            keys::KEY_SYNC_MODE.to_string(),
            json!(profile.mode.as_config_value()),
        );
        return;
    }
    let mut list = match document.remove(keys::KEY_PROFILES) {
        Some(Value::Array(list)) => list,
        _ => Vec::new(),
    };
    if implicit_exists {
        let mut implicit = json!({
            constants::profile::KEY_ID: constants::profile::DEFAULT_PROFILE_ID,
            constants::profile::KEY_PROVIDER: config.provider,
            keys::KEY_SYNC_MODE: config.sync_mode,
        });
        if let Some(local) = config.configured_local_root() {
            implicit[keys::KEY_LOCAL_ROOT] = json!(local);
        }
        if let Some(cloud) = config.configured_cloud_root() {
            implicit[keys::KEY_CLOUD_ROOT] = json!(cloud);
        }
        list.push(implicit);
        // The roots belong to that profile now; left at the top level
        // they would be inherited by every profile that sets none.
        document.remove(keys::KEY_LOCAL_ROOT);
        document.remove(keys::KEY_CLOUD_ROOT);
    }
    list.push(profile_entry(profile));
    document.insert(keys::KEY_PROFILES.to_string(), Value::Array(list));
}

/// `vapor profiles add`. `prompter` is given on a terminal: it confirms
/// a one-way mode here, so the daemon does not have to ask.
pub fn add(
    config_path: &Path,
    storage: &ProfileStorage,
    request: &AddRequest,
    prompter: Option<&mut dyn Prompter>,
    now: SystemTime,
) -> Result<AddOutcome, ProfilesError> {
    use vapor_shared::runtime_paths::with_config_lock;
    // The question waits for a person. It is asked with the lock
    // released: the app, `vapor config set`, and the daemon write the
    // same file, and each would wait for the answer.
    let mut confirmed = None;
    if let Some(prompter) = prompter {
        let planned = with_config_lock(config_path, || plan_on_disk(config_path, request))?.planned;
        if planned.mode != SyncMode::TwoWay {
            let question = vapor_daemon::one_way::question(
                planned.mode,
                &planned.local_as_the_daemon_holds_it(),
                &planned.cloud_resolved,
            );
            if !prompter.confirm(&question)? {
                return Err(ProfilesError::Declined);
            }
            confirmed = Some(planned);
        }
    }
    let (planned, others_run) = with_config_lock(config_path, || {
        let PlanOnDisk {
            planned,
            mut document,
            config,
            existing,
        } = plan_on_disk(config_path, request)?;
        if confirmed.as_ref().is_some_and(|asked| *asked != planned) {
            // The answer was about another mirror than the file gives now.
            return Err(ProfilesError::Invalid(
                "vapor.json changed while the question was open; run the command again".to_string(),
            ));
        }
        write_profile(&mut document, &config, &planned);
        vapor_shared::runtime_paths::write_config_document(config_path, &Value::Object(document))?;
        // A profile that can run next to this one keeps the daemon
        // shaped as it is until a restart.
        let others_run = existing.iter().any(|profile| profile.invalid.is_none());
        Ok((planned, others_run))
    })?;

    let one_way = if planned.mode == SyncMode::TwoWay {
        None
    } else if confirmed.is_some() {
        // The user said yes above: record it where the daemon looks.
        let database =
            vapor_shared::runtime_paths::profile_database_path_in(&storage.state, &planned.id);
        match vapor_daemon::state_db::DurableStateDb::open(&database) {
            Ok(mut state_db) => vapor_daemon::one_way::acknowledge(
                &mut state_db,
                planned.mode,
                &planned.local_as_the_daemon_holds_it(),
                &planned.cloud_resolved,
                now,
            )
            .map(|()| Some("confirmed"))
            .unwrap_or(Some("asks")),
            Err(_) => Some("asks"),
        }
    } else {
        Some("asks")
    };
    let sign_in = constants::provider::SIGN_IN_REQUIRED
        .contains(&planned.provider.as_str())
        .then(|| format!("vapor auth login --profile {}", planned.id));
    Ok(AddOutcome {
        profile: ProfileRow {
            id: planned.id.clone(),
            name: planned.name.clone().unwrap_or_else(|| planned.id.clone()),
            provider: planned.provider.clone(),
            local_root: Some(planned.local_resolved.display().to_string()),
            cloud_root: Some(planned.cloud_resolved.clone()),
            sync_mode: planned.mode.as_config_value().to_string(),
            enabled: true,
            problem: None,
        },
        applies: if others_run { "restart" } else { "live" },
        sign_in,
        one_way,
    })
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoveOutcome {
    pub removed: String,
    /// The folders stay where they are; this names them.
    pub local_root: Option<String>,
    pub cloud_root: Option<String>,
    /// Where the files Vapor removed for this profile still are, when
    /// its trash holds any. The daemon deletes each one when its
    /// retention time ends, as it does for a profile that runs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trash: Option<String>,
}

impl RemoveOutcome {
    pub fn render_text(&self) -> String {
        let mut out = format!(
            "Removed profile {}. Vapor forgot its sync state and its sign-in. \
             The files stay: {} and {}.\n",
            self.removed,
            self.local_root.as_deref().unwrap_or("(no local folder)"),
            self.cloud_root.as_deref().unwrap_or("(no cloud folder)"),
        );
        if let Some(trash) = &self.trash {
            out.push_str(&format!(
                "Files that Vapor removed for this profile are still in {trash}. \
                 Vapor deletes each one when its trash retention time ends; \
                 copy out what you still need.\n"
            ));
        }
        out
    }
}

/// The question `remove` asks on a terminal.
pub fn remove_question(profile: &ProfileRow) -> String {
    if profile.problem.as_deref() == Some(LEFT_THE_CONFIGURATION) {
        return format!(
            "Profile {} is not in vapor.json any more. Remove what Vapor still keeps about it \
             (its sync state and its sign-in)? No file is touched.",
            profile.id
        );
    }
    format!(
        "Remove profile {}? Vapor stops syncing {} with {}. The files on both sides stay.",
        profile.id,
        profile.local_root.as_deref().unwrap_or("its local folder"),
        profile.cloud_root.as_deref().unwrap_or("its cloud folder"),
    )
}

/// What a row says about a profile that `vapor.json` no longer names
/// while its sync state is still on disk.
const LEFT_THE_CONFIGURATION: &str = "not in vapor.json; only its sync state is left";

/// The profile `profile_id` when `vapor.json` no longer names it and
/// its state is still on disk: what a hand edit of the file leaves.
fn leftover_profile(storage: &ProfileStorage, profile_id: &str) -> Option<ProfileRow> {
    if !is_valid_profile_id(profile_id) {
        return None;
    }
    let database =
        vapor_shared::runtime_paths::profile_database_path_in(&storage.state, profile_id);
    let state_is_there = if profile_id == constants::profile::DEFAULT_PROFILE_ID {
        database.exists()
    } else {
        database.parent().is_some_and(Path::exists)
    };
    state_is_there.then(|| ProfileRow {
        id: profile_id.to_string(),
        name: profile_id.to_string(),
        provider: String::new(),
        local_root: None,
        cloud_root: None,
        sync_mode: String::new(),
        enabled: false,
        problem: Some(LEFT_THE_CONFIGURATION.to_string()),
    })
}

/// `vapor profiles remove`: takes the profile out of `vapor.json` and
/// deletes its sync state and its credentials. The user's folders are
/// never touched. The daemon has to be stopped: it holds the state.
///
/// A profile that `vapor.json` no longer names, with its state still on
/// disk, is removed the same way: there is nothing to take out of the
/// file, and the state and the credentials go.
///
/// `open_secrets` is called only when the profile can own a credential:
/// its provider has an account, or the provider is not known any more.
pub fn remove(
    config_path: &Path,
    storage: &ProfileStorage,
    profile_id: &str,
    daemon_running: bool,
    open_secrets: &dyn Fn() -> Arc<dyn vapor_platform::SecretStore>,
    confirm: &mut dyn FnMut(&ProfileRow) -> io::Result<bool>,
) -> Result<RemoveOutcome, ProfilesError> {
    use constants::config as keys;
    // With a file that cannot name profiles, "the profile is not in
    // vapor.json" is not known, and its state must not go as a leftover.
    let problems = vapor_daemon::profiles::unusable_configuration(
        &vapor_shared::config::load_from(config_path),
    );
    if !problems.is_empty() {
        return Err(ProfilesError::UnreadableConfig(problems.join("; ")));
    }
    let report = list(config_path);
    let configured = report
        .profiles
        .iter()
        .find(|profile| profile.id == profile_id)
        .cloned();
    let in_configuration = configured.is_some();
    let Some(profile) = configured.or_else(|| leftover_profile(storage, profile_id)) else {
        return Err(ProfilesError::UnknownProfile {
            requested: profile_id.to_string(),
            known: report.profiles.iter().map(|p| p.id.clone()).collect(),
        });
    };
    if daemon_running {
        return Err(ProfilesError::DaemonRunning);
    }
    if !confirm(&profile)? {
        return Err(ProfilesError::Declined);
    }
    if in_configuration {
        vapor_shared::runtime_paths::with_config_lock(config_path, || {
            let mut document = read_document(config_path)?;
            let list_entries = match document.get(keys::KEY_PROFILES) {
                Some(Value::Array(list)) => list.clone(),
                _ => Vec::new(),
            };
            if list_entries.is_empty() {
                // The implicit profile: its two roots are what make it one.
                document.remove(keys::KEY_LOCAL_ROOT);
                document.remove(keys::KEY_CLOUD_ROOT);
                document.remove(keys::KEY_SYNC_MODE);
            } else {
                let kept: Vec<Value> = list_entries
                    .into_iter()
                    .filter(|entry| {
                        entry[constants::profile::KEY_ID].as_str().map(str::trim)
                            != Some(profile_id)
                    })
                    .collect();
                if kept.is_empty() {
                    document.remove(keys::KEY_PROFILES);
                } else {
                    document.insert(keys::KEY_PROFILES.to_string(), Value::Array(kept));
                }
            }
            vapor_shared::runtime_paths::write_config_document(
                config_path,
                &Value::Object(document),
            )
            .map_err(ProfilesError::Io)
        })?;
    }
    let may_hold_a_credential = !in_configuration
        || constants::provider::SIGN_IN_REQUIRED.contains(&profile.provider.as_str());
    let secrets = may_hold_a_credential.then(open_secrets);
    vapor_daemon::profiles::forget_profile(&storage.state, profile_id, secrets.as_deref())?;
    let trash = storage.trash.join(profile_id);
    let trash_holds_files = std::fs::read_dir(&trash)
        .map(|mut entries| entries.next().is_some())
        .unwrap_or(false);
    Ok(RemoveOutcome {
        removed: profile.id,
        local_root: profile.local_root,
        cloud_root: profile.cloud_root,
        trash: trash_holds_files.then(|| trash.display().to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn config_path(temp: &TempDir) -> PathBuf {
        temp.path().join("vapor.json")
    }

    fn storage(temp: &TempDir) -> ProfileStorage {
        ProfileStorage {
            state: temp.path().join("state"),
            trash: temp.path().join("trash"),
        }
    }

    /// The secret store of a profile that must have none.
    fn no_secrets() -> Arc<dyn vapor_platform::SecretStore> {
        panic!("a profile whose provider has no account must not open the secret store")
    }

    fn now() -> SystemTime {
        SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_750_000_000)
    }

    /// A request for a filesystem profile under `temp`.
    fn request(temp: &TempDir, name: &str) -> AddRequest {
        AddRequest {
            local: Some(
                temp.path()
                    .join(format!("{name}-local"))
                    .display()
                    .to_string(),
            ),
            cloud: Some(
                temp.path()
                    .join(format!("{name}-cloud"))
                    .display()
                    .to_string(),
            ),
            ..AddRequest::default()
        }
    }

    struct Scripted {
        answers: Vec<&'static str>,
        asked: Vec<String>,
    }

    impl Scripted {
        fn new(answers: &[&'static str]) -> Self {
            Self {
                answers: answers.to_vec(),
                asked: Vec::new(),
            }
        }
    }

    impl Prompter for Scripted {
        fn ask(&mut self, question: &str, default: &str) -> io::Result<String> {
            self.asked.push(question.to_string());
            let answer = self.answers.remove(0);
            Ok(if answer.is_empty() {
                default.to_string()
            } else {
                answer.to_string()
            })
        }

        fn confirm(&mut self, question: &str) -> io::Result<bool> {
            self.asked.push(question.to_string());
            Ok(self.answers.remove(0) == "y")
        }
    }

    #[test]
    fn a_fresh_install_lists_no_profile_and_names_the_command() {
        let temp = TempDir::new().expect("temp");
        let report = list(&config_path(&temp));
        assert!(report.profiles.is_empty());
        assert!(report.problems.is_empty());
        assert!(report.render_text().contains("vapor init"));
    }

    #[test]
    fn the_first_add_writes_the_top_level_scope_and_applies_live() {
        let temp = TempDir::new().expect("temp");
        let path = config_path(&temp);
        let outcome =
            add(&path, &storage(&temp), &request(&temp, "home"), None, now()).expect("add");
        assert_eq!(outcome.profile.id, "default");
        assert_eq!(outcome.applies, "live");
        assert_eq!(outcome.sign_in, None);
        assert_eq!(outcome.one_way, None);

        let document: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("read")).expect("json");
        assert_eq!(document["provider"], "filesystem");
        assert_eq!(document["syncMode"], "two-way");
        assert!(
            document["localRoot"]
                .as_str()
                .expect("root")
                .ends_with("home-local")
        );
        assert!(document.get("profiles").is_none());

        let listed = list(&path);
        assert_eq!(listed.profiles.len(), 1);
        assert_eq!(listed.profiles[0].problem, None);
        assert!(
            !temp.path().join("home-local").exists(),
            "adding a profile creates no folder; the daemon adopts the roots"
        );
    }

    #[test]
    fn a_second_profile_keeps_the_first_one_and_waits_for_a_restart() {
        let temp = TempDir::new().expect("temp");
        let path = config_path(&temp);
        add(&path, &storage(&temp), &request(&temp, "home"), None, now()).expect("first");
        let needs_id =
            add(&path, &storage(&temp), &request(&temp, "work"), None, now()).expect_err("no id");
        assert!(needs_id.to_string().contains("--id"), "{needs_id}");

        let second = AddRequest {
            id: Some("work".to_string()),
            name: Some("Work files".to_string()),
            ..request(&temp, "work")
        };
        let outcome = add(&path, &storage(&temp), &second, None, now()).expect("second");
        assert_eq!(outcome.applies, "restart");
        assert_eq!(outcome.profile.name, "Work files");

        let listed = list(&path);
        let ids: Vec<&str> = listed.profiles.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, vec!["default", "work"], "the first profile stays");
        assert!(
            listed.profiles.iter().all(|p| p.problem.is_none()),
            "{listed:?}"
        );
        assert!(
            listed.profiles[0]
                .local_root
                .as_deref()
                .is_some_and(|root| root.ends_with("home-local"))
        );
        let document: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("read")).expect("json");
        assert!(
            document.get("localRoot").is_none(),
            "the first profile's roots moved into its entry"
        );
    }

    #[test]
    fn add_refuses_what_the_daemon_would_refuse() {
        let temp = TempDir::new().expect("temp");
        let path = config_path(&temp);
        let base = request(&temp, "home");
        let cases = [
            (
                AddRequest {
                    provider: Some("dropbox".to_string()),
                    ..base.clone()
                },
                "unknown provider 'dropbox'",
            ),
            (
                AddRequest {
                    mode: Some("mirror".to_string()),
                    ..base.clone()
                },
                "unknown syncMode \"mirror\"",
            ),
            (
                AddRequest {
                    id: Some("Not A Slug".to_string()),
                    ..base.clone()
                },
                "is not a profile id",
            ),
            (
                AddRequest {
                    cloud: Some(format!("{}/inside", base.local.clone().unwrap())),
                    ..base.clone()
                },
                "invalid sync directories",
            ),
        ];
        for (request, expected) in cases {
            let error = add(&path, &storage(&temp), &request, None, now()).expect_err(expected);
            assert!(error.to_string().contains(expected), "{error}");
        }
        assert!(!path.exists(), "a refused add writes nothing");
    }

    #[test]
    fn a_relative_folder_is_written_as_a_full_path() {
        let temp = TempDir::new().expect("temp");
        let path = config_path(&temp);
        let outcome = add(
            &path,
            &storage(&temp),
            &AddRequest {
                local: Some("vapor-test-relative-local".to_string()),
                cloud: Some(temp.path().join("cloud").display().to_string()),
                ..AddRequest::default()
            },
            None,
            now(),
        )
        .expect("add");
        let written = outcome.profile.local_root.expect("local root");
        assert!(Path::new(&written).is_absolute(), "{written}");
        assert!(written.ends_with("vapor-test-relative-local"), "{written}");
        let listed = list(&path);
        assert_eq!(listed.profiles[0].problem, None, "{listed:?}");

        // A relative root written by hand is a profile that cannot run,
        // and the list says why.
        std::fs::write(
            &path,
            r#"{ "localRoot": "photos", "cloudRoot": "/tmp/vapor-test-cloud" }"#,
        )
        .expect("write");
        let listed = list(&path);
        assert!(
            listed.profiles[0]
                .problem
                .as_deref()
                .is_some_and(|problem| problem.contains("relative path")),
            "{listed:?}"
        );
    }

    #[test]
    fn a_provider_with_an_account_names_the_sign_in_command() {
        let temp = TempDir::new().expect("temp");
        let path = config_path(&temp);
        let outcome = add(
            &path,
            &storage(&temp),
            &AddRequest {
                provider: Some("gdrive".to_string()),
                local: Some(temp.path().join("drive").display().to_string()),
                cloud: Some("/Vapor".to_string()),
                ..AddRequest::default()
            },
            None,
            now(),
        )
        .expect("add");
        assert_eq!(outcome.profile.cloud_root.as_deref(), Some("/Vapor"));
        assert_eq!(
            outcome.sign_in.as_deref(),
            Some("vapor auth login --profile default")
        );
    }

    #[test]
    fn with_no_terminal_both_folders_have_to_be_given() {
        let temp = TempDir::new().expect("temp");
        let path = config_path(&temp);
        let both = request(&temp, "home");
        for incomplete in [
            AddRequest::default(),
            AddRequest {
                cloud: None,
                ..both.clone()
            },
            AddRequest {
                local: None,
                ..both.clone()
            },
            AddRequest {
                local: Some("  ".to_string()),
                ..both.clone()
            },
        ] {
            let refused = add(&path, &storage(&temp), &incomplete, None, now())
                .expect_err("a suggestion that nobody saw is no choice");
            assert!(
                refused
                    .to_string()
                    .contains("--local <path> --cloud <path>"),
                "{refused}"
            );
            assert!(!path.exists(), "nothing is written");
        }
        add(&path, &storage(&temp), &both, None, now()).expect("both folders given");
    }

    #[test]
    fn with_no_flags_the_setup_asks_and_an_empty_answer_takes_the_suggestion() {
        let mut prompter = Scripted::new(&["", "/tmp/vapor-test-local", "", "push-only"]);
        let filled = ask_for_missing(AddRequest::default(), &[], &mut prompter).expect("answers");
        assert_eq!(filled.id, None, "the first profile needs no id");
        assert_eq!(filled.provider.as_deref(), Some("filesystem"));
        assert_eq!(filled.local.as_deref(), Some("/tmp/vapor-test-local"));
        assert_eq!(filled.cloud.as_deref(), Some("~/cloud/Vapor"));
        assert_eq!(filled.mode.as_deref(), Some("push-only"));
        assert_eq!(prompter.asked.len(), 4);

        // A flag that was given is not asked again.
        let mut prompter = Scripted::new(&["", "", ""]);
        let filled = ask_for_missing(
            AddRequest {
                provider: Some("gdrive".to_string()),
                ..AddRequest::default()
            },
            &[],
            &mut prompter,
        )
        .expect("answers");
        assert_eq!(
            filled.cloud.as_deref(),
            Some("/Vapor"),
            "the provider's suggestion"
        );
        assert_eq!(prompter.asked.len(), 3);

        // Next to a profile that exists, the new one needs an id: it is
        // asked, and the folder's name is the suggestion.
        let taken = ["default".to_string()];
        let mut prompter = Scripted::new(&["", "", "", "", ""]);
        let filled = ask_for_missing(
            AddRequest {
                local: Some("/tmp/vapor-test/Work Files".to_string()),
                ..AddRequest::default()
            },
            &taken,
            &mut prompter,
        )
        .expect("answers");
        assert_eq!(filled.id.as_deref(), Some("work-files"));
        assert!(
            prompter
                .asked
                .last()
                .expect("asked")
                .contains("Id of the new profile")
        );
        assert_eq!(prompter.asked.len(), 4, "the folder was given");
        assert_eq!(suggested_id("/tmp/default", &taken), "profile-2");
        assert_eq!(suggested_id("/", &taken), "profile-2");

        // An id that was given is not asked.
        let mut prompter = Scripted::new(&["", "", "", ""]);
        let filled = ask_for_missing(
            AddRequest {
                id: Some("work".to_string()),
                ..AddRequest::default()
            },
            &taken,
            &mut prompter,
        )
        .expect("answers");
        assert_eq!(filled.id.as_deref(), Some("work"));
        assert_eq!(prompter.asked.len(), 4);
    }

    #[test]
    fn the_one_way_question_is_asked_with_the_configuration_lock_released() {
        /// Says yes after it took the configuration lock itself, the
        /// way the app or `vapor config set` would while the question
        /// is open.
        struct TakesTheLock {
            config_path: PathBuf,
            took_it: bool,
        }
        impl Prompter for TakesTheLock {
            fn ask(&mut self, _: &str, default: &str) -> io::Result<String> {
                Ok(default.to_string())
            }
            fn confirm(&mut self, _: &str) -> io::Result<bool> {
                let (sender, receiver) = std::sync::mpsc::channel();
                let config_path = self.config_path.clone();
                std::thread::spawn(move || {
                    let taken = vapor_shared::runtime_paths::with_config_lock(&config_path, || {
                        Ok::<(), io::Error>(())
                    });
                    let _ = sender.send(taken.is_ok());
                });
                self.took_it = receiver
                    .recv_timeout(std::time::Duration::from_secs(10))
                    .unwrap_or(false);
                Ok(true)
            }
        }
        let temp = TempDir::new().expect("temp");
        let path = config_path(&temp);
        let mut prompter = TakesTheLock {
            config_path: path.clone(),
            took_it: false,
        };
        let outcome = add(
            &path,
            &storage(&temp),
            &AddRequest {
                mode: Some("push-only".to_string()),
                ..request(&temp, "mirror")
            },
            Some(&mut prompter),
            now(),
        )
        .expect("add");
        assert!(
            prompter.took_it,
            "another writer of vapor.json does not wait for the answer"
        );
        assert_eq!(outcome.one_way, Some("confirmed"));
    }

    #[test]
    fn a_file_the_daemon_refuses_is_neither_added_to_nor_removed_from() {
        let temp = TempDir::new().expect("temp");
        let path = config_path(&temp);
        let work_state = storage(&temp).state.join("profiles").join("work");
        std::fs::create_dir_all(&work_state).expect("state");
        std::fs::write(work_state.join("vapor.sqlite"), b"state").expect("state");
        let mut yes = |_: &ProfileRow| Ok(true);
        let wrong_shape = r#"{ "profiles": { "id": "work", "localRoot": "/tmp/vapor-test-w" } }"#;
        let not_json = r#"{ "profiles": [ { "id": "work", "#;

        for contents in [wrong_shape, not_json] {
            std::fs::write(&path, contents).expect("write");
            let refused = add(&path, &storage(&temp), &request(&temp, "home"), None, now())
                .expect_err("the file names no profile the daemon would run");
            assert!(
                matches!(refused, ProfilesError::UnreadableConfig(_)),
                "{refused}"
            );
            assert_eq!(
                std::fs::read_to_string(&path).expect("read"),
                contents,
                "the file is the user's to fix"
            );

            // `work` may still be in the file. Its state is not a
            // leftover, and nothing of it goes.
            let refused = remove(&path, &storage(&temp), "work", false, &no_secrets, &mut yes)
                .expect_err("the file cannot say whether it names the profile");
            assert!(
                matches!(refused, ProfilesError::UnreadableConfig(_)),
                "{refused}"
            );
            assert!(work_state.join("vapor.sqlite").exists());
        }
    }

    #[test]
    fn a_one_way_profile_asks_on_a_terminal_and_leaves_it_to_the_daemon_elsewhere() {
        let temp = TempDir::new().expect("temp");
        let path = config_path(&temp);
        let one_way = AddRequest {
            mode: Some("pull-only".to_string()),
            ..request(&temp, "mirror")
        };

        // No terminal: the profile is written, and the daemon will ask.
        let outcome = add(&path, &storage(&temp), &one_way, None, now()).expect("add");
        assert_eq!(outcome.one_way, Some("asks"));
        assert!(outcome.render_text().contains("vapor decisions list"));
        std::fs::remove_file(&path).expect("reset");

        // On a terminal a "no" writes nothing.
        let mut no = Scripted::new(&["n"]);
        let declined =
            add(&path, &storage(&temp), &one_way, Some(&mut no), now()).expect_err("declined");
        assert!(matches!(declined, ProfilesError::Declined));
        assert!(no.asked[0].contains("removes each file that is only on this device"));
        assert!(!path.exists());

        // A "yes" is recorded where the daemon looks, so it does not ask.
        let mut yes = Scripted::new(&["y"]);
        let outcome = add(&path, &storage(&temp), &one_way, Some(&mut yes), now()).expect("add");
        assert_eq!(outcome.one_way, Some("confirmed"));
        let state_db = vapor_daemon::state_db::DurableStateDb::open(
            vapor_shared::runtime_paths::profile_database_path_in(&storage(&temp).state, "default"),
        )
        .expect("state");
        // The daemon asks with the canonical form of the folder, which
        // it creates. The confirmation has to be for that path.
        let local = PathBuf::from(one_way.local.as_deref().expect("local"));
        std::fs::create_dir_all(&local).expect("the daemon creates the folder");
        assert!(
            vapor_daemon::one_way::acknowledged(
                &state_db,
                SyncMode::PullOnly,
                &vapor_shared::paths::canonicalize(&local).expect("canonical"),
                outcome.profile.cloud_root.as_deref().expect("cloud"),
            )
            .expect("read")
        );
    }

    #[test]
    fn remove_takes_the_profile_out_and_never_touches_the_folders() {
        use vapor_platform::SecretStore;
        let temp = TempDir::new().expect("temp");
        let path = config_path(&temp);
        add(&path, &storage(&temp), &request(&temp, "home"), None, now()).expect("first");
        let work_local = temp.path().join("work-local");
        add(
            &path,
            &storage(&temp),
            &AddRequest {
                id: Some("work".to_string()),
                provider: Some("gdrive".to_string()),
                local: Some(work_local.display().to_string()),
                cloud: Some("/Work".to_string()),
                ..AddRequest::default()
            },
            None,
            now(),
        )
        .expect("second");
        std::fs::create_dir_all(&work_local).expect("folder");
        std::fs::write(work_local.join("keep.txt"), b"mine").expect("file");

        // What Vapor keeps about the two profiles, and a sign-in under
        // the default profile's name that this install does not own.
        let secrets = Arc::new(vapor_platform::InMemorySecretStore::new());
        for profile in ["work", "default"] {
            secrets
                .set(
                    &vapor_daemon::profiles::secret_key(profile, "gdrive", "token"),
                    "ya29.test",
                )
                .expect("seed");
        }
        let open_secrets = || -> Arc<dyn SecretStore> { secrets.clone() };
        let work_state = storage(&temp).state.join("profiles").join("work");
        std::fs::create_dir_all(&work_state).expect("state");
        std::fs::write(work_state.join("vapor.sqlite"), b"state").expect("state");
        let work_trash = storage(&temp).trash.join("work");
        std::fs::create_dir_all(work_trash.join("entry-1")).expect("trash");
        let mut yes = |_: &ProfileRow| Ok(true);
        let mut no = |_: &ProfileRow| Ok(false);

        let running = remove(
            &path,
            &storage(&temp),
            "work",
            true,
            &open_secrets,
            &mut yes,
        )
        .expect_err("daemon runs");
        assert!(matches!(running, ProfilesError::DaemonRunning));
        let unknown = remove(
            &path,
            &storage(&temp),
            "play",
            false,
            &open_secrets,
            &mut yes,
        )
        .expect_err("unknown");
        assert!(unknown.to_string().contains("default, work"), "{unknown}");
        assert!(matches!(
            remove(
                &path,
                &storage(&temp),
                "work",
                false,
                &open_secrets,
                &mut no
            ),
            Err(ProfilesError::Declined)
        ));
        assert_eq!(list(&path).profiles.len(), 2, "a no changes nothing");
        assert!(work_state.exists(), "a no changes nothing");

        let outcome = remove(
            &path,
            &storage(&temp),
            "work",
            false,
            &open_secrets,
            &mut yes,
        )
        .expect("remove");
        assert_eq!(outcome.removed, "work");
        assert!(work_local.join("keep.txt").exists(), "the files stay");
        assert!(!work_state.exists(), "the sync state goes");
        assert_eq!(
            secrets.list().expect("list"),
            vec![vapor_daemon::profiles::secret_key(
                "default", "gdrive", "token"
            )],
            "the profile's sign-in goes; another profile's stays"
        );
        assert_eq!(
            outcome.trash.as_deref(),
            Some(work_trash.display().to_string().as_str()),
            "the trash stays and the outcome names it"
        );
        assert!(work_trash.join("entry-1").exists());
        let ids: Vec<String> = list(&path).profiles.into_iter().map(|p| p.id).collect();
        assert_eq!(ids, vec!["default"]);

        // The default profile syncs with a plain folder: it has no
        // sign-in, so the secret store is not opened at all.
        let outcome = remove(
            &path,
            &storage(&temp),
            "default",
            false,
            &no_secrets,
            &mut yes,
        )
        .expect("remove the last one");
        assert_eq!(outcome.trash, None);
        let report = list(&path);
        assert!(
            report.profiles.is_empty() && report.problems.is_empty(),
            "{report:?}"
        );
    }

    #[test]
    fn a_profile_that_left_the_file_by_hand_can_still_be_removed() {
        use vapor_platform::SecretStore;
        let temp = TempDir::new().expect("temp");
        let path = config_path(&temp);
        add(&path, &storage(&temp), &request(&temp, "home"), None, now()).expect("first");
        // What a hand edit leaves: state and a sign-in for a profile
        // that the file no longer names.
        let old_state = storage(&temp).state.join("profiles").join("old");
        std::fs::create_dir_all(&old_state).expect("state");
        std::fs::write(old_state.join("vapor.sqlite"), b"state").expect("state");
        let secrets = Arc::new(vapor_platform::InMemorySecretStore::new());
        secrets
            .set(
                &vapor_daemon::profiles::secret_key("old", "gdrive", "token"),
                "ya29.test",
            )
            .expect("seed");
        let open_secrets = || -> Arc<dyn SecretStore> { secrets.clone() };
        let before = std::fs::read_to_string(&path).expect("config");

        let asked = std::cell::RefCell::new(Vec::new());
        let mut yes = |profile: &ProfileRow| {
            asked.borrow_mut().push(remove_question(profile));
            Ok(true)
        };
        let outcome = remove(
            &path,
            &storage(&temp),
            "old",
            false,
            &open_secrets,
            &mut yes,
        )
        .expect("remove the leftover");
        assert_eq!(outcome.removed, "old");
        assert!(
            asked.borrow()[0].contains("not in vapor.json any more"),
            "{:?}",
            asked.borrow()
        );
        assert!(!old_state.exists(), "the state goes");
        assert!(secrets.list().expect("list").is_empty(), "the sign-in goes");
        assert_eq!(
            std::fs::read_to_string(&path).expect("config"),
            before,
            "the profile that is in the file keeps its configuration"
        );

        // A name with nothing behind it is still unknown.
        let unknown = remove(
            &path,
            &storage(&temp),
            "never",
            false,
            &open_secrets,
            &mut yes,
        )
        .expect_err("unknown");
        assert!(matches!(unknown, ProfilesError::UnknownProfile { .. }));
    }

    #[test]
    fn the_json_shapes_are_stable() {
        let temp = TempDir::new().expect("temp");
        let path = config_path(&temp);
        // What the command writes for this folder: the path itself where
        // it is absolute, and with the drive on a host that needs one.
        let local = full_path("/tmp/vapor-shape-local");
        let outcome = add(
            &path,
            &storage(&temp),
            &AddRequest {
                provider: Some("gdrive".to_string()),
                local: Some("/tmp/vapor-shape-local".to_string()),
                cloud: Some("/Shape".to_string()),
                mode: Some("push-only".to_string()),
                ..AddRequest::default()
            },
            None,
            now(),
        )
        .expect("add");
        assert_eq!(
            serde_json::to_value(&outcome).expect("json"),
            json!({
                "profile": {
                    "id": "default",
                    "name": "default",
                    "provider": "gdrive",
                    "localRoot": local,
                    "cloudRoot": "/Shape",
                    "syncMode": "push-only",
                    "enabled": true
                },
                "applies": "live",
                "signIn": "vapor auth login --profile default",
                "oneWay": "asks"
            })
        );
        assert_eq!(
            serde_json::to_value(list(&path)).expect("json"),
            json!({
                "profiles": [{
                    "id": "default",
                    "name": "default",
                    "provider": "gdrive",
                    "localRoot": local,
                    "cloudRoot": "/Shape",
                    "syncMode": "push-only",
                    "enabled": true
                }],
                "problems": []
            })
        );
        let mut yes = |_: &ProfileRow| Ok(true);
        assert_eq!(
            serde_json::to_value(
                remove(
                    &path,
                    &storage(&temp),
                    "default",
                    false,
                    &|| -> Arc<dyn vapor_platform::SecretStore> {
                        Arc::new(vapor_platform::InMemorySecretStore::new())
                    },
                    &mut yes
                )
                .expect("remove")
            )
            .expect("json"),
            json!({
                "removed": "default",
                "localRoot": local,
                "cloudRoot": "/Shape"
            })
        );
    }
}
