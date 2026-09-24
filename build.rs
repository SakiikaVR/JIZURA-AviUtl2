fn main() {
    cc::Build::new().cpp(true).file("webview_host.cpp")
        .include("../webview2/build/native/include")
        .flag("/std:c++17").flag("/EHsc").flag("/utf-8")
        .define("UNICODE",None).define("_UNICODE",None)
        .compile("jizura_webview");
    let lib=std::fs::canonicalize("../webview2/build/native/x64").unwrap();
    println!("cargo:rustc-link-search=native={}",lib.display());
    println!("cargo:rustc-link-lib=static=WebView2LoaderStatic");
    // WebView2LoaderStatic depends on ETW (wevtapi) and registry APIs
    // (advapi32). Declare the system libraries explicitly for the cdylib link.
    for lib in ["user32","comctl32","ole32","oleaut32","shlwapi","version","advapi32","wevtapi"] {println!("cargo:rustc-link-lib=dylib={lib}");}
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=webview_host.cpp");
}
