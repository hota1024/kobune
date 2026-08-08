//! Repository chores that do not belong in the product.
//!
//! Run with `cargo xtask <command>`.

use std::path::{Path, PathBuf};

mod docs;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();

    let result = match args.as_slice() {
        ["docs", "snapshot", version] => docs::snapshot(&repo_root(), version),
        ["docs", "snapshot"] => Err("usage: cargo xtask docs snapshot <version>".into()),
        _ => {
            usage();
            std::process::exit(2);
        }
    };

    if let Err(err) = result {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}

fn usage() {
    eprintln!("usage: cargo xtask <command>");
    eprintln!();
    eprintln!("commands:");
    eprintln!("  docs snapshot <version>   Freeze the current docs as /v<version>/");
}

/// The repository root.
///
/// `CARGO_MANIFEST_DIR` points at `xtask/`, and the root is its parent. This
/// holds however the command was invoked, unlike the working directory.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask/ always has a parent")
        .to_path_buf()
}
