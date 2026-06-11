fn main() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os == "windows" {
        // WINDOWED selects the stable icon in rift.rc; dev builds embed the
        // monochrome icon so the two channels are visually distinct.
        let macros: &[&str] = if std::env::var_os("CARGO_FEATURE_WINDOWED").is_some() {
            &["WINDOWED"]
        } else {
            &[]
        };
        embed_resource::compile("resources/windows/rift.rc", macros)
            .manifest_required()
            .expect("failed to embed Windows resources");
    }
}
