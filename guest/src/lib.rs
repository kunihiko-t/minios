//! MiniOS guest programが共有するlibrary。
//!
//! 各guest program (`src/main.rs`と`src/bin/*.rs`) は`_start`と
//! `panic_handler`を自前で持ち、syscall wrapperとheap allocatorをここから使う。

#![no_std]

pub mod heap;
pub mod sys;
