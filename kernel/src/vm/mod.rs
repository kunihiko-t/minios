pub mod address;
pub mod pte;
pub mod storage;
pub mod table;

pub use address::{AddressError, PhysAddr, PhysPageNum, VirtAddr, VirtPage};
pub use pte::{PageFlags, PageTableEntry, PteError};
pub use storage::FrameStore;
pub use table::{
    AddressSpace, AddressSpaceBuilder, AddressSpaceStorage, DestroyError, FrameKind,
    MAX_OWNED_FRAMES, MappedFrame, OwnedFrame, VmError,
};
