fn main() -> anyhow::Result<()> {
    #[cfg(windows)]
    {
        windows_capture::pipeline::cli()
    }
    #[cfg(not(windows))]
    {
        anyhow::bail!("Windows capture backend required; Linux capture is not implemented yet")
    }
}
