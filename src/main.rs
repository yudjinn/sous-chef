//! `sous-chef` command line. Each subcommand calls one library entry point, so the owning
//! tasks change their library file, not this one:
//!
//! - `serve` -> `mcp::serve` (T2.5)
//! - `index` -> `mcp::index_command` (T2.5)
//! - `lint`  -> `lint::cli`, arguments in `lint::LintCliArgs` (T4.1)
//! - `hook`  -> `hook::handle` (T7.1)

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Context;
use clap::{Parser, Subcommand};

use sous_chef::config::{CliOverrides, Config};

#[derive(Debug, Parser)]
#[command(
    name = "sous-chef",
    version,
    about = "MCP server for Yocto / BitBake layers"
)]
struct Cli {
    /// Build directory (where conf/bblayers.conf lives).
    #[arg(long, global = true, value_name = "DIR")]
    build_dir: Option<PathBuf>,

    /// Configuration file (default: ./sous-chef.toml if present).
    #[arg(long, global = true, value_name = "FILE")]
    config: Option<PathBuf>,

    /// Extra layer root; repeatable.
    #[arg(long = "layer", global = true, value_name = "DIR")]
    layers: Vec<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Serve MCP over stdio.
    Serve,
    /// Refresh the index and print what changed.
    Index,
    /// Lint layers, recipes or files.
    Lint(sous_chef::lint::LintCliArgs),
    /// Claude Code hook: reads the event JSON on stdin, writes the response on stdout.
    Hook,
}

fn load_config(cli: &Cli) -> anyhow::Result<Config> {
    let overrides = CliOverrides {
        build_dir: cli.build_dir.clone(),
        config: cli.config.clone(),
        layers: cli.layers.clone(),
        work_dir: std::env::current_dir().context("reading the working directory")?,
    };
    Ok(Config::load(&overrides)?)
}

fn run(cli: Cli) -> anyhow::Result<ExitCode> {
    match &cli.command {
        Command::Serve => {
            let rt = tokio::runtime::Runtime::new().context("starting the async runtime")?;
            match load_config(&cli) {
                Ok(cfg) => rt.block_on(sous_chef::mcp::serve(cfg))?,
                // Exiting would leave the MCP client with a dead server and no reason; serve
                // degraded instead, so every tool call and `status` report the error.
                Err(e) => {
                    let cfg = Config {
                        build_dir: cli.build_dir.clone(),
                        layers: cli.layers.clone(),
                        work_dir: std::env::current_dir()
                            .context("reading the working directory")?,
                        ..Config::default()
                    };
                    rt.block_on(sous_chef::mcp::serve_degraded(
                        cfg,
                        format!("loading the configuration: {e:#}"),
                    ))?;
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Index => {
            let cfg = load_config(&cli)?;
            let report = sous_chef::mcp::index_command(&cfg)?;
            println!("{report}");
            Ok(ExitCode::SUCCESS)
        }
        Command::Lint(args) => {
            let cfg = load_config(&cli)?;
            let code = sous_chef::lint::cli(&cfg, args)?;
            Ok(ExitCode::from(u8::try_from(code).unwrap_or(2)))
        }
        Command::Hook => {
            // Hooks fail open: any error means no output and exit status 0. The workspace
            // is found from the event's `cwd`; the global flags apply relative to it.
            // A watchdog exits 0 with no output if the call blocks.
            sous_chef::hook::arm_watchdog();
            let opts = sous_chef::hook::Options {
                build_dir: cli.build_dir.clone(),
                config: cli.config.clone(),
                layers: cli.layers.clone(),
                ..Default::default()
            };
            let mut input = String::new();
            if std::io::stdin().read_to_string(&mut input).is_err() {
                return Ok(ExitCode::SUCCESS);
            }
            if let Some(out) = sous_chef::hook::run(&input, &opts) {
                sous_chef::hook::begin_output();
                let mut stdout = std::io::stdout().lock();
                let _ = stdout.write_all(out.as_bytes());
                let _ = stdout.flush();
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("sous-chef: {e:#}");
            ExitCode::FAILURE
        }
    }
}
