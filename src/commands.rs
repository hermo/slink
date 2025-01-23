use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use prettytable::{Table, row};
use rusqlite::Connection;

use crate::{Config, FileShare, ShareInfo};

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

