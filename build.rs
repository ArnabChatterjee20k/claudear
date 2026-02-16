use std::path::Path;

fn main() {
    let dashboard_dist = Path::new("dashboard/dist");

    // Ensure the directory exists so rust-embed doesn't fail at compile time.
    // When building without a pre-built dashboard, this creates an empty dir
    // and the binary will run in API-only mode.
    if !dashboard_dist.exists() {
        std::fs::create_dir_all(dashboard_dist).expect("Failed to create dashboard/dist directory");
        println!("cargo:warning=dashboard/dist not found — created empty directory. Dashboard will not be embedded.");
    } else if !dashboard_dist.join("index.html").exists() {
        println!(
            "cargo:warning=dashboard/dist/index.html not found. Dashboard will not be embedded."
        );
    }

    println!("cargo:rerun-if-changed=dashboard/dist");
}
