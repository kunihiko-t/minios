use std::path::PathBuf;

fn main() {
    let linker = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("linker.ld");
    println!("cargo:rerun-if-changed=linker.ld");
    // `minios-kernel`だけをOpenSBIの読み込み規約に合わせてリンクする。
    // ホスト用ライブラリーのテストには、RISC-V固有の絶対アドレスと`ENTRY(_start)`を渡さない。
    println!(
        "cargo:rustc-link-arg-bin=minios-kernel=-T{}",
        linker.display()
    );
}
