fn main() -> anyhow::Result<()> {
    #[cfg(all(target_os = "macos", feature = "apple-adapters"))]
    fileid_engine::apple::run_hosted();
    #[cfg(not(all(target_os = "macos", feature = "apple-adapters")))]
    fileid_engine::server::run()
}
