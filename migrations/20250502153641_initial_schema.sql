-- Define the initial database schema

CREATE TABLE IF NOT EXISTS files (
    uuid CHAR(36) NOT NULL PRIMARY KEY,
    filename TEXT NOT NULL,
    date_added DATETIME NOT NULL
);

CREATE TABLE IF NOT EXISTS shares (
    uuid CHAR(36) NOT NULL,
    recipient TEXT NOT NULL,
    share_hash TEXT NOT NULL,
    date_shared DATETIME NOT NULL,
    date_removed DATETIME,
    active BOOLEAN NOT NULL DEFAULT 1,
    expires_at DATETIME NULL,                     -- When the share expires (NULL for no expiry)
    delete_file_on_expiry BOOLEAN NOT NULL DEFAULT 0, -- Delete file when this share expires?
    PRIMARY KEY (uuid, recipient),
    FOREIGN KEY (uuid) REFERENCES files(uuid)
);
