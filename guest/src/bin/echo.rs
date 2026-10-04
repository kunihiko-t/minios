//! user shellの`echo`（`BIN/ECHO.ELF`）。
//!
//! argv[1..]を空白1個で区切り、改行を付けてstdoutへ書いて0で終了する。
//! 出力は`print!`の1回分なので、256 byteまでなら1 frameになる。

#![no_std]
#![no_main]

use core::fmt;

use minios_abi::syscall::SPAWN_MAX_ARGC;
use minios_guest::{Args, println};

/// argvを空白区切りで書くための`Display`。
struct Joined<'a>(&'a [&'a [u8]]);

impl fmt::Display for Joined<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, word) in self.0.iter().enumerate() {
            if index > 0 {
                f.write_str(" ")?;
            }
            // kernelはUTF-8でないargvを拒否するため、ここでは常に成功する。
            f.write_str(core::str::from_utf8(word).unwrap_or("?"))?;
        }
        Ok(())
    }
}

minios_guest::entry!(main);

fn main(args: Args) -> i32 {
    let mut words: [&[u8]; SPAWN_MAX_ARGC] = [&[]; SPAWN_MAX_ARGC];
    let mut count = 0;
    for word in args.skip(1) {
        words[count] = word;
        count += 1;
    }
    println!("{}", Joined(&words[..count]));
    0
}
