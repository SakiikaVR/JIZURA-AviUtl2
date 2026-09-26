fn main() -> anyhow::Result<()> {
    jizura_aviutl2::export_catalog(std::path::Path::new(env!("CARGO_MANIFEST_DIR")))
}
