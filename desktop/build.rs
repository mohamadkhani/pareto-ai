//! Places the gateway binary where Tauri's bundler expects the sidecar.
//!
//! `bundle.externalBin` in `tauri.conf.json` lists `binaries/pareto-gateway`
//! with no target-triple suffix; Tauri appends `-<target-triple>` (and `.exe`
//! on Windows) when it looks the file up. The gateway is built from `server/`
//! as a separate crate, so this script copies the already-built binary into
//! place (`desktop/binaries/`, git-ignored) instead of nesting the servers'
//! build inside this crate.
//!
//! Resolution order for the source binary:
//! 1. `PARETO_GATEWAY_BIN` — an explicit path (CI uses this).
//! 2. `server/target/<target>/<profile>/gateway[.exe]` — cross/target builds.
//! 3. `server/target/<profile>/gateway[.exe]` — a plain host build.
//! 4. The same two locations in the other profile (debug ↔ release).
//!
//! A missing gateway is a warning, not an error, so `cargo check` works on a
//! fresh checkout. `tauri build` then fails in the bundling phase with Tauri's
//! own "binary not found" message; run `just desktop-sidecar` first.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=PARETO_GATEWAY_BIN");

    let target = env::var("TARGET").unwrap_or_default();
    let profile = env::var("PROFILE").unwrap_or_else(|_| "debug".to_string());

    // The shell needs the triple at runtime too, to find the dev binary in
    // `binaries/` before Tauri has bundled anything.
    println!("cargo:rustc-env=PARETO_TARGET_TRIPLE={target}");

    if let Err(message) = place_sidecar(&target, &profile) {
        println!("cargo:warning={message}");
    }

    tauri_build::build();
}

/// Copies the gateway to `binaries/pareto-gateway-<triple>[.exe]`; returns a
/// message describing why the copy was skipped when no source was found.
fn place_sidecar(target: &str, profile: &str) -> Result<(), String> {
    let exe = if target.contains("windows") {
        ".exe"
    } else {
        ""
    };
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".into()));
    let out_dir = manifest_dir.join("binaries");
    let dest = out_dir.join(format!("pareto-gateway-{target}{exe}"));

    let Some(source) = gateway_source(&manifest_dir, target, profile, exe) else {
        return Err(format!(
            "gateway sidecar not found for {target} ({profile}); \
             build it first (`just desktop-sidecar`) or set PARETO_GATEWAY_BIN. \
             Bundling will fail until {} exists",
            dest.display()
        ));
    };

    let up_to_date = match (fs::metadata(&source), fs::metadata(&dest)) {
        (Ok(src), Ok(dst)) => src.len() == dst.len() && src.modified().ok() <= dst.modified().ok(),
        _ => false,
    };
    if !up_to_date {
        fs::create_dir_all(&out_dir)
            .map_err(|e| format!("cannot create {}: {e}", out_dir.display()))?;
        fs::copy(&source, &dest).map_err(|e| {
            format!(
                "cannot copy {} to {}: {e}",
                source.display(),
                dest.display()
            )
        })?;
    }

    println!("cargo:rerun-if-changed={}", source.display());
    if !up_to_date {
        println!(
            "cargo:warning=sidecar: copied {} to {}",
            source.display(),
            dest.display()
        );
    }
    Ok(())
}

/// Finds the gateway binary, honouring `PARETO_GATEWAY_BIN` first.
fn gateway_source(manifest_dir: &Path, target: &str, profile: &str, exe: &str) -> Option<PathBuf> {
    if let Some(explicit) = env::var_os("PARETO_GATEWAY_BIN") {
        let path = PathBuf::from(explicit);
        return path.is_file().then_some(path);
    }

    let server_target = manifest_dir.join("../server/target");
    let other_profile = if profile == "release" {
        "debug"
    } else {
        "release"
    };
    [
        server_target
            .join(target)
            .join(profile)
            .join(format!("gateway{exe}")),
        server_target.join(profile).join(format!("gateway{exe}")),
        server_target
            .join(target)
            .join(other_profile)
            .join(format!("gateway{exe}")),
        server_target
            .join(other_profile)
            .join(format!("gateway{exe}")),
    ]
    .into_iter()
    .find(|candidate| candidate.is_file())
}
