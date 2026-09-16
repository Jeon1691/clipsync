use std::path::PathBuf;

use clap::{Parser, Subcommand};

/// ClipSync — end-to-end encrypted clipboard sync
#[derive(Debug, Parser)]
#[command(name = "clipsync", version, about, propagate_version = true)]
pub struct Cli {
    /// Emit machine-readable JSON
    #[arg(long, global = true)]
    pub json: bool,
    /// Confirm destructive actions
    #[arg(long, global = true)]
    pub yes: bool,
    /// Never prompt
    #[arg(long, global = true)]
    pub non_interactive: bool,
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Create local device identity and config
    Init {
        #[arg(long)]
        relay_url: Option<String>,
    },
    /// Manage pairing rooms
    #[command(subcommand)]
    Room(RoomCmd),
    /// Send stdin, clipboard, or files
    Push {
        #[arg(long, value_parser = ["auto", "text", "image", "files"])]
        r#type: Option<String>,
        #[arg(long)]
        file: Vec<PathBuf>,
    },
    /// Receive the latest item
    Pull {
        #[arg(long)]
        copy: bool,
        #[arg(long)]
        wait: bool,
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Foreground bidirectional sync
    Watch {
        #[arg(long, default_value = "both")]
        direction: String,
    },
    #[command(subcommand)]
    Sync(SyncCmd),
    #[command(subcommand)]
    Daemon(DaemonCmd),
    /// Connection, room, and daemon status
    Status,
    /// Paired devices
    Devices,
    #[command(subcommand)]
    Config(ConfigCmd),
    /// Inspect adapters, relay, and crypto
    Doctor,
    /// Show daemon logs
    Logs {
        #[arg(long)]
        follow: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum RoomCmd {
    Create {
        #[arg(long)]
        ttl: Option<String>,
        #[arg(long)]
        no_auto_sync: bool,
    },
    Join {
        code: String,
        #[arg(long)]
        no_auto_sync: bool,
    },
    Leave {
        #[arg(long)]
        yes: bool,
    },
    List,
}

#[derive(Debug, Subcommand)]
pub enum SyncCmd {
    Pause,
    Resume,
}

#[derive(Debug, Subcommand)]
pub enum DaemonCmd {
    Start,
    Stop,
    Restart,
    /// Internal: run the daemon in the foreground
    Run,
}

#[derive(Debug, Subcommand)]
pub enum ConfigCmd {
    Get { key: Option<String> },
    Set { key: String, value: String },
}
