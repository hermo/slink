# slink - Simple Secure File Sharing

```slink``` is a self-hosted solution for sharing files via HTTPS with unique sharing links.
It manages files on your web server and creates secure, recipient-specific sharing URLs.

## Features

- Self-hosted file sharing with your preferred web server
- Unique sharing links per recipient
- Command line interface
- Share history tracking
- Uses BLAKE3 for secure link generation
- Configurable hash entropy
- Interactive configuration setup with validation
- Secure configuration file creation with strict permissions (```0600```)
- Expiring shares and optional automatic file deletion upon expiry

## Installation

```bash
cargo install slink
```

### Advanced Permissions with Capabilities (Optional)

In some setups, you might run `slink` as your regular user, but need it to create files/directories owned by a different user/group (e.g., the user your web server runs as, like `www-data`). Directly using `sudo` or SETUID for `slink` can cause issues with accessing the user-specific configuration file (`~/.config/slink/slink.conf`).

A more robust solution is to use Linux capabilities. This grants the `slink` binary specific privileges without changing the user it runs as.

1.  **Ensure `base_dir` Permissions:** The directory specified as `base_dir` in your configuration must be writable by the group `slink` will run under, and ideally owned by the target web user/group. It should also have the SETGID bit set so that new directories created inside it inherit the correct group ownership.
    ```bash
    # Example: Make base_dir owned by www-data:yourgroup, writable by group, with SETGID
    sudo chown www-data:yourgroup /var/www/dump
    sudo chmod g+w,g+s /var/www/dump # Results in permissions like 2770 (rwxrws---)
    ```
    Replace `yourgroup` with the group the user running `slink` belongs to, and `/var/www/dump` with your actual `base_dir`. The `web_user` (`www-data` in this example) and `web_group` (`yourgroup`) should match your `slink.conf`.

2.  **Grant Capabilities:** Use `setcap` to grant `slink` the ability to change file ownership (`cap_chown`) and bypass file permission checks (`cap_dac_override`) when necessary. Replace `/path/to/slink` with the actual path to your installed binary (e.g., `$(which slink)` or `/home/user/.cargo/bin/slink`).
    ```bash
    sudo setcap cap_chown,cap_dac_override+eip /path/to/slink
    ```
    *   `cap_chown`: Allows changing file user/group ownership.
    *   `cap_dac_override`: Allows bypassing file read, write, and execute permission checks.
    *   `+eip`: Applies these capabilities to the Effective, Inheritable, and Permitted sets.

3.  **(Optional) Verify Capabilities:**
    ```bash
    getcap /path/to/slink
    # Expected output: /path/to/slink = cap_chown,cap_dac_override+eip
    ```

With this setup, `slink` runs as your user (accessing your config/db) but can create directories in `base_dir` and set their ownership to the configured `web_user` and `web_group`.

**Security Note:** Granting capabilities reduces the security surface compared to running as root or using SETUID, but still provides elevated privileges. Understand the implications before applying them.

## Configuration

On first run, ```slink``` will not automatically create a configuration file. Instead, you must
initialize it using the ```slink init``` command. This ensures that all configuration values are
explicitly set by the user and validated.

To initialize the configuration, run:

```bash
slink init
```

You will be prompted to provide the following values:

- **Base URL**: The URL where your files will be accessible (default: ```http://localhost:8080```).
- **Base Directory**: The directory where files will be stored. This must already exist (default: ```/var/www```).
- **Database Path**: The path to the SQLite database file (default: ```~/.local/share/slink/shares.db```).
- **Hash Secret**: A secret used for generating secure hashes. If left empty, a random secret will be generated.
- **Web User**: The user that owns the files (default: ```www-data```).
- **Web Group**: The group that owns the files (default: ```www-data```).
- **Hash Bytes**: The length of the hash in bytes (must be between 2 and 32, default: ```7```).

The configuration file will be saved at ```~/.config/slink/slink.conf``` with strict permissions
(```0600```), ensuring it is only readable and writable by the owner.

Example configuration file:

```toml
base_url = "http://localhost:8080"
base_dir = "/var/www"
db_path = "/home/user/.local/share/slink/shares.db"
hash_secret = "random-generated-secret"
web_user = "www-data"
web_group = "www-data"
hash_bytes = 7
```

Ensure your web server is configured to serve files from ```base_dir``` and that symlinks are followed.

The ```hash_bytes``` setting controls the length of the generated share hashes. The default of 7
bytes (56 bits of entropy) provides a balance between URL length and security, requiring on average
over 11 years of continuous guessing at 100M attempts per second to find a valid hash. Increase
this value if you need additional security.

## Usage

### Initialize Configuration
Run the following command to create the configuration file:

```bash
slink init
```

You will be prompted to provide configuration values. If the configuration file already exists,
this command will fail.

### Add a File
The following command creates a UUID and copies ```document.pdf``` to the proper location.

```bash
slink add document.pdf
# Added file with UUID: 09d1cc19-1efe-42f2-9292-a33e60d44de5
```

### Share a File
Now that ```document.pdf``` is known by ```slink```, we can refer
to it with the filename or UUID and share it with a recipient. The
recipient name can be anything; an email-like address is just an example.

```bash
slink share alice@example.com document.pdf
# Shared document.pdf with alice@example.com:
# http://localhost:8080/KJh8h7G6dT/document.pdf
```

You can also create shares that expire after a certain duration using the `--expires-in` (or `-e`) flag. The duration format accepts units like `s`, `m`, `h`, `d`.

```bash
# Share for 1 hour
slink share alice@example.com document.pdf --expires-in 1h
# Shared document.pdf with alice@example.com:
# http://localhost:8080/aBcDeFgH/document.pdf (Expires in 1 hour)
```

Additionally, you can specify that the underlying file should be deleted when the share expires using `--delete-file-on-expiry`. This only happens if it's the *last active share* for that file. This flag requires `--expires-in`.

```bash
# Share for 5 minutes, then delete the file (if no other shares exist)
slink share bob@example.com sensitive.dat -e 5m --delete-file-on-expiry
# Shared sensitive.dat with bob@example.com:
# http://localhost:8080/xYz123Ab/sensitive.dat (Expires in 5 minutes, file will be deleted)
```

**Note:** Expired shares are removed by the `slink cleanup` command, which needs to be run periodically (see "Automatic Cleanup" section below).

### Add and Share in One Step
You can add a file and immediately share it using the `-s` flag:

```bash
slink add document.pdf -s alice@example.com
# BLAKE3: 7d05258389f606f31856a295b5a7f72dd82a8f3e8d6a7b5f0c4f8e6d5c4b3a2
# Added file with UUID: 09d1cc19-1efe-42f2-9292-a33e60d44de5
# Shared document.pdf with alice@example.com:
# http://localhost:8080/KJh8h7G6dT/document.pdf
```

The `--expires-in` (`-e`) and `--delete-file-on-expiry` flags can also be used here, just like with the `share` command, to set an expiry for the initial share created with `-s`.

```bash
# Add and share for 10 minutes, then delete the file
slink add report.docx -s reviewer@corp.com -e 10m --delete-file-on-expiry
```

### Show File Information
```bash
slink show document.pdf
# File: document.pdf
# UUID: 09d1cc19-1efe-42f2-9292-a33e60d44de5
# Added: 2025-01-23 20:15:30
# 
# Shares:
# +-----------------+----------+---------------------+---------------------+-----------------------------------------------+
# | Recipient       | Status   | Shared              | Removed             | URL                                           |
# +-----------------+----------+---------------------+---------------------+-----------------------------------------------+
# | alice@example.com| Active  | 2025-01-23 20:16:00 | -                   | http://localhost:8080/eUgCTjtB_Q/document.pdf |
# | bob@example.com  | Removed | 2025-01-23 20:16:30 | 2025-01-23 20:17:00 | http://localhost:8080/KJh8h7G6dT/document.pdf |
# +-----------------+----------+---------------------+---------------------+-----------------------------------------------+
```

### List All Files
```bash
slink ls
# +--------------+--------------------------------------+--------------------+---------------+
# | Filename     | UUID                                 | Added              | Active Shares |
# +--------------+--------------------------------------+--------------------+---------------+
# | document.pdf | 09d1cc19-1efe-42f2-9292-a33e60d44de5| 2025-01-23 20:15:30 | 1             |
# +--------------+--------------------------------------+--------------------+---------------+
```

### Remove Share
```bash
slink unshare alice@example.com document.pdf
# Removed share for document.pdf from alice@example.com
```

### Remove File
```bash
slink rm document.pdf
# Are you sure you want to remove document.pdf? [y/N] y
# Removed file: document.pdf
```

Force remove without confirmation:
```bash
slink rm -f document.pdf
```

### Multiple Files with Same Name
When multiple files with the same name exist, they are indexed by age:

```bash
slink show report.pdf
# Multiple files found:
# report.pdf/1: 09d1cc19-1efe-42f2-9292-a33e60d44de5 (2025-01-20 10:00:00)
# report.pdf/2: 7f8af9a4-420b-464e-a0e6-5861b230e34a (2025-01-23 15:30:00)
# Please specify file index
#
slink show report.pdf/1
```

You can also reference files directly by UUID:

```bash
slink show 09d1cc19-1efe-42f2-9292-a33e60d44de5
```

### Remote Usage

Files can be uploaded directly to a remote server using SSH. The `-` argument tells `slink` to read from stdin, and the `-n` flag specifies the filename to use.

Upload a file:
```bash
ssh example.com slink add - -n "document.pdf" < document.pdf
# BLAKE3: 7d05258389f606f31856a295b5a7f72dd82a8f3e8d6a7b5f0c4f8e6d5c4b3a2
# Added file with UUID: 09d1cc19-1efe-42f2-9292-a33e60d44de5
```

Using pipe:
```bash
cat document.pdf | ssh example.com slink add - -n "document.pdf"
```

Generate and upload archive:
```bash
tar czf - files/ | ssh example.com slink add - -n "files.tar.gz"
```

Upload with progress using `pv`:
```bash
pv document.pdf | ssh example.com slink add - -n "document.pdf"
# 156MB 0:00:15 [10.4MB/s] [======================>] 100%
# BLAKE3: 7d05258389f606f31856a295b5a7f72dd82a8f3e8d6a7b5f0c4f8e6d5c4b3a2
# Added file with UUID: 09d1cc19-1efe-42f2-9292-a33e60d44de5
```

The BLAKE3 hash is printed after successful upload and can be used to verify file integrity.

### Helper Script (`contrib/sl`)

To simplify these remote operations, a helper Bash script `sl` is provided in the `contrib/` directory. This script wraps the common `ssh` commands for `add`, `ls`, and `rm` operations on a remote `slink` server.

**Features:**

*   Provides `add`, `ls`, and `rm` subcommands mirroring `slink`.
*   Handles piping local files to the remote `slink add` command.
*   Configurable default remote server via an XDG-compliant config file (`~/.config/slink/config`).
*   Prompts for server configuration using the `init` command if not already set.

**Setup:**

1.  **Configure:** Run `contrib/sl init` and enter the hostname of your remote server where `slink` is running. This saves the server name to `~/.config/slink/config`.
2.  **(Optional) Install Locally:** Copy the `contrib/sl` script to a location in your `$PATH`, for example:
    ```bash
    sudo cp contrib/sl /usr/local/bin/sl
    sudo chmod +x /usr/local/bin/sl
    ```

**Usage:**

Once configured (and optionally installed), you can use the script like this:

*   **Add & Share:**
    ```bash
    sl add local_document.pdf -s recipient@example.com [-n remote_name.pdf] [-h other.server.com]
    # Uses server from ~/.config/slink/config unless -h is provided
    ```
*   **List Files:**
    ```bash
    sl ls [-h other.server.com]
    ```
*   **Remove File:**
    ```bash
    sl rm <filename_or_uuid> [-h other.server.com]
    ```

The script requires the `-h <host>` flag if the configuration file has not been created using `sl init`.

### Cleanup Expired Shares
The `cleanup` command processes expired shares, removing their symlinks and updating the database. If a share was created with `--delete-file-on-expiry` and it was the last active share for the file, this command will also delete the file itself.

```bash
slink cleanup
# Starting cleanup of expired shares...
#  Expiring share for UUID (recipient: ...)
#  ...
# Processed 1 expired shares.
# Checking 1 files for potential deletion...
#  File UUID has no remaining active shares. Attempting deletion...
#   Successfully removed file directory /var/www/UUID
#   Successfully removed file record UUID from database.
# Cleanup finished. Expired shares processed: 1. Files deleted: 1.
```

This command is intended to be run periodically by a scheduler (see below).

## Automatic Cleanup (Scheduling)

Since `slink` does not run as a long-running process, the `slink cleanup` command must be executed periodically by an external scheduler like `systemd-timers` or `cron` to automatically remove expired shares and files.

**Using systemd-timers (Recommended):**

1.  **Create a service file** (e.g., `/etc/systemd/system/slink-cleanup.service`):
    ```ini
    [Unit]
    Description=Slink Expired Share Cleanup
    After=network.target

    [Service]
    Type=oneshot
    User=<user_running_slink>  # Replace with the user who runs slink commands
    Group=<group_of_user> # Replace with the primary group of the user
    ExecStart=/path/to/slink cleanup # Replace with the actual path to your slink binary
    ```

2.  **Create a timer file** (e.g., `/etc/systemd/system/slink-cleanup.timer`):
    ```ini
    [Unit]
    Description=Run slink cleanup every 5 minutes

    [Timer]
    OnBootSec=5min
    OnUnitActiveSec=5min # Run 5 minutes after the last run
    Unit=slink-cleanup.service

    [Install]
    WantedBy=timers.target
    ```

3.  **Enable and start the timer:**
    ```bash
    sudo systemctl enable slink-cleanup.timer
    sudo systemctl start slink-cleanup.timer
    # Check status: sudo systemctl list-timers slink-cleanup.timer
    ```

**Using cron:**

Edit the crontab for the user running `slink`:
```bash
crontab -e
```

Add a line to run the cleanup command (e.g., every 5 minutes):
```cron
*/5 * * * * /path/to/slink cleanup > /dev/null 2>&1 # Replace with actual path
```
Make sure the `slink` binary is in the user's `$PATH` or provide the full path. Redirecting output (`> /dev/null 2>&1`) is optional but recommended for cron jobs.

## Web Server Configuration

Example nginx configuration:

```nginx
location /f/ {
    alias /var/www/;
    try_files $uri =404;
    autoindex off;
}
```

## License

GPL2 License
