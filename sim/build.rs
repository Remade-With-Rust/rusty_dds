//! Links the DirectXTex shim when the `dxtex` feature is on.
//!
//! It tries to build `shim/` with CMake automatically, and if that fails it says
//! exactly what to run rather than failing obscurely — the DirectXTex build
//! wants an MSVC environment, which a plain shell may not have.

use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=shim/dxtex_provider.cpp");
    println!("cargo:rerun-if-changed=shim/dxtex_provider.h");
    println!("cargo:rerun-if-changed=shim/CMakeLists.txt");
    println!("cargo:rerun-if-changed=shaders/quad.wgsl");

    #[cfg(feature = "vulkan-shaders")]
    compile_spirv();

    if std::env::var("CARGO_FEATURE_DXTEX").is_err() {
        return;
    }

    let shim = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("shim");
    let build = shim.join("build");
    let libs = build.join("lib");

    // Always ask CMake to build. Gating this on "does the .lib exist" meant an
    // edit to dxtex_provider.cpp silently did nothing — the rerun-if-changed
    // above would re-run this script, and this script would then skip the build
    // because a stale library was sitting there. A benchmark harness that
    // quietly measures last week's peer code is worse than one that fails.
    // CMake's own build is incremental, so this costs nothing when nothing moved.
    try_cmake(&shim, &build);
    if !have_libs(&libs) {
        panic!(
            "\n\nThe DirectXTex shim is not built.\n\
             From an **x64 Native Tools / vcvars64** shell, run:\n\n  \
             cmake -S {} -B {} -G Ninja -DCMAKE_BUILD_TYPE=Release\n  \
             cmake --build {}\n\n\
             Then rebuild. (Or drop the `dxtex` feature to build without the peer arm.)\n",
            shim.display(),
            build.display(),
            build.display()
        );
    }

    println!("cargo:rustc-link-search=native={}", libs.display());
    println!("cargo:rustc-link-lib=static=dxtex_provider");
    println!("cargo:rustc-link-lib=static=DirectXTex");
    // DirectXTex's WIC paths pull these in even when only DDS is used.
    for lib in ["ole32", "oleaut32", "uuid", "windowscodecs"] {
        println!("cargo:rustc-link-lib=dylib={lib}");
    }
}

fn have_libs(libs: &Path) -> bool {
    libs.join("dxtex_provider.lib").exists() && libs.join("DirectXTex.lib").exists()
}

fn try_cmake(shim: &Path, build: &Path) {
    let configure = std::process::Command::new("cmake")
        .args(["-S", &shim.display().to_string()])
        .args(["-B", &build.display().to_string()])
        .args(["-G", "Ninja", "-DCMAKE_BUILD_TYPE=Release"])
        .status();
    if !matches!(configure, Ok(s) if s.success()) {
        return;
    }
    let _ = std::process::Command::new("cmake")
        .args(["--build", &build.display().to_string()])
        .status();
}

/// WGSL -> SPIR-V with naga, at build time.
///
/// Pure Rust: this box has `vulkan-1.dll` but no Vulkan SDK, so `glslc`, `dxc`
/// and `glslangValidator` are all absent. Compiling here rather than at runtime
/// means a broken shader fails the build instead of the demo.
#[cfg(feature = "vulkan-shaders")]
fn compile_spirv() {
    let src_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("shaders/quad.wgsl");
    let src = std::fs::read_to_string(&src_path)
        .unwrap_or_else(|e| panic!("reading {}: {e}", src_path.display()));

    let module = naga::front::wgsl::parse_str(&src)
        .unwrap_or_else(|e| panic!("WGSL parse failed: {}", e.emit_to_string(&src)));

    // PUSH_CONSTANT is not in the default capability set; the MVP lives there.
    let mut validator = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::PUSH_CONSTANT,
    );
    let info = validator
        .validate(&module)
        .unwrap_or_else(|e| panic!("WGSL validation failed: {e:?}"));

    let options = naga::back::spv::Options {
        lang_version: (1, 0),
        ..Default::default()
    };
    // No pipeline options: emit every entry point into one module, so the
    // viewport creates a single VkShaderModule and names the stages.
    let words = naga::back::spv::write_vec(&module, &info, &options, None)
        .unwrap_or_else(|e| panic!("SPIR-V generation failed: {e}"));

    let mut bytes = Vec::with_capacity(words.len() * 4);
    for w in words {
        bytes.extend_from_slice(&w.to_le_bytes());
    }
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR")).join("quad.spv");
    std::fs::write(&out, &bytes).unwrap_or_else(|e| panic!("writing {}: {e}", out.display()));
}
