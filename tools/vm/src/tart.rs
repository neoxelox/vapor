//! The one module that starts `tart`. Everything the tier does to a
//! virtual machine goes through here, so the rules live in one place:
//! the harness only ever changes a VM whose name starts with
//! [`VM_PREFIX`], and it never pulls an image during a run.

use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use vapor_e2e::Failure;

/// The image every guest is cloned from. Pinned by digest, so a run
/// tests the same guest on every machine. A new digest is a change set
/// of its own that runs the tier.
pub const IMAGE_REPOSITORY: &str = "ghcr.io/cirruslabs/macos-golden-gate-base";
pub const IMAGE_DIGEST: &str =
    "sha256:4859ea9483a1f6e377301f23a5243798044266a373fc36635e9dcd045e09292f";

/// Every VM the harness makes has this prefix, and the harness deletes
/// nothing without it.
pub const VM_PREFIX: &str = "vapor-vm-";

/// macOS guests need this Tart for the macOS 27 images.
pub const MIN_TART_VERSION: (u32, u32, u32) = (2, 33, 0);

/// Apple permits two macOS guests on one host at a time.
pub const MAX_MACOS_GUESTS: usize = 2;

/// The screen of a guest, so screenshots have one size on every host.
pub const DISPLAY: &str = "1440x900";

/// How often a running `tart exec` is asked whether it ended.
const EXEC_POLL: Duration = Duration::from_millis(10);

pub fn image() -> String {
    format!("{IMAGE_REPOSITORY}@{IMAGE_DIGEST}")
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Vm {
    pub name: String,
    /// `local` for a clone, `OCI` for a pulled image.
    pub source: String,
    /// `running`, `stopped`, or `suspended`.
    pub state: String,
}

impl Vm {
    pub fn is_running(&self) -> bool {
        self.state == "running"
    }

    pub fn is_ours(&self) -> bool {
        self.source == "local" && self.name.starts_with(VM_PREFIX)
    }
}

/// A host directory a guest sees under `/Volumes/My Shared Files/<name>`.
#[derive(Clone, Debug)]
pub struct Share<'a> {
    pub name: &'a str,
    pub path: &'a Path,
    pub read_only: bool,
}

fn tart() -> Command {
    Command::new("tart")
}

fn run(command: &mut Command, what: &str) -> Result<Output, Failure> {
    let output = command
        .stdin(Stdio::null())
        .output()
        .map_err(|error| Failure::new(format!("could not start tart to {what}: {error}")))?;
    Ok(output)
}

fn run_ok(command: &mut Command, what: &str) -> Result<String, Failure> {
    let output = run(command, what)?;
    if !output.status.success() {
        return Err(Failure::new(format!(
            "tart could not {what}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn ours(name: &str) -> Result<(), Failure> {
    if name.starts_with(VM_PREFIX) && name.len() > VM_PREFIX.len() {
        Ok(())
    } else {
        Err(Failure::new(format!(
            "refusing to change the VM `{name}`: the tier only changes VMs named `{VM_PREFIX}*`"
        )))
    }
}

/// The installed Tart version, or why it cannot be read.
pub fn version() -> Result<(u32, u32, u32), Failure> {
    let text = run_ok(tart().arg("--version"), "print its version")?;
    parse_version(text.trim()).ok_or_else(|| {
        Failure::new(format!(
            "cannot read the Tart version from `{}`",
            text.trim()
        ))
    })
}

pub fn parse_version(text: &str) -> Option<(u32, u32, u32)> {
    let mut parts = text.split('.').map(|part| part.parse::<u32>().ok());
    Some((parts.next()??, parts.next()??, parts.next()??))
}

pub fn list() -> Result<Vec<Vm>, Failure> {
    let text = run_ok(tart().args(["list", "--format", "json"]), "list its VMs")?;
    parse_list(&text)
}

pub fn parse_list(text: &str) -> Result<Vec<Vm>, Failure> {
    let rows: Vec<serde_json::Value> = serde_json::from_str(text)?;
    Ok(rows
        .iter()
        .map(|row| {
            let field = |name: &str| row[name].as_str().unwrap_or_default().to_string();
            Vm {
                name: field("Name"),
                source: field("Source"),
                state: field("State"),
            }
        })
        .collect())
}

/// Whether the pinned image is in Tart's store.
pub fn image_present() -> Result<bool, Failure> {
    let image = image();
    Ok(list()?.iter().any(|vm| vm.name == image))
}

/// Downloads the pinned image. The only call that uses the network,
/// and only `setup` makes it.
pub fn pull_image() -> Result<(), Failure> {
    let status = tart()
        .args(["pull", &image()])
        .stdin(Stdio::null())
        .status()
        .map_err(|error| {
            Failure::new(format!("could not start tart to pull the image: {error}"))
        })?;
    if status.success() {
        Ok(())
    } else {
        Err(Failure::new("tart could not pull the image"))
    }
}

pub fn clone(source: &str, name: &str) -> Result<(), Failure> {
    ours(name)?;
    run_ok(tart().args(["clone", source, name]), "clone a VM").map(drop)
}

/// Gives a VM the display of the tier and a network address of its
/// own. A clone keeps the address of its source, and Tart refuses to
/// run two VMs with one address.
pub fn configure(name: &str) -> Result<(), Failure> {
    ours(name)?;
    run_ok(
        tart().args(["set", name, "--display", DISPLAY, "--random-mac"]),
        "set the display and the network address of a VM",
    )
    .map(drop)
}

/// Starts a VM with no sound and no shared clipboard. Its network
/// reaches the host only, unless `internet` asks for Tart's shared
/// network: that is for a manual guest that tests a real cloud
/// provider, never for a run. It has no window either, unless `window`
/// asks for Tart's own: that is for a person who wants to use a manual
/// guest. A `detached` VM outlives the harness. The child lives as
/// long as the VM runs; its output goes to `log`.
pub fn start(
    name: &str,
    shares: &[Share<'_>],
    log: &Path,
    window: bool,
    internet: bool,
    detached: bool,
) -> Result<Child, Failure> {
    ours(name)?;
    let out = File::create(log)?;
    let err = out.try_clone()?;
    let mut command = tart();
    command.args(run_arguments(name, shares, window, internet));
    // A guest for a person must stay up for as long as that person
    // wants it. In a process group of its own it does not end when the
    // process that started the harness is stopped or times out.
    #[cfg(unix)]
    if detached {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(not(unix))]
    let _ = detached;
    command
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err)
        .spawn()
        .map_err(|error| Failure::new(format!("could not start tart to run `{name}`: {error}")))
}

/// The arguments of `tart run` for a guest. A guest has no window and
/// a host-only network unless the caller asks for the one or the
/// other, and it never has sound or a shared clipboard.
fn run_arguments(name: &str, shares: &[Share<'_>], window: bool, internet: bool) -> Vec<String> {
    let mut arguments = vec!["run".to_string(), name.to_string()];
    if !window {
        arguments.push("--no-graphics".to_string());
    }
    arguments.extend(["--no-audio", "--no-clipboard", "--suspendable"].map(String::from));
    if !internet {
        arguments.push("--net-host".to_string());
    }
    for share in shares {
        let mut spec = format!("{}:{}", share.name, share.path.display());
        if share.read_only {
            spec.push_str(":ro");
        }
        arguments.push("--dir".to_string());
        arguments.push(spec);
    }
    arguments
}

/// Runs a command in a running guest, as the logged-in user. A command
/// that does not end within `timeout` is a failure: a guest that hangs
/// must not hang the run.
pub fn exec(name: &str, arguments: &[&str], timeout: Duration) -> Result<Output, Failure> {
    ours(name)?;
    let mut child = tart()
        .arg("exec")
        .arg(name)
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            Failure::new(format!(
                "could not start tart to run a command in a guest: {error}"
            ))
        })?;
    // The pipes are read while the command runs, so a command with a
    // lot of output cannot block on a full pipe.
    let reader = |pipe: Option<Box<dyn Read + Send>>| {
        thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut bytes);
            }
            bytes
        })
    };
    let stdout = reader(
        child
            .stdout
            .take()
            .map(|pipe| Box::new(pipe) as Box<dyn Read + Send>),
    );
    let stderr = reader(
        child
            .stderr
            .take()
            .map(|pipe| Box::new(pipe) as Box<dyn Read + Send>),
    );
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => thread::sleep(EXEC_POLL),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(Failure::new(format!(
                    "a command in the guest `{name}` did not end within {}s: {}",
                    timeout.as_secs(),
                    arguments.join(" ")
                )));
            }
            Err(error) => {
                return Err(Failure::new(format!(
                    "could not wait for a command in the guest `{name}`: {error}"
                )));
            }
        }
    };
    Ok(Output {
        status,
        stdout: stdout.join().unwrap_or_default(),
        stderr: stderr.join().unwrap_or_default(),
    })
}

/// Whether a VM runs macOS. Apple's limit of two guests counts macOS
/// guests only, and `tart list` does not say what a VM runs.
pub fn is_macos(name: &str) -> bool {
    run(
        tart().args(["get", name, "--format", "json"]),
        "read a VM's configuration",
    )
    .ok()
    .and_then(|output| serde_json::from_slice::<serde_json::Value>(&output.stdout).ok())
    .is_none_or(|configuration| configuration["OS"] != "linux")
}

pub fn suspend(name: &str) -> Result<(), Failure> {
    ours(name)?;
    run_ok(tart().args(["suspend", name]), "suspend a VM").map(drop)
}

/// Stops a VM. A VM that does not run is not an error.
pub fn stop(name: &str) -> Result<(), Failure> {
    ours(name)?;
    run(tart().args(["stop", name]), "stop a VM").map(drop)
}

pub fn delete(name: &str) -> Result<(), Failure> {
    ours(name)?;
    run_ok(tart().args(["delete", name]), "delete a VM").map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_vm_without_the_prefix_is_never_changed() {
        for name in [
            "",
            "vapor-vm-",
            "macos",
            "ghcr.io/cirruslabs/macos-golden-gate-base",
        ] {
            assert!(ours(name).is_err(), "`{name}` must be refused");
            assert!(delete(name).is_err(), "`{name}` must not be deleted");
            assert!(stop(name).is_err(), "`{name}` must not be stopped");
        }
        assert!(ours("vapor-vm-run-1").is_ok());
    }

    #[test]
    fn the_list_tells_our_clones_from_images_and_other_vms() {
        let list = parse_list(
            r#"[
              {"Name":"vapor-vm-run-1","Source":"local","State":"running","Running":true},
              {"Name":"my-own-vm","Source":"local","State":"stopped","Running":false},
              {"Name":"ghcr.io/x/y@sha256:1","Source":"OCI","State":"stopped","Running":false}
            ]"#,
        )
        .expect("the list parses");
        assert_eq!(list.len(), 3);
        assert!(list[0].is_ours() && list[0].is_running());
        assert!(!list[1].is_ours() && !list[1].is_running());
        assert!(!list[2].is_ours());
    }

    #[test]
    fn a_guest_is_offline_and_has_no_window_unless_asked() {
        let stage = Path::new("/stage");
        let shares = [Share {
            name: "stage",
            path: stage,
            read_only: true,
        }];
        let has = |arguments: &[String], flag: &str| arguments.iter().any(|a| a == flag);

        // What a run of the suite starts.
        let run = run_arguments("vapor-vm-x-run-1-shared", &shares, false, false);
        assert!(has(&run, "--net-host") && has(&run, "--no-graphics"));
        assert!(has(&run, "--no-audio") && has(&run, "--no-clipboard"));
        assert!(has(&run, "stage:/stage:ro"));

        // Each option lifts its own limit and no other.
        let window = run_arguments("vapor-vm-x-manual-1", &shares, true, false);
        assert!(has(&window, "--net-host") && !has(&window, "--no-graphics"));
        let internet = run_arguments("vapor-vm-x-manual-1", &shares, false, true);
        assert!(!has(&internet, "--net-host") && has(&internet, "--no-graphics"));
        assert!(has(&internet, "--no-audio") && has(&internet, "--no-clipboard"));
    }

    #[test]
    fn versions_parse_and_compare() {
        assert_eq!(parse_version("2.42.0"), Some((2, 42, 0)));
        assert_eq!(parse_version("2.33"), None);
        assert!(parse_version("2.42.0").expect("parses") >= MIN_TART_VERSION);
        assert!((2, 32, 9) < MIN_TART_VERSION);
    }
}
