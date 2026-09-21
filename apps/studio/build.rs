//! Guarantees `web/dist` exists before `rust-embed` looks for it.
//!
//! The page is built by `pnpm build`, which a fresh clone has not run —
//! and a missing folder is a compile error rather than an empty bundle.
//! Failing `cargo build` on a checkout that has never seen node would
//! make the Rust side hostage to the JavaScript one, so a placeholder
//! stands in and says what to run.

fn main() {
    println!("cargo:rerun-if-changed=web/dist");

    let dist = std::path::Path::new("web/dist");
    if dist.join("index.html").exists() {
        return;
    }

    if std::fs::create_dir_all(dist).is_ok() {
        let _ = std::fs::write(
            dist.join("index.html"),
            "<!doctype html><meta charset=\"utf-8\"><title>kobune studio</title>\
             <body style=\"font:14px ui-monospace,monospace;padding:2rem\">\
             The dashboard was not built. Run <code>pnpm install &amp;&amp; pnpm build</code> \
             in <code>apps/studio/web</code>, then rebuild.</body>",
        );
    }
}
