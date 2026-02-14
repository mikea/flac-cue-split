use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-env-changed=PKG_CONFIG_PATH");

    let wavpack = pkg_config::Config::new()
        .atleast_version("5.9.0")
        .probe("wavpack")
        .unwrap_or_else(|err| {
            panic!(
                "failed to locate native WavPack >= 5.9.0 via pkg-config ({err}). \
Install WavPack 5.9.0 from https://www.wavpack.com/index.html \
and ensure wavpack.pc is visible in PKG_CONFIG_PATH."
            )
        });

    let mut builder = bindgen::Builder::default()
        .header_contents(
            "wavpack-wrapper.h",
            r#"
#if __has_include(<wavpack.h>)
#include <wavpack.h>
#elif __has_include(<wavpack/wavpack.h>)
#include <wavpack/wavpack.h>
#else
#error "wavpack header not found"
#endif
"#,
        )
        .allowlist_function("^Wavpack.*")
        .allowlist_type("^Wavpack.*")
        .allowlist_var("^OPEN_TAGS$")
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()));

    for include in wavpack.include_paths {
        builder = builder.clang_arg(format!("-I{}", include.display()));
    }

    let bindings = builder
        .generate()
        .expect("failed to generate WavPack bindings");

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR is not set"));
    bindings
        .write_to_file(out_dir.join("wavpack_bindings.rs"))
        .expect("failed to write WavPack bindings");
}
