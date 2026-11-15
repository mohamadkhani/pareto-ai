//! Build-time checks.
//!
//! The embedded UI needs the Angular build output to exist before the crate
//! compiles. Failing here with an actionable message is better than a cryptic
//! `rust-embed` error deep inside a macro.

fn main() {
    println!("cargo:rerun-if-changed=../web/dist/web");
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_EMBEDDED_UI");

    let embedded = std::env::var_os("CARGO_FEATURE_EMBEDDED_UI").is_some();
    if !embedded {
        return;
    }

    let index = std::path::Path::new("../web/dist/web/browser/index.html");
    if !index.exists() {
        panic!(
            "\n\nThe embedded UI needs the Angular build output, but {} is missing.\n\
             Build it first:      cd web && npm run build\n\
             Or skip the UI:      cargo build --no-default-features   (api-only binary)\n",
            index.display()
        );
    }
}
