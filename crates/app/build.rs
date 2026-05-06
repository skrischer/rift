fn main() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os == "windows" {
        embed_resource::compile("resources/windows/rift.rc", embed_resource::NONE)
            .manifest_required()
            .expect("failed to embed Windows manifest");
    }
}
