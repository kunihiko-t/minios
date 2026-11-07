//! guest専用linker scriptを、呼び出しcwdに依存しない絶対pathで渡す。

fn main() {
    let linker = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("linker.ld");
    // linker scriptはguest binだけへ渡す。host上のlib unit testは通常のlinkで作る。
    println!("cargo:rustc-link-arg-bins=-T{}", linker.display());
    println!("cargo:rerun-if-changed=linker.ld");
}
