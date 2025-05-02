// src/main.rs
mod commands;
use chrono::{DateTime, Duration, Utc, NaiveDateTime}; // Added NaiveDateTime
use dirs::config_dir;
use humantime::parse_duration;
// Remove rusqlite import: use rusqlite::{params, Connection};
use sqlx::{sqlite::{SqlitePoolOptions, SqliteConnectOptions}, FromRow, SqlitePool}; // Added SqliteConnectOptions
use base64::{engine::general_purpose::URL_SAFE_NO_PAD as b64, Engine as _};
use serde::{Deserialize, Serialize};
use std::os::unix::fs::PermissionsExt;
use std::{
    fs::{self, create_dir_all, remove_dir_all, set_permissions, Permissions},
    os::unix::fs::symlink as unix_symlink,
    path::{Path, PathBuf},
};
use nix::unistd::chown;
use nix::unistd::{Gid, Uid};

use structopt::StructOpt;
use uuid::Uuid;

use anyhow::{anyhow, Result};
use std::io::{self, Write};

/* ... (comments remain the same) ... */

#[derive(Debug, Serialize, Deserialize)]
struct Config {
    base_url: String,
    base_dir: String,
    db_path: String,
    hash_secret: String,
    web_user: String,
    web_group: String,
    hash_bytes: usize,
}

#[derive(Debug, StructOpt)]
#[structopt(name = "slink", about = "Secure file sharing utility")]
enum Opt {
    #[structopt(name = "init")]
    Init,
    #[structopt(name = "add")]
    Add {
        file: String,
        #[structopt(short = "n", long = "name")]
        name: Option<String>,
        #[structopt(short = "s", long = "share")]
        share: Option<String>, // Recipient email

        #[structopt(
            long,
            short = "e",
            help = "Set expiry for the initial share (requires -s/--share)",
            requires = "share" // Only relevant if -s is used
        )]
        expires_in: Option<String>,

        #[structopt(
            long,
            short = "d",
            help = "Delete file when the initial share expires (requires -s/--share and --expires-in)",
            requires = "share" // Only relevant if -s is used
        )]
        delete_file_on_expiry: bool,
    },
    #[structopt(name = "share")]
    Share {
        recipient: String,
        file: String,

        #[structopt(
            long,
            short = "e",
            help = "Set an expiry duration for the share (e.g., '5m', '1h', '2d')"
        )]
        expires_in: Option<String>,

        #[structopt(
            long,
            short = "d",
            help = "Delete the file when this share expires (requires --expires-in)"
        )]
        delete_file_on_expiry: bool,
    },
    #[structopt(name = "unshare")]
    Unshare {
        recipient: String,
        file: String,
    },
    #[structopt(name = "show")]
    Show {
        file: String,
    },
    #[structopt(name = "ls")]
    List,
    #[structopt(name = "rm")]
    Remove {
        file: String,
        #[structopt(short = "f", long = "force")]
        force: bool,
    },
    #[structopt(name = "info")]
    Info,
    #[structopt(name = "cleanup", about = "Clean up expired shares and optionally files")]
    Cleanup {
        #[structopt(short = "q", long = "quiet", help = "Suppress output except when removing files")]
        quiet: bool,
    },
}

#[derive(FromRow, Debug)] // Added Debug
struct FileShare {
    uuid: String,
    filename: String,
    date_added: NaiveDateTime, // Changed to NaiveDateTime for sqlx compatibility
}

#[derive(FromRow, Debug)] // Added Debug
struct ShareInfo {
    recipient: String,
    share_hash: String,
    date_shared: NaiveDateTime, // Changed to NaiveDateTime
    date_removed: Option<NaiveDateTime>, // Changed to Option<NaiveDateTime>
    active: bool,
    expires_at: Option<NaiveDateTime>, // Changed to Option<NaiveDateTime>
    delete_file_on_expiry: bool,
}

impl Config {
     fn check_permissions(config_path: &Path) -> Result<()> {
        let metadata = fs::metadata(config_path)?;
        let mode = metadata.permissions().mode();

        // Check if file is readable by group or others
        if mode & 0o077 != 0 {
            return Err(anyhow!("Config file permissions too loose. Use chmod 600 {}",
                config_path.display()));
        }
        Ok(())
    }

    // Made async, returns pool instead of config directly for now
    async fn load_config_and_pool() -> Result<(Self, SqlitePool)> {
        let config_path = config_dir()
            .ok_or_else(|| anyhow!("Could not determine config directory"))?
            .join("slink")
            .join("slink.conf");

        if !config_path.exists() {
            return Err(anyhow!(
                "Configuration file not found. Please run `slink init` to create one."
            ));
        }

        // Check config file permissions
        if let Err(e) = Config::check_permissions(&config_path) {
            eprintln!("WARNING: {}", e);
        }

        let content = fs::read_to_string(&config_path)
            .map_err(|e| anyhow!("Failed to read config file {}: {}", config_path.display(), e))?;
        let config: Config = toml::from_str(&content)?;

        // Ensure the directory for the database exists
        if let Some(parent) = Path::new(&config.db_path).parent() {
             if !parent.exists() {
                 fs::create_dir_all(parent)?;
             }
        }

        // Configure connection options to create the DB if missing
        let connect_options = SqliteConnectOptions::new()
            .filename(Path::new(&config.db_path))
            .create_if_missing(true); // Add this line

        // Establish connection pool using sqlx with the configured options
        let pool = SqlitePoolOptions::new()
            .max_connections(5) // Example: configure pool size
            .connect_with(connect_options) // Use connect_with and the options
            .await?;
            // .map_err(|e| anyhow!("Failed to connect to database {}: {}", config.db_path, e))?;

        // Removed init_database call - migrations handle this

        Ok((config, pool))
    }
}

// Removed init_database function - migrations handle schema creation

fn calculate_share_hash(uuid: &str, recipient: &str, secret: &str, hash_bytes: usize) -> Result<String> {
    let key = blake3::derive_key("slink", secret.as_bytes());
    let keyed_hash = blake3::keyed_hash(
        &key,
        format!("{}{}", uuid, recipient).as_bytes(),
    );

    Ok(b64.encode(&keyed_hash.as_bytes()[..hash_bytes]))
}

fn set_permissions_recursive(
    path: &Path,
    dir_mode: u32,
    file_mode: u32,
    web_user: &str,
    web_group: &str,
) -> Result<()> {
    // Resolve the user and group IDs
    let web_uid = users::get_user_by_name(web_user)
        .ok_or_else(|| anyhow::anyhow!("User {} not found", web_user))?
        .uid();
    let web_gid = users::get_group_by_name(web_group)
        .ok_or_else(|| anyhow::anyhow!("Group {} not found", web_group))?
        .gid();

    // Get current user's UID and primary GID
    let current_uid = nix::unistd::getuid();
    let current_gid = nix::unistd::getgid();

    if path.is_dir() {
        // Set directory permissions to allow owner access first
        set_permissions(path, Permissions::from_mode(0o700))?;

        // Change ownership to current user temporarily
        chown(path, Some(current_uid), Some(current_gid))?;

        for entry in fs::read_dir(path)? {
            let entry = entry?;
            set_permissions_recursive(&entry.path(), dir_mode, file_mode, web_user, web_group)?;
        }

        // Now set final permissions and ownership
        set_permissions(path, Permissions::from_mode(dir_mode))?;
        chown(path, Some(Uid::from_raw(web_uid)), Some(Gid::from_raw(web_gid)))?;
    } else {
        // For files, temporarily make them fully accessible to owner
        set_permissions(path, Permissions::from_mode(0o600))?;
        chown(path, Some(current_uid), Some(current_gid))?;

        // Set final permissions and ownership
        set_permissions(path, Permissions::from_mode(file_mode))?;
        chown(path, Some(Uid::from_raw(web_uid)), Some(Gid::from_raw(web_gid)))?;
    }
    Ok(())
}

pub fn remove_file_with_access(path: &Path) -> Result<()> { // Added pub
    // Get current user's UID and GID
    let current_uid = nix::unistd::getuid();
    let current_gid = nix::unistd::getgid();

    // Temporarily take ownership and full permissions
    chown(path, Some(current_uid), Some(current_gid))?;
    set_permissions(path, Permissions::from_mode(0o700))?;

    if path.is_dir() {
        remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }.map_err(Into::into)
}


impl FileShare {
    // Made async, takes pool
    // Adjusted return type and map for NaiveDateTime
    async fn find_by_name(pool: &SqlitePool, filename: &str) -> Result<Vec<(String, NaiveDateTime)>> {
        let results = sqlx::query!(
            "SELECT uuid, date_added FROM files WHERE filename = ? ORDER BY date_added",
            filename
        )
        .fetch_all(pool)
        .await?
        .into_iter()
        // date_added is already NaiveDateTime from sqlx
        .map(|row| (row.uuid, row.date_added))
        .collect();

        Ok(results)
    }

    // Made async, takes pool
    async fn find_by_uuid(pool: &SqlitePool, uuid: &str) -> Result<Option<FileShare>> {
         let result = sqlx::query_as!(
            FileShare, // Use query_as! with the struct
            "SELECT uuid, filename, date_added FROM files WHERE uuid = ?",
            uuid
        )
        .fetch_optional(pool) // Use fetch_optional for Option result
        .await?;

        Ok(result)
    }

    // Made async, takes pool
    async fn remove(&self, pool: &SqlitePool, config: &Config, force: bool) -> Result<()> {
        if !force {
            print!("Are you sure you want to remove {}? [y/N] ", self.filename);
            io::stdout().flush()?;
            let mut input = String::new();
            io::stdin().read_line(&mut input)?;
            if !input.trim().eq_ignore_ascii_case("y") {
                return Ok(());
            }
        }

        // Remove all symlinks
        let shares_dir = PathBuf::from(&config.base_dir);
        for entry in fs::read_dir(&shares_dir)? {
            let entry = entry?;
            if let Ok(target) = fs::read_link(entry.path()) {
                if target.ends_with(&self.uuid) {
                    remove_file_with_access(&entry.path())?;
                }
            }
        }

        // Remove the file directory
        let file_dir = PathBuf::from(&config.base_dir).join(&self.uuid);
        remove_file_with_access(&file_dir)?;

        // Update database using sqlx
        // Fix Utc::now() borrowing
        let now = Utc::now().naive_utc(); // Use NaiveDateTime for DB
        sqlx::query!(
            "UPDATE shares SET active = 0, date_removed = ? WHERE uuid = ?",
            now, self.uuid
        )
        .execute(pool)
        .await?;

        // First, delete all associated shares for this file
        sqlx::query!("DELETE FROM shares WHERE uuid = ?", self.uuid)
            .execute(pool)
            .await?;

        // Then, delete the file record
        sqlx::query!("DELETE FROM files WHERE uuid = ?", self.uuid)
            .execute(pool)
            .await?;

        Ok(())
    }

}

impl ShareInfo {
    // Made async, takes pool
    async fn share(
        pool: &SqlitePool, // Changed conn to pool
        config: &Config,
        uuid: &str,
        recipient: &str,
        expires_at: Option<NaiveDateTime>, // Changed to NaiveDateTime
        delete_file_on_expiry: bool,
    ) -> Result<String> {
        let share_hash = calculate_share_hash(uuid, recipient, &config.hash_secret, config.hash_bytes)?;

        // Create symlink with relative path
        let source = PathBuf::from(&config.base_dir).join(&share_hash);
        // Remove existing symlink if it exists
        if source.exists() {
            fs::remove_file(&source)?;
        }
        unix_symlink(uuid, source)?;

        // Use sqlx query for INSERT OR REPLACE
        let now_naive = Utc::now().naive_utc(); // Use NaiveDateTime for DB
        sqlx::query!(
            "INSERT OR REPLACE INTO shares (uuid, recipient, share_hash, date_shared, active, expires_at, delete_file_on_expiry)
             VALUES (?1, ?2, ?3, ?4, 1, ?5, ?6)",
            uuid,
            recipient,
            share_hash,
            now_naive, // Use NaiveDateTime
            expires_at, // Already Option<NaiveDateTime>
            delete_file_on_expiry,
        )
        .execute(pool) // Use pool
        .await?; // Use await

        Ok(share_hash)
    }

    // Made async, takes pool
    async fn unshare(pool: &SqlitePool, config: &Config, uuid: &str, recipient: &str) -> Result<()> {
        let share_hash = calculate_share_hash(uuid, recipient, &config.hash_secret, config.hash_bytes)?;

        // Remove symlink
        let symlink = PathBuf::from(&config.base_dir).join(&share_hash);
        if symlink.exists() {
            fs::remove_file(symlink)?;
        }

        // Use sqlx query for UPDATE
        // Fix Utc::now() borrowing
        let now = Utc::now().naive_utc(); // Use NaiveDateTime for DB
        sqlx::query!(
            "UPDATE shares SET active = 0, date_removed = ?
             WHERE uuid = ? AND recipient = ? AND active = 1",
            now, uuid, recipient
        )
        .execute(pool) // Use pool
        .await?; // Use await

        Ok(())
    }

    // Made async, takes pool
    async fn get_shares(pool: &SqlitePool, uuid: &str) -> Result<Vec<ShareInfo>> {
        // Use query_as! to map directly to ShareInfo struct
        let shares = sqlx::query_as!(
            ShareInfo,
            "SELECT recipient, share_hash, date_shared, date_removed, active, expires_at, delete_file_on_expiry
             FROM shares WHERE uuid = ?",
            uuid
        )
        .fetch_all(pool) // Use fetch_all
        .await?; // Use await

        Ok(shares)
    }
}

#[tokio::main] // Added tokio runtime
async fn main() -> Result<()> { // Added async back
    let opt = Opt::from_args();

    // Handle `slink init` command separately (remains synchronous)
    if let Opt::Init = opt {
        commands::initialize_config()?;
        // Consider if init should also ensure db dir exists
        println!("Configuration initialized. Please review and edit ~/.config/slink/slink.conf");
        return Ok(());
    }

    // For all other commands, load the configuration and establish DB pool
    let (config, pool) = Config::load_config_and_pool().await?; // Use new async function

    sqlx::migrate!("./migrations") // Point to the migrations directory
        .run(&pool)
        .await
        .map_err(|e| anyhow!("Database migration failed: {}", e))?;

    // Match and execute other commands (now async)
    match opt {
        Opt::Add { file, name, share, expires_in, delete_file_on_expiry } => {
            // Pass pool and await
            let uuid = commands::add_file(&pool, &config, &file, name).await?;

            if let Some(recipient) = share {
                if delete_file_on_expiry && expires_in.is_none() {
                     return Err(anyhow!("--delete-file-on-expiry requires --expires-in when using --share (-s)"));
                }
                // Calculate expires_at (DateTime<Utc>)
                let expires_at_utc: Option<DateTime<Utc>> = if let Some(duration_str) = expires_in {
                    let std_duration = parse_duration(&duration_str)
                        .map_err(|e| anyhow!("Invalid duration format '{}': {}", duration_str, e))?;
                    let chrono_duration = Duration::from_std(std_duration)
                        .map_err(|e| anyhow!("Duration conversion error: {}", e))?;
                    Some(Utc::now() + chrono_duration) // Keep as Utc here for calculation
                } else {
                    None
                };

                // Convert expires_at_utc to Option<NaiveDateTime> for ShareInfo::share
                let expires_at_naive: Option<NaiveDateTime> = expires_at_utc.map(|dt| dt.naive_utc());

                // Use the pool for sharing, passing the NaiveDateTime
                // ShareInfo::share expects Option<NaiveDateTime> now
                let share_hash = ShareInfo::share(&pool, &config, &uuid, &recipient, expires_at_naive, delete_file_on_expiry).await?;
                let share_url = format!("{}/{}/{}", config.base_url, share_hash, PathBuf::from(&file).file_name().unwrap().to_str().unwrap());
                println!("Share created for {}: {}", recipient, share_url);
            }
        }
        Opt::Share { recipient, file, expires_in, delete_file_on_expiry } => {
            if delete_file_on_expiry && expires_in.is_none() {
                 return Err(anyhow!("--delete-file-on-expiry requires --expires-in"));
            }
            // Calculate expires_at (DateTime<Utc>)
            let expires_at_utc: Option<DateTime<Utc>> = if let Some(duration_str) = expires_in {
                let std_duration = parse_duration(&duration_str)
                    .map_err(|e| anyhow!("Invalid duration format '{}': {}", duration_str, e))?;
                let chrono_duration = Duration::from_std(std_duration)
                    .map_err(|e| anyhow!("Duration conversion error: {}", e))?;
                Some(Utc::now() + chrono_duration) // Keep as Utc here for calculation
            } else {
                None
            };

            // Call async command function, passing the calculated expires_at_utc
            // commands::share_file expects Option<DateTime<Utc>>
            commands::share_file(&pool, &config, &recipient, &file, expires_at_utc, delete_file_on_expiry).await?;
        }
        Opt::Unshare { recipient, file } => {
            // Call async command function
            commands::unshare_file(&pool, &config, &recipient, &file).await?;
        }
        Opt::Show { file } => {
            // Call async command function
            commands::show_file(&pool, &config, &file).await?;
        }
        Opt::List => {
            // Call async command function
            commands::list_files(&pool, &config).await?;
        }
        Opt::Remove { file, force } => {
            // Call async command function
            commands::remove_file(&pool, &config, &file, force).await?;
        }
        Opt::Info => {
             // Call async command function
             commands::show_info(&pool, &config).await?;
        }
        Opt::Cleanup { quiet } => {
            // Call async command function
            commands::cleanup_expired(&pool, &config, quiet).await?;
        }
        Opt::Init => unreachable!(), // Already handled
    }

    Ok(())
}
