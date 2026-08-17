# maclean

Finds reclaimable disk space on macOS. Run it in a terminal and you get a TUI. Pass a subcommand if you need it in a script.

I built this for machines with a lot of project folders — `node_modules`, `target`, Docker images, that kind of thing. Anyone trying to free space on a Mac can use it. It is a power-user tool. It will not run as root. If something needs a password, it asks once, for that action only.

macOS only.

## Install

Needs a Rust toolchain.

    cargo install --git https://github.com/kirkl4nd/maclean

Once it is on crates.io:

    cargo install maclean

Homebrew is not set up yet. When it is, the formula should run `maclean uninstall` before it deletes the binary, so launchd jobs do not get left behind.

## Uninstall

Do this first. `cargo uninstall` only removes the binary. It does not know about launchd.

    maclean uninstall
    cargo uninstall maclean

`maclean uninstall` only removes jobs maclean created. Those plists live in `~/Library/LaunchAgents` and are tagged (`com.maclean.job.*` plus a `MacleanSchema` / `MacleanManaged` key, or the older "Managed by maclean" comment). Anything else in LaunchAgents is left alone.

Config and logs stay unless you ask:

    maclean uninstall --purge-data

## Usage

    maclean                          # TUI
    maclean scan
    maclean reclaim cargo:foo --yes
    maclean schedule add spotify:cache --every 2w
    maclean schedule list

In the TUI, `s` manages scheduled jobs. Do not edit the plist files by hand.

## Config

`~/Library/Application Support/maclean/config.toml`

Missing file means defaults. `maclean config init` writes a starter file with every module listed.

## License

MIT
