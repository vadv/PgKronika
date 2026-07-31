//! Extract the committed UI asset tarball into `OUT_DIR` for `rust-embed`.
//!
//! The SPA build output is not tracked in git (it is generated and noisy).
//! Instead the repo pins one deterministic `static.tar.gz` produced by
//! `make web-frontend`. Every build re-extracts it, so debug and release
//! binaries serve the same committed assets.
#![allow(
    clippy::exit,
    reason = "a build script must abort the build when the committed UI assets are missing or corrupt"
)]

use std::path::{Path, PathBuf};
use std::process::exit;

use flate2::read::GzDecoder;
use tar::Archive;

fn fail(message: &str) -> ! {
    eprintln!("pg_kronika-web build.rs: {message}");
    exit(1);
}

fn main() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let tarball = manifest.join("static.tar.gz");
    println!("cargo:rerun-if-changed={}", tarball.display());

    let out_dir: PathBuf = std::env::var("OUT_DIR")
        .map_or_else(|_| fail("OUT_DIR is not set by cargo"), PathBuf::from);
    let target = out_dir.join("static");
    if target.exists()
        && let Err(error) = std::fs::remove_dir_all(&target)
    {
        fail(&format!("cannot clear {}: {error}", target.display()));
    }

    let file = match std::fs::File::open(&tarball) {
        Ok(file) => file,
        Err(error) => fail(&format!(
            "cannot open {}: {error}; run `make web-frontend` to build UI assets",
            tarball.display()
        )),
    };
    if let Err(error) = Archive::new(GzDecoder::new(file)).unpack(&target) {
        fail(&format!(
            "cannot extract {} to {}: {error}",
            tarball.display(),
            target.display()
        ));
    }
}
