fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN/browser-runtime:$ORIGIN:$ORIGIN/..");
        println!("cargo:rustc-link-arg=-Wl,--exclude-libs,ALL");
    }
}
