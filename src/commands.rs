// src/commands.rs
use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use prettytable::{Table, row};
use rusqlite::Connection;
use std::path::Path;
use dirs::config_dir;
use std::fs;
use crate::{init_database, create_dir_all};
use crate::{Config, FileShare, ShareInfo};
use crate::Uuid;
use crate::{Permissions, PermissionsExt, set_permissions};
use std::io::{self, Write};

pub fn initialize_config() -> Result<()> {
    let config_dir = config_dir()
        .ok_or_else(|| anyhow!("Could not determine config directory"))?
        .join("slink");
    let config_path = config_dir.join("slink.conf");

    if config_path.exists() {
        return Err(anyhow!("Configuration file already exists at {}", config_path.display()));
    }

    println!("Initializing configuration...");

    // Prompt for each configuration value
    let base_url = prompt_with_default("Base URL", "http://localhost:8080")?;
    let base_dir = prompt_with_validation("Base directory", "/var/www", |input| {
        let path = Path::new(input);
        if path.exists() && path.is_dir() {
            Ok(())
        } else {
            Err("Base directory must exist and be a valid directory")
        }
    })?;
    let db_path = prompt_with_default("Database path", &dirs::data_dir()
        .ok_or_else(|| anyhow!("Could not determine data directory"))?
        .join("slink")
        .join("shares.db")
        .to_string_lossy())?;
    let hash_secret = prompt_with_default("Hash secret (leave empty to generate)", "*generate*")?;
    let hash_secret = if hash_secret == "*generate*" || hash_secret.is_empty() {
        // Generate a random password
        Uuid::new_v4().to_string()
    } else {
        hash_secret
    };
    let web_user = prompt_with_validation("Web user", "www-data", |input| {
        if users::get_user_by_name(input).is_some() {
            Ok(())
        } else {
            Err("Web user must exist")
        }
    })?;
    let web_group = prompt_with_validation("Web group", "www-data", |input| {
        if users::get_group_by_name(input).is_some() {
            Ok(())
        } else {
            Err("Web group must exist")
        }
    })?;
    let hash_bytes = prompt_with_validation("Hash bytes (2-32)", "7", |input| {
        input.parse::<usize>()
            .map_err(|_| "Hash bytes must be a number")
            .and_then(|value| {
                if (2..=32).contains(&value) {
                    Ok(())
                } else {
                    Err("Hash bytes must be between 2 and 32")
                }
            })
    })?.parse::<usize>()?;

    // Create configuration
    let config = Config {
        base_url,
        base_dir,
        db_path,
        hash_secret,
        web_user,
        web_group,
        hash_bytes,
    };

    // Create config directory and write the configuration file
    create_dir_all(&config_dir)
        .map_err(|e| anyhow!("Failed to create config directory: {}", e))?;
    fs::write(&config_path, toml::to_string(&config)?)
        .map_err(|e| anyhow!("Failed to write config file {}: {}", config_path.display(), e))?;

    // Set permissions to 0600
    set_permissions(&config_path, Permissions::from_mode(0o600))
        .map_err(|e| anyhow!("Failed to set permissions on config file {}: {}", config_path.display(), e))?;

    // Initialize the database
    create_dir_all(Path::new(&config.db_path).parent().unwrap())
        .map_err(|e| anyhow!("Failed to create database directory: {}", e))?;
    init_database(&config.db_path)
        .map_err(|e| anyhow!("Failed to initialize database {}: {}", config.db_path, e))?;

    println!("Configuration saved to {}", config_path.display());
    Ok(())
}

fn prompt_with_default(prompt: &str, default: &str) -> Result<String> {
    print!("{} [{}]: ", prompt, default);
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let input = input.trim();
    Ok(if input.is_empty() { default.to_string() } else { input.to_string() })
}

fn prompt_with_validation<F>(prompt: &str, default: &str, validate: F) -> Result<String>
where
    F: Fn(&str) -> Result<(), &str>,
{
    loop {
        let input = prompt_with_default(prompt, default)?;
        if let Err(err) = validate(&input) {
            println!("Invalid input: {}", err);
        } else {
            return Ok(input);
        }
    }
}

pub fn add_file(config: &Config, file_path: &str) -> Result<String> {
    let conn = Connection::open(&config.db_path)?;
    let uuid = FileShare::add(&conn, config, file_path)?;
    println!("Added file with UUID: {}", uuid);
    Ok(uuid)
}

pub fn share_file(config: &Config, recipient: &str, file_spec: &str) -> Result<()> {
    let conn = Connection::open(&config.db_path)?;
    let uuid = resolve_file_spec(&conn, file_spec)?;

    let share_hash = ShareInfo::share(&conn, config, &uuid, recipient)?;
    let file = FileShare::find_by_uuid(&conn, &uuid)?.ok_or_else(|| anyhow!("File not found"))?;

    println!("Shared {} with {}:", file.filename, recipient);
    println!("{}/{}/{}", config.base_url, share_hash, file.filename);
    Ok(())
}

pub fn unshare_file(config: &Config, recipient: &str, file_spec: &str) -> Result<()> {
    let conn = Connection::open(&config.db_path)?;
    let uuid = resolve_file_spec(&conn, file_spec)?;

    ShareInfo::unshare(&conn, config, &uuid, recipient)?;
    println!("Removed share for {} from {}", file_spec, recipient);
    Ok(())
}

pub fn show_file(config: &Config, file_spec: &str) -> Result<()> {
    let conn = Connection::open(&config.db_path)?;
    let uuid = resolve_file_spec(&conn, file_spec)?;

    let file = FileShare::find_by_uuid(&conn, &uuid)?.ok_or_else(|| anyhow!("File not found"))?;
    let shares = ShareInfo::get_shares(&conn, &uuid)?;

    println!("File: {}", file.filename);
    println!("UUID: {}", file.uuid);
    println!("Added: {}", file.date_added.format("%Y-%m-%d %H:%M:%S"));
    println!("\nShares:");

    let mut table = Table::new();
    table.add_row(row!["Recipient", "Status", "Shared", "Removed", "URL"]);

    for share in shares {
        let status = if share.active { "Active" } else { "Removed" };
        let removed = share.date_removed.map_or("-".to_string(), 
            |d| d.format("%Y-%m-%d %H:%M:%S").to_string());
        let url = format!("{}/{}/{}", config.base_url, share.share_hash, file.filename);

        table.add_row(row![
            share.recipient,
            status,
            share.date_shared.format("%Y-%m-%d %H:%M:%S"),
            removed,
            url
        ]);
    }

    table.printstd();
    Ok(())
}

pub fn list_files(config: &Config) -> Result<()> {
    let conn = Connection::open(&config.db_path)?;
    let mut stmt = conn.prepare(
        "SELECT f.uuid, f.filename, f.date_added, COUNT(s.uuid) as share_count 
         FROM files f 
         LEFT JOIN shares s ON f.uuid = s.uuid AND s.active = 1
         GROUP BY f.uuid 
         ORDER BY f.date_added DESC"
    )?;

    let mut table = Table::new();
    table.add_row(row!["Filename", "UUID", "Added", "Active Shares"]);

    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, DateTime<Utc>>(2)?,
            row.get::<_, i64>(3)?
        ))
    })?;

    for row in rows {
        let (uuid, filename, date_added, share_count) = row?;
        table.add_row(row![
            filename,
            uuid,
            date_added.format("%Y-%m-%d %H:%M:%S"),
            share_count
        ]);
    }

    table.printstd();
    Ok(())
}

pub fn remove_file(config: &Config, file_spec: &str, force: bool) -> Result<()> {
    let conn = Connection::open(&config.db_path)?;
    let uuid = resolve_file_spec(&conn, file_spec)?;

    if let Some(file) = FileShare::find_by_uuid(&conn, &uuid)? {
        file.remove(&conn, config, force)?;
        println!("Removed file: {}", file.filename);
    }
    Ok(())
}

fn resolve_file_spec(conn: &Connection, file_spec: &str) -> Result<String> {
    // If input looks like a UUID, use it directly
    if file_spec.len() == 36 && file_spec.chars().filter(|c| *c == '-').count() == 4 {
        return Ok(file_spec.to_string());
    }

    // Split filename and optional index
    let parts: Vec<&str> = file_spec.split('/').collect();
    let (filename, index) = match parts.as_slice() {
        [filename] => (filename, 1),
        [filename, index_str] => (filename, index_str.parse::<usize>()
            .map_err(|_| anyhow!("Invalid index format"))?),
        _ => return Err(anyhow!("Invalid file specification")),
    };

    let matches = FileShare::find_by_name(conn, filename)?;

    if matches.is_empty() {
        return Err(anyhow!("File not found: {}", filename));
    }

    if matches.len() > 1 && parts.len() == 1 {
        println!("Multiple files found:");
        for (i, (uuid, date)) in matches.iter().enumerate() {
            println!("{}/{}: {} ({})", filename, i + 1, uuid, 
                    date.format("%Y-%m-%d %H:%M:%S"));
        }
        return Err(anyhow!("Please specify file index"));
    }

    matches.get(index - 1)
        .map(|(uuid, _)| uuid.clone())
        .ok_or_else(|| anyhow!("Invalid file index"))
}

pub fn show_info(config: &Config) -> Result<()> {
    println!("slink v{}", env!("CARGO_PKG_VERSION"));
    println!("\nConfiguration:");

    let config_path = config_dir()
        .ok_or_else(|| anyhow!("Could not determine config directory"))?
        .join("slink")
        .join("slink.conf");

    println!("Config file: {}", config_path.display());

    if config_path.exists() {
        println!("\nCurrent configuration:");
        println!("Base URL: {}", config.base_url);
        println!("Base directory: {}", config.base_dir);
        println!("Database path: {}", config.db_path);
        println!("Hash secret: {}..[REDACTED]..{}", 
            &config.hash_secret[..2],
            &config.hash_secret[config.hash_secret.len()-2..]
        );
        println!("Web user: {}", config.web_user);
        println!("Web group: {}", config.web_group);
        println!("Hash bytes: {} ({} bits of entropy)", config.hash_bytes, config.hash_bytes*8);
    } else {
        println!("\nNo configuration file found. Default configuration will be created on first use.");
    }

    // Database statistics
    let db_path = Path::new(&config.db_path);
    if db_path.exists() {
        println!("\nDatabase statistics:");
        let conn = Connection::open(&config.db_path)?;

        let file_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM files",
            [],
            |row| row.get(0)
        )?;

        let total_shares: i64 = conn.query_row(
            "SELECT COUNT(*) FROM shares",
            [],
            |row| row.get(0)
        )?;

        let active_shares: i64 = conn.query_row(
            "SELECT COUNT(*) FROM shares WHERE active = 1",
            [],
            |row| row.get(0)
        )?;

        // Handle NULL case explicitly for oldest file
        let oldest_file: String = if file_count > 0 {
            conn.query_row(
                "SELECT date_added FROM files ORDER BY date_added ASC LIMIT 1",
                [],
                |row| row.get::<_, DateTime<Utc>>(0)
            ).map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())?
        } else {
            "No files".to_string()
        };

        println!("Total files: {}", file_count);
        println!("Total shares: {}", total_shares);
        println!("Active shares: {}", active_shares);
        println!("Oldest file: {}", oldest_file);
    } else {
        println!("\nDatabase not initialized yet.");
    }

    Ok(())
}

