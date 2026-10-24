// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! `ecat-tier`：tiering 管理 CLI。只做参数解析与调用，业务逻辑在库里。
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};
use ecat_data_tiering::{PartitionSpec, TieringConfig, TieringError, plan_range_partitions};
use serde::Deserialize;
use time::{Duration, OffsetDateTime};

#[derive(Debug, Parser)]
#[command(name = "ecat-tier", about = "Hot/cold data tiering management")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

/// 四个子命令共用的参数。
#[derive(Debug, Args)]
struct Common {
    /// Path to the application JSON/YAML config (reads its `tiering:` section).
    #[arg(long)]
    config: PathBuf,
    /// Print actions without executing them.
    #[arg(long)]
    dry_run: bool,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Print RANGE COLUMNS partition DDL for every configured table.
    Plan(Common),
    /// Run one archive pass.
    Run(Common),
    /// Verify archived chunks against the cold store.
    Verify(Common),
    /// Restore an archived partition through a staging table.
    Restore(Common),
}

impl Command {
    fn common(&self) -> &Common {
        match self {
            Self::Plan(common)
            | Self::Run(common)
            | Self::Verify(common)
            | Self::Restore(common) => common,
        }
    }

    fn name(&self) -> &'static str {
        match self {
            Self::Plan(_) => "plan",
            Self::Run(_) => "run",
            Self::Verify(_) => "verify",
            Self::Restore(_) => "restore",
        }
    }
}

fn main() -> ExitCode {
    match dispatch(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("ecat-tier: {e}");
            ExitCode::FAILURE
        }
    }
}

fn dispatch(cli: Cli) -> Result<(), TieringError> {
    let config = load_config(&cli.command.common().config)?;
    config.validate()?;
    match cli.command {
        Command::Plan(common) => print_plan(&config, common.dry_run),
        other => Err(TieringError::Config(format!(
            "`{}` needs a live MySQL connection and cold sink; only `plan` is wired so far",
            other.name()
        ))),
    }
}

fn load_config(path: &Path) -> Result<TieringConfig, TieringError> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| TieringError::Config(format!("read {}: {e}", path.display())))?;
    let value: serde_json::Value = match path.extension().and_then(|ext| ext.to_str()) {
        Some("yaml" | "yml") => yaml_serde::from_str(&text)
            .map_err(|e| TieringError::Config(format!("parse {}: {e}", path.display())))?,
        _ => serde_json::from_str(&text)
            .map_err(|e| TieringError::Config(format!("parse {}: {e}", path.display())))?,
    };
    let tiering = value.get("tiering").ok_or_else(|| {
        TieringError::Config(format!(
            "{}: missing top-level `tiering:` section",
            path.display()
        ))
    })?;
    TieringConfig::deserialize(tiering).map_err(|e| {
        TieringError::Config(format!(
            "{}: invalid `tiering:` section: {e}",
            path.display()
        ))
    })
}

/// 默认覆盖热窗口 `[now - hot_days, now)`；更细的时间范围请直接调库里的 `plan_range_partitions`。
fn print_plan(config: &TieringConfig, dry_run: bool) -> Result<(), TieringError> {
    if dry_run {
        println!("-- dry-run: statements are printed, not executed");
    }
    print!("{}", plan_script(config)?);
    Ok(())
}

/// 生成一整段脚本；`SET time_zone` 必须排在 ALTER 之前，否则 `TIMESTAMP` 列的分区边界
/// 会按执行会话的时区解释，与库里的 UTC 边界不一致。
fn plan_script(config: &TieringConfig) -> Result<String, TieringError> {
    let now = OffsetDateTime::now_utc();
    let from = now
        .checked_sub(Duration::days(i64::from(config.hot_days)))
        .ok_or_else(|| {
            TieringError::Config(format!("hot_days {} is out of range", config.hot_days))
        })?;
    let mut script = String::from("SET time_zone = '+00:00';\n");
    for table in &config.tables {
        let spec = PartitionSpec {
            table: table.name.clone(),
            column: table.time_col.clone(),
            months: config.partition_months,
        };
        script.push_str(&plan_range_partitions(&spec, from, now)?);
        script.push_str(";\n");
    }
    Ok(script)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;
    use ecat_data_tiering::{ColdTarget, TableTieringConfig};

    fn sample_config() -> TieringConfig {
        TieringConfig {
            enabled: true,
            hot_days: 90,
            partition_months: 1,
            tables: vec![TableTieringConfig {
                name: "events".into(),
                time_col: "ts".into(),
                id_col: "id".into(),
                retention_days: 365,
                target: ColdTarget::Clickhouse,
                filter: None,
            }],
            batch_size: 1000,
            max_rows_per_sec: None,
            verify_sample: None,
            retain_s3_days: 7,
            window: None,
            dry_run: false,
        }
    }

    #[test]
    fn cli_plan_pins_utc_session_before_alter() {
        let script = plan_script(&sample_config()).unwrap();
        let set = script
            .find("SET time_zone = '+00:00';")
            .expect("plan 必须固定 UTC 会话时区");
        let alter = script.find("ALTER TABLE `events`").expect("missing ALTER");
        assert!(set < alter, "SET time_zone 必须先于 ALTER：\n{script}");
    }

    #[test]
    fn cli_plan_parses_config_path() {
        let cli = Cli::try_parse_from(["ecat-tier", "plan", "--config", "tiering.yaml"]).unwrap();
        assert_eq!(cli.command.common().config, PathBuf::from("tiering.yaml"));
        assert!(matches!(cli.command, Command::Plan(_)));
        assert!(!cli.command.common().dry_run);
    }

    #[test]
    fn cli_lists_four_subcommands() {
        let command = Cli::command();
        let names: Vec<String> = command
            .get_subcommands()
            .map(|sub| sub.get_name().to_owned())
            .collect();
        assert_eq!(names, ["plan", "run", "verify", "restore"]);
    }
}
