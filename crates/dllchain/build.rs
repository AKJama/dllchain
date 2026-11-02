//! Embeds the manifests: modern controls for the window, and no elevation for either program.

fn main() {
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc") {
        return;
    }
    println!("cargo:rerun-if-changed=build.rs");
    for bin in ["dllchain", "dllchain-cli"] {
        println!("cargo:rustc-link-arg-bin={bin}=/MANIFEST:EMBED");
        println!("cargo:rustc-link-arg-bin={bin}=/MANIFESTUAC:level='asInvoker' uiAccess='false'");
    }
    println!(
        "cargo:rustc-link-arg-bin=dllchain=/MANIFESTDEPENDENCY:type='win32' name='Microsoft.Windows.Common-Controls' version='6.0.0.0' \
         processorArchitecture='*' publicKeyToken='6595b64144ccf1df' language='*'"
    );
}
