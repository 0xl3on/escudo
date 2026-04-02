use std::path::PathBuf;
use std::process;

use clap::{Parser, Subcommand};

use escudo::CheckOptions;
use escudo::config::load_config;
use escudo::report::OutputFormat;

#[derive(Parser)]
#[command(
    name = "escudo",
    version,
    author,
    about = "Lightweight Rust supply chain checker"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Path to the project to audit (default: current directory)
    #[arg(value_name = "PATH")]
    path: Option<PathBuf>,

    /// Path to escudo.toml config file
    #[arg(long, value_name = "FILE")]
    config: Option<String>,

    /// Minimum days since publish (overrides config)
    #[arg(long)]
    cooldown_days: Option<u32>,

    /// Allow git dependencies (overrides config)
    #[arg(long)]
    allow_git_deps: bool,

    /// Allow path dependencies (overrides config)
    #[arg(long)]
    allow_path_deps: bool,

    /// Max duplicate versions of the same crate (overrides config)
    #[arg(long)]
    max_duplicate_versions: Option<u32>,

    /// Output as JSON
    #[arg(long)]
    json: bool,

    /// Generate HTML report and open in browser
    #[arg(long)]
    html: bool,

    /// Output GitHub Actions annotations (auto-detected in CI)
    #[arg(long = "format=github")]
    format_github: bool,

    /// Skip freshness checks (no network, policy checks only)
    #[arg(long)]
    skip_freshness: bool,

    /// Disable colored output
    #[arg(long)]
    no_color: bool,
}

#[derive(Subcommand)]
enum Command {
    /// Update escudo to the latest version
    Upgrade,
}

fn main() {
    let cli = Cli::parse();

    if cli.no_color || std::env::var("NO_COLOR").is_ok() {
        colored::control::set_override(false);
    }

    if let Some(Command::Upgrade) = cli.command {
        run_upgrade();
        return;
    }

    run_audit(cli);
}

fn run_upgrade() {
    let current = env!("CARGO_PKG_VERSION");
    eprintln!("current version: {}", current);
    eprintln!("upgrading escudo...");

    let status = std::process::Command::new("cargo")
        .args(["install", "escudo", "--force"])
        .status();

    match status {
        Ok(s) if s.success() => {
            eprintln!("upgrade complete");
        }
        Ok(s) => {
            eprintln!("cargo install failed (exit {})", s.code().unwrap_or(1));
            process::exit(2);
        }
        Err(e) => {
            eprintln!("failed to run cargo: {e}");
            process::exit(2);
        }
    }
}

fn run_audit(cli: Cli) {
    let format = if cli.html {
        OutputFormat::Html
    } else if cli.json {
        OutputFormat::Json
    } else if cli.format_github || is_github_actions() {
        OutputFormat::Github
    } else {
        OutputFormat::Human
    };

    let project_dir = cli.path.as_deref().map(|p| {
        if p.ends_with("Cargo.toml") {
            p.parent().unwrap_or(p)
        } else {
            p
        }
    });

    let manifest_path = cli.path.as_ref().map(|p| {
        let p = if p.ends_with("Cargo.toml") {
            p.clone()
        } else {
            p.join("Cargo.toml")
        };
        p.to_string_lossy().to_string()
    });

    let config_path = cli.config.as_deref();
    let has_explicit_config = config_path.is_some()
        || project_dir.is_some_and(|d| d.join("escudo.toml").exists())
        || std::path::Path::new("escudo.toml").exists();
    let has_cli_overrides = cli.cooldown_days.is_some()
        || cli.allow_git_deps
        || cli.allow_path_deps
        || cli.max_duplicate_versions.is_some();

    let mut config = match load_config(config_path, project_dir) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            process::exit(2);
        }
    };

    if !has_explicit_config && !has_cli_overrides {
        eprintln!("no escudo.toml found and no CLI overrides set");
        eprintln!("running with defaults: cooldown_days=7, git/path deps blocked");
        eprintln!("license and duplicate checks are disabled until configured");
        eprintln!();
        eprintln!("create an escudo.toml or pass flags like --cooldown-days 14");
        eprintln!("see: escudo --help");
    }

    if let Some(days) = cli.cooldown_days {
        config.cooldown_days = days;
    }
    if cli.allow_git_deps {
        config.allow_git_deps = true;
    }
    if cli.allow_path_deps {
        config.allow_path_deps = true;
    }
    if let Some(max) = cli.max_duplicate_versions {
        config.max_duplicate_versions = Some(max);
    }

    let opts = CheckOptions {
        manifest_path: manifest_path.as_deref(),
        skip_freshness: cli.skip_freshness,
    };

    let rt = tokio::runtime::Runtime::new().expect("failed to create tokio runtime");
    let report = match rt.block_on(escudo::check(&config, &opts)) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            process::exit(2);
        }
    };

    match report.print(format) {
        Ok(Some(path)) => {
            eprintln!("report written to {}", path);
            if std::env::var("BROWSER").as_deref() != Ok("none") {
                let _ = open_browser(&path);
            }
        }
        Ok(None) => {}
        Err(e) => {
            eprintln!("error: {e}");
            process::exit(2);
        }
    }

    if !report.is_clean() {
        process::exit(1);
    }
}

fn is_github_actions() -> bool {
    std::env::var("GITHUB_ACTIONS").is_ok()
}

fn open_browser(path: &str) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open").arg(path).spawn()?;
    }
    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open").arg(path).spawn()?;
    }
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("cmd")
            .args(["/C", "start", path])
            .spawn()?;
    }
    Ok(())
}
