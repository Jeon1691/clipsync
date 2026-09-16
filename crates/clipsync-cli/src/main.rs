mod cli;
mod output;

use clap::Parser;
use tracing_subscriber::EnvFilter;

use clipsync_core::{run_daemon, App, CoreError};
use clipsync_daemon::{install_and_start, restart_service, stop_and_uninstall};
use clipsync_storage::LocalStore;

use crate::cli::{Cli, Commands, ConfigCmd, DaemonCmd, RoomCmd, SyncCmd};
use crate::output::{emit, emit_err};

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    init_tracing(cli.json);
    let code = match run(cli).await {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("error: {e}");
            e.exit_code()
        }
    };
    std::process::exit(code);
}

fn init_tracing(json: bool) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"));
    let builder = tracing_subscriber::fmt().with_env_filter(filter);
    if json {
        let _ = builder.json().try_init();
    } else {
        let _ = builder.compact().try_init();
    }
}

async fn run(cli: Cli) -> Result<(), CoreError> {
    match cli.command {
        Commands::Init { relay_url } => {
            let app = App::open()?;
            let id = app.init(relay_url)?;
            emit(
                cli.json,
                serde_json::json!({
                    "device_id": id.device_id,
                    "fingerprint": id.fingerprint(),
                }),
            );
            if !cli.json {
                println!("initialized device {}", id.device_id);
                println!("fingerprint {}", id.fingerprint());
            }
            Ok(())
        }
        Commands::Room(RoomCmd::Create { ttl, no_auto_sync }) => {
            let app = App::open()?;
            let _ = app.identity()?;
            let dur = ttl
                .map(|s| humantime::parse_duration(&s))
                .transpose()
                .map_err(|e| CoreError::Message(e.to_string()))?;
            let json = cli.json;
            let offer = app
                .room_create(dur, no_auto_sync, json, |offer| {
                    if json {
                        println!(
                            "{}",
                            serde_json::json!({
                                "event": "pairing_code",
                                "pairing_code": offer.pairing_code,
                                "room_id": offer.room_id,
                                "expires_at": offer.expires_at,
                            })
                        );
                    } else {
                        println!("pairing code: {}", offer.pairing_code);
                        println!("room: {}", offer.room_id);
                        println!("expires: {}", offer.expires_at);
                        println!("waiting for the other device to join…");
                    }
                })
                .await?;
            emit(
                json,
                serde_json::json!({
                    "ok": true,
                    "pairing_code": offer.pairing_code,
                    "room_id": offer.room_id,
                }),
            );
            if !json {
                println!("paired");
            }
            Ok(())
        }
        Commands::Room(RoomCmd::Join { code, no_auto_sync }) => {
            let app = App::open()?;
            let room = app.room_join(&code, no_auto_sync).await?;
            emit(
                cli.json,
                serde_json::json!({"room_id": room.room_id, "role": room.role}),
            );
            if !cli.json {
                println!("joined room {}", room.room_id);
            }
            Ok(())
        }
        Commands::Room(RoomCmd::Leave { yes }) => {
            if !yes && !cli.yes && !cli.non_interactive {
                eprintln!("pass --yes to leave the current room");
                return Err(CoreError::Message("confirmation required".into()));
            }
            App::open()?.room_leave()?;
            emit(cli.json, serde_json::json!({"ok": true}));
            Ok(())
        }
        Commands::Room(RoomCmd::List) => {
            let rooms = App::open()?.room_list()?;
            emit(cli.json, serde_json::to_value(&rooms)?);
            if !cli.json {
                for r in rooms {
                    println!("{}  epoch {}  {:?}", r.room_id, r.epoch, r.role);
                }
            }
            Ok(())
        }
        Commands::Push { r#type, file } => {
            let summary = App::open()?.push(r#type.as_deref(), &file, true).await?;
            emit(
                cli.json,
                serde_json::json!({"ok": true, "summary": summary}),
            );
            if !cli.json {
                println!("{summary}");
            }
            Ok(())
        }
        Commands::Pull { copy, wait, output } => {
            let result = App::open()?.pull(copy, wait, output).await?;
            emit(
                cli.json,
                serde_json::json!({"kind": result.kind, "summary": result.summary}),
            );
            if !cli.json {
                if let Some(text) = result.text {
                    print!("{text}");
                    if !text.ends_with('\n') {
                        println!();
                    }
                } else {
                    println!("{}", result.summary);
                }
            }
            Ok(())
        }
        Commands::Watch { direction } => {
            let _ = direction;
            std::env::set_var("CLIPSYNC_NO_DAEMON", "1");
            run_daemon(None).await
        }
        Commands::Sync(SyncCmd::Pause) => {
            App::open()?.pause().await?;
            emit(cli.json, serde_json::json!({"paused": true}));
            Ok(())
        }
        Commands::Sync(SyncCmd::Resume) => {
            App::open()?.resume().await?;
            emit(cli.json, serde_json::json!({"paused": false}));
            Ok(())
        }
        Commands::Daemon(DaemonCmd::Start) => {
            let store = LocalStore::open()?;
            install_and_start(&store.paths)?;
            emit(cli.json, serde_json::json!({"daemon": "started"}));
            Ok(())
        }
        Commands::Daemon(DaemonCmd::Stop) => {
            stop_and_uninstall()?;
            emit(cli.json, serde_json::json!({"daemon": "stopped"}));
            Ok(())
        }
        Commands::Daemon(DaemonCmd::Restart) => {
            let store = LocalStore::open()?;
            restart_service(&store.paths)?;
            emit(cli.json, serde_json::json!({"daemon": "restarted"}));
            Ok(())
        }
        Commands::Daemon(DaemonCmd::Run) => run_daemon(None).await,
        Commands::Status => {
            let v = App::open()?.status().await?;
            emit(cli.json, v.clone());
            if !cli.json {
                println!("{}", serde_json::to_string_pretty(&v)?);
            }
            Ok(())
        }
        Commands::Devices => {
            let devices = App::open()?.devices()?;
            emit(cli.json, serde_json::to_value(&devices)?);
            if !cli.json {
                for d in devices {
                    println!(
                        "{}  {}  {}  {}",
                        d.device_id, d.device_name, d.os, d.fingerprint
                    );
                }
            }
            Ok(())
        }
        Commands::Config(ConfigCmd::Get { key }) => {
            let v = App::open()?.config_get(key.as_deref())?;
            emit(cli.json, v.clone());
            if !cli.json {
                println!("{}", serde_json::to_string_pretty(&v)?);
            }
            Ok(())
        }
        Commands::Config(ConfigCmd::Set { key, value }) => {
            App::open()?.config_set(&key, &value)?;
            emit(
                cli.json,
                serde_json::json!({"ok": true, "key": key, "value": value}),
            );
            Ok(())
        }
        Commands::Doctor => {
            let v = App::open()?.doctor().await?;
            emit(cli.json, v.clone());
            if !cli.json {
                println!("{}", serde_json::to_string_pretty(&v)?);
            }
            Ok(())
        }
        Commands::Logs { follow } => {
            let store = LocalStore::open()?;
            let path = store.paths.log_file();
            if follow {
                let child = std::process::Command::new("tail")
                    .args(["-n", "100", "-f"])
                    .arg(&path)
                    .status()?;
                let _ = child;
            } else if path.exists() {
                print!("{}", std::fs::read_to_string(path)?);
            } else {
                emit_err(cli.json, "no logs yet");
            }
            Ok(())
        }
    }
}
