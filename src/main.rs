// Use the "windows" subsystem in release so launching from Explorer or the
// Start Menu doesn't flash a console window. We re-attach to the parent
// console at runtime when invoked from a shell, so CLI output still works.
// In debug builds we keep the console subsystem for easy logging.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use clap::CommandFactory;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();

    // `/?` and `/h` are the Windows convention for help; clap doesn't
    // recognise them natively, so intercept before clap parses.
    if args.iter().skip(1).any(|a| a == "/?" || a == "/h") {
        attach_parent_console();
        let mut cmd = dcmfree::cli::Cli::command();
        let _ = cmd.print_help();
        println!();
        return ExitCode::SUCCESS;
    }

    // No arguments: launch the GUI.
    if args.len() == 1 {
        return match dcmfree::gui::run() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                attach_parent_console();
                eprintln!("dcmfree: {e:#}");
                ExitCode::FAILURE
            }
        };
    }

    // CLI subcommand path: re-attach to the calling shell's console so
    // println!/eprintln! show up where the user ran us from.
    attach_parent_console();
    match dcmfree::cli::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("dcmfree: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn attach_parent_console() {
    use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
    // SAFETY: AttachConsole is safe to call from any process; it returns
    // an error if there is no parent console, which we intentionally ignore.
    unsafe {
        let _ = AttachConsole(ATTACH_PARENT_PROCESS);
    }
}
