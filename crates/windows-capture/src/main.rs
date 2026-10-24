fn main() -> anyhow::Result<()> {
    #[cfg(windows)]
    {
        rcr_windows::pipeline::cli()
    }
    #[cfg(not(windows))]
    {
        anyhow::bail!("Windows capture backend required; Linux capture is not implemented yet")
    }
}
