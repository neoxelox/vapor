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
    Guest,
    /// Stop and delete the manual guests of this checkout.
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
        Cmd::Run { only, keep, json } => runner::run(&RunOptions {
            repo_root,
            only,
            keep,
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
        Cmd::Guest => runner::manual_guest(&repo_root).map(|()| true),
        Cmd::GuestStop => runner::stop_manual_guests(&repo_root).map(|count| {
            println!("[vm] deleted {count} manual guest(s)");
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
