fn main() {
    // napi-build's setup() calls setup_gnu() on Windows-GNU, which requires a
    // libnode.dll for static linking. napi-sys resolves N-API symbols at
    // runtime via libloading on Windows, so the cdylib never links node.
    // Skip setup on windows-gnu so the build does not depend on libnode.dll.
    #[cfg(not(all(windows, target_env = "gnu")))]
    napi_build::setup();
}
