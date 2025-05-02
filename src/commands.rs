// src/commands.rs
use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc, NaiveDateTime}; // Added NaiveDateTime
use prettytable::{Table, row};
// Remove rusqlite: use rusqlite::{Connection, params};
use sqlx::SqlitePool; // Removed Row
use std::path::Path;
use dirs::config_dir;
use std::fs;
use std::path::PathBuf;
// Remove init_database: use crate::{init_database, create_dir_all, remove_file_with_access};
use crate::{create_dir_all, remove_file_with_access}; // Added remove_file_with_access
use crate::{Config, FileShare, ShareInfo};
use crate::Uuid;
use uuid::Timestamp;
use crate::{Permissions, PermissionsExt, set_permissions, set_permissions_recursive};
use std::io::{self, Write, Read, BufReader, BufWriter};
use std::collections::HashSet; // Added for cleanup logic
use tempfile::NamedTempFile;

// This function remains synchronous as it deals with initial setup before DB pool exists
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
        // Generate ID
        Uuid::new_v7(Timestamp::now(uuid::NoContext)).to_string()
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

    // Ensure DB directory exists, but don't initialize DB (migrations handle this)
    if let Some(parent) = Path::new(&config.db_path).parent() {
        create_dir_all(parent)
            .map_err(|e| anyhow!("Failed to create database directory {}: {}", parent.display(), e))?;
    }
    // Removed init_database call

    println!("Configuration saved to {}", config_path.display());
    println!("Database will be created and migrated on first run.");
    Ok(())
}

// Synchronous helper
fn prompt_with_default(prompt: &str, default: &str) -> Result<String> {
    print!("{} [{}]: ", prompt, default);
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let input = input.trim();
    Ok(if input.is_empty() { default.to_string() } else { input.to_string() })
}

// Synchronous helper
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

// Made async, takes pool
pub async fn add_file(pool: &SqlitePool, config: &Config, file_path: &str, name: Option<String>) -> Result<String> {
    // Removed Connection::open and WAL pragma

    // Sanitize and validate the provided name or get from file_path
    let filename = if let Some(name) = name {
        sanitize_filename(&name)?
    } else {
        let path = PathBuf::from(file_path);
        path.file_name()
            .ok_or_else(|| anyhow!("Invalid filename"))?
            .to_string_lossy()
            .to_string()
    };

    let (final_path, checksum) = if file_path == "-" {
        // Handle stdin input
        handle_stdin_upload(&config.base_dir, &filename)?
    } else {
        // Handle regular file
        let path = PathBuf::from(file_path);
        (path.clone(), calculate_file_hash(&path)?)
    };

    let uuid = Uuid::new_v7(Timestamp::now(uuid::NoContext)).to_string();
    let target_dir = PathBuf::from(&config.base_dir).join(&uuid);
    let target_file = target_dir.join(&filename);

    create_dir_all(&target_dir)?;
    fs::copy(&final_path, &target_file)?;

    // If this was a temp file, clean it up
    if final_path.to_string_lossy().contains("slink_temp_") {
        fs::remove_file(&final_path)?;
    }

    set_permissions_recursive(
        &target_dir,
        0o750,
        0o640,
        &config.web_user,
        &config.web_group,
    )?;

    // Use sqlx query
    // Fix Utc::now() borrowing
    let now_naive = Utc::now().naive_utc(); // Use NaiveDateTime for DB
    sqlx::query!(
        "INSERT INTO files (uuid, filename, date_added) VALUES (?1, ?2, ?3)",
        uuid, filename, now_naive // Use NaiveDateTime
    )
    .execute(pool) // Use pool
    .await?; // Use await

    println!("BLAKE3: {}", checksum);
    println!("Added file with UUID: {}", uuid);

    Ok(uuid)
}

// Synchronous helper
fn sanitize_filename(name: &str) -> Result<String> {
    let name = name.trim();

    // Basic security checks
    if name.is_empty() {
        return Err(anyhow!("Empty filename not allowed"));
    }

    if name.contains('/') || name.contains('\\') || name.contains("..") {
        return Err(anyhow!("Invalid characters in filename"));
    }

    // Remove any leading dots to prevent hidden files
    let name = name.trim_start_matches('.');
    if name.is_empty() {
        return Err(anyhow!("Invalid filename (hidden files not allowed)"));
    }

    // Additional checks for problematic characters
    if name.chars().any(|c| {
        c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*')
    }) {
        return Err(anyhow!("Invalid characters in filename"));
    }

    Ok(name.to_string())
}

// Synchronous helper
fn handle_stdin_upload(base_dir: &str, filename: &str) -> Result<(PathBuf, String)> {
    // Create temp file with prefix
    let temp_dir = PathBuf::from(base_dir);
    let temp_file = NamedTempFile::new_in(&temp_dir)?
        .into_temp_path();
    let temp_path = temp_file.to_path_buf();

    // Rename with our prefix and the actual filename
    let new_name = temp_dir.join(format!("slink_temp_{}_{}", Uuid::new_v7(Timestamp::now(uuid::NoContext)), filename));
    fs::rename(&temp_path, &new_name)?;

    let file = fs::OpenOptions::new()
        .write(true)
        .create(false)
        .open(&new_name)?;
    let mut writer = BufWriter::new(file);

    // Setup BLAKE3 hasher
    let mut hasher = blake3::Hasher::new();

    // Read from stdin and write to file while updating hash
    let mut stdin = BufReader::new(io::stdin());
    let mut buffer = [0; 8192];

    loop {
        match stdin.read(&mut buffer) {
            Ok(0) => break, // EOF
            Ok(n) => {
                writer.write_all(&buffer[..n])?;
                hasher.update(&buffer[..n]);
            }
            Err(e) => {
                // Clean up temp file on error
                let _ = fs::remove_file(&new_name);
                return Err(anyhow!("Error reading from stdin: {}", e));
            }
        }
    }

    writer.flush()?;

    Ok((new_name, hasher.finalize().to_hex().to_string()))
}

// Synchronous helper
fn calculate_file_hash(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0; 8192];

    loop {
        match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => hasher.update(&buffer[..n]),
            Err(e) => return Err(anyhow!("Error reading file for hash: {}", e)),
        };
    }

    Ok(hasher.finalize().to_hex().to_string())
}

// Made async, takes pool
pub async fn share_file(
    pool: &SqlitePool, // Changed conn to pool
    config: &Config,
    recipient: &str,
    file_spec: &str,
    expires_at_utc: Option<DateTime<Utc>>, // Changed name for clarity, still Utc from main
    delete_file_on_expiry: bool,
) -> Result<()> {
    // Removed Connection::open
    let uuid = resolve_file_spec(pool, file_spec).await?; // Use pool and await

    // Convert expires_at_utc to Option<NaiveDateTime> for ShareInfo::share
    let expires_at_naive: Option<NaiveDateTime> = expires_at_utc.map(|dt| dt.naive_utc());

    // Call async ShareInfo::share with NaiveDateTime
    let share_hash = ShareInfo::share(
        pool, // Use pool
        config,
        &uuid,
        recipient,
        expires_at_naive, // Pass NaiveDateTime
        delete_file_on_expiry,
    ).await?; // Use await

    // Call async FileShare::find_by_uuid
    let file = FileShare::find_by_uuid(pool, &uuid).await? // Use pool and await
        .ok_or_else(|| anyhow!("File not found after resolving spec"))?;

    println!("Shared {} with {}:", file.filename, recipient);
    println!("{}/{}/{}", config.base_url, share_hash, file.filename);
    Ok(())
}

// Made async, takes pool
pub async fn unshare_file(pool: &SqlitePool, config: &Config, recipient: &str, file_spec: &str) -> Result<()> {
    // Removed Connection::open
    let uuid = resolve_file_spec(pool, file_spec).await?; // Use pool and await

    // Call async ShareInfo::unshare
    ShareInfo::unshare(pool, config, &uuid, recipient).await?; // Use pool and await
    println!("Removed share for {} from {}", file_spec, recipient);
    Ok(())
}

// Made async, takes pool
pub async fn show_file(pool: &SqlitePool, config: &Config, file_spec: &str) -> Result<()> {
    // Removed Connection::open
    let uuid = resolve_file_spec(pool, file_spec).await?; // Use pool and await

    // Call async FileShare::find_by_uuid
    let file = FileShare::find_by_uuid(pool, &uuid).await? // Use pool and await
        .ok_or_else(|| anyhow!("File not found after resolving spec"))?;
    // Call async ShareInfo::get_shares
    let shares = ShareInfo::get_shares(pool, &uuid).await?; // Use pool and await

    println!("File: {}", file.filename);
    println!("UUID: {}", file.uuid);
    println!("Added: {}", file.date_added.format("%Y-%m-%d %H:%M:%S")); // NaiveDateTime formats directly
    println!("\nShares:");

    let mut table = Table::new();
    table.add_row(row!["Recipient", "Status", "Shared", "Removed", "Expires At", "Delete on Expiry", "URL"]); // Added columns

    for share in shares {
        let status = if share.active { "Active" } else { "Removed" };
        let removed = share.date_removed.map_or("-".to_string(),
            |d| d.format("%Y-%m-%d %H:%M:%S").to_string()); // NaiveDateTime formats directly
        let expires = share.expires_at.map_or("-".to_string(), // Added expires formatting
            |d| d.format("%Y-%m-%d %H:%M:%S").to_string()); // NaiveDateTime formats directly
        let delete_on_expiry = if share.delete_file_on_expiry { "Yes" } else { "No" }; // Added delete flag formatting
        let url = format!("{}/{}/{}", config.base_url, share.share_hash, file.filename);

        table.add_row(row![
            share.recipient,
            status,
            share.date_shared.format("%Y-%m-%d %H:%M:%S"), // NaiveDateTime formats directly
            removed,
            expires, // Added expires
            delete_on_expiry, // Added delete flag
            url
        ]);
    }

    table.printstd();
    Ok(())
}

// Made async, takes pool
pub async fn list_files(pool: &SqlitePool, _config: &Config) -> Result<()> { // Prefix unused config
    // Removed Connection::open

    // Use sqlx query
    let rows = sqlx::query!(
        r#"
         SELECT
             f.uuid,
             f.filename,
             f.date_added,
             COUNT(CASE WHEN s.active = 1 THEN s.uuid END) as "active_share_count!", -- ! needed for non-Option count
             COUNT(CASE WHEN s.active = 1 AND s.expires_at IS NOT NULL THEN s.uuid END) as "expiring_share_count!",
             COUNT(CASE WHEN s.active = 1 AND s.expires_at IS NOT NULL AND s.delete_file_on_expiry = 1 THEN s.uuid END) as "deleting_share_count!"
          FROM files f
          LEFT JOIN shares s ON f.uuid = s.uuid
          GROUP BY f.uuid, f.filename, f.date_added
          ORDER BY f.date_added DESC
        "#
    )
    .fetch_all(pool) // Use pool
    .await?; // Use await

    let mut table = Table::new();
    table.add_row(row!["Filename", "UUID", "Added", "Active Shares", "Expiring", "Deletes File"]);

    for row in rows {
        table.add_row(row![
            row.filename,
            row.uuid,
            row.date_added.format("%Y-%m-%d %H:%M:%S"), // NaiveDateTime formats directly
            row.active_share_count, // Already i64 from COUNT
            row.expiring_share_count, // Already i64 from COUNT
            row.deleting_share_count  // Already i64 from COUNT
        ]);
    }

    table.printstd();
    Ok(())
}

// Made async, takes pool
pub async fn remove_file(pool: &SqlitePool, config: &Config, file_spec: &str, force: bool) -> Result<()> {
    // Removed Connection::open
    let uuid = resolve_file_spec(pool, file_spec).await?; // Use pool and await

    // Call async FileShare::find_by_uuid
    if let Some(file) = FileShare::find_by_uuid(pool, &uuid).await? { // Use pool and await
        // Call async file.remove
        file.remove(pool, config, force).await?; // Use pool and await
        println!("Removed file: {}", file.filename);
    } else {
        // This case might be unreachable if resolve_file_spec works correctly
         println!("File '{}' not found or already removed.", file_spec);
    }
    Ok(())
}

// Made async, takes pool
async fn resolve_file_spec(pool: &SqlitePool, file_spec: &str) -> Result<String> {
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

    // Call async FileShare::find_by_name
    let matches = FileShare::find_by_name(pool, filename).await?; // Use pool and await

    if matches.is_empty() {
        return Err(anyhow!("File not found: {}", filename));
    }

    if matches.len() > 1 && parts.len() == 1 {
        println!("Multiple files found:");
        for (i, (uuid, date)) in matches.iter().enumerate() {
            println!("{}/{}: {} ({})", filename, i + 1, uuid,
                    date.format("%Y-%m-%d %H:%M:%S")); // NaiveDateTime formats directly
        }
        return Err(anyhow!("Please specify file index"));
    }

    matches.get(index - 1)
        .map(|(uuid, _)| uuid.clone())
        .ok_or_else(|| anyhow!("Invalid file index"))
}

// Made async, takes pool
pub async fn show_info(pool: &SqlitePool, config: &Config) -> Result<()> { // Added pool param
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
        println!("\nNo configuration file found. Run `slink init` to create one.");
    }

    // Database statistics (only if DB file exists)
    let db_path_str = &config.db_path;
    if Path::new(db_path_str).exists() {
        println!("\nDatabase statistics:");

        // Use query_scalar! for single value results
        // Change type to i32 for COUNT results
        let file_count: i32 = sqlx::query_scalar!("SELECT COUNT(*) FROM files")
            .fetch_one(pool)
            .await?;

        let total_shares: i32 = sqlx::query_scalar!("SELECT COUNT(*) FROM shares")
            .fetch_one(pool)
            .await?;

        let active_shares: i32 = sqlx::query_scalar!("SELECT COUNT(*) FROM shares WHERE active = 1")
            .fetch_one(pool)
            .await?;

        // Handle NULL case explicitly for oldest file using fetch_optional
        // Change type to Option<NaiveDateTime>
        let oldest_file_dt: Option<NaiveDateTime> = sqlx::query_scalar!(
                "SELECT date_added FROM files ORDER BY date_added ASC LIMIT 1"
            )
            .fetch_optional(pool)
            .await?;

        let oldest_file_str = oldest_file_dt
            .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string()) // NaiveDateTime formats directly
            .unwrap_or_else(|| "-".to_string());


        println!("Total files: {}", file_count);
        println!("Total shares: {}", total_shares);
        println!("Active shares: {}", active_shares);
        println!("Oldest file added: {}", oldest_file_str);
    } else {
        println!("\nDatabase not found at {}. It will be created and migrated on first run.", config.db_path);
    }

    Ok(())
}


// --- Cleanup Command ---

// Structure to hold share details relevant for cleanup (can reuse)
#[derive(sqlx::FromRow, Debug)] // Added Debug
struct ExpiringShare {
    uuid: String,
    recipient: String,
    share_hash: String,
    expires_at: Option<NaiveDateTime>, // Changed to Option<NaiveDateTime>
    delete_file_on_expiry: bool,
}

// Made async, takes pool
pub async fn cleanup_expired(pool: &SqlitePool, config: &Config, quiet: bool) -> Result<()> { // Added quiet flag
    let now = Utc::now();
    let now_naive = now.naive_utc(); // Use NaiveDateTime for DB comparison
    if !quiet {
        println!("Running cleanup at {}...", now.format("%Y-%m-%d %H:%M:%S"));
    }
    // Removed initial println! if quiet

    // 1. Find expired shares using sqlx
    let expired_shares = sqlx::query_as!(
        ExpiringShare,
        r#"
        SELECT uuid, recipient, share_hash, expires_at, delete_file_on_expiry
        FROM shares
        WHERE active = 1 AND expires_at IS NOT NULL AND expires_at < ?
        "#,
        now_naive // Use NaiveDateTime for comparison
    )
    .fetch_all(pool)
    .await?;

    let mut files_to_delete = HashSet::new(); // Track UUIDs of files to delete
    let mut expired_shares_count = 0;
    let mut files_deleted_count = 0;

    for share in &expired_shares {
         expired_shares_count += 1;
         // Suppress finding/marking messages if quiet
         if !quiet {
             println!(
                 "Found expired share for file {} (recipient: {}, expires: {})",
                 share.uuid, share.recipient, share.expires_at.map_or_else(|| "-".to_string(), |dt| dt.format("%Y-%m-%d %H:%M:%S").to_string()) // Handle Option
             );
         }
         if share.delete_file_on_expiry {
             // Suppress marking message if quiet
             if !quiet {
                 println!("  -> Marked file {} for deletion.", share.uuid);
             }
             files_to_delete.insert(share.uuid.clone());
         }
    }


    if expired_shares.is_empty() && files_to_delete.is_empty() {
        if !quiet {
             // Suppress "nothing found" message if quiet
             println!("No expired shares or files marked for deletion found.");
        }
        return Ok(());
    }

    // 2. Deactivate expired shares and remove symlinks (in a transaction)
    let mut tx = pool.begin().await?; // Start sqlx transaction

    for share in &expired_shares {
        // Remove symlink
        let symlink_path = PathBuf::from(&config.base_dir).join(&share.share_hash);
        if symlink_path.exists() {
            if let Err(e) = fs::remove_file(&symlink_path) {
                eprintln!("Warning: Failed to remove symlink {}: {}", symlink_path.display(), e);
                // Log and continue
                // Keep error messages even if quiet
            } else {
                println!("Removed symlink: {}", symlink_path.display());
            }
        }

        // Deactivate share in DB using sqlx
        let deactivate_time = Utc::now().naive_utc(); // Use NaiveDateTime for DB
        sqlx::query!(
            "UPDATE shares SET active = 0, date_removed = ? WHERE uuid = ? AND recipient = ?",
            deactivate_time, share.uuid, share.recipient
        )
        .execute(&mut *tx) // Execute within the transaction
        .await?;
        // Suppress deactivation message if quiet
        if !quiet {
            println!("Deactivated share for file {} (recipient: {})", share.uuid, share.recipient);
        }

    }

    // 3. Delete files marked for deletion (if any)
    for uuid_to_delete in &files_to_delete {
        // Check if there are any *other* active shares for this file using sqlx
        // Change type to i32 for COUNT result
        let active_shares_count: i32 = sqlx::query_scalar!(
            "SELECT COUNT(*) FROM shares WHERE uuid = ? AND active = 1",
            uuid_to_delete
        )
        .fetch_one(&mut *tx) // Fetch within the transaction
        .await?;

        if active_shares_count == 0 {
            // Suppress "Proceeding" message if quiet
            if !quiet {
                println!("Proceeding with deletion of file {}", uuid_to_delete);
            }
            let file_dir = PathBuf::from(&config.base_dir).join(uuid_to_delete);

            // Use remove_file_with_access for potentially permissioned files
            if file_dir.exists() { // Check if dir exists before trying to remove
                if let Err(e) = remove_file_with_access(&file_dir) {
                     eprintln!("Error deleting file directory {}: {}", file_dir.display(), e);
                     // Keep error messages even if quiet
                     // Log and continue, transaction will rollback on error if not committed
                } else {
                     println!("Deleted file directory: {}", file_dir.display());
                     files_deleted_count += 1;
                     // First, remove all associated shares for this file
                     sqlx::query!("DELETE FROM shares WHERE uuid = ?", uuid_to_delete)
                        .execute(&mut *tx)
                        .await?;
                     // Keep removal messages even if quiet
                     println!("Removed associated shares record for file {} from database.", uuid_to_delete);

                     // Then, remove the file record from the 'files' table using sqlx
                     sqlx::query!("DELETE FROM files WHERE uuid = ?", uuid_to_delete)
                        .execute(&mut *tx) // Execute within the transaction
                        .await?;
                     // Keep removal messages even if quiet
                     println!("Removed file record {} from database.", uuid_to_delete);
                  }
             } else {
                  // Keep warning messages even if quiet
                  println!("Warning: Directory {} not found, skipping deletion.", file_dir.display());
                  // If directory doesn't exist, still try to remove DB record
                  match sqlx::query!("DELETE FROM files WHERE uuid = ?", uuid_to_delete)
                     .execute(&mut *tx)
                     .await {
                      Ok(result) if result.rows_affected() > 0 => {
                          // Keep removal messages even if quiet
                          println!("Removed potentially orphaned file record {} from database.", uuid_to_delete);
                          // Suppress this message if quiet
                          if !quiet {
                              // This specific message is now redundant if the one above is always shown
                              // println!("Removed potentially orphaned file record {} from database.", uuid_to_delete);
                          }
                      }
                      Ok(_) => {} // No rows affected, record likely already gone
                      // Keep error messages even if quiet
                      Err(e) => eprintln!("Error removing file record {} from database: {}", uuid_to_delete, e),
                  }
             }
        } else {
            if !quiet {
                println!(
                    "Skipping deletion of file {}: {} other active shares exist.",
                    // Suppress skipping message if quiet
                    uuid_to_delete, active_shares_count
                );
            }
        }

    }

    // Commit transaction
    tx.commit().await?; // Commit async transaction

    if !quiet {
         println!("Cleanup finished. Expired shares processed: {}. Files deleted: {}.", expired_shares_count, files_deleted_count);
         // Suppress final summary if quiet
    }
    Ok(())

}
