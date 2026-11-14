//! `vapor decisions list|show|resolve` — the questions the daemon
//! parked because an irreversible action rested on ambiguous evidence.
//!
//! A decision lives in the profile's durable state DB, so this command
//! reads and answers it with or without a running daemon; the daemon
//! applies the answer on its next tick (or at its next start). A
//! `conflict` is the exception: its answer is applied here, at once.
//! App surfaces drive this command with `--json`. Design:
//! `docs/architecture/data-flow.md` §Decisions and
//! `docs/architecture/conflict-resolution.md`.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

use serde::Serialize;
use vapor_daemon::conflict::{self, Conflict};
use vapor_daemon::profiles::resolve_profiles;
use vapor_daemon::state_db::{DecisionRecord, DurableStateDb};
use vapor_daemon::trash::{LocalTrash, TrashSettings};
use vapor_shared::config::VaporConfig;
use vapor_shared::constants;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DecisionOptionJson {
    pub key: String,
    pub label: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DecisionJson {
    pub profile_id: String,
    pub id: i64,
    pub kind: String,
    pub scope: String,
    pub path: Option<PathBuf>,
    pub question: String,
    pub options: Vec<DecisionOptionJson>,
    pub evidence: serde_json::Value,
    pub created_at_ms: u64,
    pub resolved_at_ms: Option<u64>,
    pub choice: Option<String>,
    pub applied_at_ms: Option<u64>,
    pub held: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DecisionListReport {
    pub decisions: Vec<DecisionJson>,
    /// Profiles whose state DB could not be opened; listed so an empty
    /// result is never silently incomplete.
    pub skipped_profiles: Vec<String>,
}

fn millis(time: SystemTime) -> u64 {
    time.duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn to_json(profile_id: &str, record: DecisionRecord) -> DecisionJson {
    DecisionJson {
        profile_id: profile_id.to_string(),
        id: record.id,
        kind: record.kind,
        scope: record.scope.label().to_string(),
        path: record.path,
        question: record.question,
        options: record
            .options
            .into_iter()
            .map(|option| DecisionOptionJson {
                key: option.key,
                label: option.label,
            })
            .collect(),
        evidence: record.evidence,
        created_at_ms: millis(record.created_at),
        resolved_at_ms: record.resolved_at.map(millis),
        choice: record.choice,
        applied_at_ms: record.applied_at.map(millis),
        held: record.held_intents,
    }
}

fn open_profile_db(profile_id: &str) -> Result<DurableStateDb, String> {
    let path = vapor_shared::runtime_paths::profile_database_path(profile_id);
    if !path.exists() {
        return Err(format!("profile {profile_id} has no state database yet"));
    }
    DurableStateDb::open(&path).map_err(|error| format!("profile {profile_id}: {error}"))
}

/// Every decision of every enabled profile (open ones first).
pub fn list_decisions(config: &VaporConfig, include_closed: bool) -> DecisionListReport {
    let mut decisions = Vec::new();
    let mut skipped_profiles = Vec::new();
    for profile in resolve_profiles(config)
        .into_iter()
        .filter(|profile| profile.enabled)
    {
        let path = vapor_shared::runtime_paths::profile_database_path(&profile.id);
        if !path.exists() {
            continue;
        }
        match DurableStateDb::open(&path) {
            Ok(db) => match db.decisions(include_closed) {
                Ok(records) => decisions.extend(
                    records
                        .into_iter()
                        .filter(|record| !is_settled_conflict(record))
                        .map(|record| to_json(&profile.id, record)),
                ),
                Err(_) => skipped_profiles.push(profile.id.clone()),
            },
            Err(_) => skipped_profiles.push(profile.id.clone()),
        }
    }
    DecisionListReport {
        decisions,
        skipped_profiles,
    }
}

/// An open conflict question with no copy left (resolved in the Finder
/// while the daemon was stopped). The daemon closes it on its next
/// pass; until then a listing does not show it.
fn is_settled_conflict(record: &DecisionRecord) -> bool {
    record.kind == constants::decision::KIND_CONFLICT
        && record.is_open()
        && match record.path.as_deref() {
            Some(path) => matches!(Conflict::find(path), Ok(None)),
            None => true,
        }
}

/// One decision by id; `profile` disambiguates when several profiles
/// exist (ids are per profile).
pub fn show_decision(
    config: &VaporConfig,
    id: i64,
    profile: Option<&str>,
) -> Result<DecisionJson, String> {
    let profile_id = pick_profile(config, id, profile)?;
    let db = open_profile_db(&profile_id)?;
    let record = db
        .decision(id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("no decision {id} in profile {profile_id}"))?;
    Ok(to_json(&profile_id, record))
}

/// Records the answer. The daemon applies it on its next tick; with the
/// daemon stopped, at its next start. A `conflict` answer is plain file
/// operations, so it is applied here and now: the daemon syncs the
/// result like any edit, and it works with the daemon stopped.
pub fn resolve_decision(
    config: &VaporConfig,
    id: i64,
    choice: &str,
    profile: Option<&str>,
) -> Result<DecisionJson, String> {
    let profile_id = pick_profile(config, id, profile)?;
    let mut db = open_profile_db(&profile_id)?;
    let now = SystemTime::now();
    let current = db
        .decision(id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("no decision {id} in profile {profile_id}"))?;
    if current.kind == constants::decision::KIND_CONFLICT && current.is_open() {
        resolve_conflict(config, &profile_id, &mut db, &current, choice, now)?;
        let record = db
            .decision(id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("decision {id} vanished"))?;
        return Ok(to_json(&profile_id, record));
    }
    let record = db
        .resolve_decision(id, choice, now)
        .map_err(|error| error.to_string())?;
    Ok(to_json(&profile_id, record))
}

/// Applies a conflict answer with the profile's trash and this device's
/// id. The daemon never applies one, so nothing races it.
fn resolve_conflict(
    config: &VaporConfig,
    profile_id: &str,
    db: &mut DurableStateDb,
    current: &DecisionRecord,
    choice: &str,
    now: SystemTime,
) -> Result<(), String> {
    let config_path = vapor_shared::runtime_paths::vapor_directory()
        .join(constants::runtime::CONFIGURATION_FILE_NAME);
    let device_id = vapor_shared::device_id::persisted(&config_path).unwrap_or_default();
    let sync_root = resolve_profiles(config)
        .into_iter()
        .find(|profile| profile.id == profile_id)
        .and_then(|profile| profile.scope.local_root);
    let trash = LocalTrash::new(
        profile_id,
        vapor_shared::runtime_paths::profile_trash_directory(profile_id),
        TrashSettings::resolve(config),
        Arc::new(vapor_platform::NativeTrashBin::for_current_host()),
    )
    .with_sync_root(sync_root);
    answer_conflict(db, current, choice, &device_id, &trash, now)
}

/// Keeps the chosen version of a conflicted file and sends the others
/// to `trash`. Refuses when the versions on disk are no longer the ones
/// the question lists, sizes and times included, so an answer never
/// reaches a version the user did not see. The answer is claimed only
/// while the question is still open; when applying it fails part way,
/// the question opens again with the versions there are then.
pub(crate) fn answer_conflict(
    db: &mut DurableStateDb,
    current: &DecisionRecord,
    choice: &str,
    device_id: &str,
    trash: &LocalTrash,
    now: SystemTime,
) -> Result<(), String> {
    let id = current.id;
    let original = current.path.clone().unwrap_or_default();
    let found = Conflict::find(&original).map_err(|error| {
        format!(
            "cannot list the folder of {}: {error}; the question stays open",
            original.display()
        )
    })?;
    let Some(found) = found else {
        db.withdraw_decision(id, now)
            .map_err(|error| error.to_string())?;
        return Err(format!(
            "no conflict copy of {} is left, so there is nothing to decide; the question is closed",
            original.display()
        ));
    };
    let enabled = trash.settings().enabled;
    let fresh = conflict::question(&found, device_id, enabled);
    let keys = |options: &[vapor_daemon::state_db::DecisionOption]| {
        options
            .iter()
            .map(|option| option.key.clone())
            .collect::<Vec<_>>()
    };
    if keys(&fresh.options) != keys(&current.options) || fresh.evidence != current.evidence {
        db.update_open_decision(id, &fresh.question, &fresh.options, &fresh.evidence)
            .map_err(|error| error.to_string())?;
        return Err(format!(
            "the versions of {} changed since the question was asked; see them with \
             `vapor decisions show {id}` and answer again",
            original.display()
        ));
    }
    db.resolve_decision(id, choice, now)
        .map_err(|error| error.to_string())?;
    let kept = conflict::keep(&found, choice, |path| {
        trash
            .discard(path, constants::trash::REASON_CONFLICT, now)
            .map(|_| ())
    });
    if let Err(error) = kept {
        db.reopen_decision(id).map_err(|error| error.to_string())?;
        if let Ok(Some(now_found)) = Conflict::find(&original) {
            let asked = conflict::question(&now_found, device_id, enabled);
            db.update_open_decision(id, &asked.question, &asked.options, &asked.evidence)
                .map_err(|error| error.to_string())?;
        }
        return Err(format!(
            "{error}; the question is open again with the versions there are now"
        ));
    }
    db.mark_decision_applied(id, now)
        .map_err(|error| error.to_string())
}

fn pick_profile(config: &VaporConfig, id: i64, profile: Option<&str>) -> Result<String, String> {
    if let Some(profile) = profile {
        return Ok(profile.to_string());
    }
    let mut holders = Vec::new();
    for profile in resolve_profiles(config)
        .into_iter()
        .filter(|profile| profile.enabled)
    {
        let path = vapor_shared::runtime_paths::profile_database_path(&profile.id);
        if !path.exists() {
            continue;
        }
        if let Ok(db) = DurableStateDb::open(&path)
            && db.decision(id).ok().flatten().is_some()
        {
            holders.push(profile.id.clone());
        }
    }
    match holders.as_slice() {
        [one] => Ok(one.clone()),
        [] => Err(format!("no profile holds decision {id}")),
        many => Err(format!(
            "decision {id} exists in several profiles ({}); pass --profile",
            many.join(", ")
        )),
    }
}

pub fn render_list(report: &DecisionListReport) -> String {
    let mut lines = Vec::new();
    if report.decisions.is_empty() {
        lines.push("No pending decisions.".to_string());
    }
    for decision in &report.decisions {
        let state = match (&decision.choice, decision.applied_at_ms) {
            (None, _) => "open".to_string(),
            (Some(choice), None) => format!("answered {choice}, waiting for the daemon"),
            (Some(choice), Some(_)) => format!("applied {choice}"),
        };
        lines.push(format!(
            "#{} [{}] {} ({}{}): {}",
            decision.id,
            decision.profile_id,
            decision.kind,
            state,
            if decision.held > 0 {
                format!(", {} held", decision.held)
            } else {
                String::new()
            },
            decision.question
        ));
        if decision.choice.is_none() {
            for option in option_lines(&decision.options) {
                lines.push(format!("    {option}"));
            }
        }
    }
    for profile in &report.skipped_profiles {
        lines.push(format!("(profile {profile}: state database unreadable)"));
    }
    lines.join("\n")
}

/// `--choose <key>  <label>`, keys padded to the longest so the labels
/// line up (a conflict's keys are file names).
fn option_lines(options: &[DecisionOptionJson]) -> Vec<String> {
    let width = options
        .iter()
        .map(|option| option.key.len())
        .max()
        .unwrap_or(0);
    options
        .iter()
        .map(|option| format!("--choose {:<width$}  {}", option.key, option.label))
        .collect()
}

pub fn render_show(decision: &DecisionJson) -> String {
    let mut lines = vec![
        format!(
            "Decision #{} ({}) in profile {}",
            decision.id, decision.kind, decision.profile_id
        ),
        format!("Scope: {}", decision.scope),
    ];
    if let Some(path) = &decision.path {
        lines.push(format!("Path: {}", path.display()));
    }
    lines.push(format!("Question: {}", decision.question));
    lines.push("Options:".to_string());
    for option in option_lines(&decision.options) {
        lines.push(format!("  {option}"));
    }
    match (&decision.choice, decision.applied_at_ms) {
        (None, _) => lines.push("Status: open".to_string()),
        (Some(choice), None) => lines.push(format!(
            "Status: answered {choice}; the daemon applies it on its next tick"
        )),
        (Some(choice), Some(_)) => lines.push(format!("Status: applied {choice}")),
    }
    if decision.held > 0 {
        lines.push(format!(
            "Changes held until it is answered: {}",
            decision.held
        ));
    }
    if !decision.evidence.is_null() {
        lines.push("Evidence:".to_string());
        for line in serde_json::to_string_pretty(&decision.evidence)
            .unwrap_or_default()
            .lines()
        {
            lines.push(format!("  {line}"));
        }
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    mod conflict_answers {
        use super::*;
        use std::fs;
        use vapor_daemon::state_db::DecisionScope;

        const THEIRS: &str = "report~conflict-other-mac-1750000000123.md";

        struct Setup {
            _temp: tempfile::TempDir,
            db: DurableStateDb,
            trash: LocalTrash,
            original: PathBuf,
            id: i64,
        }

        /// `report.md`, one copy, and the open question about them, as
        /// the daemon would have asked it.
        fn setup() -> Setup {
            let temp = tempfile::TempDir::new().expect("temp");
            let root = temp.path().join("Vapor");
            fs::create_dir_all(&root).expect("root");
            let original = root.join("report.md");
            fs::write(&original, b"the cloud's version").expect("seed");
            fs::write(root.join(THEIRS), b"the other mac's version").expect("seed");
            let mut db = DurableStateDb::open(temp.path().join("vapor.sqlite")).expect("db");
            let trash = LocalTrash::new(
                "default",
                temp.path().join("trash"),
                TrashSettings::default(),
                Arc::new(vapor_platform::InMemoryTrashBin::unsupported()),
            );
            let found = Conflict::find(&original)
                .expect("listable")
                .expect("a conflict");
            let asked = conflict::question(&found, "this-mac", true);
            let id = db
                .create_decision(
                    constants::decision::KIND_CONFLICT,
                    DecisionScope::Path,
                    Some(&original),
                    &asked.question,
                    &asked.options,
                    &asked.evidence,
                    SystemTime::now(),
                )
                .expect("ask");
            Setup {
                _temp: temp,
                db,
                trash,
                original,
                id,
            }
        }

        fn record(setup: &Setup) -> DecisionRecord {
            setup
                .db
                .decision(setup.id)
                .expect("read")
                .expect("decision")
        }

        fn answer(setup: &mut Setup, choice: &str) -> Result<(), String> {
            let current = record(setup);
            answer_conflict(
                &mut setup.db,
                &current,
                choice,
                "this-mac",
                &setup.trash,
                SystemTime::now(),
            )
        }

        #[test]
        fn an_answer_keeps_the_version_and_trashes_the_rest() {
            let mut setup = setup();
            answer(&mut setup, THEIRS).expect("applied");
            assert_eq!(
                fs::read(&setup.original).expect("read"),
                b"the other mac's version"
            );
            assert!(!setup.original.with_file_name(THEIRS).exists());
            let trashed = setup.trash.list();
            assert_eq!(trashed.len(), 1);
            assert_eq!(trashed[0].reason, constants::trash::REASON_CONFLICT);
            let done = record(&setup);
            assert_eq!(done.choice.as_deref(), Some(THEIRS));
            assert!(done.applied_at.is_some());
        }

        #[test]
        fn a_version_that_changed_since_the_question_is_never_answered_blind() {
            let mut setup = setup();
            fs::write(&setup.original, b"a newer download, longer than before").expect("changes");
            let error = answer(&mut setup, THEIRS).expect_err("refused");
            assert!(error.contains("changed since"), "{error}");
            assert!(record(&setup).is_open());
            assert!(setup.original.with_file_name(THEIRS).exists());
            // The question now shows what is there, so the next answer
            // goes through.
            answer(&mut setup, "report.md").expect("applied");
        }

        #[test]
        fn a_question_with_no_copy_left_closes() {
            let mut setup = setup();
            fs::remove_file(setup.original.with_file_name(THEIRS)).expect("by hand");
            let error = answer(&mut setup, "report.md").expect_err("nothing to decide");
            assert!(error.contains("nothing to decide"), "{error}");
            assert_eq!(record(&setup).choice.as_deref(), Some("withdrawn"));
        }

        #[test]
        fn a_question_answered_meanwhile_is_not_answered_twice() {
            let mut setup = setup();
            let stale = record(&setup);
            setup
                .db
                .resolve_decision(setup.id, THEIRS, SystemTime::now())
                .expect("another answer won");
            let error = answer_conflict(
                &mut setup.db,
                &stale,
                "report.md",
                "this-mac",
                &setup.trash,
                SystemTime::now(),
            )
            .expect_err("lost the claim");
            assert!(
                error.contains("already resolved") || error.contains("meanwhile"),
                "{error}"
            );
            assert!(setup.original.exists() && setup.original.with_file_name(THEIRS).exists());
        }

        #[cfg(unix)]
        #[test]
        fn a_failure_part_way_opens_the_question_again() {
            use std::os::unix::fs::PermissionsExt;
            let mut setup = setup();
            // A trash that cannot take the file under the name.
            let trash_root = setup.trash.root().to_path_buf();
            fs::create_dir_all(&trash_root).expect("trash root");
            fs::set_permissions(&trash_root, fs::Permissions::from_mode(0o500)).expect("lock");
            let writable = fs::File::create(trash_root.join("probe")).is_ok(); // root
            let result = answer(&mut setup, THEIRS);
            fs::set_permissions(&trash_root, fs::Permissions::from_mode(0o700)).expect("unlock");
            if writable {
                return;
            }
            let error = result.expect_err("cannot trash");
            assert!(error.contains("open again"), "{error}");
            let reopened = record(&setup);
            assert!(reopened.is_open() && reopened.choice.is_none());
            assert_eq!(
                fs::read(&setup.original).expect("untouched"),
                b"the cloud's version"
            );
            assert!(setup.original.with_file_name(THEIRS).exists());
        }
    }

    #[test]
    fn json_shape_is_locked() {
        let report = DecisionListReport {
            decisions: vec![DecisionJson {
                profile_id: "default".into(),
                id: 7,
                kind: "mass-delete".into(),
                scope: "batch".into(),
                path: None,
                question: "Apply 240 deletions?".into(),
                options: vec![DecisionOptionJson {
                    key: "apply".into(),
                    label: "Apply the deletions".into(),
                }],
                evidence: serde_json::json!({ "count": 240 }),
                created_at_ms: 1_700_000_000_000,
                resolved_at_ms: None,
                choice: None,
                applied_at_ms: None,
                held: 240,
            }],
            skipped_profiles: vec![],
        };
        let rendered = serde_json::to_string_pretty(&report).expect("serialize");
        assert_eq!(
            rendered,
            r#"{
  "decisions": [
    {
      "profileId": "default",
      "id": 7,
      "kind": "mass-delete",
      "scope": "batch",
      "path": null,
      "question": "Apply 240 deletions?",
      "options": [
        {
          "key": "apply",
          "label": "Apply the deletions"
        }
      ],
      "evidence": {
        "count": 240
      },
      "createdAtMs": 1700000000000,
      "resolvedAtMs": null,
      "choice": null,
      "appliedAtMs": null,
      "held": 240
    }
  ],
  "skippedProfiles": []
}"#
        );
    }

    #[test]
    fn text_rendering_lists_options_only_while_open() {
        let mut decision = DecisionJson {
            profile_id: "default".into(),
            id: 1,
            kind: "type-mismatch".into(),
            scope: "path".into(),
            path: Some(PathBuf::from("/r/x")),
            question: "x is a file here and a folder in the cloud".into(),
            options: vec![DecisionOptionJson {
                key: "keep-both".into(),
                label: "Keep both".into(),
            }],
            evidence: serde_json::Value::Null,
            created_at_ms: 1,
            resolved_at_ms: None,
            choice: None,
            applied_at_ms: None,
            held: 0,
        };
        let open = render_list(&DecisionListReport {
            decisions: vec![DecisionJson { ..decision }],
            skipped_profiles: vec![],
        });
        assert!(open.contains("--choose keep-both"));
        decision = DecisionJson {
            choice: Some("keep-both".into()),
            resolved_at_ms: Some(2),
            profile_id: "default".into(),
            id: 1,
            kind: "type-mismatch".into(),
            scope: "path".into(),
            path: Some(PathBuf::from("/r/x")),
            question: "x is a file here and a folder in the cloud".into(),
            options: vec![],
            evidence: serde_json::Value::Null,
            created_at_ms: 1,
            applied_at_ms: None,
            held: 0,
        };
        let answered = render_list(&DecisionListReport {
            decisions: vec![decision],
            skipped_profiles: vec![],
        });
        assert!(answered.contains("answered keep-both, waiting for the daemon"));
    }
}
