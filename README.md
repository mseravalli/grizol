# Grizol

Grizol is a [syncthing](https://github.com/syncthing/syncthing) compatible client
that can be used for affordable self hosting of backups.

Grizol allows to automatically sync your data to multiple
[rclone](https://rclone.org/) compatible backends to provide redundancy at
resiliency. The data can be also accessed locally through a FUSE filesystem for
lower latency. This combination allows to store large amounts of data at a
lower cost than it would require if the data was stored purely on disk: 1TB of
S3 compatible storage costs around 7 dollars/month, while 1TB for a HDD on a cloud
provider starts around 40 dollars/month.

## Development

The source of truth is at https://gitlab.com/com.marcoseravalli/grizol/.

Install the following dependencies.

```shell
sudo apt install -y protobuf-compiler bats bats-assert
cargo install cargo-watch
cargo install sqlx-cli
```


Export a variable that will set the development config:

```shell
export GRIZOL_CONFIG_TYPE="dev_dbg" 
```

Run the following commands:

```shell
./scripts/reset_db.sh
```

Then start the development instance with 

```shell
./scripts/run.sh run
```

I prefer to run a custom version of grizol with the following changes in `lib/protocol/protocol.go`:
* `MinBlockSize = 8 << MiB`
* `DesiredPerFileBlocks = 1`
