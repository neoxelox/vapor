//! Daemon process health: resident memory, CPU time, threads, open
//! descriptors. Sampled through `ps` and `lsof` so the driver never
//! links against anything the daemon does not expose; the numbers feed
//! the SLO checks in the report.

use std::process::Command;
use std::time::{Duration, Instant};

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct HealthSample {
    pub at_seconds: f64,
    pub rss_bytes: u64,
    /// CPU utilisation over the interval since the previous sample.
    pub cpu_percent: f64,
    pub threads: u64,
    pub open_files: Option<u64>,
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct HealthSummary {
    pub samples: usize,
    pub rss_max_bytes: u64,
    pub rss_p95_bytes: u64,
    pub cpu_avg_percent: f64,
    pub cpu_p95_percent: f64,
    pub threads_max: u64,
    pub open_files_max: Option<u64>,
}

#[derive(Debug)]
pub struct HealthMonitor {
    started: Instant,
    last_cpu: Option<(Instant, Duration)>,
    pub samples: Vec<HealthSample>,
}

impl HealthMonitor {
    pub fn new() -> Self {
        Self {
            started: Instant::now(),
            last_cpu: None,
            samples: Vec::new(),
        }
    }

    /// The daemon restarted: CPU deltas restart from zero.
    pub fn reset_process(&mut self) {
        self.last_cpu = None;
    }

    pub fn sample(&mut self, pid: u32, with_open_files: bool) -> Option<HealthSample> {
        let output = Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "rss=,cputime="])
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&output.stdout);
        let mut parts = text.split_whitespace();
        let rss_kib: u64 = parts.next()?.parse().ok()?;
        let cpu_time = parse_cputime(parts.next()?)?;
        let now = Instant::now();
        let cpu_percent = match self.last_cpu {
            Some((then, previous)) if cpu_time >= previous => {
                let wall = now.saturating_duration_since(then).as_secs_f64();
                if wall > 0.0 {
                    (cpu_time - previous).as_secs_f64() / wall * 100.0
                } else {
                    0.0
                }
            }
            _ => 0.0,
        };
        self.last_cpu = Some((now, cpu_time));
        let threads = Command::new("ps")
            .args(["-M", "-p", &pid.to_string()])
            .output()
            .ok()
            .map(|output| {
                String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .count()
                    .saturating_sub(1) as u64
            })
            .unwrap_or(0);
        let open_files = if with_open_files {
            Command::new("lsof")
                .args(["-p", &pid.to_string()])
                .output()
                .ok()
                .map(|output| {
                    String::from_utf8_lossy(&output.stdout)
                        .lines()
                        .count()
                        .saturating_sub(1) as u64
                })
        } else {
            None
        };
        let sample = HealthSample {
            at_seconds: now.saturating_duration_since(self.started).as_secs_f64(),
            rss_bytes: rss_kib * 1024,
            cpu_percent,
            threads,
            open_files,
        };
        self.samples.push(sample.clone());
        Some(sample)
    }

    pub fn summary(&self) -> HealthSummary {
        if self.samples.is_empty() {
            return HealthSummary::default();
        }
        let mut rss: Vec<u64> = self.samples.iter().map(|s| s.rss_bytes).collect();
        rss.sort_unstable();
        // The first sample has no CPU delta; skip it for the averages.
        let cpu: Vec<f64> = self.samples.iter().skip(1).map(|s| s.cpu_percent).collect();
        let mut cpu_sorted = cpu.clone();
        cpu_sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let p95 =
            |sorted_len: usize| ((sorted_len as f64 * 0.95).ceil() as usize).saturating_sub(1);
        HealthSummary {
            samples: self.samples.len(),
            rss_max_bytes: *rss.last().unwrap_or(&0),
            rss_p95_bytes: rss.get(p95(rss.len())).copied().unwrap_or(0),
            cpu_avg_percent: if cpu.is_empty() {
                0.0
            } else {
                cpu.iter().sum::<f64>() / cpu.len() as f64
            },
            cpu_p95_percent: cpu_sorted
                .get(p95(cpu_sorted.len()))
                .copied()
                .unwrap_or(0.0),
            threads_max: self.samples.iter().map(|s| s.threads).max().unwrap_or(0),
            open_files_max: self.samples.iter().filter_map(|s| s.open_files).max(),
        }
    }
}

impl Default for HealthMonitor {
    fn default() -> Self {
        Self::new()
    }
}

/// `ps` prints cumulative CPU time as `[[dd-]hh:]mm:ss[.cc]`.
fn parse_cputime(text: &str) -> Option<Duration> {
    let (days, rest) = match text.split_once('-') {
        Some((days, rest)) => (days.parse::<u64>().ok()?, rest),
        None => (0, text),
    };
    let parts: Vec<&str> = rest.split(':').collect();
    let (hours, minutes, seconds) = match parts.as_slice() {
        [h, m, s] => (
            h.parse::<u64>().ok()?,
            m.parse::<u64>().ok()?,
            s.parse::<f64>().ok()?,
        ),
        [m, s] => (0, m.parse::<u64>().ok()?, s.parse::<f64>().ok()?),
        _ => return None,
    };
    Some(Duration::from_secs_f64(
        (days * 86_400 + hours * 3_600 + minutes * 60) as f64 + seconds,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cputime_formats_parse() {
        assert_eq!(
            parse_cputime("0:01.23"),
            Some(Duration::from_secs_f64(1.23))
        );
        assert_eq!(parse_cputime("1:02:03"), Some(Duration::from_secs(3723)));
        assert_eq!(
            parse_cputime("2-00:00:10"),
            Some(Duration::from_secs(172_810))
        );
        assert_eq!(parse_cputime("junk"), None);
    }

    #[test]
    fn summary_reports_max_and_p95() {
        let mut monitor = HealthMonitor::new();
        for index in 1..=20u64 {
            monitor.samples.push(HealthSample {
                at_seconds: index as f64,
                rss_bytes: index * 1000,
                cpu_percent: index as f64,
                threads: index,
                open_files: Some(index),
            });
        }
        let summary = monitor.summary();
        assert_eq!(summary.rss_max_bytes, 20_000);
        assert_eq!(summary.rss_p95_bytes, 19_000);
        assert_eq!(summary.threads_max, 20);
        assert_eq!(summary.open_files_max, Some(20));
        assert!(summary.cpu_avg_percent > 10.0);
    }
}
