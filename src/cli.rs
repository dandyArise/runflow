use std::path::PathBuf;

use anyhow::{Result, bail};
use clap::{Parser, Subcommand};

use crate::schemas;

#[derive(Debug, Parser)]
#[command(name = "flow")]
#[command(about = "RunFlow workflow runner")]
pub struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    Validate { workflow: PathBuf },
    Version,
}

impl Cli {
    pub async fn run(self) -> Result<()> {
        match self.command.unwrap_or(Command::Version) {
            Command::Validate { workflow } => {
                let diagnostics = schemas::validate_workflow_file(&workflow)?;
                if diagnostics.is_empty() {
                    println!("valid: {}", workflow.display());
                } else {
                    eprintln!("{}", serde_json::to_string_pretty(&diagnostics)?);
                    bail!("invalid workflow: {}", workflow.display());
                }
            }
            Command::Version => {
                println!("runflow {}", env!("CARGO_PKG_VERSION"));
            }
        }

        Ok(())
    }
}
