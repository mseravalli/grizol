CREATE TABLE IF NOT EXISTS bep_folders
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
    type INTEGER NOT NULL ,
    primary key (type)
);

CREATE TABLE IF NOT EXISTS bep_devices
(
    -- TODO: this should be removed so that the NxM is expressed in the index
    folder                     TEXT    NOT NULL,

    id                         TEXT    NOT NULL,
    name                       TEXT    NOT NULL,
    addresses                  TEXT    NOT NULL,
    compression                INTEGER NOT NULL,
    cert_name                  TEXT    NOT NULL,
    max_sequence               INTEGER NOT NULL,
    introducer                 INTEGER NOT NULL,
    index_id                   BLOB    NOT NULL,
    skip_introduction_removals INTEGER NOT NULL,
    encryption_password_token  BLOB    NOT NULL,

    PRIMARY KEY(id),
    FOREIGN KEY(folder)      REFERENCES bep_folders(id),
    FOREIGN KEY(compression) REFERENCES bep_compression(type)
);

CREATE TABLE IF NOT EXISTS bep_index
(
    device TEXT NOT NULL ,

    folder TEXT NOT NULL ,

    PRIMARY KEY (folder, device),
    FOREIGN KEY(device) REFERENCES bep_devices(id),
    FOREIGN KEY(folder) REFERENCES bep_folders(id)
);


CREATE TABLE IF NOT EXISTS bep_file_info_type (
    type INTEGER NOT NULL ,
    PRIMARY KEY (type)
);

CREATE TABLE IF NOT EXISTS bep_storage_status (
    storage_status INTEGER NOT NULL ,
    PRIMARY KEY (storage_status)
);

CREATE TABLE IF NOT EXISTS bep_file_info
(
    folder         TEXT    NOT NULL ,
    device         TEXT    NOT NULL ,

    name           TEXT    NOT NULL ,
    type           INTEGER NOT NULL ,
    size           INTEGER NOT NULL ,
    permissions    INTEGER NOT NULL ,
    modified_s     INTEGER NOT NULL ,
    modified_ns    INTEGER NOT NULL ,
    modified_by    BLOB    NOT NULL ,
    deleted        INTEGER NOT NULL ,
    invalid        INTEGER NOT NULL ,
    no_permissions INTEGER NOT NULL ,
    sequence       INTEGER NOT NULL ,
    block_size     INTEGER NOT NULL ,
    symlink_target TEXT    NOT NULL ,

    PRIMARY KEY(folder, device, name) ,
    FOREIGN KEY(folder, device) REFERENCES bep_index(folder, device),
    FOREIGN KEY(type)   REFERENCES bep_file_info_type(type),
    UNIQUE(sequence)
);

CREATE TABLE IF NOT EXISTS bep_file_location
(
    loc_folder      TEXT    NOT NULL ,
    loc_device      TEXT    NOT NULL ,
    loc_name        TEXT    NOT NULL ,

    storage_backend TEXT    NOT NULL ,
    location        TEXT    NOT NULL ,

    PRIMARY KEY(loc_folder, loc_device, loc_name, storage_backend, location) ,
    FOREIGN KEY(loc_name, loc_folder, loc_device) REFERENCES bep_file_info(name, folder, device) ON DELETE CASCADE ON UPDATE CASCADE 
);

-- Using a differnt table from bep_file_location as this table is expected to
-- have a much smaller number of entries and a much higher turnover.
-- The files in the local cache will be read_only.
CREATE TABLE IF NOT EXISTS bep_local_cache
(
    cache_folder      TEXT    NOT NULL ,
    cache_device      TEXT    NOT NULL ,
    cache_file_name   TEXT    NOT NULL ,

    -- unix timestamp in milliseconds for when this file was added to the cache
    timestamp_added   INTEGER NOT NULL ,
    -- TODO: think about further possible attributes that could be tracked for
    -- better cache efficiency, e.g. last_accessed, number of accesses,
    -- timestamp of every access, other.

    PRIMARY KEY(cache_folder, cache_device, cache_file_name) ,
    FOREIGN KEY(cache_folder, cache_device, cache_file_name) REFERENCES bep_file_info(folder, device, name) ON DELETE CASCADE ON UPDATE CASCADE 
);

CREATE TABLE IF NOT EXISTS bep_block_info (
    file_name   TEXT    NOT NULL ,
    file_folder TEXT    NOT NULL ,
    file_device TEXT    NOT NULL ,

    offset    INTEGER NOT NULL ,
    bi_size   INTEGER NOT NULL ,
    hash      BLOB    NOT NULL ,
    weak_hash INTEGER ,

    storage_status  INTEGER NOT NULL,

    PRIMARY KEY (file_name, file_folder, file_device, offset, bi_size, hash) ,
    FOREIGN KEY(file_name, file_folder, file_device) REFERENCES bep_file_info(name, folder, device) ON DELETE CASCADE ON UPDATE CASCADE ,
    FOREIGN KEY(storage_status) REFERENCES bep_storage_status(storage_status)
);

CREATE TABLE IF NOT EXISTS bep_file_version (
    file_name   TEXT    NOT NULL ,  
    file_folder TEXT    NOT NULL ,  
    file_device TEXT    NOT NULL ,  

    id        BLOB    NOT NULL ,
    value     INTEGER NOT NULL , -- we will need to put a u64 into a i64 the assumption is that there won't be overflows.

    PRIMARY KEY (file_name, file_folder, file_device, id, value) ,
    FOREIGN KEY(file_name, file_folder, file_device) REFERENCES bep_file_info(name, folder, device) ON DELETE CASCADE ON UPDATE CASCADE
);

INSERT INTO bep_compression VALUES( 0 );
INSERT INTO bep_compression VALUES( 1 );
INSERT INTO bep_compression VALUES( 2 );

INSERT INTO bep_storage_status VALUES( 0 ); -- not stored
INSERT INTO bep_storage_status VALUES( 1 ); -- stored locally

INSERT INTO bep_file_info_type VALUES( 0 ); -- 'FILE'             
INSERT INTO bep_file_info_type VALUES( 1 ); -- 'DIRECTORY'        
INSERT INTO bep_file_info_type VALUES( 2 ); -- 'SYMLINK_FILE'     
INSERT INTO bep_file_info_type VALUES( 3 ); -- 'SYMLINK_DIRECTORY'
INSERT INTO bep_file_info_type VALUES( 4 ); -- 'SYMLINK'          
