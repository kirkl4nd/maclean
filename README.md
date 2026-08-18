# maclean

Finds reclaimable disk space on macOS. Run it in a terminal and you get a TUI. Pass a subcommand if you need it in a script.

I built this for machines with a lot of project folders - `node_modules`, `target`, etc.

macOS only.

## Install

Needs a Rust toolchain.

    cargo install maclean

From git, if you want a specific checkout:

    cargo install --git https://github.com/kirkl4nd/maclean

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
    maclean schedule add cargo:projects --every 1d
    maclean schedule add cargo:projects node:caches --every 1w
    maclean schedule list

In the TUI, `s` opens the schedule manager. From there, `+` creates a job: pick what should run (module actions, not a scan result), then how often. Do not edit the plist files by hand.

Jobs are per-user LaunchAgents. They run while you are logged in. "Every week" means about seven days since the last successful run, not seven days of the Mac staying on.

- Restart or log out does not reset that clock.
- If a run is overdue when you next log in, it runs then.
- Missed weeks are not stacked: three weeks away is one run, not three.
- Sleep: an overdue job usually runs on wake.

## Config

`~/Library/Application Support/maclean/config.toml`

Missing file means defaults. `maclean config init` writes a starter file with every module listed.

## License

MIT
