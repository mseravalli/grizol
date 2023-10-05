CREATE TABLE IF NOT EXISTS bep_folder
(
    id                   TEXT    NOT NULL ,
    label                TEXT    NOT NULL ,
    read_only            INTEGER NOT NULL ,
    ignore_permissions   INTEGER NOT NULL ,
    ignore_delete        INTEGER NOT NULL ,
    disable_temp_indexes INTEGER NOT NULL ,
    paused               INTEGER NOT NULL ,

    PRIMARY KEY (id)
);


CREATE TABLE IF NOT EXISTS bep_compression
(
    type TEXT NOT NULL ,
    primary key (type)
);

CREATE TABLE IF NOT EXISTS bep_device
(
    folder                     TEXT    NOT NULL,

    id                         TEXT    NOT NULL,
    name                       TEXT    NOT NULL,
    addresses                  TEXT    NOT NULL,
    compression                TEXT    NOT NULL,
    cert_name                  TEXT    NOT NULL,
    max_sequence               INTEGER NOT NULL,
    introducer                 INTEGER NOT NULL,
    index_id                   INTEGER NOT NULL,
    skip_introduction_removals INTEGER NOT NULL,
    encryption_password_token  BLOB    NOT NULL,

    PRIMARY KEY(id),
    FOREIGN KEY(folder)      REFERENCES bep_folder(id),
    FOREIGN KEY(compression) REFERENCES bep_compression(type)
);

CREATE TABLE IF NOT EXISTS bep_index
(
    device TEXT NOT NULL ,

    folder TEXT NOT NULL ,

    PRIMARY KEY (folder, device),
    FOREIGN KEY(device) REFERENCES bep_device(id),
    FOREIGN KEY(folder) REFERENCES bep_folder(id)
);


CREATE TABLE IF NOT EXISTS bep_file_info_type (
    type TEXT NOT NULL ,
    PRIMARY KEY (type)
);

CREATE TABLE IF NOT EXISTS bep_file_info
(
    folder         TEXT    NOT NULL ,

    name           TEXT    NOT NULL ,
    type           TEXT    NOT NULL ,
    size           INTEGER NOT NULL ,
    permissions    INTEGER NOT NULL ,
    modified_s     INTEGER NOT NULL ,
    modified_ns    INTEGER NOT NULL ,
    modified_by    INTEGER NOT NULL ,
    deleted        INTEGER NOT NULL ,
    invalid        INTEGER NOT NULL ,
    no_permissions INTEGER NOT NULL ,
    sequence       INTEGER NOT NULL ,
    block_size     INTEGER NOT NULL ,
    symlink_target TEXT    NOT NULL ,

    PRIMARY KEY(folder, name) ,
    FOREIGN KEY(folder) REFERENCES bep_index(folder) ,
    FOREIGN KEY(type)   REFERENCES bep_file_info_type(type)
);

CREATE TABLE IF NOT EXISTS bep_block_info (
    file_name TEXT    NOT NULL ,  

    offset    INTEGER NOT NULL ,
    size      INTEGER NOT NULL ,
    hash      BLOB    NOT NULL ,
    weak_hash INTEGER ,

    PRIMARY KEY (file_name, offset, size, hash) ,
    FOREIGN KEY(file_name) REFERENCES bep_file_info(name)
);

CREATE TABLE IF NOT EXISTS bep_file_version (
    file_name TEXT    NOT NULL ,  

    id        INTEGER NOT NULL ,
    value     INTEGER NOT NULL ,

    PRIMARY KEY (file_name, id, value) ,
    FOREIGN KEY(file_name) REFERENCES bep_file_info(name)
);

INSERT INTO bep_compression VALUES( 'METADATA' );
INSERT INTO bep_compression VALUES( 'NEVER'    );
INSERT INTO bep_compression VALUES( 'ALWAYS'   );

INSERT INTO bep_file_info_type VALUES( 'FILE'              );
INSERT INTO bep_file_info_type VALUES( 'DIRECTORY'         );
INSERT INTO bep_file_info_type VALUES( 'SYMLINK_FILE'      );
INSERT INTO bep_file_info_type VALUES( 'SYMLINK_DIRECTORY' );
INSERT INTO bep_file_info_type VALUES( 'SYMLINK'           );
