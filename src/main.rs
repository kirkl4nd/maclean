fn main() -> anyhow::Result<()> {
    if maclean::running_as_root() {
        anyhow::bail!(
            "maclean refuses to run as root. Start it as yourself; actions that need a password will ask for one, once, for that command only."
        );
    }
    maclean::run()
}
