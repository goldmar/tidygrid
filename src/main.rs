mod backup;
mod device;
mod layout;

use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use serde_json::{json, Value};

#[derive(Parser)]
#[command(
    name = "tidygrid",
    version,
    about = "Back up and rearrange an iPhone Home Screen deterministically",
    long_about = "Read, validate, compare, and safely apply iPhone Home Screen layouts over USB. Every write is backed up and verified."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List connected iPhones.
    Devices,

    /// Save the current Home Screen layout and device grid metrics.
    Snapshot {
        #[arg(long = "device", alias = "serial", value_name = "UDID")]
        device: Option<String>,

        #[arg(short, long, value_name = "FILE")]
        output: Option<PathBuf>,
    },

    /// Validate a layout without connecting to an iPhone.
    Validate {
        #[arg(value_name = "LAYOUT")]
        layout: PathBuf,

        /// Require the same set of apps as this snapshot or layout.
        #[arg(long, value_name = "LAYOUT")]
        inventory: Option<PathBuf>,
    },

    /// Compare two layout files without connecting to an iPhone.
    Diff {
        #[arg(value_name = "BEFORE")]
        before: PathBuf,

        #[arg(value_name = "AFTER")]
        after: PathBuf,
    },

    /// Verify that a plan is valid and its baseline still matches the iPhone.
    Check {
        #[arg(long = "device", alias = "serial", value_name = "UDID")]
        device: Option<String>,

        #[arg(long, value_name = "SNAPSHOT")]
        baseline: PathBuf,

        #[arg(long, value_name = "LAYOUT")]
        plan: PathBuf,
    },

    /// Apply a plan only if its baseline still matches, then read it back.
    Apply {
        #[arg(long = "device", alias = "serial", value_name = "UDID")]
        device: Option<String>,

        #[arg(long, value_name = "SNAPSHOT")]
        baseline: PathBuf,

        #[arg(long, value_name = "LAYOUT")]
        plan: PathBuf,
    },

    /// Restore a saved layout, preserving the current layout as an undo backup.
    Restore {
        #[arg(long = "device", alias = "serial", value_name = "UDID")]
        device: Option<String>,

        #[arg(value_name = "BACKUP")]
        backup: PathBuf,
    },
}

fn load(path: &Path) -> Result<Value, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("could not read {}: {error}", path.display()))?;
    serde_json::from_str(&text)
        .map_err(|error| format!("{} is not valid JSON: {error}", path.display()))
}

fn state_from(value: &Value) -> &Value {
    value.get("state").unwrap_or(value)
}

fn metrics_from(value: &Value) -> Value {
    value.get("metrics").cloned().unwrap_or_else(|| json!({}))
}

fn write_atomic(path: &Path, value: &Value) -> Result<(), String> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("snapshot.json");
    let mut body = serde_json::to_vec_pretty(value)
        .map_err(|error| format!("could not serialize JSON: {error}"))?;
    body.push(b'\n');

    for suffix in 0..1000 {
        let temporary = parent.join(format!(".{name}.{}.{suffix}.tmp", std::process::id()));
        let mut file = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(format!("could not create {}: {error}", temporary.display())),
        };
        if let Err(error) = file.write_all(&body).and_then(|()| file.sync_all()) {
            let _ = std::fs::remove_file(&temporary);
            return Err(format!("could not write {}: {error}", temporary.display()));
        }
        drop(file);
        return std::fs::rename(&temporary, path).map_err(|error| {
            let _ = std::fs::remove_file(&temporary);
            format!("could not publish {}: {error}", path.display())
        });
    }
    Err("could not choose a unique temporary snapshot filename".to_string())
}

fn print_json(value: &Value) -> Result<(), String> {
    println!(
        "{}",
        serde_json::to_string_pretty(value)
            .map_err(|error| format!("could not serialize output: {error}"))?
    );
    Ok(())
}

async fn write_and_verify(
    device_id: &str,
    expected: &Value,
    metrics: &Value,
) -> Result<(), String> {
    let (_, observed) = device::write_icon_state(device_id, expected).await?;
    layout::verify_settled(expected, &observed, metrics)
}

async fn recover(device_id: &str, expected: &Value, metrics: &Value) -> String {
    match device::icon_state(Some(device_id)).await {
        Ok((_, resolved, observed, _))
            if resolved == device_id
                && layout::verify_settled(expected, &observed, metrics).is_ok() =>
        {
            "the phone still matches the saved layout; no recovery write was needed".to_string()
        }
        _ => match write_and_verify(device_id, expected, metrics).await {
            Ok(()) => "the saved layout was restored and verified".to_string(),
            Err(error) => format!("automatic recovery failed: {error}"),
        },
    }
}

async fn run(command: Command) -> Result<Value, String> {
    match command {
        Command::Devices => Ok(json!({
            "ok": true,
            "devices": device::devices().await?,
        })),
        Command::Snapshot { device, output } => {
            let (about, _, state, metrics) = device::icon_state(device.as_deref()).await?;
            layout::validate_live_metrics(&metrics)?;
            let validation = layout::validate(&state, &metrics, Some(&state));
            if !validation.ok {
                return Err(format!(
                    "the live layout failed validation: {}",
                    serde_json::to_string(&validation).unwrap_or_default()
                ));
            }
            let answer = json!({
                "ok": true,
                "device": about,
                "metrics": metrics,
                "state": state,
                "validation": validation,
            });
            if let Some(path) = output {
                write_atomic(&path, &answer)?;
            }
            Ok(answer)
        }
        Command::Validate {
            layout: path,
            inventory,
        } => {
            let document = load(&path)?;
            let inventory_document = inventory.as_deref().map(load).transpose()?;
            let report = layout::validate(
                state_from(&document),
                &metrics_from(&document),
                inventory_document.as_ref().map(state_from),
            );
            Ok(json!({ "ok": report.ok, "validation": report }))
        }
        Command::Diff { before, after } => {
            let before = load(&before)?;
            let after = load(&after)?;
            Ok(json!({
                "ok": true,
                "diff": layout::diff(state_from(&before), state_from(&after))?,
            }))
        }
        Command::Check {
            device,
            baseline,
            plan,
        } => {
            let baseline_document = load(&baseline)?;
            let plan_document = load(&plan)?;
            let baseline_state = state_from(&baseline_document);
            let plan_state = state_from(&plan_document);
            let (_, _, current, metrics) = device::icon_state(device.as_deref()).await?;
            layout::validate_live_metrics(&metrics)?;
            if &current != baseline_state {
                return Err("the iPhone layout changed after the baseline snapshot; take a new snapshot before proceeding".to_string());
            }
            let validation = layout::validate(plan_state, &metrics, Some(&current));
            let changes = layout::diff(&current, plan_state)?;
            Ok(json!({
                "ok": validation.ok,
                "current_matches_baseline": true,
                "validation": validation,
                "diff": changes,
            }))
        }
        Command::Apply {
            device,
            baseline,
            plan,
        } => {
            let baseline_document = load(&baseline)?;
            let plan_document = load(&plan)?;
            let baseline_state = state_from(&baseline_document);
            let plan_state = state_from(&plan_document);
            let resolved = device::resolve_device(device.as_deref()).await?;
            let _lock = backup::lock_device(&resolved)?;
            let (_, observed_device, current, metrics) =
                device::icon_state(Some(&resolved)).await?;
            if observed_device != resolved {
                return Err("the selected iPhone identity changed".to_string());
            }
            layout::validate_live_metrics(&metrics)?;
            if &current != baseline_state {
                return Err(
                    "refusing to write: the iPhone layout changed after the baseline snapshot"
                        .to_string(),
                );
            }
            let validation = layout::validate(plan_state, &metrics, Some(&current));
            if !validation.ok {
                return Err(format!(
                    "refusing to write an invalid plan: {}",
                    serde_json::to_string(&validation).unwrap_or_default()
                ));
            }
            let changes = layout::diff(&current, plan_state)?;
            if changes["empty"].as_bool() == Some(true) {
                return Ok(json!({
                    "ok": true,
                    "status": "no_change",
                    "validation": validation,
                    "diff": changes,
                }));
            }

            let saved = backup::save_before_write(&current, &resolved)?;
            if let Err(write_error) = write_and_verify(&resolved, plan_state, &metrics).await {
                let recovery = recover(&resolved, &current, &metrics).await;
                return Err(format!(
                    "write or verification failed: {write_error}; {recovery}; backup: {}",
                    saved.display()
                ));
            }

            Ok(json!({
                "ok": true,
                "status": "applied_and_verified",
                "backup": saved,
                "validation": validation,
                "diff": changes,
            }))
        }
        Command::Restore {
            device,
            backup: path,
        } => {
            let state = load(&path)?;
            let saved = state_from(&state);
            let resolved = device::resolve_device(device.as_deref()).await?;
            let _lock = backup::lock_device(&resolved)?;
            let (_, observed_device, current, metrics) =
                device::icon_state(Some(&resolved)).await?;
            if observed_device != resolved {
                return Err("the selected iPhone identity changed".to_string());
            }
            layout::validate_live_metrics(&metrics)?;
            let expected = layout::refresh_icon_payloads(saved, &current)?;
            let validation = layout::validate(&expected, &metrics, Some(&current));
            if !validation.ok {
                return Err(format!(
                    "refusing to restore a layout that does not match the phone's current app inventory: {}",
                    serde_json::to_string(&validation).unwrap_or_default()
                ));
            }
            let safety = backup::mark_restore(&current, &resolved)?;
            if let Err(restore_error) = write_and_verify(&resolved, &expected, &metrics).await {
                let recovery = recover(&resolved, &current, &metrics).await;
                return Err(format!(
                    "restore or verification failed: {restore_error}; {recovery}; undo backup: {}",
                    safety.display()
                ));
            }
            Ok(json!({
                "ok": true,
                "status": "restored_and_verified",
                "restored_from": path,
                "undo_backup": safety,
                "validation": validation,
            }))
        }
    }
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    match run(cli.command).await {
        Ok(value) => {
            let success = value.get("ok").and_then(Value::as_bool) != Some(false);
            if let Err(error) = print_json(&value) {
                eprintln!("{}", json!({ "ok": false, "error": error }));
                std::process::exit(1);
            }
            if !success {
                std::process::exit(2);
            }
        }
        Err(error) => {
            eprintln!("{}", json!({ "ok": false, "error": error }));
            std::process::exit(1);
        }
    }
}
