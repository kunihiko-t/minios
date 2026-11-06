//! Validation, load planning, and loading for static RISC-V ELF64 images.
//!
//! Parsing and planning are allocation-free. The loader reads the file
//! through [`ElfSource`] piecewise, buffering only the bounded program-header
//! table and one page of segment bytes at a time.

pub mod header;
pub mod load;
pub mod plan;

pub use header::{
    ELF_HEADER_LEN, ElfError, ElfHeader, ElfImage, MAX_LOAD_SEGMENTS, MAX_PROGRAM_HEADERS,
    ProgramHeader,
};
pub use load::{
    ElfSource, LoadError, LoadedImage, LoadedImageDestroyError, load_image,
    load_image_with_kernel_mappings,
};
pub use plan::{
    LoadPlan, LoadSegment, MAX_USER_IMAGE_PAGES, USER_END, USER_GUARD_BOTTOM, USER_STACK_BOTTOM,
    USER_STACK_TOP, USER_START,
};

#[cfg(any(
    test,
    feature = "qemu-test-elf",
    feature = "qemu-test-user-syscall",
    feature = "qemu-test-user-exit"
))]
pub mod fixture;
