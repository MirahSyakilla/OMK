fn main() {
    println!("cargo:rerun-if-changed=src/api.cpp");
    println!("cargo:rerun-if-changed=src/payload.h");
    println!("cargo:rerun-if-changed=src/zygisk.hpp");

    let target = std::env::var("TARGET").unwrap_or_default();
    if !target.contains("android") {
        return;
    }

    println!("cargo:rustc-link-lib=log");
    println!("cargo:rustc-link-lib=c++_static");
    println!("cargo:rustc-link-lib=c++abi");

    cc::Build::new()
        .cpp(true)
        .std("c++17")
        .cpp_link_stdlib(None)
        .file("src/api.cpp")
        .include("src")
        .flag("-fno-exceptions")
        .flag("-fno-rtti")
        .flag("-fvisibility=hidden")
        .compile("omk_integrity_api");
}
