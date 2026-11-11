fn main() {
    // On Linux, sherpa-onnx shared libraries are copied next to the binary (and bundled under lib/opstrellis).
    // macOS links sherpa-onnx statically (see Cargo.toml).
    #[cfg(target_os = "linux")]
    println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN:$ORIGIN/../lib/opstrellis");
    #[cfg(target_os = "macos")]
    link_clang_runtime();
    tauri_build::build()
}

/// llama.cpp's Metal code uses `@available`, which needs `___isPlatformVersionAtLeast` from clang's
/// runtime; rustc links with `-nodefaultlibs`, so nothing else brings it in.
#[cfg(target_os = "macos")]
fn link_clang_runtime() {
    let out = std::process::Command::new("clang").arg("--print-resource-dir").output().expect("running clang --print-resource-dir");
    assert!(out.status.success(), "clang --print-resource-dir failed");
    let dir = std::path::Path::new(String::from_utf8_lossy(&out.stdout).trim()).join("lib/darwin");
    assert!(dir.join("libclang_rt.osx.a").exists(), "no libclang_rt.osx.a in {}", dir.display());
    println!("cargo:rustc-link-search=native={}", dir.display());
    println!("cargo:rustc-link-lib=static=clang_rt.osx");
}
