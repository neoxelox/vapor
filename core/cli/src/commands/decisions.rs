//! `vapor decisions list|show|resolve` — the questions the daemon
//! parked because an irreversible action rested on ambiguous evidence.
//!
//! A decision lives in the profile's durable state DB, so this command
//! reads and answers it with or without a running daemon; the daemon
//! applies the answer on its next tick (or at its next start). App
//! surfaces drive this command with `--json`, the same shim pattern as
//! `vapor conflicts`. Design: `docs/architecture/data-flow.md`
//! §Decisions.

use std::path::PathBuf;
use std::time::SystemTime;

use serde::Serialize;
use vapor_daemon::profiles::resolve_profiles;
use vapor_daemon::state_db::{DecisionRecord, DurableStateDb};
use vapor_shared::config::VaporConfig;

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
    pub held_intents: usize,
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
        held_intents: record.held_intents,
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
/// daemon stopped, at its next start.
pub fn resolve_decision(
    config: &VaporConfig,
    id: i64,
    choice: &str,
    profile: Option<&str>,
) -> Result<DecisionJson, String> {
    let profile_id = pick_profile(config, id, profile)?;
    let mut db = open_profile_db(&profile_id)?;
    let record = db
        .resolve_decision(id, choice, SystemTime::now())
        .map_err(|error| error.to_string())?;
    Ok(to_json(&profile_id, record))
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
            if decision.held_intents > 0 {
                format!(", {} held", decision.held_intents)
            } else {
                String::new()
            },
            decision.question
        ));
        if decision.choice.is_none() {
            for option in &decision.options {
                lines.push(format!("    --choose {:<14} {}", option.key, option.label));
            }
        }
    }
    for profile in &report.skipped_profiles {
        lines.push(format!("(profile {profile}: state database unreadable)"));
    }
    lines.join("\n")
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
    for option in &decision.options {
        lines.push(format!("  --choose {:<14} {}", option.key, option.label));
    }
    match (&decision.choice, decision.applied_at_ms) {
        (None, _) => lines.push("Status: open".to_string()),
        (Some(choice), None) => lines.push(format!(
            "Status: answered {choice}; the daemon applies it on its next tick"
        )),
        (Some(choice), Some(_)) => lines.push(format!("Status: applied {choice}")),
    }
    if decision.held_intents > 0 {
        lines.push(format!("Held intents: {}", decision.held_intents));
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

    #[test]
    fn json_shape_is_locked() {
        let report = DecisionListReport {
            decisions: vec![DecisionJson {
                profile_id: "default".into(),
                id: 7,
                kind: "mass-deletion".into(),
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
                held_intents: 240,
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
      "kind": "mass-deletion",
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
      "heldIntents": 240
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
            held_intents: 0,
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
            held_intents: 0,
        };
        let answered = render_list(&DecisionListReport {
            decisions: vec![decision],
            skipped_profiles: vec![],
        });
        assert!(answered.contains("answered keep-both, waiting for the daemon"));
    }
}
