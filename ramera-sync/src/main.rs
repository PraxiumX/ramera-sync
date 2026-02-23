mod b2;
mod config;
mod discovery;
mod error;
mod ffmpeg;
mod http_auth;
mod nvr;
mod providers;
mod service;
mod storage;
mod types;

use std::path::{Path, PathBuf};
use std::process;

use clap::{Parser, Subcommand};
use tracing::info;
use tracing_subscriber::EnvFilter;

use crate::b2::B2Client;
use crate::config::AppConfig;
use crate::error::Result;
use crate::ffmpeg::install_local_ffmpeg;
use crate::service::{
    discover_only, fetch_records_to_local, fetch_video_clips_local, run_local_loop, run_loop,
    sync_once,
};

#[derive(Debug, Parser)]
#[command(
    name = "ramera-sync",
    version,
    about = "NVR discovery and Backblaze B2 sync backend"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    Run {
        #[arg(short, long, default_value = "settings.conf")]
        config: PathBuf,
    },
    RunLocal {
        #[arg(short, long, default_value = "settings.conf")]
        config: PathBuf,
    },
    Discover {
        #[arg(short, long, default_value = "settings.conf")]
        config: PathBuf,
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    SyncOnce {
        #[arg(short, long, default_value = "settings.conf")]
        config: PathBuf,
    },
    VideoRecords {
        #[arg(short, long, default_value = "settings.conf")]
        config: PathBuf,
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    VideoClips {
        #[arg(short, long, default_value = "settings.conf")]
        config: PathBuf,
        #[arg(long, default_value_t = 1)]
        days: u32,
        #[arg(
            long,
            default_value_t = 3,
            help = "Maximum clips to download (0 = no limit)"
        )]
        max_clips: usize,
        #[arg(
            long,
            default_value_t = 10,
            help = "Seconds per saved clip (0 = full record)"
        )]
        clip_seconds: u32,
    },
    InstallFfmpeg {
        #[arg(long, default_value = "ffmpeg")]
        dir: PathBuf,
        #[arg(long, default_value_t = false)]
        force: bool,
    },
    InitConfig {
        #[arg(short, long, default_value = "settings.conf")]
        path: PathBuf,
        #[arg(long, default_value_t = false)]
        force: bool,
    },
    Healthcheck {
        #[arg(short, long, default_value = "settings.conf")]
        config: PathBuf,
        #[arg(long, default_value_t = false)]
        check_b2: bool,
    },
}

#[tokio::main]
async fn main() {
    init_tracing();
    let cli = Cli::parse();
    if let Err(err) = run(cli).await {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}

async fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Commands::Run { config } => {
            let _lock = acquire_writer_lock()?;
            ensure_local_ffmpeg_for_run()?;
            let cfg = load_config_or_fail(&config)?;
            run_loop(&cfg).await?;
        }
        Commands::RunLocal { config } => {
            let _lock = acquire_writer_lock()?;
            ensure_local_ffmpeg_for_run()?;
            let cfg = load_config_or_fail(&config)?;
            run_local_loop(&cfg).await?;
        }
        Commands::Discover { config, json } => {
            let cfg = load_config_or_fail(&config)?;
            let devices = discover_only(&cfg).await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&devices)?);
            } else {
                if devices.is_empty() {
                    println!("No NVR devices found on {}", cfg.scan.cidr);
                } else {
                    println!("Found {} device(s):", devices.len());
                    for device in &devices {
                        println!(
                            "- {} provider={} ports={:?} vendor={} model={} serial={}",
                            device.ip,
                            device.provider,
                            device.open_ports,
                            device.vendor.as_deref().unwrap_or("-"),
                            device.model.as_deref().unwrap_or("-"),
                            device.serial.as_deref().unwrap_or("-")
                        );
                    }
                }
            }
        }
        Commands::SyncOnce { config } => {
            let _lock = acquire_writer_lock()?;
            let cfg = load_config_or_fail(&config)?;
            let outcome = sync_once(&cfg).await?;
            println!(
                "Saved {} devices and {} records to snapshot {} and raw {}. Cloud sync: uploaded day(s)={}, local day(s) deleted={}, cloud file(s) deleted={}",
                outcome.snapshot.device_count,
                outcome.snapshot.record_count,
                outcome.local_file.display(),
                outcome.raw_records_dir.display(),
                outcome.cloud.uploaded_days.len(),
                outcome.cloud.deleted_local_days.len(),
                outcome.cloud.deleted_cloud_files
            );
        }
        Commands::VideoRecords { config, json } => {
            let _lock = acquire_writer_lock()?;
            let cfg = load_config_or_fail(&config)?;
            let outcome = fetch_records_to_local(&cfg).await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&outcome.snapshot)?);
            } else {
                println!(
                    "Fetched {} devices and {} records over network; snapshot {} raw {}",
                    outcome.snapshot.device_count,
                    outcome.snapshot.record_count,
                    outcome.local_file.display(),
                    outcome.raw_records_dir.display()
                );
            }
        }
        Commands::VideoClips {
            config,
            days,
            max_clips,
            clip_seconds,
        } => {
            let _lock = acquire_writer_lock()?;
            let cfg = load_config_or_fail(&config)?;
            let outcome = fetch_video_clips_local(&cfg, days, max_clips, clip_seconds).await?;
            if outcome.saved_clips.is_empty() {
                println!(
                    "Checked {} device(s), no video clips downloaded in requested range.",
                    outcome.device_count
                );
            } else {
                println!(
                    "Checked {} device(s), downloaded {} clip(s):",
                    outcome.device_count,
                    outcome.saved_clips.len()
                );
                for path in outcome.saved_clips {
                    println!("- {}", path.display());
                }
            }
        }
        Commands::InstallFfmpeg { dir, force } => {
            install_local_ffmpeg(&dir, force)?;
            println!("Installed ffmpeg binary under {}", dir.display());
        }
        Commands::InitConfig { path, force } => {
            if path.exists() && !force {
                eprintln!(
                    "Refusing to overwrite existing file {} (use --force to override)",
                    path.display()
                );
                std::process::exit(2);
            }
            AppConfig::write_default(&path)?;
            println!("Wrote default config to {}", path.display());
        }
        Commands::Healthcheck { config, check_b2 } => {
            let cfg = load_config_or_fail(&config)?;
            run_healthcheck(&cfg, check_b2).await?;
        }
    }
    Ok(())
}

fn load_config_or_fail(path: &Path) -> Result<AppConfig> {
    let cfg = AppConfig::load(path)?;
    cfg.validate()?;
    info!("Loaded config from {}", path.display());
    Ok(cfg)
}

fn init_tracing() {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,hyper=warn"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

struct WriterLock {
    path: PathBuf,
}

impl Drop for WriterLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn acquire_writer_lock() -> Result<WriterLock> {
    let lock_path = PathBuf::from(".ramera-sync.lock");
    let pid = process::id();
    let content = format!("{pid}\n");

    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock_path)
    {
        Ok(mut file) => {
            use std::io::Write as _;
            file.write_all(content.as_bytes())?;
            Ok(WriterLock { path: lock_path })
        }
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            if let Some(existing_pid) = read_lock_pid(&lock_path) {
                if process_is_alive(existing_pid) {
                    return Err(crate::error::AppError::Command(format!(
                        "another ramera-sync writer process is running (pid={existing_pid}); lock={}",
                        lock_path.display()
                    )));
                }
            }
            // Stale lock: remove and retry once.
            let _ = std::fs::remove_file(&lock_path);
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&lock_path)?;
            use std::io::Write as _;
            file.write_all(content.as_bytes())?;
            Ok(WriterLock { path: lock_path })
        }
        Err(err) => Err(err.into()),
    }
}

fn read_lock_pid(path: &Path) -> Option<u32> {
    let raw = std::fs::read_to_string(path).ok()?;
    raw.trim().parse::<u32>().ok()
}

fn process_is_alive(pid: u32) -> bool {
    // Linux check.
    Path::new("/proc").join(pid.to_string()).exists()
}

async fn run_healthcheck(cfg: &AppConfig, check_b2: bool) -> Result<()> {
    let ffmpeg_ok = has_ffmpeg();
    let ffprobe_ok = has_ffprobe();
    let sha1_tool_ok = has_sha1_tool();

    println!("Healthcheck:");
    println!("- config: ok");
    println!("- ffmpeg: {}", if ffmpeg_ok { "ok" } else { "missing" });
    println!("- ffprobe: {}", if ffprobe_ok { "ok" } else { "missing" });
    println!(
        "- sha1 tool: {}",
        if sha1_tool_ok {
            "ok (sha1sum/shasum/openssl)"
        } else {
            "missing"
        }
    );

    if !ffmpeg_ok || !ffprobe_ok || !sha1_tool_ok {
        return Err(crate::error::AppError::Command(
            "healthcheck failed: missing runtime dependencies".to_string(),
        ));
    }

    if check_b2 {
        if cfg.b2.key_id.is_empty()
            || cfg.b2.application_key.is_empty()
            || cfg.b2.bucket_id.is_empty()
        {
            return Err(crate::error::AppError::Command(
                "healthcheck failed: b2 credentials missing".to_string(),
            ));
        }
        let b2 = B2Client::new(cfg.b2.clone());
        let prefix = cfg.b2.file_prefix.trim_end_matches('/');
        let files = b2.list_files(prefix).await?;
        println!(
            "- b2(list): ok ({} file(s) visible under prefix)",
            files.len()
        );
    }

    println!("Healthcheck passed.");
    Ok(())
}

fn has_ffmpeg() -> bool {
    if let Ok(path) = std::env::var("FFMPEG_BIN") {
        if !path.trim().is_empty() && Path::new(path.trim()).exists() {
            return true;
        }
    }
    if Path::new("ffmpeg").join("ffmpeg").exists() {
        return true;
    }
    has_binary("ffmpeg")
}

fn has_ffprobe() -> bool {
    if let Ok(path) = std::env::var("FFPROBE_BIN") {
        if !path.trim().is_empty() && Path::new(path.trim()).exists() {
            return true;
        }
    }
    if Path::new("ffmpeg").join("ffprobe").exists() {
        return true;
    }
    has_binary("ffprobe")
}

fn has_sha1_tool() -> bool {
    has_binary("sha1sum") || has_binary("shasum") || has_binary("openssl")
}

fn has_binary(bin: &str) -> bool {
    std::process::Command::new(bin)
        .arg("--help")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok()
}

fn ensure_local_ffmpeg_for_run() -> Result<()> {
    let local_dir = Path::new("ffmpeg");
    let local_ffmpeg = local_dir.join("ffmpeg");
    let local_ffprobe = local_dir.join("ffprobe");
    if local_ffmpeg.exists() && local_ffprobe.exists() {
        return Ok(());
    }

    if has_ffmpeg() && has_ffprobe() {
        eprintln!(
            "Local ffmpeg missing at {}, using available runtime ffmpeg/ffprobe.",
            local_dir.display()
        );
        return Ok(());
    }

    eprintln!(
        "Local ffmpeg not found at {}. Installing with scripts/install_ffmpeg.sh ...",
        local_dir.display()
    );
    install_local_ffmpeg(local_dir, false).map_err(|err| {
        crate::error::AppError::Command(format!(
            "failed to auto-install local ffmpeg for run mode: {err}"
        ))
    })?;

    if !local_ffmpeg.exists() || !local_ffprobe.exists() {
        return Err(crate::error::AppError::Command(format!(
            "ffmpeg installer completed but local binaries are still missing under {}",
            local_dir.display()
        )));
    }

    Ok(())
}
