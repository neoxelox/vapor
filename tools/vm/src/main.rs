//! `vapor-vm`: the Tier VM command. `scripts/vm.sh` wraps it.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use vapor_vm::runner::{self, RunOptions};
use vapor_vm::scenario::GuestKind;
use vapor_vm::scenarios;

#[derive(Parser, Debug)]
#[command(
    name = "vapor-vm",
    about = "Vapor virtual machine verification harness"
)]
struct Args {
    /// Repository root (default: discovered from the executable path).
    #[arg(long, global = true)]
    repo_root: Option<PathBuf>,
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Run the scenario suite.
    Run {
        /// Scenario ids or names to run (repeatable, comma-separated).
        #[arg(long, value_delimiter = ',')]
        only: Vec<String>,
        /// Keep the guest of a failed scenario, stopped, for a look.
        #[arg(long)]
        keep: bool,
        /// Record a video of each scenario. For a change that moves on
        /// the screen; the screenshots cover a screen that does not.
        #[arg(long)]
        video: bool,
        /// Write the JSON report here (default: inside the run directory).
        #[arg(long)]
        json: Option<PathBuf>,
    },
    /// List the scenarios with the guest each one needs.
    List,
    /// Pull the guest image when it is missing and make the prepared
    /// guest. The one command that uses the network.
    Setup,
    /// Start a guest with the build installed, for manual work.
    Guest {
        /// Show the guest's screen in a window on this Mac, to use the
        /// app in it by hand. Only when a person asked to look.
        #[arg(long)]
        window: bool,
        /// Take the place of the fresh guests, not the one of the
        /// shared guest: the scenarios of the installed app, and a
        /// manual guest of another checkout, still run next to it.
        #[arg(long)]
        second: bool,
        /// Give the guest a route to the Internet, to test a real
        /// cloud provider. A guest has none without this option.
        #[arg(long)]
        internet: bool,
    },
    /// Stop and delete every guest of this checkout: a manual guest,
    /// a guest that `--keep` left, a guest of a run that was killed.
    GuestStop,
    /// Stop and delete the guests of this checkout and the prepared
    /// guests. The image stays.
    Clean,
}

fn main() -> ExitCode {
    let args = Args::parse();
    let repo_root = match args
        .repo_root
        .clone()
        .map(Ok)
        .unwrap_or_else(runner::discover_repo_root)
    {
        Ok(root) => root,
        Err(failure) => {
            eprintln!("[vm] {failure}");
            return ExitCode::from(2);
        }
    };
    let outcome = match args.command {
        Cmd::Run {
            only,
            keep,
            video,
            json,
        } => runner::run(&RunOptions {
            repo_root,
            only,
            keep,
            video,
            json,
        }),
        Cmd::List => {
            for scenario in scenarios::all() {
                let guest = match scenario.guest {
                    GuestKind::Fresh => "fresh",
                    GuestKind::Shared(_) => "shared",
                };
                println!(
                    "{}  {:<24} {:<7} {}",
                    scenario.id, scenario.name, guest, scenario.proves
                );
            }
            Ok(true)
        }
        Cmd::Setup => runner::setup(&repo_root).map(|()| true),
        Cmd::Guest {
            window,
            second,
            internet,
        } => runner::manual_guest(&repo_root, window, second, internet).map(|()| true),
        Cmd::GuestStop => runner::stop_guests(&repo_root).map(|count| {
            println!("[vm] deleted {count} guest(s) of this checkout");
            true
        }),
        Cmd::Clean => runner::clean(&repo_root).map(|count| {
            println!("[vm] deleted {count} VM(s) of the tier");
            true
        }),
    };
    match outcome {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(failure) => {
            eprintln!("[vm] {failure}");
            ExitCode::from(2)
        }
    }
}
