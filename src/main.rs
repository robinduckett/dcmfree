use std::process::ExitCode;

fn main() -> ExitCode {
    match dcmfree::cli::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("dcmfree: {err:#}");
            ExitCode::FAILURE
        }
    }
}
