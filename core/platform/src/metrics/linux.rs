//! Linux `PlatformMetricsSampler`.
//!
//! One sample per throttle interval reads: system CPU from
//! `/proc/stat`, this process's CPU from `/proc/self/stat`, resident
//! memory from `/proc/self/statm`, physical memory from
//! `/proc/meminfo`, power source from `/sys/class/power_supply`, and
//! thermal pressure from the hottest `/sys/class/thermal` zone
//! against its trip points. Calls inside the interval return the
//! cached reading. Anything the host does not expose (a container
//! without `/sys/class/power_supply`, a VM without thermal zones)
//! keeps the neutral default for that input. User presence follows
//! the idle notifier: a headless host is idle, a desktop is not.
#![allow(unsafe_code)]

use std::fs;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use vapor_shared::{ThermalPressure, ThrottleInputs, constants};

use super::{PlatformMetricsSampler, cpu_percentages};
use crate::idle::{IdleNotifier, NativeIdleNotifier};

#[derive(Debug)]
pub struct NativePlatformMetricsSampler {
    cpus: u32,
    clock_ticks_per_second: u64,
    page_bytes: u64,
    device_memory_bytes: Option<u64>,
    idle: NativeIdleNotifier,
    state: Mutex<SampleState>,
}

#[derive(Debug)]
struct SampleState {
    sampled_at: Option<Instant>,
    cpu_ticks: Option<[u32; 4]>,
    process_cpu: Option<Duration>,
    inputs: ThrottleInputs,
}

impl Default for NativePlatformMetricsSampler {
    fn default() -> Self {
        Self::for_current_host()
    }
}

impl NativePlatformMetricsSampler {
    pub fn for_current_host() -> Self {
        // SAFETY: `sysconf` with a valid name has no preconditions.
        let cpus = unsafe { libc::sysconf(libc::_SC_NPROCESSORS_ONLN) };
        // SAFETY: as above.
        let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        // SAFETY: as above.
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        Self {
            cpus: u32::try_from(cpus).unwrap_or(1).max(1),
            clock_ticks_per_second: u64::try_from(ticks).unwrap_or(100).max(1),
            page_bytes: u64::try_from(page).unwrap_or(4096).max(1),
            device_memory_bytes: device_memory_bytes(),
            idle: NativeIdleNotifier::for_current_host(),
            state: Mutex::new(SampleState {
                sampled_at: None,
                cpu_ticks: None,
                process_cpu: None,
                inputs: ThrottleInputs::default(),
            }),
        }
    }

    /// `true`: CPU, memory, power, and thermal state are read from
    /// `/proc` and `/sys` on Linux.
    pub fn has_native_sampling() -> bool {
        true
    }

    /// The inputs this host feeds the throttle, for logs and doctor.
    pub fn input_sources() -> &'static str {
        "CPU load, power source, thermal zones, memory, and whether a desktop session exists"
    }

    fn refresh(&self, state: &mut SampleState, now: Instant) {
        let mut inputs = state.inputs;
        let ticks = cpu_ticks();
        let process_cpu = process_cpu_time(self.clock_ticks_per_second);
        if let (Some(prev_ticks), Some(prev_cpu), Some(prev_at), Some(ticks), Some(process_cpu)) = (
            state.cpu_ticks,
            state.process_cpu,
            state.sampled_at,
            ticks,
            process_cpu,
        ) {
            let (system, vapor) = cpu_percentages(
                (&prev_ticks, &ticks),
                (prev_cpu, process_cpu),
                now.saturating_duration_since(prev_at),
                self.cpus,
            );
            inputs.system_cpu_load_percent = system;
            inputs.vapor_cpu_load_percent = vapor;
        }
        if ticks.is_some() {
            state.cpu_ticks = ticks;
        }
        if process_cpu.is_some() {
            state.process_cpu = process_cpu;
        }
        if let Some(on_battery) = on_battery() {
            inputs.on_battery = on_battery;
        }
        if let Some(thermal) = thermal_pressure() {
            inputs.thermal_pressure = thermal;
        }
        inputs.vapor_memory_bytes = resident_bytes(self.page_bytes);
        inputs.device_memory_bytes = self.device_memory_bytes;
        inputs.user_active = self.idle.has_gui_session()
            && self.idle.idle_for()
                < Duration::from_millis(constants::engine::USER_ACTIVE_INPUT_WINDOW_MILLIS);
        state.inputs = inputs;
        state.sampled_at = Some(now);
    }
}

impl PlatformMetricsSampler for NativePlatformMetricsSampler {
    fn sample(&self) -> ThrottleInputs {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Instant::now();
        let interval = Duration::from_millis(constants::engine::THROTTLE_SAMPLE_INTERVAL_MILLIS);
        let due = state
            .sampled_at
            .is_none_or(|last| now.saturating_duration_since(last) >= interval);
        if due {
            self.refresh(&mut state, now);
        }
        state.inputs
    }
}

/// `[user, system, idle, nice]` ticks from the aggregate `cpu` line,
/// the same slot order the shared arithmetic expects (idle third).
fn cpu_ticks() -> Option<[u32; 4]> {
    let stat = fs::read_to_string("/proc/stat").ok()?;
    parse_cpu_line(&stat)
}

fn parse_cpu_line(stat: &str) -> Option<[u32; 4]> {
    let line = stat.lines().find(|line| line.starts_with("cpu "))?;
    let fields: Vec<u64> = line
        .split_whitespace()
        .skip(1)
        .filter_map(|field| field.parse().ok())
        .collect();
    // user nice system idle iowait irq softirq steal ...
    if fields.len() < 4 {
        return None;
    }
    let user = fields[0];
    let nice = fields[1];
    let system =
        fields[2] + fields.get(5).copied().unwrap_or(0) + fields.get(6).copied().unwrap_or(0);
    let idle = fields[3] + fields.get(4).copied().unwrap_or(0);
    Some([user as u32, system as u32, idle as u32, nice as u32])
}

fn process_cpu_time(ticks_per_second: u64) -> Option<Duration> {
    let stat = fs::read_to_string("/proc/self/stat").ok()?;
    parse_process_cpu(&stat, ticks_per_second)
}

fn parse_process_cpu(stat: &str, ticks_per_second: u64) -> Option<Duration> {
    // The command name is in parentheses and may hold spaces; fields
    // count from after it: utime is the 14th field, stime the 15th.
    let after = stat.rsplit_once(')')?.1;
    let fields: Vec<&str> = after.split_whitespace().collect();
    let utime: u64 = fields.get(11)?.parse().ok()?;
    let stime: u64 = fields.get(12)?.parse().ok()?;
    Some(Duration::from_secs_f64(
        (utime + stime) as f64 / ticks_per_second as f64,
    ))
}

fn resident_bytes(page_bytes: u64) -> Option<u64> {
    let statm = fs::read_to_string("/proc/self/statm").ok()?;
    let resident_pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
    Some(resident_pages * page_bytes)
}

fn device_memory_bytes() -> Option<u64> {
    let meminfo = fs::read_to_string("/proc/meminfo").ok()?;
    parse_meminfo_total(&meminfo)
}

fn parse_meminfo_total(meminfo: &str) -> Option<u64> {
    let line = meminfo.lines().find(|line| line.starts_with("MemTotal:"))?;
    let kib: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kib * 1024)
}

/// `Some(true)` when every present battery is discharging and no
/// mains supply reports online; `None` on a host with no power supply
/// entries (a container, a VM), where the neutral default stands.
fn on_battery() -> Option<bool> {
    let entries = fs::read_dir("/sys/class/power_supply").ok()?;
    let mut saw_supply = false;
    let mut mains_online = false;
    let mut battery_discharging = false;
    for entry in entries.flatten() {
        let path = entry.path();
        let kind = fs::read_to_string(path.join("type")).unwrap_or_default();
        match kind.trim() {
            "Mains" | "USB" | "Wireless" => {
                saw_supply = true;
                if fs::read_to_string(path.join("online"))
                    .map(|s| s.trim() == "1")
                    .unwrap_or(false)
                {
                    mains_online = true;
                }
            }
            "Battery" => {
                saw_supply = true;
                if fs::read_to_string(path.join("status"))
                    .map(|s| s.trim() == "Discharging")
                    .unwrap_or(false)
                {
                    battery_discharging = true;
                }
            }
            _ => {}
        }
    }
    saw_supply.then_some(battery_discharging && !mains_online)
}

/// The hottest thermal zone against its own trip points: past the
/// `critical` or `hot` trip is `Critical`, past `passive` is `Serious`,
/// within 5 °C of `passive` is `Fair`. Zones without trip points fall
/// back to fixed thresholds.
fn thermal_pressure() -> Option<ThermalPressure> {
    let zones = fs::read_dir("/sys/class/thermal").ok()?;
    let mut worst: Option<ThermalPressure> = None;
    for entry in zones.flatten() {
        let path = entry.path();
        if !entry
            .file_name()
            .to_string_lossy()
            .starts_with("thermal_zone")
        {
            continue;
        }
        let Some(temp) = read_millidegrees(&path.join("temp")) else {
            continue;
        };
        let mut passive = None;
        let mut critical = None;
        for index in 0..8 {
            let kind = fs::read_to_string(path.join(format!("trip_point_{index}_type")));
            let Ok(kind) = kind else { break };
            let value = read_millidegrees(&path.join(format!("trip_point_{index}_temp")));
            match (kind.trim(), value) {
                ("passive" | "active", Some(value)) => {
                    passive = Some(passive.map_or(value, |p: i64| p.min(value)));
                }
                ("critical" | "hot", Some(value)) => {
                    critical = Some(critical.map_or(value, |c: i64| c.min(value)));
                }
                _ => {}
            }
        }
        let pressure = classify_temperature(temp, passive, critical);
        worst = Some(match worst {
            Some(current) if current >= pressure => current,
            _ => pressure,
        });
    }
    worst
}

fn classify_temperature(
    millidegrees: i64,
    passive: Option<i64>,
    critical: Option<i64>,
) -> ThermalPressure {
    let critical = critical.unwrap_or(95_000);
    let passive = passive.unwrap_or(80_000);
    if millidegrees >= critical {
        ThermalPressure::Critical
    } else if millidegrees >= passive {
        ThermalPressure::Serious
    } else if millidegrees + 5_000 >= passive {
        ThermalPressure::Fair
    } else {
        ThermalPressure::Nominal
    }
}

fn read_millidegrees(path: &std::path::Path) -> Option<i64> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proc_stat_cpu_line_folds_into_the_shared_slots() {
        let stat = "cpu  100 20 50 800 30 5 5 0 0 0\ncpu0 1 2 3 4 5 6 7 8 9 10\n";
        // user=100, nice=20, system=50+irq 5+softirq 5=60, idle=800+iowait 30=830.
        assert_eq!(parse_cpu_line(stat), Some([100, 60, 830, 20]));
        assert_eq!(parse_cpu_line("intr 1 2 3\n"), None);
    }

    #[test]
    fn process_cpu_is_read_after_the_parenthesised_name() {
        let stat = "4242 (vapor d) S 1 2 3 4 5 6 7 8 9 10 250 150 0 0 20 0 1 0 100 200\n";
        let cpu = parse_process_cpu(stat, 100).expect("parses");
        assert_eq!(cpu, Duration::from_secs(4), "250 + 150 ticks at 100 Hz");
    }

    #[test]
    fn meminfo_total_is_bytes() {
        assert_eq!(
            parse_meminfo_total("MemTotal:       16384 kB\nMemFree: 1 kB\n"),
            Some(16384 * 1024)
        );
    }

    #[test]
    fn temperatures_classify_against_trip_points_or_fixed_thresholds() {
        assert_eq!(
            classify_temperature(40_000, Some(85_000), Some(100_000)),
            ThermalPressure::Nominal
        );
        assert_eq!(
            classify_temperature(81_000, Some(85_000), Some(100_000)),
            ThermalPressure::Fair
        );
        assert_eq!(
            classify_temperature(90_000, Some(85_000), Some(100_000)),
            ThermalPressure::Serious
        );
        assert_eq!(
            classify_temperature(100_000, Some(85_000), Some(100_000)),
            ThermalPressure::Critical
        );
        assert_eq!(
            classify_temperature(96_000, None, None),
            ThermalPressure::Critical
        );
        assert_eq!(
            classify_temperature(50_000, None, None),
            ThermalPressure::Nominal
        );
    }

    #[test]
    fn the_sampler_reads_this_host() {
        let sampler = NativePlatformMetricsSampler::for_current_host();
        let inputs = sampler.sample();
        assert!(inputs.vapor_memory_bytes.is_some_and(|bytes| bytes > 0));
        assert!(inputs.device_memory_bytes.is_some_and(|bytes| bytes > 0));
        assert!(inputs.system_cpu_load_percent <= 100);
    }
}
