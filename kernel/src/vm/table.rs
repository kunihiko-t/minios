use alloc::vec::Vec;
use core::fmt;

use crate::{
    memory::frame::{FrameError, FrameSource, PhysFrame},
    vm::{
        AddressError, FrameStore, PageFlags, PageTableEntry, PhysAddr, PhysPageNum, PteError,
        VirtAddr, VirtPage,
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameKind {
    PageTable,
    User,
    Stack,
}

#[derive(Debug, PartialEq, Eq)]
pub struct OwnedFrame {
    frame: PhysFrame,
    kind: FrameKind,
}

/// 1つのaddress spaceが所有するframeのLIFO台帳。heap上の`Vec`で持ち、
/// 件数はframe poolとheap成長が許す限り可変である。
/// `destroy`/rollback経路がpop順にframeを返す。
pub struct AddressSpaceStorage {
    frames: Vec<OwnedFrame>,
}

impl AddressSpaceStorage {
    pub const fn new() -> Self {
        Self { frames: Vec::new() }
    }

    pub fn len(&self) -> usize {
        self.frames.len()
    }

    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// 台帳への追記はheap割り当てを伴うため失敗しうる。失敗時はframeを
    /// 呼び出し側へ返し、callerがallocatorへ解放する。
    fn push<E>(
        &mut self,
        frame: PhysFrame,
        kind: FrameKind,
    ) -> Result<(), (VmError<E>, PhysFrame)> {
        if self.frames.try_reserve(1).is_err() {
            return Err((VmError::CapacityExceeded, frame));
        }
        self.frames.push(OwnedFrame { frame, kind });
        Ok(())
    }

    fn pop(&mut self) -> Option<OwnedFrame> {
        self.frames.pop()
    }

    /// pop直後の再格納専用。popがcapacityを残すため失敗しない。
    fn restore_last(&mut self, owned: OwnedFrame) {
        self.frames.push(owned);
    }
}

impl Default for AddressSpaceStorage {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MappedFrame {
    physical: PhysAddr,
}

impl MappedFrame {
    pub const fn physical(self) -> PhysAddr {
        self.physical
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmError<E> {
    OutOfFrames,
    CapacityExceeded,
    AlreadyMapped,
    NotMapped,
    Address(AddressError),
    Pte(PteError),
    Store(E),
}

pub struct AddressSpaceBuilder<'alloc, 'memory, M: FrameStore> {
    allocator: &'alloc mut dyn FrameSource,
    memory: &'memory mut M,
    storage: Option<AddressSpaceStorage>,
    root: PhysAddr,
    allocator_id: u64,
}

impl<'alloc, 'memory, M: FrameStore> AddressSpaceBuilder<'alloc, 'memory, M> {
    /// 空の所有権台帳を内部に作り、root page table frameを割り当てる。
    /// 台帳はspace専用にbuilderが所有するため、共有arenaの事前検査は要らない。
    pub fn new(
        allocator: &'alloc mut dyn FrameSource,
        memory: &'memory mut M,
    ) -> Result<Self, VmError<M::Error>> {
        let mut storage = AddressSpaceStorage::new();
        let frame = allocator.allocate().ok_or(VmError::OutOfFrames)?;
        let frame_start = frame.start();
        let root = match PhysAddr::try_new(frame_start as u64) {
            Ok(root) => root,
            Err(error) => {
                let _ = allocator.deallocate(frame);
                return Err(VmError::Address(error));
            }
        };
        if let Err((error, frame)) = storage.push(frame, FrameKind::PageTable) {
            let _ = allocator.deallocate(frame);
            return Err(error);
        }
        if let Err(error) = memory.zero_frame(frame_start) {
            let owned = storage.pop().expect("newly pushed root frame exists");
            let _ = allocator.deallocate(owned.frame);
            return Err(VmError::Store(error));
        }

        Ok(Self {
            allocator_id: allocator.allocator_id(),
            allocator,
            memory,
            storage: Some(storage),
            root,
        })
    }

    pub fn root(&self) -> PhysAddr {
        self.root
    }

    pub fn map_new_zeroed(
        &mut self,
        page: VirtPage,
        flags: PageFlags,
    ) -> Result<MappedFrame, VmError<M::Error>> {
        self.map_new_zeroed_with_kind(page, flags, FrameKind::User)
    }

    pub(crate) fn map_new_zeroed_with_kind(
        &mut self,
        page: VirtPage,
        flags: PageFlags,
        kind: FrameKind,
    ) -> Result<MappedFrame, VmError<M::Error>> {
        let table = self.walk_to_leaf_table(page)?;
        let leaf_index = page.vpn()[0];
        let old = self.read_entry(table, leaf_index)?;
        if old.is_valid() {
            return Err(VmError::AlreadyMapped);
        }

        let physical = self.allocate_zeroed(kind)?;
        let ppn = PhysPageNum::from_start(physical.as_u64()).map_err(VmError::Address)?;
        let leaf = PageTableEntry::leaf(ppn, flags).map_err(VmError::Pte)?;
        if let Err(error) = self
            .memory
            .write_u64(table.as_u64() as usize, leaf_index, leaf.bits())
        {
            self.release_last(physical);
            return Err(VmError::Store(error));
        }

        Ok(MappedFrame { physical })
    }

    pub(crate) fn copy_into(
        &mut self,
        mapped: MappedFrame,
        offset: usize,
        bytes: &[u8],
    ) -> Result<(), M::Error> {
        let frame_start = usize::try_from(mapped.physical.as_u64())
            .expect("mapped frames originate from usize allocator addresses");
        self.memory.copy_into(frame_start, offset, bytes)
    }

    pub fn map_borrowed(
        &mut self,
        page: VirtPage,
        physical: PhysAddr,
        flags: PageFlags,
    ) -> Result<(), VmError<M::Error>> {
        let ppn = PhysPageNum::from_start(physical.as_u64()).map_err(VmError::Address)?;
        let table = self.walk_to_leaf_table(page)?;
        let leaf_index = page.vpn()[0];
        let old = self.read_entry(table, leaf_index)?;
        if old.is_valid() {
            return Err(VmError::AlreadyMapped);
        }

        let leaf = PageTableEntry::leaf(ppn, flags).map_err(VmError::Pte)?;
        self.memory
            .write_u64(table.as_u64() as usize, leaf_index, leaf.bits())
            .map_err(VmError::Store)
    }

    pub fn finish(mut self) -> AddressSpace {
        let storage = self
            .storage
            .take()
            .expect("unfinished builder retains its storage");
        AddressSpace {
            root: self.root,
            storage,
            allocator_id: self.allocator_id,
        }
    }

    fn walk_to_leaf_table(&mut self, page: VirtPage) -> Result<PhysAddr, VmError<M::Error>> {
        let vpn = page.vpn();
        let mut table = self.root;
        for level in [2, 1] {
            let index = vpn[level];
            let entry = self.read_entry(table, index)?;
            if entry.is_valid() {
                if entry.is_leaf() {
                    return Err(VmError::AlreadyMapped);
                }
                table = entry.ppn().map_err(VmError::Pte)?.start();
                continue;
            }

            let child = self.allocate_zeroed(FrameKind::PageTable)?;
            let child_ppn = PhysPageNum::from_start(child.as_u64()).map_err(VmError::Address)?;
            let branch = PageTableEntry::branch(child_ppn).map_err(VmError::Pte)?;
            if let Err(error) = self
                .memory
                .write_u64(table.as_u64() as usize, index, branch.bits())
            {
                self.release_last(child);
                return Err(VmError::Store(error));
            }
            table = child;
        }
        Ok(table)
    }

    fn read_entry(
        &self,
        table: PhysAddr,
        index: usize,
    ) -> Result<PageTableEntry, VmError<M::Error>> {
        let bits = self
            .memory
            .read_u64(table.as_u64() as usize, index)
            .map_err(VmError::Store)?;
        PageTableEntry::from_bits(bits).map_err(VmError::Pte)
    }

    fn allocate_zeroed(&mut self, kind: FrameKind) -> Result<PhysAddr, VmError<M::Error>> {
        let frame = self.allocator.allocate().ok_or(VmError::OutOfFrames)?;
        let frame_start = frame.start();
        let physical = match PhysAddr::try_new(frame_start as u64) {
            Ok(physical) => physical,
            Err(error) => {
                let _ = self.allocator.deallocate(frame);
                return Err(VmError::Address(error));
            }
        };
        if let Err((error, frame)) = self.storage_mut().push(frame, kind) {
            let _ = self.allocator.deallocate(frame);
            return Err(error);
        }
        if let Err(error) = self.memory.zero_frame(frame_start) {
            let owned = self.storage_mut().pop().expect("newly pushed frame exists");
            let _ = self.allocator.deallocate(owned.frame);
            return Err(VmError::Store(error));
        }
        Ok(physical)
    }

    fn release_last(&mut self, physical: PhysAddr) {
        let owned = self
            .storage_mut()
            .pop()
            .expect("failed installation retains its frame");
        debug_assert_eq!(owned.frame.start() as u64, physical.as_u64());
        let _ = self.allocator.deallocate(owned.frame);
    }

    fn storage_mut(&mut self) -> &mut AddressSpaceStorage {
        self.storage
            .as_mut()
            .expect("unfinished builder retains its storage")
    }
}

impl<M: FrameStore> Drop for AddressSpaceBuilder<'_, '_, M> {
    fn drop(&mut self) {
        while let Some(owned) = self.storage.as_mut().and_then(AddressSpaceStorage::pop) {
            let _ = self.allocator.deallocate(owned.frame);
        }
    }
}

pub struct AddressSpace {
    root: PhysAddr,
    storage: AddressSpaceStorage,
    allocator_id: u64,
}

pub struct DestroyError {
    frame_error: FrameError,
    space: AddressSpace,
}

impl DestroyError {
    pub const fn frame_error(&self) -> FrameError {
        self.frame_error
    }

    pub fn into_parts(self) -> (FrameError, AddressSpace) {
        (self.frame_error, self.space)
    }
}

impl fmt::Debug for DestroyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DestroyError")
            .field("frame_error", &self.frame_error)
            .finish_non_exhaustive()
    }
}

impl AddressSpace {
    pub const fn root(&self) -> PhysAddr {
        self.root
    }

    pub(crate) const fn allocator_id(&self) -> u64 {
        self.allocator_id
    }

    /// 台帳が記録する所有frame数。回収経路の診断とhost testの検査に使う。
    pub fn owned_frames(&self) -> usize {
        self.storage.len()
    }

    pub fn translate<M: FrameStore>(
        &self,
        memory: &M,
        address: VirtAddr,
    ) -> Result<(PhysAddr, PageFlags), VmError<M::Error>> {
        let vpn = address.vpn();
        let mut table = self.root;
        for level in [2, 1] {
            let entry = read_entry(memory, table, vpn[level])?;
            if !entry.is_valid() {
                return Err(VmError::NotMapped);
            }
            if entry.is_leaf() {
                return Err(VmError::Pte(PteError::NotLeaf));
            }
            table = entry.ppn().map_err(VmError::Pte)?.start();
        }

        let leaf = read_entry(memory, table, vpn[0])?;
        if !leaf.is_valid() {
            return Err(VmError::NotMapped);
        }
        if !leaf.is_leaf() {
            return Err(VmError::Pte(PteError::NotLeaf));
        }
        let flags = leaf.flags().map_err(VmError::Pte)?;
        let physical =
            leaf.ppn().map_err(VmError::Pte)?.start().as_u64() + address.page_offset() as u64;
        Ok((
            PhysAddr::try_new(physical).map_err(VmError::Address)?,
            flags,
        ))
    }

    pub fn destroy(mut self, allocator: &mut dyn FrameSource) -> Result<(), DestroyError> {
        if allocator.allocator_id() != self.allocator_id {
            return Err(DestroyError {
                frame_error: FrameError::WrongAllocator,
                space: self,
            });
        }

        while let Some(owned) = self.storage.pop() {
            let OwnedFrame { frame, kind } = owned;
            if let Err((frame_error, frame)) = allocator.deallocate_recoverable(frame) {
                self.storage.restore_last(OwnedFrame { frame, kind });
                return Err(DestroyError {
                    frame_error,
                    space: self,
                });
            }
        }
        Ok(())
    }
}

fn read_entry<M: FrameStore>(
    memory: &M,
    table: PhysAddr,
    index: usize,
) -> Result<PageTableEntry, VmError<M::Error>> {
    let bits = memory
        .read_u64(table.as_u64() as usize, index)
        .map_err(VmError::Store)?;
    PageTableEntry::from_bits(bits).map_err(VmError::Pte)
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::{boxed::Box, cell::Cell, collections::BTreeMap, vec::Vec};

    use super::{AddressSpaceBuilder, VmError};
    use crate::{
        memory::frame::{FrameAllocator, FrameError, FrameStats},
        vm::{FrameStore, PageFlags, PhysAddr, VirtAddr, VirtPage},
    };

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum TestStoreError {
        MissingFrame,
        IndexOutOfBounds,
        RangeOutOfBounds,
        InjectedFailure,
    }

    #[derive(Default)]
    struct TestFrameStore {
        frames: BTreeMap<usize, Box<[u8; 4096]>>,
        zeroes: usize,
        reads: Cell<usize>,
        writes: usize,
        fail_zero_frame: Option<usize>,
        fail_read_u64: Option<usize>,
        fail_write: Option<usize>,
    }

    impl TestFrameStore {
        fn fail_on_write(write: usize) -> Self {
            Self {
                fail_write: Some(write),
                ..Self::default()
            }
        }

        fn fail_on_zero_frame(zero: usize) -> Self {
            Self {
                fail_zero_frame: Some(zero),
                ..Self::default()
            }
        }

        fn fail_on_read_u64(read: usize) -> Self {
            Self {
                fail_read_u64: Some(read),
                ..Self::default()
            }
        }

        fn frame(&self, frame_start: usize) -> Result<&[u8; 4096], TestStoreError> {
            self.frames
                .get(&frame_start)
                .map(Box::as_ref)
                .ok_or(TestStoreError::MissingFrame)
        }

        fn frame_mut(&mut self, frame_start: usize) -> Result<&mut [u8; 4096], TestStoreError> {
            self.frames
                .get_mut(&frame_start)
                .map(Box::as_mut)
                .ok_or(TestStoreError::MissingFrame)
        }

        fn range(offset: usize, len: usize) -> Result<core::ops::Range<usize>, TestStoreError> {
            let end = offset
                .checked_add(len)
                .ok_or(TestStoreError::RangeOutOfBounds)?;
            if end > 4096 {
                return Err(TestStoreError::RangeOutOfBounds);
            }
            Ok(offset..end)
        }
    }

    impl FrameStore for TestFrameStore {
        type Error = TestStoreError;

        fn zero_frame(&mut self, frame_start: usize) -> Result<(), Self::Error> {
            self.zeroes += 1;
            if self.fail_zero_frame == Some(self.zeroes) {
                return Err(TestStoreError::InjectedFailure);
            }
            self.frames.insert(frame_start, Box::new([0; 4096]));
            Ok(())
        }

        fn read_u64(&self, frame_start: usize, index: usize) -> Result<u64, Self::Error> {
            let reads = self.reads.get() + 1;
            self.reads.set(reads);
            if self.fail_read_u64 == Some(reads) {
                return Err(TestStoreError::InjectedFailure);
            }
            if index >= 512 {
                return Err(TestStoreError::IndexOutOfBounds);
            }
            let offset = index * 8;
            let mut bytes = [0; 8];
            bytes.copy_from_slice(&self.frame(frame_start)?[offset..offset + 8]);
            Ok(u64::from_le_bytes(bytes))
        }

        fn write_u64(
            &mut self,
            frame_start: usize,
            index: usize,
            value: u64,
        ) -> Result<(), Self::Error> {
            if index >= 512 {
                return Err(TestStoreError::IndexOutOfBounds);
            }
            self.writes += 1;
            if self.fail_write == Some(self.writes) {
                return Err(TestStoreError::InjectedFailure);
            }
            let offset = index * 8;
            self.frame_mut(frame_start)?[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
            Ok(())
        }

        fn copy_into(
            &mut self,
            frame_start: usize,
            offset: usize,
            bytes: &[u8],
        ) -> Result<(), Self::Error> {
            let range = Self::range(offset, bytes.len())?;
            self.frame_mut(frame_start)?[range].copy_from_slice(bytes);
            Ok(())
        }

        fn copy_out(
            &self,
            frame_start: usize,
            offset: usize,
            output: &mut [u8],
        ) -> Result<(), Self::Error> {
            let range = Self::range(offset, output.len())?;
            output.copy_from_slice(&self.frame(frame_start)?[range]);
            Ok(())
        }
    }

    fn test_allocator<const WORDS: usize>(base: usize, end: usize) -> FrameAllocator<WORDS> {
        // Safety: each test owns its independent bitmap model and never dereferences these
        // synthetic physical addresses.
        unsafe { FrameAllocator::new(base, end) }.unwrap()
    }

    #[test]
    fn failed_mapping_returns_root_intermediate_and_leaf_frames() {
        let mut allocator = test_allocator::<8>(0x1000, 0x21_000);
        let before = allocator.stats();
        let mut store = TestFrameStore::default();

        let error = {
            let mut builder = AddressSpaceBuilder::new(&mut allocator, &mut store).unwrap();
            builder
                .map_new_zeroed(
                    VirtPage::from_start(0x0010_0000).unwrap(),
                    PageFlags::new(true, true, false, true).unwrap(),
                )
                .unwrap();
            builder
                .map_new_zeroed(
                    VirtPage::from_start(0x0010_0000).unwrap(),
                    PageFlags::new(true, false, true, true).unwrap(),
                )
                .unwrap_err()
        };

        assert_eq!(error, VmError::AlreadyMapped);
        assert_eq!(allocator.stats(), before);
    }

    #[test]
    fn translate_preserves_leaf_permissions() {
        let mut allocator = test_allocator::<16>(0x1000, 0x41_000);
        let mut store = TestFrameStore::default();
        let mut builder = AddressSpaceBuilder::new(&mut allocator, &mut store).unwrap();

        let user = VirtPage::from_start(0x0010_0000).unwrap();
        let flags = PageFlags::new(true, false, true, true).unwrap();
        let physical = builder.map_new_zeroed(user, flags).unwrap();
        let space = builder.finish();

        assert_eq!(
            space.translate(&store, user.start()).unwrap(),
            (physical.physical(), flags),
        );
        assert_eq!(
            space
                .translate(&store, VirtAddr::try_new(0x0010_0123).unwrap())
                .unwrap(),
            (PhysAddr::try_new(0x4123).unwrap(), flags),
        );
    }

    #[test]
    fn borrowed_kernel_mapping_remains_supervisor_only() {
        let mut allocator = test_allocator::<16>(0x1000, 0x41_000);
        let mut store = TestFrameStore::default();
        let mut builder = AddressSpaceBuilder::new(&mut allocator, &mut store).unwrap();
        let page = VirtPage::from_start(0xffff_ffc0_0020_0000).unwrap();
        let physical = PhysAddr::try_new(0x8020_0000).unwrap();
        let flags = PageFlags::new(true, false, true, false).unwrap();

        builder.map_borrowed(page, physical, flags).unwrap();
        let space = builder.finish();

        assert_eq!(
            space.translate(&store, page.start()).unwrap(),
            (physical, flags)
        );
        assert!(!space.translate(&store, page.start()).unwrap().1.user());
    }

    // Catches the ledger silently capping ownership at a fixed capacity:
    // a space must keep recording frames as long as the heap keeps growing,
    // well past the arena sizes the previous array storage allowed.
    #[test]
    fn storage_grows_beyond_the_old_fixed_capacity() {
        let mut allocator = test_allocator::<256>(0x1000, 0x101_000);
        let mut store = TestFrameStore::default();
        let mut builder = AddressSpaceBuilder::new(&mut allocator, &mut store).unwrap();

        // 1 GiB刻みの8領域へmapすると、領域ごとに中間table+leafが必要に
        // なり、所有frameは旧fixture容量(3)を大きく超える。
        for index in 0..8u64 {
            builder
                .map_new_zeroed(
                    VirtPage::from_start(0x0010_0000 + index * 0x4000_0000).unwrap(),
                    PageFlags::new(true, true, false, true).unwrap(),
                )
                .unwrap();
        }
        let space = builder.finish();

        assert!(space.owned_frames() > 3 * 8);
        space.destroy(&mut allocator).unwrap();
        assert_eq!(allocator.stats().allocated, 0);
    }

    #[test]
    fn allocator_exhaustion_rolls_back_builder_frames() {
        let mut allocator = test_allocator::<1>(0x1000, 0x4000);
        let before = allocator.stats();
        let mut store = TestFrameStore::default();

        let error = {
            let mut builder = AddressSpaceBuilder::new(&mut allocator, &mut store).unwrap();
            builder
                .map_new_zeroed(
                    VirtPage::from_start(0x0030_0000).unwrap(),
                    PageFlags::new(true, true, false, true).unwrap(),
                )
                .unwrap_err()
        };

        assert_eq!(error, VmError::OutOfFrames);
        assert_eq!(allocator.stats(), before);
    }

    #[test]
    fn zero_frame_failure_while_allocating_an_intermediate_table_returns_everything_for_reuse() {
        // Catches a rollback mutation that leaves the root table recorded after child zeroing fails.
        let mut allocator = test_allocator::<8>(0x1000, 0x21_000);
        let before = allocator.stats();
        let mut store = TestFrameStore::fail_on_zero_frame(2);

        let error = {
            let mut builder = AddressSpaceBuilder::new(&mut allocator, &mut store).unwrap();
            builder
                .map_new_zeroed(
                    VirtPage::from_start(0x0040_0000).unwrap(),
                    PageFlags::new(true, false, true, true).unwrap(),
                )
                .unwrap_err()
        };

        assert_eq!(error, VmError::Store(TestStoreError::InjectedFailure));
        assert_eq!(allocator.stats(), before);
        let retry = AddressSpaceBuilder::new(&mut allocator, &mut store).unwrap();
        assert_eq!(retry.root().as_u64(), 0x1000);
        drop(retry);
    }

    #[test]
    fn read_u64_failure_before_the_page_walk_returns_the_root_for_reuse() {
        // Catches a mutation that bypasses builder-drop rollback after the first PTE read fails.
        let mut allocator = test_allocator::<8>(0x1000, 0x21_000);
        let before = allocator.stats();
        let mut store = TestFrameStore::fail_on_read_u64(1);

        let error = {
            let mut builder = AddressSpaceBuilder::new(&mut allocator, &mut store).unwrap();
            builder
                .map_new_zeroed(
                    VirtPage::from_start(0x0040_0000).unwrap(),
                    PageFlags::new(true, false, true, true).unwrap(),
                )
                .unwrap_err()
        };

        assert_eq!(error, VmError::Store(TestStoreError::InjectedFailure));
        assert_eq!(allocator.stats(), before);
        let retry = AddressSpaceBuilder::new(&mut allocator, &mut store).unwrap();
        assert_eq!(retry.root().as_u64(), 0x1000);
        drop(retry);
    }

    #[test]
    fn leaf_pte_write_failure_returns_every_owned_frame_for_reuse() {
        // Catches a mutation that leaks the leaf frame when its final PTE installation fails.
        let mut allocator = test_allocator::<8>(0x1000, 0x21_000);
        let before = allocator.stats();
        let mut store = TestFrameStore::fail_on_write(3);

        let error = {
            let mut builder = AddressSpaceBuilder::new(&mut allocator, &mut store).unwrap();
            builder
                .map_new_zeroed(
                    VirtPage::from_start(0x0040_0000).unwrap(),
                    PageFlags::new(true, false, true, true).unwrap(),
                )
                .unwrap_err()
        };

        assert_eq!(error, VmError::Store(TestStoreError::InjectedFailure));
        assert_eq!(allocator.stats(), before);
        let retry = AddressSpaceBuilder::new(&mut allocator, &mut store).unwrap();
        assert_eq!(retry.root().as_u64(), 0x1000);
        drop(retry);
    }

    #[test]
    fn translate_reports_an_unmapped_page() {
        let mut allocator = test_allocator::<8>(0x1000, 0x21_000);
        let mut store = TestFrameStore::default();
        let builder = AddressSpaceBuilder::new(&mut allocator, &mut store).unwrap();
        let space = builder.finish();

        assert_eq!(
            space.translate(&store, VirtAddr::try_new(0x0080_0000).unwrap()),
            Err(VmError::NotMapped)
        );
    }

    #[test]
    fn destroy_returns_owned_frames_but_not_borrowed_frames() {
        let mut allocator = test_allocator::<8>(0x1000, 0x21_000);
        let before = allocator.stats();
        let mut store = TestFrameStore::default();
        let mut builder = AddressSpaceBuilder::new(&mut allocator, &mut store).unwrap();
        builder
            .map_borrowed(
                VirtPage::from_start(0xffff_ffc0_0040_0000).unwrap(),
                PhysAddr::try_new(0x8020_0000).unwrap(),
                PageFlags::new(true, false, true, false).unwrap(),
            )
            .unwrap();
        let space = builder.finish();

        assert_eq!(allocator.stats().allocated, 3);
        space.destroy(&mut allocator).unwrap();

        assert_eq!(allocator.stats(), before);
    }

    #[test]
    fn destroyed_storage_can_build_a_new_address_space() {
        let mut allocator = test_allocator::<8>(0x1000, 0x21_000);
        let mut store = TestFrameStore::default();

        {
            let mut first = AddressSpaceBuilder::new(&mut allocator, &mut store).unwrap();
            first
                .map_new_zeroed(
                    VirtPage::from_start(0x0010_0000).unwrap(),
                    PageFlags::new(true, true, false, true).unwrap(),
                )
                .unwrap();
            first.finish().destroy(&mut allocator).unwrap();
        }

        let second = AddressSpaceBuilder::new(&mut allocator, &mut store).unwrap();
        assert_eq!(second.root().as_u64(), 0x1000);
        drop(second);
    }

    #[test]
    fn test_frame_store_checks_word_and_byte_ranges() {
        let mut store = TestFrameStore::default();
        store.zero_frame(0x1000).unwrap();
        let mut output = Vec::from([0_u8; 2]);

        assert_eq!(
            store.read_u64(0x1000, 512),
            Err(TestStoreError::IndexOutOfBounds)
        );
        assert_eq!(
            store.write_u64(0x1000, 512, 0),
            Err(TestStoreError::IndexOutOfBounds)
        );
        assert_eq!(
            store.copy_into(0x1000, 4095, &[1, 2]),
            Err(TestStoreError::RangeOutOfBounds)
        );
        assert_eq!(
            store.copy_out(0x1000, usize::MAX, &mut output),
            Err(TestStoreError::RangeOutOfBounds)
        );
    }

    #[test]
    fn mapped_user_page_uses_four_owned_frames() {
        let mut allocator = test_allocator::<8>(0x1000, 0x21_000);
        let mut store = TestFrameStore::default();
        let mut builder = AddressSpaceBuilder::new(&mut allocator, &mut store).unwrap();
        let mapped = builder
            .map_new_zeroed(
                VirtPage::from_start(0x0010_0000).unwrap(),
                PageFlags::new(true, true, false, true).unwrap(),
            )
            .unwrap();
        let space = builder.finish();

        assert_eq!(mapped.physical().as_u64(), 0x4000);
        assert_eq!(
            allocator.stats(),
            FrameStats {
                total: 32,
                allocated: 4,
                free: 28,
            }
        );
        space.destroy(&mut allocator).unwrap();
    }

    #[test]
    fn wrong_allocator_destroy_returns_the_space_for_retry() {
        let mut allocator = test_allocator::<8>(0x1000, 0x21_000);
        let before = allocator.stats();
        let mut store = TestFrameStore::default();
        let mut builder = AddressSpaceBuilder::new(&mut allocator, &mut store).unwrap();
        builder
            .map_new_zeroed(
                VirtPage::from_start(0x0010_0000).unwrap(),
                PageFlags::new(true, true, false, true).unwrap(),
            )
            .unwrap();
        let space = builder.finish();
        let mut other = test_allocator::<1>(0x41_000, 0x42_000);

        let failure = space.destroy(&mut other).unwrap_err();
        assert_eq!(failure.frame_error(), FrameError::WrongAllocator);
        assert_eq!(allocator.stats().allocated, 4);
        assert_eq!(other.stats().allocated, 0);

        let (error, space) = failure.into_parts();
        assert_eq!(error, FrameError::WrongAllocator);
        assert_eq!(
            space
                .translate(&store, VirtAddr::try_new(0x0010_0123).unwrap())
                .unwrap()
                .0
                .as_u64(),
            0x4123
        );
        space.destroy(&mut allocator).unwrap();
        assert_eq!(allocator.stats(), before);

        let retry = AddressSpaceBuilder::new(&mut allocator, &mut store).unwrap();
        assert_eq!(retry.root().as_u64(), 0x1000);
        drop(retry);
    }

    #[test]
    fn same_range_allocator_instance_cannot_destroy_the_space() {
        let mut allocator = test_allocator::<8>(0x1000, 0x21_000);
        let before = allocator.stats();
        let mut store = TestFrameStore::default();
        let space = AddressSpaceBuilder::new(&mut allocator, &mut store)
            .unwrap()
            .finish();
        let mut same_range = test_allocator::<8>(0x1000, 0x21_000);

        let failure = space.destroy(&mut same_range).unwrap_err();
        assert_eq!(failure.frame_error(), FrameError::WrongAllocator);
        assert_eq!(allocator.stats().allocated, 1);
        assert_eq!(same_range.stats().allocated, 0);

        let (_, space) = failure.into_parts();
        space.destroy(&mut allocator).unwrap();
        assert_eq!(allocator.stats(), before);
    }
}
