//! Which faults a run injects and when. The injection itself lives in
//! the driver because it needs the daemon handle; this module owns the
//! vocabulary and the selection.

use crate::rng::Rng;

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FaultKind {
    /// SIGKILL the daemon between two operations, then restart it.
    Crash,
    /// SIGKILL the daemon while a transfer is in flight, then restart.
    CrashMidTransfer,
    /// SIGSTOP for a while, then SIGCONT: a frozen daemon.
    Freeze,
    /// `vapor pause`, keep working, `vapor resume`.
    PauseResume,
    /// Rename the cloud root away for a while, then put it back.
    CloudRootVanish,
    /// Walk the throttle inputs through every state during a phase.
    ThrottleWalk,
    /// Lower and restore a resource ceiling with `vapor config set`.
    ConfigReload,
    /// Fill the cloud root's volume, keep it full for a while, free it.
    DiskFull,
}

impl FaultKind {
    pub fn label(self) -> &'static str {
        match self {
            FaultKind::Crash => "crash",
            FaultKind::CrashMidTransfer => "crash-mid-transfer",
            FaultKind::Freeze => "freeze",
            FaultKind::PauseResume => "pause-resume",
            FaultKind::CloudRootVanish => "cloud-root-vanish",
            FaultKind::ThrottleWalk => "throttle-walk",
            FaultKind::ConfigReload => "config-reload",
            FaultKind::DiskFull => "disk-full",
        }
    }

    pub const ALL: &'static [FaultKind] = &[
        FaultKind::Crash,
        FaultKind::CrashMidTransfer,
        FaultKind::Freeze,
        FaultKind::PauseResume,
        FaultKind::CloudRootVanish,
        FaultKind::ThrottleWalk,
        FaultKind::ConfigReload,
        FaultKind::DiskFull,
    ];

    /// Faults injected between operations (the rest run per phase).
    pub fn is_per_op(self) -> bool {
        matches!(
            self,
            FaultKind::Crash
                | FaultKind::CrashMidTransfer
                | FaultKind::Freeze
                | FaultKind::PauseResume
                | FaultKind::ConfigReload
        )
    }
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct FaultPlan {
    pub kinds: Vec<FaultKind>,
    /// Chance, in percent, that a per-op fault fires after one op.
    pub per_op_percent: u64,
}

impl FaultPlan {
    /// `none`, `crash`, `all`, or a comma-separated list of labels.
    pub fn parse(text: &str) -> Result<Self, String> {
        let text = text.trim();
        let kinds = match text {
            "none" | "" => Vec::new(),
            "all" => FaultKind::ALL.to_vec(),
            "crash" => vec![FaultKind::Crash, FaultKind::CrashMidTransfer],
            list => {
                let mut kinds = Vec::new();
                for item in list.split(',') {
                    let item = item.trim();
                    let kind = FaultKind::ALL
                        .iter()
                        .find(|kind| kind.label() == item)
                        .ok_or_else(|| format!("unknown fault {item:?}"))?;
                    kinds.push(*kind);
                }
                kinds
            }
        };
        Ok(Self {
            kinds,
            per_op_percent: 2,
        })
    }

    pub fn is_empty(&self) -> bool {
        self.kinds.is_empty()
    }

    pub fn has(&self, kind: FaultKind) -> bool {
        self.kinds.contains(&kind)
    }

    /// Picks a per-op fault to fire now, if any.
    pub fn pick_per_op(&self, rng: &mut Rng) -> Option<FaultKind> {
        let candidates: Vec<FaultKind> = self
            .kinds
            .iter()
            .copied()
            .filter(|kind| kind.is_per_op())
            .collect();
        if candidates.is_empty() || !rng.chance(self.per_op_percent) {
            return None;
        }
        Some(*rng.pick(&candidates))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plans_parse_their_shorthands_and_lists() {
        assert!(FaultPlan::parse("none").unwrap().is_empty());
        assert_eq!(
            FaultPlan::parse("all").unwrap().kinds.len(),
            FaultKind::ALL.len()
        );
        let list = FaultPlan::parse("crash, freeze").unwrap();
        assert_eq!(list.kinds, vec![FaultKind::Crash, FaultKind::Freeze]);
        assert!(FaultPlan::parse("meteor").is_err());
    }
}
