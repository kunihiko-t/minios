pub mod address;
pub mod kernel;
pub mod pte;
pub mod storage;
pub mod table;

pub use address::{AddressError, PhysAddr, PhysPageNum, VirtAddr, VirtPage};
pub use kernel::{KernelMapError, KernelMapPlan, KernelMapping};
pub use pte::{PageFlags, PageTableEntry, PteError};
#[cfg(target_arch = "riscv64")]
pub use storage::IdentityFrameStore;
pub use storage::{FrameStore, IdentityFrameStoreError};
pub use table::{
    AddressSpace, AddressSpaceBuilder, AddressSpaceStorage, DestroyError, FrameKind,
    MAX_OWNED_FRAMES, MappedFrame, OwnedFrame, VmError,
};
