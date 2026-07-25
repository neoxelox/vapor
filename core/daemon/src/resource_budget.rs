//! User resource budgets + idle-boost state machine.
//!
//! Resolves the effective ceilings each tick per
//! `docs/architecture/data-flow.md §User resource budgets`:
//!
//! 1. MIN-lowering across the global `resourceLimits` and every enabled
//!    profile's override; any enabled profile with `idleBoost.enabled =
//!    false` disables boost daemon-wide.
//! 2. Idle-boost engages only in `IdleDrain`, after `minIdleSeconds` of
//!    user idleness, with non-Vapor CPU at or below the headroom gate.
//!    Ceilings ramp linearly toward `boost*Percent` over
//!    `rampUpSeconds`; a gating break ramps back down over
//!    `rampDownSeconds`.
//! 3. Deterministic transition rules (§Ceiling transitions): a throttle
//!    exit from `IdleDrain` snaps the cap to
//!    `min(current, resourceLimits.*)` in the same tick — no down-ramp;
//!    a return to `IdleDrain` never auto-resumes, the gates re-evaluate
//!    from scratch.
//!
//! Ceilings are hard caps: they only ever *lower* what the throttle
//! controller already allows. A `Suspended` decision always wins.

use std::time::{Duration, Instant};

use vapor_shared::config::{ResourceLimitsConfig, VaporConfig};
use vapor_shared::{ThrottleInputs, ThrottleState, constants};

use crate::logging;

/// Clamped, MIN-lowered budget configuration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectiveBudgetConfig {
    pub cpu_percent: u8,
    pub memory_percent: u8,
    pub bandwidth_percent: u8,
    /// Optional user ceiling on concurrent uploads / downloads (each
    /// direction). `None` = automatic (core-derived throttle tier).
    pub max_concurrent_transfers: Option<usize>,
    pub boost_enabled: bool,
    pub min_idle: Duration,
    pub boost_cpu_percent: u8,
    pub boost_memory_percent: u8,
    pub boost_bandwidth_percent: u8,
    pub headroom_cpu_percent: u8,
    pub ramp_up: Duration,
    pub ramp_down: Duration,
}

impl EffectiveBudgetConfig {
    /// Resolves the daemon-wide budget from the configuration: clamp
    /// every percent into `1..=100` with a
    /// classified warning, MIN-lower profile overrides, clamp boost
    /// ceilings up to at least their base ceiling, and bound the
    /// down-ramp by the up-ramp.
    pub fn resolve(config: &VaporConfig) -> Self {
        let mut limits = clamp_limits(&config.resource_limits, "top-level");
        let mut boost = config.idle_boost.clone();
        let mut boost_enabled = boost.enabled;
        let mut max_concurrent_transfers =
            clamp_transfer_ceiling(config.resource_limits.max_concurrent_transfers, "top-level");

        for profile in &config.profiles {
            if !profile.enabled.unwrap_or(true) {
                continue;
            }
            if let Some(profile_limits) = &profile.resource_limits {
                let clamped = clamp_limits(profile_limits, &profile.id);
                limits.cpu_percent = limits.cpu_percent.min(clamped.cpu_percent);
                limits.memory_percent = limits.memory_percent.min(clamped.memory_percent);
                limits.bandwidth_percent = limits.bandwidth_percent.min(clamped.bandwidth_percent);
                if let Some(profile_ceiling) =
                    clamp_transfer_ceiling(profile_limits.max_concurrent_transfers, &profile.id)
                {
                    // Ceilings are MIN-lowered like the percent limits: a
                    // profile can only tighten the daemon-wide value.
                    max_concurrent_transfers = Some(
                        max_concurrent_transfers
                            .map_or(profile_ceiling, |current| current.min(profile_ceiling)),
                    );
                }
            }
            if let Some(profile_boost) = &profile.idle_boost {
                if !profile_boost.enabled {
                    boost_enabled = false;
                }
                boost.boost_cpu_percent =
                    boost.boost_cpu_percent.min(profile_boost.boost_cpu_percent);
                boost.boost_memory_percent = boost
                    .boost_memory_percent
                    .min(profile_boost.boost_memory_percent);
                boost.boost_bandwidth_percent = boost
                    .boost_bandwidth_percent
                    .min(profile_boost.boost_bandwidth_percent);
            }
        }

        // Boost ceilings below their base ceiling are contradictions:
        // clamp up with a warning at load time.
        let boost_cpu = ensure_boost_at_least(boost.boost_cpu_percent, limits.cpu_percent, "cpu");
        let boost_memory =
            ensure_boost_at_least(boost.boost_memory_percent, limits.memory_percent, "memory");
        let boost_bandwidth = ensure_boost_at_least(
            boost.boost_bandwidth_percent,
            limits.bandwidth_percent,
            "bandwidth",
        );

        let ramp_up = Duration::from_secs(boost.ramp_up_seconds.max(1));
        let mut ramp_down = Duration::from_secs(boost.ramp_down_seconds.max(1));
        if ramp_down > ramp_up {
            logging::warning(
                "idleBoost.rampDownSeconds exceeds rampUpSeconds; clamping so activity resumption stays non-invasive",
                &[],
            );
            ramp_down = ramp_up;
        }

        Self {
            cpu_percent: limits.cpu_percent,
            memory_percent: limits.memory_percent,
            bandwidth_percent: limits.bandwidth_percent,
            max_concurrent_transfers,
            boost_enabled,
            min_idle: Duration::from_secs(boost.min_idle_seconds),
            boost_cpu_percent: boost_cpu.clamp(1, 100),
            boost_memory_percent: boost_memory.clamp(1, 100),
            boost_bandwidth_percent: boost_bandwidth.clamp(1, 100),
            headroom_cpu_percent: boost.headroom_cpu_percent.clamp(1, 100),
            ramp_up,
            ramp_down,
        }
    }
}

/// Clamps a configured transfer-concurrency ceiling into the accepted
/// range with a logged warning; `None` passes through (automatic).
fn clamp_transfer_ceiling(configured: Option<u8>, source: &str) -> Option<usize> {
    let configured = configured? as usize;
    let clamped = configured.clamp(
        constants::resource_limits::MIN_CONCURRENT_TRANSFERS,
        constants::resource_limits::MAX_CONCURRENT_TRANSFERS,
    );
    if clamped != configured {
        logging::warning(
            "Clamped resourceLimits.maxConcurrentTransfers into the accepted range",
            &[
                ("source", source.to_string()),
                ("configured", configured.to_string()),
                ("effective", clamped.to_string()),
            ],
        );
    }
    Some(clamped)
}

fn clamp_limits(limits: &ResourceLimitsConfig, origin: &str) -> ResourceLimitsConfig {
    let clamp = |value: u8, key: &str| -> u8 {
        let clamped = value.clamp(
            constants::resource_limits::MIN_PERCENT,
            constants::resource_limits::MAX_PERCENT,
        );
        if clamped != value {
            logging::warning(
                "resourceLimits value out of range; clamped",
                &[
                    ("origin", origin.to_string()),
                    ("key", key.to_string()),
                    ("configured", value.to_string()),
                    ("clamped", clamped.to_string()),
                ],
            );
        }
        clamped
    };
    ResourceLimitsConfig {
        cpu_percent: clamp(limits.cpu_percent, "cpuPercent"),
        memory_percent: clamp(limits.memory_percent, "memoryPercent"),
        bandwidth_percent: clamp(limits.bandwidth_percent, "bandwidthPercent"),
        // Clamped separately (`clamp_transfer_ceiling`); passed through
        // here so callers holding the clamped struct keep the raw value.
        max_concurrent_transfers: limits.max_concurrent_transfers,
    }
}

fn ensure_boost_at_least(boost: u8, base: u8, resource: &str) -> u8 {
    if boost < base {
        logging::warning(
            "idleBoost ceiling below its resourceLimits base; clamped up",
            &[
                ("resource", resource.to_string()),
                ("boost", boost.to_string()),
                ("base", base.to_string()),
            ],
        );
        base
    } else {
        boost
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BoostState {
    Off,
    RampingUp { started: Instant },
    Active,
    RampingDown { started: Instant, from_cpu: u8 },
}

/// Effective ceilings published each tick.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectiveCeilings {
    pub cpu_percent: u8,
    pub memory_percent: u8,
    pub bandwidth_percent: u8,
    /// `off` / `ramping-up` / `active` / `ramping-down`.
    pub boost_state: &'static str,
    /// Human-readable reason for the current boost state.
    pub reason: String,
}

pub struct ResourceBudget {
    config: EffectiveBudgetConfig,
    state: BoostState,
    /// CPU ceiling actually in force (the ramping value).
    current_cpu: u8,
    current_memory: u8,
    current_bandwidth: u8,
    last_reason: String,
}

impl ResourceBudget {
    pub fn new(config: EffectiveBudgetConfig) -> Self {
        let (cpu, memory, bandwidth) = (
            config.cpu_percent,
            config.memory_percent,
            config.bandwidth_percent,
        );
        Self {
            config,
            state: BoostState::Off,
            current_cpu: cpu,
            current_memory: memory,
            current_bandwidth: bandwidth,
            last_reason: "base ceilings in force".to_string(),
        }
    }

    pub fn config(&self) -> &EffectiveBudgetConfig {
        &self.config
    }

    /// One evaluation on the 1s throttle cadence.
    pub fn tick(
        &mut self,
        throttle_state: ThrottleState,
        inputs: &ThrottleInputs,
        idle_for: Duration,
        now_inst: Instant,
    ) -> EffectiveCeilings {
        let gates_pass = self.gates_pass(throttle_state, inputs, idle_for);

        match self.state {
            BoostState::Off => {
                if gates_pass {
                    self.state = BoostState::RampingUp { started: now_inst };
                    self.last_reason = "idle-boost gates passed; ramping up".to_string();
                } else {
                    self.last_reason = self.gate_failure_reason(throttle_state, inputs, idle_for);
                }
            }
            BoostState::RampingUp { started } => {
                if throttle_state != ThrottleState::IdleDrain {
                    // §Ceiling transitions rule 1: snap, no down-ramp.
                    self.snap_to_base("throttle left IdleDrain; snapped to base ceilings");
                } else if !gates_pass {
                    self.state = BoostState::RampingDown {
                        started: now_inst,
                        from_cpu: self.current_cpu,
                    };
                    self.last_reason =
                        "idle-boost gate broke mid-ramp; ramping back down".to_string();
                } else {
                    let progress = ramp_progress(started, now_inst, self.config.ramp_up);
                    self.apply_ramp(progress);
                    if progress >= 1.0 {
                        self.state = BoostState::Active;
                        self.last_reason = "idle boost active at full headroom".to_string();
                    } else {
                        self.last_reason =
                            format!("idle boost ramping up ({}%)", (progress * 100.0) as u32);
                    }
                }
            }
            BoostState::Active => {
                if throttle_state != ThrottleState::IdleDrain {
                    self.snap_to_base("throttle left IdleDrain; snapped to base ceilings");
                } else if !gates_pass {
                    self.state = BoostState::RampingDown {
                        started: now_inst,
                        from_cpu: self.current_cpu,
                    };
                    self.last_reason = self.gate_failure_reason(throttle_state, inputs, idle_for);
                }
            }
            BoostState::RampingDown { started, from_cpu } => {
                if throttle_state != ThrottleState::IdleDrain {
                    self.snap_to_base("throttle left IdleDrain; snapped to base ceilings");
                } else if gates_pass {
                    // Headroom recovered mid-descent (e.g. a 1s background
                    // CPU blip). Re-checking the gate here — instead of
                    // riding the full down-ramp and then a fresh up-ramp —
                    // avoids ~40s of reduced ceilings for a momentary blip.
                    // Resume climbing from the *current* level by
                    // back-dating the up-ramp start to the matching
                    // progress, so we neither drop to base nor snap up.
                    let span =
                        self.config
                            .boost_cpu_percent
                            .saturating_sub(self.config.cpu_percent) as f64;
                    let fraction = if span > 0.0 {
                        (self.current_cpu.saturating_sub(self.config.cpu_percent) as f64 / span)
                            .clamp(0.0, 1.0)
                    } else {
                        1.0
                    };
                    let elapsed_equiv = self.config.ramp_up.mul_f64(fraction);
                    self.state = BoostState::RampingUp {
                        started: now_inst.checked_sub(elapsed_equiv).unwrap_or(now_inst),
                    };
                    self.last_reason =
                        "idle-boost headroom recovered mid-ramp-down; resuming ramp up".to_string();
                } else {
                    let progress = ramp_progress(started, now_inst, self.config.ramp_down);
                    let span = from_cpu.saturating_sub(self.config.cpu_percent) as f64;
                    self.current_cpu =
                        self.config.cpu_percent + ((1.0 - progress) * span).round() as u8;
                    // Memory / bandwidth ramp proportionally to CPU.
                    self.current_memory = ramp_between(
                        self.config.memory_percent,
                        self.config.boost_memory_percent,
                        boosted_fraction(
                            self.current_cpu,
                            self.config.cpu_percent,
                            self.config.boost_cpu_percent,
                        ),
                    );
                    self.current_bandwidth = ramp_between(
                        self.config.bandwidth_percent,
                        self.config.boost_bandwidth_percent,
                        boosted_fraction(
                            self.current_cpu,
                            self.config.cpu_percent,
                            self.config.boost_cpu_percent,
                        ),
                    );
                    if progress >= 1.0 {
                        self.snap_to_base("idle boost ramped down to base ceilings");
                    } else {
                        self.last_reason =
                            format!("idle boost ramping down ({}%)", (progress * 100.0) as u32);
                    }
                }
            }
        }

        EffectiveCeilings {
            cpu_percent: self.current_cpu,
            memory_percent: self.current_memory,
            bandwidth_percent: self.current_bandwidth,
            boost_state: match self.state {
                BoostState::Off => "off",
                BoostState::RampingUp { .. } => "ramping-up",
                BoostState::Active => "active",
                BoostState::RampingDown { .. } => "ramping-down",
            },
            reason: self.last_reason.clone(),
        }
    }

    fn gates_pass(
        &self,
        throttle_state: ThrottleState,
        inputs: &ThrottleInputs,
        idle_for: Duration,
    ) -> bool {
        self.config.boost_enabled
            && throttle_state == ThrottleState::IdleDrain
            && idle_for >= self.config.min_idle
            && non_vapor_cpu(inputs) <= self.config.headroom_cpu_percent
    }

    fn gate_failure_reason(
        &self,
        throttle_state: ThrottleState,
        inputs: &ThrottleInputs,
        idle_for: Duration,
    ) -> String {
        if !self.config.boost_enabled {
            "idle boost disabled by configuration".to_string()
        } else if throttle_state != ThrottleState::IdleDrain {
            format!("throttle state is {throttle_state:?}; boost requires IdleDrain")
        } else if idle_for < self.config.min_idle {
            format!(
                "user idle for {}s of the required {}s",
                idle_for.as_secs(),
                self.config.min_idle.as_secs()
            )
        } else if non_vapor_cpu(inputs) > self.config.headroom_cpu_percent {
            format!(
                "non-Vapor CPU at {}% exceeds the {}% headroom gate",
                non_vapor_cpu(inputs),
                self.config.headroom_cpu_percent
            )
        } else {
            "base ceilings in force".to_string()
        }
    }

    fn apply_ramp(&mut self, progress: f64) {
        self.current_cpu = ramp_between(
            self.config.cpu_percent,
            self.config.boost_cpu_percent,
            progress,
        );
        self.current_memory = ramp_between(
            self.config.memory_percent,
            self.config.boost_memory_percent,
            progress,
        );
        self.current_bandwidth = ramp_between(
            self.config.bandwidth_percent,
            self.config.boost_bandwidth_percent,
            progress,
        );
    }

    fn snap_to_base(&mut self, reason: &str) {
        // §Ceiling transitions rule 1: min(current, base) — the ramped
        // value can only ever exceed the base, so this is the base.
        self.current_cpu = self.config.cpu_percent;
        self.current_memory = self.config.memory_percent;
        self.current_bandwidth = self.config.bandwidth_percent;
        self.state = BoostState::Off;
        self.last_reason = reason.to_string();
    }
}

/// System CPU attributable to everything except Vapor.
fn non_vapor_cpu(inputs: &ThrottleInputs) -> u8 {
    inputs
        .system_cpu_load_percent
        .saturating_sub(inputs.vapor_cpu_load_percent)
}

fn ramp_progress(started: Instant, now: Instant, span: Duration) -> f64 {
    let elapsed = now.saturating_duration_since(started);
    (elapsed.as_secs_f64() / span.as_secs_f64()).min(1.0)
}

fn ramp_between(base: u8, boost: u8, fraction: f64) -> u8 {
    let span = boost.saturating_sub(base) as f64;
    base + (span * fraction.clamp(0.0, 1.0)).round() as u8
}

fn boosted_fraction(current: u8, base: u8, boost: u8) -> f64 {
    let span = boost.saturating_sub(base);
    if span == 0 {
        0.0
    } else {
        current.saturating_sub(base) as f64 / span as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vapor_shared::config::ProfileConfig;

    fn config_with(boost_enabled: bool) -> EffectiveBudgetConfig {
        let mut config = VaporConfig::default();
        config.idle_boost.enabled = boost_enabled;
        EffectiveBudgetConfig::resolve(&config)
    }

    fn idle_inputs() -> ThrottleInputs {
        ThrottleInputs::default()
    }

    fn long_idle() -> Duration {
        Duration::from_secs(3_600)
    }

    #[test]
    fn defaults_resolve_to_the_documented_ceilings() {
        let resolved = config_with(true);
        assert_eq!(resolved.cpu_percent, 15);
        assert_eq!(resolved.memory_percent, 10);
        assert_eq!(resolved.bandwidth_percent, 25);
        assert!(resolved.boost_enabled);
        assert_eq!(resolved.boost_cpu_percent, 50);
    }

    #[test]
    fn out_of_range_values_are_clamped() {
        let mut config = VaporConfig::default();
        config.resource_limits.cpu_percent = 0;
        config.resource_limits.bandwidth_percent = 200;
        let resolved = EffectiveBudgetConfig::resolve(&config);
        assert_eq!(resolved.cpu_percent, 1);
        assert_eq!(resolved.bandwidth_percent, 100);
    }

    #[test]
    fn profile_overrides_min_lower_and_boost_disable_wins_daemon_wide() {
        // Overrides can only tighten; one enabled profile with
        // boost off disables boost for everyone.
        let mut config = VaporConfig::default();
        config.profiles = vec![
            ProfileConfig {
                id: "tight".to_string(),
                resource_limits: Some(vapor_shared::config::ResourceLimitsConfig {
                    cpu_percent: 5,
                    memory_percent: 50,
                    bandwidth_percent: 10,
                    max_concurrent_transfers: None,
                }),
                ..ProfileConfig::default()
            },
            ProfileConfig {
                id: "no-boost".to_string(),
                idle_boost: Some(vapor_shared::config::IdleBoostConfig {
                    enabled: false,
                    ..vapor_shared::config::IdleBoostConfig::default()
                }),
                ..ProfileConfig::default()
            },
        ];
        let resolved = EffectiveBudgetConfig::resolve(&config);
        assert_eq!(resolved.cpu_percent, 5, "MIN-lowered");
        assert_eq!(resolved.memory_percent, 10, "profile cannot raise");
        assert_eq!(resolved.bandwidth_percent, 10);
        assert!(!resolved.boost_enabled, "one disable wins daemon-wide");
    }

    #[test]
    fn disabled_profiles_do_not_lower_ceilings() {
        let mut config = VaporConfig::default();
        config.profiles = vec![ProfileConfig {
            id: "disabled".to_string(),
            enabled: Some(false),
            resource_limits: Some(vapor_shared::config::ResourceLimitsConfig {
                cpu_percent: 1,
                memory_percent: 1,
                bandwidth_percent: 1,
                max_concurrent_transfers: None,
            }),
            ..ProfileConfig::default()
        }];
        let resolved = EffectiveBudgetConfig::resolve(&config);
        assert_eq!(resolved.cpu_percent, 15);
    }

    #[test]
    fn boost_below_base_is_clamped_up() {
        let mut config = VaporConfig::default();
        config.idle_boost.boost_cpu_percent = 5; // below the 15% base
        let resolved = EffectiveBudgetConfig::resolve(&config);
        assert_eq!(resolved.boost_cpu_percent, 15);
    }

    #[test]
    fn ramp_down_never_exceeds_ramp_up() {
        let mut config = VaporConfig::default();
        config.idle_boost.ramp_up_seconds = 10;
        config.idle_boost.ramp_down_seconds = 60;
        let resolved = EffectiveBudgetConfig::resolve(&config);
        assert_eq!(resolved.ramp_down, resolved.ramp_up);
    }

    #[test]
    fn boost_ramps_up_only_when_every_gate_passes() {
        let mut budget = ResourceBudget::new(config_with(true));
        let t0 = Instant::now();

        // Not idle long enough: stays at base.
        let ceilings = budget.tick(
            ThrottleState::IdleDrain,
            &idle_inputs(),
            Duration::from_secs(10),
            t0,
        );
        assert_eq!(ceilings.boost_state, "off");
        assert_eq!(ceilings.cpu_percent, 15);
        assert!(ceilings.reason.contains("idle for 10s"));

        // All gates pass: ramp starts and progresses linearly.
        let ceilings = budget.tick(ThrottleState::IdleDrain, &idle_inputs(), long_idle(), t0);
        assert_eq!(ceilings.boost_state, "ramping-up");
        let mid = budget.tick(
            ThrottleState::IdleDrain,
            &idle_inputs(),
            long_idle(),
            t0 + Duration::from_secs(15),
        );
        assert!(
            mid.cpu_percent > 15 && mid.cpu_percent < 50,
            "mid-ramp ceiling must sit between base and boost: {}",
            mid.cpu_percent
        );
        let full = budget.tick(
            ThrottleState::IdleDrain,
            &idle_inputs(),
            long_idle(),
            t0 + Duration::from_secs(31),
        );
        assert_eq!(full.boost_state, "active");
        assert_eq!(full.cpu_percent, 50);
    }

    #[test]
    fn throttle_exit_snaps_to_base_in_the_same_tick() {
        // §Ceiling transitions rule 1.
        let mut budget = ResourceBudget::new(config_with(true));
        let t0 = Instant::now();
        budget.tick(ThrottleState::IdleDrain, &idle_inputs(), long_idle(), t0);
        let boosted = budget.tick(
            ThrottleState::IdleDrain,
            &idle_inputs(),
            long_idle(),
            t0 + Duration::from_secs(31),
        );
        assert_eq!(boosted.cpu_percent, 50);

        let snapped = budget.tick(
            ThrottleState::Throttled,
            &idle_inputs(),
            long_idle(),
            t0 + Duration::from_secs(32),
        );
        assert_eq!(snapped.boost_state, "off");
        assert_eq!(
            snapped.cpu_percent, 15,
            "post-IdleDrain states never run against boosted caps"
        );
    }

    #[test]
    fn return_to_idle_drain_does_not_auto_resume() {
        // §Ceiling transitions rule 2: gates re-evaluate from scratch.
        let mut budget = ResourceBudget::new(config_with(true));
        let t0 = Instant::now();
        budget.tick(ThrottleState::IdleDrain, &idle_inputs(), long_idle(), t0);
        budget.tick(
            ThrottleState::IdleDrain,
            &idle_inputs(),
            long_idle(),
            t0 + Duration::from_secs(31),
        );
        budget.tick(
            ThrottleState::Throttled,
            &idle_inputs(),
            long_idle(),
            t0 + Duration::from_secs(32),
        );

        // Back to IdleDrain but the user is no longer idle: no resume.
        let ceilings = budget.tick(
            ThrottleState::IdleDrain,
            &idle_inputs(),
            Duration::from_secs(5),
            t0 + Duration::from_secs(33),
        );
        assert_eq!(ceilings.boost_state, "off");
        assert_eq!(ceilings.cpu_percent, 15);

        // Idle again: a FRESH ramp starts from base.
        let ceilings = budget.tick(
            ThrottleState::IdleDrain,
            &idle_inputs(),
            long_idle(),
            t0 + Duration::from_secs(34),
        );
        assert_eq!(ceilings.boost_state, "ramping-up");
        assert!(ceilings.cpu_percent <= 16, "fresh ramp starts at base");
    }

    #[test]
    fn gating_break_ramps_down_gracefully() {
        // §Ceiling transitions rule 3.
        let mut budget = ResourceBudget::new(config_with(true));
        let t0 = Instant::now();
        budget.tick(ThrottleState::IdleDrain, &idle_inputs(), long_idle(), t0);
        budget.tick(
            ThrottleState::IdleDrain,
            &idle_inputs(),
            long_idle(),
            t0 + Duration::from_secs(31),
        );

        // User HID input breaks the idle gate while throttle stays
        // IdleDrain: linear ramp-down, not a snap.
        let down_start = t0 + Duration::from_secs(32);
        let first = budget.tick(
            ThrottleState::IdleDrain,
            &idle_inputs(),
            Duration::from_secs(0),
            down_start,
        );
        assert_eq!(first.boost_state, "ramping-down");
        let mid = budget.tick(
            ThrottleState::IdleDrain,
            &idle_inputs(),
            Duration::from_secs(1),
            down_start + Duration::from_secs(5),
        );
        assert!(
            mid.cpu_percent > 15 && mid.cpu_percent < 50,
            "mid-down-ramp must sit between: {}",
            mid.cpu_percent
        );
        let done = budget.tick(
            ThrottleState::IdleDrain,
            &idle_inputs(),
            Duration::from_secs(2),
            down_start + Duration::from_secs(11),
        );
        assert_eq!(done.boost_state, "off");
        assert_eq!(done.cpu_percent, 15);
    }

    #[test]
    fn recovered_headroom_mid_ramp_down_resumes_ramping_up() {
        // A momentary gate break (e.g. a 1s CPU blip) must not cost the
        // full down-ramp + a fresh up-ramp; the moment headroom returns,
        // the budget resumes climbing from its current level.
        let mut budget = ResourceBudget::new(config_with(true));
        let t0 = Instant::now();
        budget.tick(ThrottleState::IdleDrain, &idle_inputs(), long_idle(), t0);
        budget.tick(
            ThrottleState::IdleDrain,
            &idle_inputs(),
            long_idle(),
            t0 + Duration::from_secs(31),
        );

        // Break the idle gate: start ramping down.
        let down_start = t0 + Duration::from_secs(32);
        assert_eq!(
            budget
                .tick(
                    ThrottleState::IdleDrain,
                    &idle_inputs(),
                    Duration::from_secs(0),
                    down_start,
                )
                .boost_state,
            "ramping-down"
        );
        let mid = budget.tick(
            ThrottleState::IdleDrain,
            &idle_inputs(),
            Duration::from_secs(0),
            down_start + Duration::from_secs(4),
        );
        assert_eq!(mid.boost_state, "ramping-down");

        // Headroom recovers (idle again): resume ramping up, holding the
        // current level this tick rather than continuing to descend.
        let recovered = budget.tick(
            ThrottleState::IdleDrain,
            &idle_inputs(),
            long_idle(),
            down_start + Duration::from_secs(5),
        );
        assert_eq!(recovered.boost_state, "ramping-up");
        assert!(
            recovered.cpu_percent >= mid.cpu_percent,
            "must not drop below the current level on recovery: {} vs {}",
            recovered.cpu_percent,
            mid.cpu_percent
        );
        // The next tick climbs back toward boost, not down to base.
        let climbing = budget.tick(
            ThrottleState::IdleDrain,
            &idle_inputs(),
            long_idle(),
            down_start + Duration::from_secs(6),
        );
        assert!(
            climbing.cpu_percent >= recovered.cpu_percent,
            "must climb from the recovered level: {} vs {}",
            climbing.cpu_percent,
            recovered.cpu_percent
        );
    }

    #[test]
    fn headroom_gate_blocks_boost_under_foreign_load() {
        let mut budget = ResourceBudget::new(config_with(true));
        let busy = ThrottleInputs {
            system_cpu_load_percent: 34,
            vapor_cpu_load_percent: 2,
            ..ThrottleInputs::default()
        };
        let ceilings = budget.tick(ThrottleState::IdleDrain, &busy, long_idle(), Instant::now());
        assert_eq!(ceilings.boost_state, "off");
        assert!(ceilings.reason.contains("headroom"));
    }

    #[test]
    fn disabled_boost_never_engages() {
        let mut budget = ResourceBudget::new(config_with(false));
        let ceilings = budget.tick(
            ThrottleState::IdleDrain,
            &idle_inputs(),
            long_idle(),
            Instant::now(),
        );
        assert_eq!(ceilings.boost_state, "off");
        assert!(ceilings.reason.contains("disabled"));
    }
}
