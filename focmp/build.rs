use std::{env, fs, path::PathBuf};

fn main() {
    // A deliberately absent input makes Cargo run this pre-build hook every
    // invocation, including when no Rust source has changed. Cargo serializes
    // builds sharing its target directory.
    println!("cargo:rerun-if-changed=.always-bump-build-version");
    println!("cargo:rerun-if-env-changed=FOCMP_RELEASE_VERSION");
    let path = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("BUILD_VERSION");
    let version = match env::var("FOCMP_RELEASE_VERSION") {
        Ok(version) => {
            // A release tag is exact, including x.y.0; local builds still increment.
            let parts: Vec<u64> = version.split('.').map(|v| v.parse().expect("invalid FOCMP_RELEASE_VERSION")).collect();
            assert_eq!(parts.len(), 3, "FOCMP_RELEASE_VERSION must be MAJOR.MINOR.PATCH");
            assert_eq!(version, format!("{}.{}.{}", parts[0], parts[1], parts[2]), "non-canonical release version");
            version
        }
        Err(env::VarError::NotPresent) => {
            let old = fs::read_to_string(&path).unwrap_or_else(|_| "0.1.0".into());
            let mut parts: Vec<u64> = old.trim().split('.').map(|v| v.parse().expect("invalid BUILD_VERSION")).collect();
            assert_eq!(parts.len(), 3);
            parts[2] = parts[2].checked_add(1).expect("build version overflow");
            format!("{}.{}.{}", parts[0], parts[1], parts[2])
        }
        Err(e) => panic!("invalid FOCMP_RELEASE_VERSION: {e}"),
    };
    fs::write(&path, format!("{version}\n")).expect("write BUILD_VERSION");
    println!("cargo:rustc-env=FOCMP_BUILD_VERSION={version}");
}
