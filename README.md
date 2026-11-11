# Kasma

A multi-connection download accelerator for the command line, written in Rust.

Kasma is also available as an engine option for [Brisk](https://github.com/BrisklyDev/brisk).

## :rocket: Features

- Dynamic range splitting: idle connections take over work from slow ones
- Pause and resume, including across runs: quit, run the same command again, and it continues
- Automatic reset of stalled connections, with configurable timeout and retries
- Download speed limit 

## :package: Installation

### :snowflake: Nix / NixOS

Run without installing:

```sh
nix run github:BrisklyDev/kasma -- <URL>
```

Install into your profile:

```sh
nix profile install github:BrisklyDev/kasma
```

On NixOS, add the flake as an input and use its overlay:

```nix
{
  inputs.kasma.url = "github:BrisklyDev/kasma";

  outputs = { nixpkgs, kasma, ... }: {
    nixosConfigurations.myhost = nixpkgs.lib.nixosSystem {
      modules = [
        { nixpkgs.overlays = [ kasma.overlays.default ]; }
        ({ pkgs, ... }: { environment.systemPackages = [ pkgs.kasma ]; })
        # ...
      ];
    };
  };
}
```

### :crab: Cargo

Requires Rust 1.85 or newer (edition 2024), plus `cmake` and a C compiler for `aws-lc-sys`.

```sh
cargo install --git https://github.com/BrisklyDev/kasma
```

### :hammer_and_wrench: From source

```sh
git clone https://github.com/BrisklyDev/kasma
cd kasma
cargo build --release
./target/release/kasma --help
```

## :computer: Usage

```
kasma [OPTIONS] <URL>
```

| Option | Description | Default |
| --- | --- | --- |
| `-o, --output-dir <DIR>` | Directory to save the file in | current directory |
| `-n, --name <NAME>` | Output file name | detected from the server |
| `-c, --connections <N>` | Maximum number of connections (1-64) | `8` |
| `-l, --limit <RATE>` | Total speed limit in bytes/sec, e.g. `500K`, `2M`, `1.5M` | none |
| `--temp-dir <DIR>` | Directory for temporary files | `$XDG_CACHE_HOME/kasma` or `~/.cache/kasma` |
| `--timeout <SECS>` | Seconds without data before a connection is reset | `15` |
| `--retries <N\|inf>` | Max consecutive retries per connection | `10` |
| `--log` | Write engine logs to `<temp-dir>/logs` | off |
| `-q, --quiet` | Don't show the progress bar | off |
| `-h, --help` | Print help | |
| `-V, --version` | Print version | |

Speed limit units are decimal: `K` = 1,000, `M` = 1,000,000, `G` = 1,000,000,000 bytes/sec.

### :keyboard: Keys

In an interactive terminal:

| Key | Action |
| --- | --- |
| `p` / `Space` | Pause / resume |
| `q` / `Ctrl+C` | Quit. Progress is kept. |

### :bulb: Examples

Download with 16 connections into `~/Downloads`:

```sh
kasma -c 16 -o ~/Downloads https://example.com/file.iso
```

Limit the total speed to 2 MB/s and save under a different name:

```sh
kasma -l 2M -n debian.iso https://example.com/file.iso
```

Retry forever on an unreliable connection:

```sh
kasma --retries inf --timeout 30 https://example.com/file.iso
```

## :scroll: License

[MIT](LICENSE)
