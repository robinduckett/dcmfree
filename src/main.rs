// Use the "windows" subsystem in release so launching from Explorer or the
// Start Menu doesn't flash a console window. We re-attach to the parent
// console at runtime when invoked from a shell, so CLI output still works.
// In debug builds we keep the console subsystem for easy logging.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::process::ExitCode;

use clap::CommandFactory;
use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
use windows::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
use windows::core::PCWSTR;

use dcmfree::util::wide_nul;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();

    // `/?` and `/h` are the Windows convention for help; clap doesn't
    // recognise them natively, so intercept before clap parses.
    if args.iter().skip(1).any(|a| a == "/?" || a == "/h") {
        let _ = attach_parent_console();
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
                let msg = format!("{e:#}");
                // Try to surface the error wherever the user will see it:
                // attach to the parent console first (works for shell
                // launches), and fall back to a MessageBox when there is
                // no parent console (Explorer / Start-menu launches).
                if attach_parent_console() {
                    eprintln!("dcmfree: {msg}");
                } else {
                    show_error_box("dcmfree could not start", &msg);
                }
                ExitCode::FAILURE
            }
        };
    }

    // CLI subcommand path: re-attach to the calling shell's console so
    // println!/eprintln! show up where the user ran us from.
    let _ = attach_parent_console();
    match dcmfree::cli::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("dcmfree: {err:#}");
            ExitCode::FAILURE
        }
    }
}

/// Re-attach the process to its parent console (if any) so the runtime's
/// stdio handles are wired up to a terminal. Returns `true` when a console
/// was attached — i.e. when stderr/stdout will actually be visible to the
/// user.
fn attach_parent_console() -> bool {
    // SAFETY: AttachConsole is safe to call from any process state; it
    // returns an error if there is no parent console, which is the case
    // when the binary is launched from Explorer or the Start menu.
    unsafe { AttachConsole(ATTACH_PARENT_PROCESS).is_ok() }
}

/// Show a modal error message box. Used as a fallback when no console is
/// attached (Explorer / Start-menu launch) so the user still sees the
/// failure reason rather than the window vanishing silently.
fn show_error_box(title: &str, body: &str) {
    let title_w = wide_nul(title);
    let body_w = wide_nul(body);
    // SAFETY: both buffers are NUL-terminated UTF-16 and live for the
    // duration of the call. MessageBoxW with a NULL owner is valid.
    unsafe {
        let _ = MessageBoxW(
            None,
            PCWSTR(body_w.as_ptr()),
            PCWSTR(title_w.as_ptr()),
            MB_OK | MB_ICONERROR,
        );
    }
}
