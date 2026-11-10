use std::{env, fs, path::PathBuf};
fn main() {
    println!("cargo:rerun-if-env-changed=FOCMP_BUNDLE_DIR");
    let root = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let bundle = env::var_os("FOCMP_BUNDLE_DIR")
        .map(PathBuf::from)
        .unwrap_or(root.join("../dist/bundle"));
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    for name in ["manifest.json", "payload.zip"] {
        let path = bundle.join(name);
        println!("cargo:rerun-if-changed={}", path.display());
        fs::copy(&path, out.join(name)).unwrap_or_else(|e| {
            panic!(
                "Cannot embed {}: {e}. Run scripts/package_release.py first.",
                path.display()
            )
        });
    }
}
