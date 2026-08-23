use std::{env, fs, path::PathBuf};

fn main() {
    for key in [
        "GOOGLE_CLIENT_ID",
        "GOOGLE_CLIENT_SECRET",
        "MICROSOFT_CLIENT_ID",
        "MICROSOFT_CLIENT_SECRET",
        "PEBBLE_OAUTH_REDIRECT_URL",
    ] {
        println!("cargo:rerun-if-env-changed={key}");
    }
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let package_json = manifest_dir
        .parent()
        .expect("src-web must live below the Pebble repository root")
        .join("package.json");
    println!("cargo:rerun-if-changed={}", package_json.display());

    let version = fs::read_to_string(&package_json)
        .ok()
        .and_then(|contents| serde_json::from_str::<serde_json::Value>(&contents).ok())
        .and_then(|value| value.get("version").and_then(|version| version.as_str()).map(str::to_owned))
        .filter(|version| !version.trim().is_empty())
        .unwrap_or_else(|| env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".to_string()));

    println!("cargo:rustc-env=PEBBLE_APP_VERSION={version}");
}
