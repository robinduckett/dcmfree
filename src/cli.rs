//! Command-line entry point. Defines the user-facing subcommands and
//! orchestrates the modules.

use crate::DEFAULT_LAYERS_DIR;
use crate::format::{age as fmt_age, bytes as fmt_bytes};
use crate::layers::{self, LayerInfo};
use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

#[derive(Debug, Parser)]
#[command(
    name = "dcmfree",
    version,
    about = "Reclaim disk space by destroying orphan Windows container layers",
    long_about = None,
)]
pub struct Cli {
    /// Verbosity: -v for info, -vv for debug, -vvv for trace.
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    pub verbose: u8,

    /// Disable colorised/styled output.
    #[arg(long, global = true)]
    pub no_color: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// List layers, their sizes, ages, and whether they're orphan.
    List(ListArgs),
    /// Print summary statistics about the layers directory.
    Info(CommonArgs),
    /// Destroy orphan layers (requires Administrator + backup/restore privilege).
    Clean(CleanArgs),
    /// Open the native Windows GUI.
    Gui,
}

#[derive(Debug, Args)]
pub struct CommonArgs {
    /// Override the layers directory (defaults to the HCS path on Windows).
    #[arg(long, value_name = "PATH")]
    pub layers_dir: Option<PathBuf>,

    /// Output as machine-readable JSON instead of a human table.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct ListArgs {
    #[command(flatten)]
    pub common: CommonArgs,
}

#[derive(Debug, Args)]
pub struct CleanArgs {
    #[command(flatten)]
    pub common: CommonArgs,

    /// Show what would be destroyed without actually destroying anything.
    #[arg(long)]
    pub dry_run: bool,

    /// Skip the interactive confirmation prompt.
    #[arg(long, short)]
    pub yes: bool,

    /// Only destroy layers older than this duration (e.g. "1d", "7d", "30m").
    /// Default: 1 day. Pass "0" to disable the age guard.
    #[arg(long, default_value = "1d", value_parser = parse_duration)]
    pub min_age: Duration,
}

fn parse_duration(s: &str) -> Result<Duration, String> {
    if s == "0" {
        return Ok(Duration::ZERO);
    }
    humantime::parse_duration(s).map_err(|e| format!("invalid duration '{s}': {e}"))
}

/// Top-level entry called from `main.rs`.
///
/// Returns `Err` only for unrecoverable failures; expected user-facing
/// errors (e.g. not elevated) print a message and return `Ok(())` with the
/// non-zero exit signalled by the caller's match.
pub fn run() -> Result<()> {
    let cli = Cli::parse();
    init_tracing(cli.verbose);

    match cli.command {
        Command::List(a) => cmd_list(&a),
        Command::Info(a) => cmd_info(&a),
        Command::Clean(a) => cmd_clean(&a),
        Command::Gui => crate::gui::run(),
    }
}

fn init_tracing(verbose: u8) {
    let level = match verbose {
        0 => "warn",
        1 => "info",
        2 => "debug",
        _ => "trace",
    };
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| format!("dcmfree={level}").into()),
        )
        .with_writer(io::stderr)
        .try_init();
}

fn layers_dir(common: &CommonArgs) -> PathBuf {
    common
        .layers_dir
        .clone()
        .unwrap_or_else(|| PathBuf::from(DEFAULT_LAYERS_DIR))
}

fn enumerate_and_classify(common: &CommonArgs) -> Result<Vec<LayerInfo>> {
    let dir = layers_dir(common);
    let mut layers = layers::enumerate(&dir).context("enumerating layers")?;
    let in_use = crate::docker::in_use_layer_dirs();
    layers::classify(&mut layers, &in_use);
    Ok(layers)
}

fn cmd_list(args: &ListArgs) -> Result<()> {
    let layers = enumerate_and_classify(&args.common)?;
    if args.common.json {
        let out = serde_json::to_string_pretty(&layers)?;
        println!("{out}");
        return Ok(());
    }
    print_table(&layers);
    Ok(())
}

fn cmd_info(args: &CommonArgs) -> Result<()> {
    let layers = enumerate_and_classify(args)?;
    let total: u64 = layers.iter().map(|l| l.size_bytes).sum();
    let orphan: u64 = layers
        .iter()
        .filter(|l| l.orphan)
        .map(|l| l.size_bytes)
        .sum();
    let known = layers.len();
    let orphan_count = layers.iter().filter(|l| l.orphan).count();

    if args.json {
        let report = serde_json::json!({
            "layers_dir": layers_dir(args),
            "layer_count": known,
            "orphan_count": orphan_count,
            "total_bytes": total,
            "orphan_bytes": orphan,
        });
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }

    println!("Layers directory: {}", layers_dir(args).display());
    println!("Total layers:     {known}");
    println!("Orphan layers:    {orphan_count}");
    println!("Total size:       {}", fmt_bytes(total));
    println!("Reclaimable:      {}", fmt_bytes(orphan));
    Ok(())
}

fn cmd_clean(args: &CleanArgs) -> Result<()> {
    use crate::errors::DcmFreeError;
    use crate::{hcs, privileges};

    let layers = enumerate_and_classify(&args.common)?;
    let now = SystemTime::now();
    let targets = layers::orphans_older_than(&layers, args.min_age, now);

    if targets.is_empty() {
        println!("No orphan layers older than {:?} to destroy.", args.min_age);
        return Ok(());
    }

    let total: u64 = targets.iter().map(|l| l.size_bytes).sum();
    println!(
        "Will destroy {} layers ({}):",
        targets.len(),
        fmt_bytes(total)
    );
    for l in &targets {
        println!(
            "  {}  {}  age {}",
            fmt_bytes(l.size_bytes),
            l.id,
            fmt_age(l.age(now))
        );
    }

    if args.dry_run {
        println!("\n--dry-run set; not destroying.");
        return Ok(());
    }

    if !privileges::is_elevated() {
        return Err(DcmFreeError::NotElevated.into());
    }

    if !args.yes && !confirm("Proceed?")? {
        println!("Aborted.");
        return Ok(());
    }

    privileges::enable_backup_restore()
        .context("enabling SeBackupPrivilege / SeRestorePrivilege")?;

    let mut ok = 0usize;
    let mut bytes_freed: u64 = 0;
    let mut failed: Vec<(PathBuf, u32)> = Vec::new();
    for l in &targets {
        match hcs::destroy_layer(&l.path) {
            Ok(()) => {
                tracing::info!("destroyed {}", l.id);
                ok += 1;
                bytes_freed = bytes_freed.saturating_add(l.size_bytes);
            }
            Err(DcmFreeError::HcsDestroyFailed { path, hresult }) => {
                tracing::warn!("destroy failed for {}: 0x{hresult:08X}", path.display());
                failed.push((path, hresult));
            }
            Err(other) => return Err(other.into()),
        }
    }

    println!(
        "\nDestroyed {ok}/{} layers, reclaimed approximately {}.",
        targets.len(),
        fmt_bytes(bytes_freed)
    );
    if !failed.is_empty() {
        println!("\nFailures:");
        for (p, hr) in failed {
            println!("  0x{hr:08X}  {}", p.display());
        }
    }
    Ok(())
}

fn print_table(layers: &[LayerInfo]) {
    if layers.is_empty() {
        println!("(no layers found)");
        return;
    }
    let now = SystemTime::now();
    println!("{:>10}  {:>7}  {:>10}  ID", "SIZE", "ORPHAN", "AGE");
    for l in layers {
        println!(
            "{:>10}  {:>7}  {:>10}  {}",
            fmt_bytes(l.size_bytes),
            if l.orphan { "yes" } else { "no" },
            fmt_age(l.age(now)),
            l.id
        );
    }
}

fn confirm(prompt: &str) -> Result<bool> {
    if !io::stdin().is_terminal() {
        return Ok(false);
    }
    print!("{prompt} [y/N] ");
    io::stdout().flush()?;
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    Ok(matches!(line.trim().to_lowercase().as_str(), "y" | "yes"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_parses_list_with_defaults() {
        let cli = Cli::try_parse_from(["dcmfree", "list"]).unwrap();
        match cli.command {
            Command::List(a) => {
                assert!(a.common.layers_dir.is_none());
                assert!(!a.common.json);
            }
            _ => panic!("expected List"),
        }
    }

    #[test]
    fn cli_parses_clean_with_dry_run_and_min_age() {
        let cli =
            Cli::try_parse_from(["dcmfree", "clean", "--dry-run", "--min-age", "7d"]).unwrap();
        match cli.command {
            Command::Clean(a) => {
                assert!(a.dry_run);
                assert_eq!(a.min_age, Duration::from_secs(7 * 86_400));
            }
            _ => panic!("expected Clean"),
        }
    }

    #[test]
    fn cli_parses_clean_with_zero_min_age() {
        let cli = Cli::try_parse_from(["dcmfree", "clean", "--dry-run", "--min-age", "0"]).unwrap();
        match cli.command {
            Command::Clean(a) => assert_eq!(a.min_age, Duration::ZERO),
            _ => panic!("expected Clean"),
        }
    }

    #[test]
    fn cli_rejects_invalid_min_age() {
        let err = Cli::try_parse_from(["dcmfree", "clean", "--min-age", "banana"]).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("banana"), "got: {msg}");
    }

    #[test]
    fn cli_supports_json_flag_on_info() {
        let cli = Cli::try_parse_from(["dcmfree", "info", "--json"]).unwrap();
        match cli.command {
            Command::Info(a) => assert!(a.json),
            _ => panic!("expected Info"),
        }
    }

    #[test]
    fn cli_supports_custom_layers_dir() {
        let cli = Cli::try_parse_from(["dcmfree", "list", "--layers-dir", r"D:\custom"]).unwrap();
        match cli.command {
            Command::List(a) => assert_eq!(
                a.common.layers_dir.as_deref(),
                Some(std::path::Path::new(r"D:\custom"))
            ),
            _ => panic!("expected List"),
        }
    }

    #[test]
    fn cli_help_compiles() {
        // Ensures all subcommand `about`/`long_about` strings are well-formed
        // and clap can render help without panicking.
        let mut cmd = Cli::command();
        let _ = cmd.render_help();
    }

    #[test]
    fn parse_duration_supports_compact_forms() {
        assert_eq!(parse_duration("30s").unwrap(), Duration::from_secs(30));
        assert_eq!(parse_duration("5m").unwrap(), Duration::from_secs(300));
        assert_eq!(parse_duration("2h").unwrap(), Duration::from_secs(7200));
        assert_eq!(parse_duration("1d").unwrap(), Duration::from_secs(86_400));
    }
}
