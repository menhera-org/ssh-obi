fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    let target = std::env::var("TARGET").unwrap_or_default();
    if target.ends_with("-unknown-netbsd") {
        println!("cargo:rerun-if-changed=src/netbsd_execinfo_stub.c");
        cc::Build::new()
            .file("src/netbsd_execinfo_stub.c")
            .warnings(false)
            .compile("execinfo");
    }
}
