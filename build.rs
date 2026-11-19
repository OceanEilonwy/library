//! Makes sure `player/dist` exists so the Player can be embedded even before
//! it has been built (the site then says how to build it), and rebuilds the
//! server when a new Player build lands there.

fn main() {
    let dist = std::path::Path::new("player/dist");
    if !dist.exists() {
        // Best effort: rust-embed also tolerates the folder being missing.
        let _ = std::fs::create_dir_all(dist);
    }
    println!("cargo:rerun-if-changed=player/dist");
    println!("cargo:rerun-if-changed=build.rs");
}
