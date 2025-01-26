# slink - Simple Secure File Sharing

`slink` is a self-hosted solution for sharing files via HTTPS with unique sharing links. It manages
files on your web server and creates secure, recipient-specific sharing URLs.

## Features

- Self-hosted file sharing with your preferred web server
- Unique sharing links per recipient
- Command line interface
- Share history tracking
- Uses BLAKE3 for secure link generation
- Configurable hash entropy

## Installation

```bash
cargo install slink
```

If you want to allow the app to alter file owners you may grant it the proper capabilities.
Note that this comes with security implications, user beware.
```
sudo setcap cap_chown+ep slink
```

## Configuration

On first run, `slink` creates a configuration file at `~/.config/slink/slink.conf`:

```toml
base_url = "http://localhost:8080"
base_dir = "/var/www"
db_path = "/home/user/.local/share/slink/shares.db"
hmac_secret = "random-generated-secret"
web_user = "www-data"
web_group = "www-data"
hash_bytes = 7
```

Ensure your web server is configured to serve files from `base_dir` and that symlinks are followed.

The hash_bytes setting controls the length of the generated share hashes. The default of 7 bytes
(56 bits of entropy) provides a balance between URL length and security, requiring on average over
11 years of continuous guessing at 100M attempts per second to find a valid hash. Increase this
value if you need additional security.


## Usage

### Add a file
The following command crates a UUID and copies document.pdf to the proper location.
```bash
slink add document.pdf
# Added file with UUID: 09d1cc19-1efe-42f2-9292-a33e60d44de5
```


### Share a file
Now that document.pdf is known by slink, we can refer to it with the filename or UUID and share it
with some recipient. The recipient name can be anything, an email-like address is just an example.

```bash
slink share alice@example.com document.pdf
# Shared document.pdf with alice@example.com:
# http://localhost:8080/KJh8h7G6dT/document.pdf
```

### Show file information
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

### List all files
```bash
slink ls
# +--------------+--------------------------------------+--------------------+---------------+
# | Filename     | UUID                                 | Added              | Active Shares |
# +--------------+--------------------------------------+--------------------+---------------+
# | document.pdf | 09d1cc19-1efe-42f2-9292-a33e60d44de5| 2025-01-23 20:15:30 | 1             |
# +--------------+--------------------------------------+--------------------+---------------+
```

### Remove share
```bash
slink unshare alice@example.com document.pdf
# Removed share for document.pdf from alice@example.com
```

### Remove file
```bash
slink rm document.pdf
# Are you sure you want to remove document.pdf? [y/N] y
# Removed file: document.pdf
```

Force remove without confirmation:
```bash
slink rm -f document.pdf
```

### Multiple files with same name
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
