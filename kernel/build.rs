use std::path::PathBuf;

fn main() {
    let linker = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("linker.ld");
    println!("cargo:rerun-if-changed=linker.ld");
    // `minios-kernel` だけを OpenSBI のロード契約に合わせてリンクし、ホスト用 lib の
    // テストには RISC-V 固有の絶対アドレスと ENTRY(_start) を渡さない。
    println!(
        "cargo:rustc-link-arg-bin=minios-kernel=-T{}",
        linker.display()
    );
}
