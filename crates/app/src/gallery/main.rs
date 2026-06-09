//! Entry point for the standalone component gallery (`just gallery`). The shell
//! and registry live in `rift_app::gallery`, gated behind the `gallery` feature.

fn main() {
    rift_app::gallery::run();
}
