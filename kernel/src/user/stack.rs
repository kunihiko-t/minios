//! 初期user stackへのargv block構築。

use crate::{
    elf::{USER_STACK_BOTTOM, USER_STACK_TOP},
    memory::frame::PAGE_SIZE,
    vm::{AddressSpace, FrameStore, VirtAddr, VmError},
};

/// argv blockの構築に失敗した理由。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitialStackError<E> {
    /// blockが64 KiBのuser stackに収まらない。
    TooLarge,
    /// layout計算が表現できる範囲を超えた。
    AddressOverflow,
    /// 対象pageがuser address spaceに存在しない。
    NotMapped,
    /// 対象pageがU=1かつ書き込み可能ではない。
    NotWritable,
    /// kernel側frame storeの書き込みが失敗した。
    Store(E),
}

/// 構築済み初期stackの位置情報。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InitialStack {
    /// guestの初期`sp`。16 byte整列済みで、この位置に`argc`が置かれる。
    pub stack_pointer: usize,
    /// `a0`へ載せる引数個数 (program nameを含む)。
    pub argc: usize,
    /// `a1`へ載せるargv配列のuser仮想address。
    pub argv_address: usize,
}

/// program nameと引数をuser stack上限へ置き、`argc/argv`の位置を返す。
///
/// layout (低位→高位): `argc`、argv pointer列、NULL終端、空のenvp、空のauxv、
/// 各文字列 (NUL終端)。spは16 byteへ整列する。書き込みはpage単位で権限を
/// 検証しながら行われ、stack領域を下回るblockは拒否される。
pub fn write_initial_argv<const N: usize, M: FrameStore>(
    space: &AddressSpace<'_, N>,
    memory: &mut M,
    program_name: &str,
    arguments: &[&str],
) -> Result<InitialStack, InitialStackError<M::Error>> {
    let total = arguments
        .len()
        .checked_add(1)
        .ok_or(InitialStackError::AddressOverflow)?;

    // strings area: 上限から順にargv[0]..argv[n-1]を詰める。
    let mut string_addresses = [0usize; 65];
    if total > string_addresses.len() {
        return Err(InitialStackError::TooLarge);
    }
    let mut cursor = USER_STACK_TOP;
    for index in (0..total).rev() {
        let text = if index == 0 {
            program_name
        } else {
            arguments[index - 1]
        };
        let len = text
            .len()
            .checked_add(1)
            .ok_or(InitialStackError::AddressOverflow)?;
        cursor = cursor
            .checked_sub(len as u64)
            .ok_or(InitialStackError::AddressOverflow)?;
        string_addresses[index] = cursor as usize;
    }
    let strings_bottom = cursor;

    // pointer block: argc + argv列 + NULL終端 + 空envp + 空auxv。
    let pointer_bytes = total
        .checked_add(3)
        .and_then(|words| words.checked_mul(8))
        .and_then(|bytes| bytes.checked_add(8))
        .ok_or(InitialStackError::AddressOverflow)?;
    let unaligned = strings_bottom
        .checked_sub(pointer_bytes as u64)
        .ok_or(InitialStackError::AddressOverflow)?;
    let aligned = unaligned & !15;
    if aligned < USER_STACK_BOTTOM {
        return Err(InitialStackError::TooLarge);
    }
    let stack_pointer = aligned as usize;
    let argv_address = stack_pointer + 8;

    copy_to_user(
        space,
        memory,
        stack_pointer as u64,
        &(total as u64).to_le_bytes(),
    )?;
    for (index, address) in string_addresses[..total].iter().enumerate() {
        copy_to_user(
            space,
            memory,
            argv_address as u64 + (index * 8) as u64,
            &(*address as u64).to_le_bytes(),
        )?;
    }
    for word in 0..3 {
        copy_to_user(
            space,
            memory,
            argv_address as u64 + ((total + word) * 8) as u64,
            &0u64.to_le_bytes(),
        )?;
    }
    copy_to_user(
        space,
        memory,
        string_addresses[0] as u64,
        program_name.as_bytes(),
    )?;
    copy_to_user(
        space,
        memory,
        string_addresses[0] as u64 + program_name.len() as u64,
        &[0],
    )?;
    for (index, text) in arguments.iter().enumerate() {
        copy_to_user(
            space,
            memory,
            string_addresses[index + 1] as u64,
            text.as_bytes(),
        )?;
        copy_to_user(
            space,
            memory,
            string_addresses[index + 1] as u64 + text.len() as u64,
            &[0],
        )?;
    }

    Ok(InitialStack {
        stack_pointer,
        argc: total,
        argv_address,
    })
}

/// user仮想rangeへ、pageごとにU=1かつW=1を確認しながらcopyする。
fn copy_to_user<const N: usize, M: FrameStore>(
    space: &AddressSpace<'_, N>,
    memory: &mut M,
    start: u64,
    bytes: &[u8],
) -> Result<(), InitialStackError<M::Error>> {
    let mut done = 0usize;
    while done < bytes.len() {
        let at = start + done as u64;
        let page_offset = at as usize % PAGE_SIZE;
        let chunk = core::cmp::min(PAGE_SIZE - page_offset, bytes.len() - done);
        let virtual_address =
            VirtAddr::try_new(at).map_err(|_| InitialStackError::AddressOverflow)?;
        let (physical, flags) =
            space
                .translate(memory, virtual_address)
                .map_err(|error| match error {
                    VmError::Store(store) => InitialStackError::Store(store),
                    _ => InitialStackError::NotMapped,
                })?;
        if !flags.user() || !flags.write() {
            return Err(InitialStackError::NotWritable);
        }
        memory
            .copy_into(
                physical.as_u64() as usize - page_offset,
                page_offset,
                &bytes[done..done + chunk],
            )
            .map_err(InitialStackError::Store)?;
        done += chunk;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::{boxed::Box, collections::BTreeMap, vec, vec::Vec};

    use super::{InitialStackError, write_initial_argv};
    use crate::{
        elf::{USER_STACK_TOP, fixture::valid_riscv64_elf, load_image},
        memory::frame::{FrameAllocator, PAGE_SIZE},
        user::context::UserContext,
        vm::{AddressSpace, AddressSpaceStorage, FrameStore, VirtAddr},
    };

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum TestStoreError {
        MissingFrame,
        RangeOutOfBounds,
    }

    #[derive(Default)]
    struct TestFrameStore {
        frames: BTreeMap<usize, Box<[u8; PAGE_SIZE]>>,
    }

    impl TestFrameStore {
        fn frame(&self, frame_start: usize) -> Result<&[u8; PAGE_SIZE], TestStoreError> {
            self.frames
                .get(&frame_start)
                .map(Box::as_ref)
                .ok_or(TestStoreError::MissingFrame)
        }

        fn frame_mut(
            &mut self,
            frame_start: usize,
        ) -> Result<&mut [u8; PAGE_SIZE], TestStoreError> {
            self.frames
                .get_mut(&frame_start)
                .map(Box::as_mut)
                .ok_or(TestStoreError::MissingFrame)
        }

        fn range(offset: usize, len: usize) -> Result<core::ops::Range<usize>, TestStoreError> {
            let end = offset
                .checked_add(len)
                .ok_or(TestStoreError::RangeOutOfBounds)?;
            if end > PAGE_SIZE {
                return Err(TestStoreError::RangeOutOfBounds);
            }
            Ok(offset..end)
        }
    }

    impl FrameStore for TestFrameStore {
        type Error = TestStoreError;

        fn zero_frame(&mut self, frame_start: usize) -> Result<(), Self::Error> {
            self.frames.insert(frame_start, Box::new([0; PAGE_SIZE]));
            Ok(())
        }

        fn read_u64(&self, frame_start: usize, index: usize) -> Result<u64, Self::Error> {
            if index >= PAGE_SIZE / 8 {
                return Err(TestStoreError::RangeOutOfBounds);
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
            if index >= PAGE_SIZE / 8 {
                return Err(TestStoreError::RangeOutOfBounds);
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

    // storageだけをimageへborrowし、memoryは独立にborrowできるようにする。
    macro_rules! loaded_fixture {
        () => {{
            let frames = unsafe { FrameAllocator::<16>::new(0x1000, 0x181_000) }.unwrap();
            let storage = AddressSpaceStorage::<2688>::new();
            (frames, storage)
        }};
    }

    macro_rules! load_image_from {
        ($frames:expr, $memory:expr, $storage:expr) => {{
            let bytes = valid_riscv64_elf();
            load_image(&bytes, $frames, $memory, $storage)
                .unwrap_or_else(|error| panic!("fixture image must load: {error:?}"))
        }};
    }

    fn read_user<const N: usize>(
        space: &AddressSpace<'_, N>,
        memory: &TestFrameStore,
        address: u64,
        output: &mut [u8],
    ) {
        let mut done = 0usize;
        while done < output.len() {
            let at = address + done as u64;
            let (physical, flags) = space
                .translate(memory, VirtAddr::try_new(at).unwrap())
                .unwrap_or_else(|error| panic!("translate {at:#x} failed: {error:?}"));
            assert!(
                flags.user() && flags.write(),
                "stack pages must stay user writable"
            );
            let offset = physical.as_u64() as usize % PAGE_SIZE;
            let chunk = core::cmp::min(PAGE_SIZE - offset, output.len() - done);
            memory
                .copy_out(
                    physical.as_u64() as usize - offset,
                    offset,
                    &mut output[done..done + chunk],
                )
                .unwrap();
            done += chunk;
        }
    }

    fn read_word<const N: usize>(
        space: &AddressSpace<'_, N>,
        memory: &TestFrameStore,
        address: u64,
    ) -> u64 {
        let mut word = [0u8; 8];
        read_user(space, memory, address, &mut word);
        u64::from_le_bytes(word)
    }

    fn read_cstr<const N: usize>(
        space: &AddressSpace<'_, N>,
        memory: &TestFrameStore,
        address: u64,
        max: usize,
    ) -> Vec<u8> {
        let mut text = Vec::new();
        for index in 0..max {
            let mut byte = [0u8; 1];
            read_user(space, memory, address + index as u64, &mut byte);
            if byte[0] == 0 {
                return text;
            }
            text.push(byte[0]);
        }
        panic!("string at {address:#x} is not NUL terminated within {max} bytes");
    }

    // Catches misaligned sp, missing NULs, wrong argc, broken argv pointers,
    // missing terminators, or wrong argument registers.
    #[test]
    fn argv_block_lays_out_strings_pointers_and_registers() {
        let (mut frames, mut storage) = loaded_fixture!();
        let mut memory = TestFrameStore::default();
        let image = load_image_from!(&mut frames, &mut memory, &mut storage);
        let initial = write_initial_argv(
            image.address_space(),
            &mut memory,
            "hello",
            &["alpha", "beta"],
        )
        .unwrap();
        let space = image.address_space();

        let sp = initial.stack_pointer as u64;
        assert_eq!(sp % 16, 0, "sp must be 16-byte aligned");
        assert!(sp < USER_STACK_TOP);
        assert_eq!(initial.argc, 3);
        assert_eq!(read_word(space, &memory, sp), 3);

        let argv = initial.argv_address as u64;
        for (index, expected) in ["hello", "alpha", "beta"].iter().enumerate() {
            let pointer = read_word(space, &memory, argv + index as u64 * 8);
            assert_eq!(
                read_cstr(space, &memory, pointer, 8),
                expected.as_bytes().to_vec(),
                "argv[{index}]"
            );
        }
        assert_eq!(read_word(space, &memory, argv + 3 * 8), 0, "argv NULL");
        assert_eq!(read_word(space, &memory, argv + 4 * 8), 0, "empty envp");
        assert_eq!(read_word(space, &memory, argv + 5 * 8), 0, "empty auxv");

        let context = UserContext::with_arguments(
            VirtAddr::try_new(0x0010_0000).unwrap(),
            VirtAddr::try_new(sp).unwrap(),
            initial.argc,
            initial.argv_address,
        );
        assert_eq!(context.register(10), 3);
        assert_eq!(context.register(11), initial.argv_address);
        assert_eq!(context.register(2), initial.stack_pointer);

        drop(image);
    }

    // Catches a single-page copy path that truncates blocks crossing the
    // stack's page boundaries.
    #[test]
    fn argv_block_spans_page_boundaries() {
        let long = "x".repeat(5000);
        let (mut frames, mut storage) = loaded_fixture!();
        let mut memory = TestFrameStore::default();
        let image = load_image_from!(&mut frames, &mut memory, &mut storage);
        let initial = write_initial_argv(
            image.address_space(),
            &mut memory,
            "hello",
            &[long.as_str()],
        )
        .unwrap();
        let space = image.address_space();

        assert_eq!(initial.argc, 2);
        let pointer = read_word(space, &memory, initial.argv_address as u64 + 8);
        assert_eq!(read_cstr(space, &memory, pointer, 5001).len(), 5000);

        drop(image);
    }

    // Catches accepting a block that no longer fits inside the 64 KiB stack.
    #[test]
    fn oversized_arguments_are_rejected() {
        let huge = "y".repeat(70_000);
        let (mut frames, mut storage) = loaded_fixture!();
        let mut memory = TestFrameStore::default();
        let image = load_image_from!(&mut frames, &mut memory, &mut storage);
        assert_eq!(
            write_initial_argv(
                image.address_space(),
                &mut memory,
                "hello",
                &[huge.as_str()]
            ),
            Err(InitialStackError::TooLarge)
        );
        drop(image);
    }
}
