# Grizol a [syncthing](https://github.com/syncthing/syncthing) compatible client.

The source of truth is at https://gitlab.com/com.marcoseravalli/grizol/.

## Developer Setup

Install the following dependencies.

```shell
sudo apt install -y protobuf-compiler bats bats-assert
cargo install cargo-watch
cargo install sqlx-cli
```

Run the following commands:

```shell
./scripts/reset_db.sh
```
