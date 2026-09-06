//! macOS `PlatformMetricsSampler`.
//!
//! One sample per throttle interval reads: system and process CPU from
//! `host_statistics64` and `getrusage`, power source from IOKit
//! (`IOPSGetTimeRemainingEstimate`), thermal state and Low Power Mode
//! from `NSProcessInfo`, resident and physical memory from
//! `proc_pidinfo` and `sysctl hw.memsize`, and user presence from the
//! HID idle clock. Calls inside the interval return the cached reading,
//! so several profile runtimes sharing one sampler cost one set of
//! syscalls per second. Every `unsafe` block states what it relies on.
#![allow(unsafe_code)]

use std::ffi::{c_char, c_void};
use std::mem::{self, MaybeUninit};
use std::ptr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use vapor_shared::{ThermalPressure, ThrottleInputs, constants};

use super::{PlatformMetricsSampler, cpu_percentages};
use crate::idle::{IdleNotifier, NativeIdleNotifier};

const HOST_CPU_LOAD_INFO: i32 = 3;
const HOST_CPU_LOAD_INFO_COUNT: u32 = 4;
const KERN_SUCCESS: i32 = 0;
/// `kIOPSTimeRemainingUnlimited`: on external power or no battery.
const TIME_REMAINING_UNLIMITED: f64 = -2.0;

unsafe extern "C" {
    fn mach_host_self() -> u32;
    fn host_statistics64(host: u32, flavor: i32, info: *mut i32, count: *mut u32) -> i32;
}

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOPSGetTimeRemainingEstimate() -> f64;
}

#[link(name = "objc", kind = "dylib")]
unsafe extern "C" {
    fn objc_getClass(name: *const c_char) -> *mut c_void;
    fn sel_registerName(name: *const c_char) -> *mut c_void;
    fn objc_msgSend();
}

// `NSProcessInfo` lives in Foundation; linking it is what makes the
// class visible to `objc_getClass`.
#[link(name = "Foundation", kind = "framework")]
unsafe extern "C" {}

#[derive(Debug)]
pub struct NativePlatformMetricsSampler {
    host: u32,
    cpus: u32,
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

impl NativePlatformMetricsSampler {
    pub fn for_current_host() -> Self {
        // SAFETY: `mach_host_self` has no preconditions; the port it
        // returns is kept for the sampler's lifetime rather than
        // re-acquired (and leaked) every second.
        let host = unsafe { mach_host_self() };
        // SAFETY: `sysconf` with a valid name has no preconditions.
        let cpus = unsafe { libc::sysconf(libc::_SC_NPROCESSORS_ONLN) };
        Self {
            host,
            cpus: u32::try_from(cpus).unwrap_or(1).max(1),
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

    /// `true`: CPU, power, thermal, memory and user presence are read
    /// from the host on macOS.
    pub fn has_native_sampling() -> bool {
        true
    }

    fn refresh(&self, state: &mut SampleState, now: Instant) {
        let mut inputs = state.inputs;

        let ticks = cpu_ticks(self.host);
        let process_cpu = process_cpu_time();
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

        inputs.on_battery = on_battery();
        if let Some((thermal, low_power)) = process_info_state() {
            inputs.thermal_pressure = thermal;
            inputs.low_power_mode = low_power;
        }
        inputs.vapor_memory_bytes = resident_bytes();
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

fn cpu_ticks(host: u32) -> Option<[u32; 4]> {
    let mut info = [0u32; HOST_CPU_LOAD_INFO_COUNT as usize];
    let mut count = HOST_CPU_LOAD_INFO_COUNT;
    // SAFETY: `info` has room for `count` 32-bit words and both pointers
    // stay valid for the call; the kernel writes at most `count` words.
    let rc = unsafe {
        host_statistics64(
            host,
            HOST_CPU_LOAD_INFO,
            info.as_mut_ptr().cast(),
            &mut count,
        )
    };
    (rc == KERN_SUCCESS).then_some(info)
}

fn process_cpu_time() -> Option<Duration> {
    let mut usage = MaybeUninit::<libc::rusage>::uninit();
    // SAFETY: `usage` is a valid out-pointer for one `rusage` and
    // `RUSAGE_SELF` never fails for the calling process.
    let rc = unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) };
    if rc != 0 {
        return None;
    }
    // SAFETY: `getrusage` returned 0, so the struct is initialised.
    let usage = unsafe { usage.assume_init() };
    Some(timeval(usage.ru_utime) + timeval(usage.ru_stime))
}

fn timeval(value: libc::timeval) -> Duration {
    let secs = u64::try_from(value.tv_sec).unwrap_or(0);
    let micros = u64::try_from(value.tv_usec).unwrap_or(0);
    Duration::from_secs(secs) + Duration::from_micros(micros)
}

fn resident_bytes() -> Option<u64> {
    let mut info = MaybeUninit::<libc::proc_taskinfo>::uninit();
    let size = i32::try_from(mem::size_of::<libc::proc_taskinfo>()).ok()?;
    // SAFETY: the buffer is exactly `size` bytes of `proc_taskinfo`
    // storage and the call writes at most that many.
    let written = unsafe {
        libc::proc_pidinfo(
            libc::getpid(),
            libc::PROC_PIDTASKINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if written != size {
        return None;
    }
    // SAFETY: the kernel filled the whole struct.
    Some(unsafe { info.assume_init() }.pti_resident_size)
}

fn device_memory_bytes() -> Option<u64> {
    let mut value: u64 = 0;
    let mut len = mem::size_of::<u64>();
    // SAFETY: the name is a NUL-terminated literal and `value` / `len`
    // describe an 8-byte buffer, which is the documented type of
    // `hw.memsize`.
    let rc = unsafe {
        libc::sysctlbyname(
            c"hw.memsize".as_ptr(),
            ptr::addr_of_mut!(value).cast(),
            &mut len,
            ptr::null_mut(),
            0,
        )
    };
    (rc == 0 && len == mem::size_of::<u64>() && value > 0).then_some(value)
}

fn on_battery() -> bool {
    // SAFETY: no arguments, no preconditions; the value is a plain
    // double.
    let remaining = unsafe { IOPSGetTimeRemainingEstimate() };
    remaining != TIME_REMAINING_UNLIMITED
}

/// `(thermal state, Low Power Mode)` from `NSProcessInfo`, or `None` if
/// the Objective-C runtime cannot resolve the class.
fn process_info_state() -> Option<(ThermalPressure, bool)> {
    type SendObject = unsafe extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void;
    type SendInteger = unsafe extern "C" fn(*mut c_void, *mut c_void) -> isize;
    type SendBool = unsafe extern "C" fn(*mut c_void, *mut c_void) -> i8;

    // SAFETY: `objc_msgSend` is a trampoline whose real signature is the
    // one of the method it dispatches to; each cast below matches the
    // Objective-C declaration (`+processInfo` returns an object,
    // `-thermalState` an NSInteger, `-isLowPowerModeEnabled` a BOOL).
    // Selectors and class names are NUL-terminated literals.
    unsafe {
        let class = objc_getClass(c"NSProcessInfo".as_ptr());
        if class.is_null() {
            return None;
        }
        let send_object: SendObject =
            mem::transmute::<unsafe extern "C" fn(), SendObject>(objc_msgSend);
        let info = send_object(class, sel_registerName(c"processInfo".as_ptr()));
        if info.is_null() {
            return None;
        }
        let send_integer: SendInteger =
            mem::transmute::<unsafe extern "C" fn(), SendInteger>(objc_msgSend);
        let thermal = send_integer(info, sel_registerName(c"thermalState".as_ptr()));
        let send_bool: SendBool = mem::transmute::<unsafe extern "C" fn(), SendBool>(objc_msgSend);
        let low_power = send_bool(info, sel_registerName(c"isLowPowerModeEnabled".as_ptr())) != 0;
        Some((thermal_pressure(thermal), low_power))
    }
}

/// `NSProcessInfoThermalState` raw values, nominal through critical.
fn thermal_pressure(raw: isize) -> ThermalPressure {
    match raw {
        0 => ThermalPressure::Nominal,
        1 => ThermalPressure::Fair,
        2 => ThermalPressure::Serious,
        _ => ThermalPressure::Critical,
    }
}
