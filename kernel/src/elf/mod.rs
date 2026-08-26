//! Allocation-free validation and load planning for static RISC-V ELF64 images.

pub mod header;
pub mod load;
pub mod plan;

pub use header::{ElfError, ElfImage, MAX_LOAD_SEGMENTS, ProgramHeader};
pub use load::{LoadError, LoadedImage, LoadedImageDestroyError, load_image};
pub use plan::{
    LoadPlan, LoadSegment, MAX_USER_IMAGE_PAGES, USER_END, USER_GUARD_BOTTOM, USER_STACK_BOTTOM,
    USER_STACK_TOP, USER_START,
};

#[cfg(any(test, feature = "qemu-test-elf"))]
pub mod fixture;
