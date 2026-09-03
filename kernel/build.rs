use std::path::PathBuf;

fn main() {
    // RISC-Vの32/64で起動契約が異なるため、ターゲットの幅でリンカースクリプトを選ぶ。
    // ホスト用ライブラリーのテストには、RISC-V固有の絶対アドレスと`ENTRY(_start)`を渡さない。
    let script = match std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("riscv32") => "linker_neorv32.ld",
        _ => "linker.ld",
    };
    let linker = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(script);
    println!("cargo:rerun-if-changed=linker.ld");
    println!("cargo:rerun-if-changed=linker_neorv32.ld");
    // `minios-kernel`だけを各ターゲットの読み込み規約に合わせてリンクする。
    println!(
        "cargo:rustc-link-arg-bin=minios-kernel=-T{}",
        linker.display()
    );
}
