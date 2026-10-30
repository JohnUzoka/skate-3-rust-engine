//! Links libcurl: devkitPro's switch-curl for the Switch, the system's with `system-curl`.
use std::{env, path::Path};

/// Where devkitPro's portlibs are in bevy-horizon's toolchain image.
const PORTLIBS: &str = "/opt/devkitpro/portlibs/switch/lib";

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("horizon") {
        // switch-curl's TLS goes through libnx's ssl service; bevy_horizon's build script links
        // libnx and newlib.
        assert!(
            Path::new(PORTLIBS).join("libcurl.a").is_file(),
            "{PORTLIBS}/libcurl.a is missing: this toolchain image was built before its Containerfile \
             installed switch-curl. Rebuild it with `docker build -t bevy-horizon-toolchain:local \
             -f Containerfile .` in bevy-horizon"
        );
        println!("cargo:rustc-link-search=native={PORTLIBS}");
        println!("cargo:rustc-link-lib=curl");
        println!("cargo:rustc-link-lib=z");
    } else if env::var_os("CARGO_FEATURE_SYSTEM_CURL").is_some() {
        println!("cargo:rustc-link-lib=curl");
    }
}
