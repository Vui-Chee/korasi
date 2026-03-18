# korasi

[![Build status](https://img.shields.io/github/actions/workflow/status/vui-chee/korasi/ci.yml)](https://github.com/vui-chee/korasi/actions)
[![Crates.io](https://img.shields.io/crates/v/korasi-cli.svg)](https://crates.io/crates/korasi-cli)

A CLI tool (and `cargo` plugin) for spinning up and managing AWS EC2 instances so you can run hardware-specific code — AVX intrinsics, CUDA, ARM, etc. — without owning the hardware. Upload your code, run it, get stdout back. Pay only for what you use; you own the infrastructure.

> **Warning:** This tool is early-stage. Use at your own risk.

## Prerequisites

- AWS credentials configured at `~/.aws/credentials`
- Rust toolchain (for installation from source)

## Installation

### Standalone CLI

```sh
cargo install korasi-cli
```

### Cargo plugin

Also installed by the above. Invoke as:

```sh
cargo korasi <subcommand>
```

## Usage

```
korasi [OPTIONS] <SUBCOMMAND>
```

### Global Options

| Flag        | Default                  | Description                                 |
|-------------|--------------------------|---------------------------------------------|
| `--profile` | `default`                | AWS credentials profile                     |
| `--region`  | `ap-southeast-1`         | AWS region                                  |
| `--tag`     | `hpc-launcher`           | Tag applied to all managed resources        |
| `--setup`   | `start_up.sh`            | Path to a shell script run at instance boot |
| `--ssh-key` | `~/.ssh/ec2-ssh-key.pem` | Path to Ed25519 SSH private key             |
| `-d`        | `false`                  | Enable debug logging                        |

### Subcommands

#### `create` — Launch a new instance

```sh
korasi create ami-0abcdef1234567890
# Prompts interactively to select machine type (e.g. c5.xlarge, p3.2xlarge)
```

#### `list` — List managed instances

```sh
korasi list   # alias: korasi ls
```

#### `start` / `stop` — Start or stop instances

```sh
korasi start            # multi-select from stopped instances
korasi stop             # multi-select from running instances
korasi stop --wait      # block until fully stopped
```

#### `delete` — Terminate instances

```sh
korasi delete           # interactive multi-select
korasi delete --wait    # block until terminated
```

#### `upload` — Copy local files to a remote instance via SFTP

```sh
korasi upload                           # uploads CWD to $HOME on remote
korasi upload ./src /home/ubuntu/app    # explicit src and dst
```

> Respects `.gitignore` during upload.

#### `run` — Execute a remote command

```sh
korasi run -- cargo build --release
korasi r   -- ./benchmark
```

#### `shell` — Open an interactive SSH session

```sh
korasi shell                    # alias: korasi sh
korasi shell --user ec2-user    # for Amazon Linux AMIs
```

#### `obliterate` — Tear down all resources

```sh
korasi obliterate
```

Deletes all managed instances, security groups, and key pairs. Does not remove IAM permissions.

## How it works

On first run, korasi auto-creates:

- An Ed25519 SSH key pair, saved to `~/.ssh/ec2-ssh-key.pem`
- A security group that whitelists your current public IP on port 22

Your IP is refreshed automatically before any SSH, upload, or run operation, so the tool works seamlessly across different networks.

