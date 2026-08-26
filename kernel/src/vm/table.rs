use crate::{
    memory::frame::{FrameAllocator, FrameError, PhysFrame},
    vm::{
        AddressError, FrameStore, PageFlags, PageTableEntry, PhysAddr, PhysPageNum, PteError,
        VirtAddr, VirtPage,
    },
};

pub const MAX_OWNED_FRAMES: usize = 2688;

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

pub struct AddressSpaceStorage<const N: usize> {
    frames: [Option<OwnedFrame>; N],
    len: usize,
}

impl<const N: usize> AddressSpaceStorage<N> {
    pub const fn new() -> Self {
        Self {
            frames: [const { None }; N],
            len: 0,
        }
    }

    pub const fn len(&self) -> usize {
        self.len
    }

    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn push<E>(
        &mut self,
        frame: PhysFrame,
        kind: FrameKind,
    ) -> Result<(), (VmError<E>, PhysFrame)> {
        if self.len == N {
            return Err((VmError::CapacityExceeded, frame));
        }
        self.frames[self.len] = Some(OwnedFrame { frame, kind });
        self.len += 1;
        Ok(())
    }

    fn pop(&mut self) -> Option<OwnedFrame> {
        if self.len == 0 {
            return None;
        }
        self.len -= 1;
        self.frames[self.len].take()
    }
}

impl<const N: usize> Default for AddressSpaceStorage<N> {
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
    StorageInUse,
    AlreadyMapped,
    NotMapped,
    Address(AddressError),
    Pte(PteError),
    Store(E),
}

pub struct AddressSpaceBuilder<
    'alloc,
    'memory,
    'storage,
    const N: usize,
    const WORDS: usize,
    M: FrameStore,
> {
    allocator: &'alloc mut FrameAllocator<WORDS>,
    memory: &'memory mut M,
    storage: Option<&'storage mut AddressSpaceStorage<N>>,
    root: PhysAddr,
}

impl<'alloc, 'memory, 'storage, const N: usize, const WORDS: usize, M: FrameStore>
    AddressSpaceBuilder<'alloc, 'memory, 'storage, N, WORDS, M>
{
    pub fn new(
        allocator: &'alloc mut FrameAllocator<WORDS>,
        memory: &'memory mut M,
        storage: &'storage mut AddressSpaceStorage<N>,
    ) -> Result<Self, VmError<M::Error>> {
        if !storage.is_empty() {
            return Err(VmError::StorageInUse);
        }

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
        let table = self.walk_to_leaf_table(page)?;
        let leaf_index = page.vpn()[0];
        let old = self.read_entry(table, leaf_index)?;
        if old.is_valid() {
            return Err(VmError::AlreadyMapped);
        }

        let physical = self.allocate_zeroed(FrameKind::User)?;
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

    pub fn finish(mut self) -> AddressSpace<'storage, N> {
        let storage = self
            .storage
            .take()
            .expect("unfinished builder retains its storage");
        AddressSpace {
            root: self.root,
            storage,
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

    fn storage_mut(&mut self) -> &mut AddressSpaceStorage<N> {
        self.storage
            .as_deref_mut()
            .expect("unfinished builder retains its storage")
    }
}

impl<const N: usize, const WORDS: usize, M: FrameStore> Drop
    for AddressSpaceBuilder<'_, '_, '_, N, WORDS, M>
{
    fn drop(&mut self) {
        while let Some(owned) = self
            .storage
            .as_deref_mut()
            .and_then(AddressSpaceStorage::pop)
        {
            let _ = self.allocator.deallocate(owned.frame);
        }
    }
}

pub struct AddressSpace<'storage, const N: usize> {
    root: PhysAddr,
    storage: &'storage mut AddressSpaceStorage<N>,
}

impl<'storage, const N: usize> AddressSpace<'storage, N> {
    pub const fn root(&self) -> PhysAddr {
        self.root
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

    pub fn destroy<const WORDS: usize>(
        self,
        allocator: &mut FrameAllocator<WORDS>,
    ) -> Result<(), FrameError> {
        while let Some(owned) = self.storage.pop() {
            let _kind = owned.kind;
            allocator.deallocate(owned.frame)?;
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

    use std::{boxed::Box, collections::BTreeMap, vec::Vec};

    use super::{AddressSpaceBuilder, AddressSpaceStorage, VmError};
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
        writes: usize,
        fail_write: Option<usize>,
    }

    impl TestFrameStore {
        fn fail_on_write(write: usize) -> Self {
            Self {
                fail_write: Some(write),
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
            self.frames.insert(frame_start, Box::new([0; 4096]));
            Ok(())
        }

        fn read_u64(&self, frame_start: usize, index: usize) -> Result<u64, Self::Error> {
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
        let mut storage = AddressSpaceStorage::<8>::new();

        let error = {
            let mut builder =
                AddressSpaceBuilder::new(&mut allocator, &mut store, &mut storage).unwrap();
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
        assert_eq!(storage.len(), 0);
    }

    #[test]
    fn translate_preserves_leaf_permissions() {
        let mut allocator = test_allocator::<16>(0x1000, 0x41_000);
        let mut store = TestFrameStore::default();
        let mut storage = AddressSpaceStorage::<16>::new();
        let mut builder =
            AddressSpaceBuilder::new(&mut allocator, &mut store, &mut storage).unwrap();

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
        let mut storage = AddressSpaceStorage::<16>::new();
        let mut builder =
            AddressSpaceBuilder::new(&mut allocator, &mut store, &mut storage).unwrap();
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

    #[test]
    fn storage_capacity_failure_returns_every_allocated_frame() {
        let mut allocator = test_allocator::<8>(0x1000, 0x21_000);
        let before = allocator.stats();
        let mut store = TestFrameStore::default();
        let mut storage = AddressSpaceStorage::<3>::new();

        let error = {
            let mut builder =
                AddressSpaceBuilder::new(&mut allocator, &mut store, &mut storage).unwrap();
            builder
                .map_new_zeroed(
                    VirtPage::from_start(0x0020_0000).unwrap(),
                    PageFlags::new(true, true, false, true).unwrap(),
                )
                .unwrap_err()
        };

        assert_eq!(error, VmError::CapacityExceeded);
        assert_eq!(allocator.stats(), before);
        assert_eq!(storage.len(), 0);
    }

    #[test]
    fn allocator_exhaustion_rolls_back_builder_frames() {
        let mut allocator = test_allocator::<1>(0x1000, 0x4000);
        let before = allocator.stats();
        let mut store = TestFrameStore::default();
        let mut storage = AddressSpaceStorage::<8>::new();

        let error = {
            let mut builder =
                AddressSpaceBuilder::new(&mut allocator, &mut store, &mut storage).unwrap();
            builder
                .map_new_zeroed(
                    VirtPage::from_start(0x0030_0000).unwrap(),
                    PageFlags::new(true, true, false, true).unwrap(),
                )
                .unwrap_err()
        };

        assert_eq!(error, VmError::OutOfFrames);
        assert_eq!(allocator.stats(), before);
        assert_eq!(storage.len(), 0);
    }

    #[test]
    fn store_failure_preserves_typed_error_and_rolls_back_frames() {
        let mut allocator = test_allocator::<8>(0x1000, 0x21_000);
        let before = allocator.stats();
        let mut store = TestFrameStore::fail_on_write(2);
        let mut storage = AddressSpaceStorage::<8>::new();

        let error = {
            let mut builder =
                AddressSpaceBuilder::new(&mut allocator, &mut store, &mut storage).unwrap();
            builder
                .map_new_zeroed(
                    VirtPage::from_start(0x0040_0000).unwrap(),
                    PageFlags::new(true, false, true, true).unwrap(),
                )
                .unwrap_err()
        };

        assert_eq!(error, VmError::Store(TestStoreError::InjectedFailure));
        assert_eq!(allocator.stats(), before);
        assert_eq!(storage.len(), 0);
    }

    #[test]
    fn translate_reports_an_unmapped_page() {
        let mut allocator = test_allocator::<8>(0x1000, 0x21_000);
        let mut store = TestFrameStore::default();
        let mut storage = AddressSpaceStorage::<8>::new();
        let builder = AddressSpaceBuilder::new(&mut allocator, &mut store, &mut storage).unwrap();
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
        let mut storage = AddressSpaceStorage::<8>::new();
        let mut builder =
            AddressSpaceBuilder::new(&mut allocator, &mut store, &mut storage).unwrap();
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
        assert_eq!(storage.len(), 0);
    }

    #[test]
    fn destroyed_storage_can_build_a_new_address_space() {
        let mut allocator = test_allocator::<8>(0x1000, 0x21_000);
        let mut store = TestFrameStore::default();
        let mut storage = AddressSpaceStorage::<8>::new();

        {
            let mut first =
                AddressSpaceBuilder::new(&mut allocator, &mut store, &mut storage).unwrap();
            first
                .map_new_zeroed(
                    VirtPage::from_start(0x0010_0000).unwrap(),
                    PageFlags::new(true, true, false, true).unwrap(),
                )
                .unwrap();
            first.finish().destroy(&mut allocator).unwrap();
        }

        let second = AddressSpaceBuilder::new(&mut allocator, &mut store, &mut storage).unwrap();
        assert_eq!(second.root().as_u64(), 0x1000);
        drop(second);
        assert_eq!(storage.len(), 0);
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
        let mut storage = AddressSpaceStorage::<8>::new();
        let mut builder =
            AddressSpaceBuilder::new(&mut allocator, &mut store, &mut storage).unwrap();
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
        assert_eq!(storage.len(), 0);
    }

    #[test]
    fn destroy_reports_allocator_errors() {
        let mut allocator = test_allocator::<8>(0x1000, 0x21_000);
        let mut store = TestFrameStore::default();
        let mut storage = AddressSpaceStorage::<8>::new();
        let space = AddressSpaceBuilder::new(&mut allocator, &mut store, &mut storage)
            .unwrap()
            .finish();
        let mut other = test_allocator::<1>(0x41_000, 0x42_000);

        assert_eq!(space.destroy(&mut other), Err(FrameError::OutOfRange));
    }
}
