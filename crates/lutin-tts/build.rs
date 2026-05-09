// Workaround for an upstream llama-cpp-sys-2 (≥0.1.146) bug: when CMake
// detects a system NCCL install it compiles ggml-cuda's NCCL call sites
// in but forgets to emit `cargo:rustc-link-lib=nccl`, so the final link
// fails with undefined `nccl*` symbols. Emit it here from the Rust crate
// that pulls in llama-cpp-2 with the cuda feature. Drop this file when
// the upstream sys crate fixes its build script.
fn main() {
    if cfg!(feature = "cuda") {
        println!("cargo:rustc-link-lib=nccl");
    }
}
