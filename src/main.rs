mod commands;
use chrono::{DateTime, Utc};
use dirs::config_dir;
use rusqlite::{params, Connection};
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

/*
slink is a self-hosted file sharing utility written in Rust that enables secure file sharing through unique URLs. The program manages files on a web server and creates secure, recipient-specific sharing links.

Core functionality:
- Files are stored with UUIDs in a base directory (e.g., /var/www/UUID/filename)
- Sharing links are created using HMAC-SHA256 of UUID + recipient identifier
- File and share information is tracked in SQLite
- Configuration stored in ~/.config/slink/slink.conf (TOML format)
- Runs on the server side, managing files directly

Command interface:
- add: Copy file to managed directory with UUID
- share: Create recipient-specific sharing link
- unshare: Remove sharing link but retain history
- show: Display file info and share status
- ls: List all managed files
- rm: Remove file and its shares

File structure:
- Original file: BASE_DIR/UUID/filename
- Share links: BASE_DIR/HMAC_HASH -> UUID (relative symlink)

URL format:
- Private: https://domain/UUID/filename
- Shared: https://domain/HMAC_HASH/filename

Security considerations:
- Runs as dedicated user with appropriate permissions
- Web server must follow symlinks
- HMAC secret stored in config
- Share history maintained in SQLite

Database schema:
- files: uuid, filename, date_added
- shares: uuid, recipient, share_hash, date_shared, date_removed, active

Configuration (slink.conf):
- base_url: Web server URL
- base_dir: File storage location
- db_path: SQLite database path
- hmac_secret: Secret for hash generation
- web_user: Owner of files
- web_group: Group for web access

The program is designed to be simple, secure, and maintainable, following Unix philosophy of doing one thing well. It integrates with existing web servers and provides a straightforward CLI for file sharing management.
*/


#[derive(Debug, Serialize, Deserialize)]
struct Config {
    base_url: String,
    base_dir: String,
    db_path: String,
    hmac_secret: String,
    web_user: String,
    web_group: String,
}

#[derive(Debug, StructOpt)]
#[structopt(name = "slink", about = "Secure file sharing utility")]
enum Opt {
    #[structopt(name = "add")]
    Add {
        file: String,
    },
    #[structopt(name = "share")]
    Share {
        recipient: String,
        file: String,
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
}

struct FileShare {
    uuid: String,
    filename: String,
    date_added: DateTime<Utc>,
}

struct ShareInfo {
    recipient: String,
    share_hash: String,
    date_shared: DateTime<Utc>,
    date_removed: Option<DateTime<Utc>>,
    active: bool,
}

impl Config {
    fn load_or_create() -> Result<Self> {
        let config_path = config_dir()
            .ok_or_else(|| anyhow!("Could not determine config directory"))?
            .join("slink")
            .join("slink.conf");

        if !config_path.exists() {
            let config = Config {
                base_url: "http://localhost:8080".to_string(),
                base_dir: "/var/www".to_string(),
                db_path: dirs::data_dir()
                    .ok_or_else(|| anyhow!("Could not determine data directory"))?
                    .join("slink")
                    .join("shares.db")
                    .to_string_lossy()
                    .to_string(),
                hmac_secret: Uuid::new_v4().to_string(),
                web_user: "www-data".to_string(),
                web_group: "www-data".to_string(),
            };

            // Check if base directory exists
            if !Path::new(&config.base_dir).exists() {
                return Err(anyhow!("Base directory {} does not exist. Please create it with appropriate permissions", config.base_dir));
            }

            // Try to create config directory
            create_dir_all(config_path.parent().unwrap())
                .map_err(|e| anyhow!("Failed to create config directory: {}", e))?;

            // Try to write config file
            fs::write(&config_path, toml::to_string(&config)?)
                .map_err(|e| anyhow!("Failed to write config file {}: {}", config_path.display(), e))?;

            // Try to create database directory
            create_dir_all(Path::new(&config.db_path).parent().unwrap())
                .map_err(|e| anyhow!("Failed to create database directory: {}", e))?;

            // Try to initialize database
            init_database(&config.db_path)
                .map_err(|e| anyhow!("Failed to initialize database {}: {}", config.db_path, e))?;

            Ok(config)
        } else {
            let content = fs::read_to_string(&config_path)
                .map_err(|e| anyhow!("Failed to read config file {}: {}", config_path.display(), e))?;
            Ok(toml::from_str(&content)?)
        }
    }
}

fn init_database(db_path: &str) -> Result<()> {
    let conn = Connection::open(db_path)?;

    conn.execute(
        "CREATE TABLE IF NOT EXISTS files (
            uuid TEXT PRIMARY KEY,
            filename TEXT NOT NULL,
            date_added DATETIME NOT NULL
        )",
        [],
    )?;

    conn.execute(
        "CREATE TABLE IF NOT EXISTS shares (
            uuid TEXT NOT NULL,
            recipient TEXT NOT NULL,
            share_hash TEXT NOT NULL,
            date_shared DATETIME NOT NULL,
            date_removed DATETIME,
            active BOOLEAN NOT NULL DEFAULT 1,
            PRIMARY KEY (uuid, recipient),
            FOREIGN KEY (uuid) REFERENCES files(uuid)
        )",
        [],
    )?;

    Ok(())
}

fn calculate_share_hash(uuid: &str, recipient: &str, secret: &str) -> Result<String> {
    // Create a 32-byte key from the secret using BLAKE3 itself
    let mut key = [0u8; 32];
    let key_hash = blake3::hash(secret.as_bytes());
    key.copy_from_slice(key_hash.as_bytes());

    let keyed_hash = blake3::keyed_hash(
        &key,
        format!("{}{}", uuid, recipient).as_bytes(),
    );

    Ok(b64.encode(keyed_hash.as_bytes()))
}

fn set_permissions_recursive(
    path: &Path,
    dir_mode: u32,
    file_mode: u32,
    web_user: &str,
    web_group: &str,
) -> Result<()> {
    // Resolve the user and group IDs
    let uid = users::get_user_by_name(web_user)
        .ok_or_else(|| anyhow::anyhow!("User {} not found", web_user))?
        .uid();
    let gid = users::get_group_by_name(web_group)
        .ok_or_else(|| anyhow::anyhow!("Group {} not found", web_group))?
        .gid();

    if path.is_dir() {
        // Set directory permissions and ownership
        set_permissions(path, Permissions::from_mode(dir_mode))?;
        chown(path, Some(Uid::from_raw(uid)), Some(Gid::from_raw(gid)))?;

        for entry in fs::read_dir(path)? {
            let entry = entry?;
            set_permissions_recursive(&entry.path(), dir_mode, file_mode, web_user, web_group)?;
        }
    } else {
        // Set file permissions and ownership
        set_permissions(path, Permissions::from_mode(file_mode))?;
        chown(path, Some(Uid::from_raw(uid)), Some(Gid::from_raw(gid)))?;
    }
    Ok(())
}

impl FileShare {
    fn add(conn: &Connection, config: &Config, file_path: &str) -> Result<String> {
        let path = PathBuf::from(file_path);
        let filename = path.file_name()
            .ok_or_else(|| anyhow!("Invalid filename"))?
            .to_string_lossy()
            .to_string();

        let uuid = Uuid::new_v4().to_string();
        let target_dir = PathBuf::from(&config.base_dir).join(&uuid);
        let target_file = target_dir.join(&filename);

        create_dir_all(&target_dir)?;
        fs::copy(&path, &target_file)?;

        set_permissions_recursive(
            &target_dir,
            0o750,
            0o640,
            &config.web_user,
            &config.web_group,
        )?;

        conn.execute(
            "INSERT INTO files (uuid, filename, date_added) VALUES (?1, ?2, ?3)",
            params![uuid, filename, Utc::now()],
        )?;

        Ok(uuid)
    }

    fn find_by_name(conn: &Connection, filename: &str) -> Result<Vec<(String, DateTime<Utc>)>> {
        let mut stmt = conn.prepare(
            "SELECT uuid, date_added FROM files WHERE filename = ? ORDER BY date_added"
        )?;

        let results = stmt.query_map([filename], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?;

        results.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    fn find_by_uuid(conn: &Connection, uuid: &str) -> Result<Option<FileShare>> {
        let mut stmt = conn.prepare(
            "SELECT filename, date_added FROM files WHERE uuid = ?"
        )?;

        let mut rows = stmt.query([uuid])?;

        if let Some(row) = rows.next()? {
            Ok(Some(FileShare {
                uuid: uuid.to_string(),
                filename: row.get(0)?,
                date_added: row.get(1)?,
            }))
        } else {
            Ok(None)
        }
    }

    fn remove(&self, conn: &Connection, config: &Config, force: bool) -> Result<()> {
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
                    fs::remove_file(entry.path())?;
                }
            }
        }

        // Remove the file directory
        let file_dir = PathBuf::from(&config.base_dir).join(&self.uuid);
        remove_dir_all(file_dir)?;

        // Update database
        conn.execute(
            "UPDATE shares SET active = 0, date_removed = ? WHERE uuid = ?",
            params![Utc::now(), self.uuid],
        )?;

        conn.execute("DELETE FROM files WHERE uuid = ?", [&self.uuid])?;

        Ok(())
    }
}

impl ShareInfo {
    fn share(conn: &Connection, config: &Config, uuid: &str, recipient: &str) -> Result<String> {
        let share_hash = calculate_share_hash(uuid, recipient, &config.hmac_secret)?;

        // Create symlink with relative path
        let source = PathBuf::from(&config.base_dir).join(&share_hash);
        // Remove existing symlink if it exists
        if source.exists() {
            fs::remove_file(&source)?;
        }
        unix_symlink(uuid, source)?;

        // Use REPLACE INTO or INSERT OR REPLACE to handle existing shares
        conn.execute(
            "INSERT OR REPLACE INTO shares (uuid, recipient, share_hash, date_shared, active)
             VALUES (?1, ?2, ?3, ?4, 1)",
            params![uuid, recipient, share_hash, Utc::now()],
        )?;

        Ok(share_hash)
    }


    fn unshare(conn: &Connection, config: &Config, uuid: &str, recipient: &str) -> Result<()> {
        let share_hash = calculate_share_hash(uuid, recipient, &config.hmac_secret)?;

        // Remove symlink
        let symlink = PathBuf::from(&config.base_dir).join(&share_hash);
        if symlink.exists() {
            fs::remove_file(symlink)?;
        }

        conn.execute(
            "UPDATE shares SET active = 0, date_removed = ? 
             WHERE uuid = ? AND recipient = ? AND active = 1",
            params![Utc::now(), uuid, recipient],
        )?;

        Ok(())
    }

    fn get_shares(conn: &Connection, uuid: &str) -> Result<Vec<ShareInfo>> {
        let mut stmt = conn.prepare(
            "SELECT recipient, share_hash, date_shared, date_removed, active 
             FROM shares WHERE uuid = ?"
        )?;

        let shares = stmt.query_map([uuid], |row| {
            Ok(ShareInfo {
                recipient: row.get(0)?,
                share_hash: row.get(1)?,
                date_shared: row.get(2)?,
                date_removed: row.get(3)?,
                active: row.get(4)?,
            })
        })?;

        shares.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }
}

fn main() -> Result<()> {
    let opt = Opt::from_args();
    let config = Config::load_or_create()?;

    match opt {
        Opt::Add { file } => {
            commands::add_file(&config, &file)?;
        }
        Opt::Share { recipient, file } => {
            commands::share_file(&config, &recipient, &file)?;
        }
        Opt::Unshare { recipient, file } => {
            commands::unshare_file(&config, &recipient, &file)?;
        }
        Opt::Show { file } => {
            commands::show_file(&config, &file)?;
        }
        Opt::List => {
            commands::list_files(&config)?;
        }
        Opt::Remove { file, force } => {
            commands::remove_file(&config, &file, force)?;
        }
    }

    Ok(())
}

