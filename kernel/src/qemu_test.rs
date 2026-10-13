#[cfg(any(
    feature = "qemu-test-fdt",
    feature = "qemu-test-virtio",
    feature = "qemu-test-heap",
    feature = "qemu-test-vm",
    feature = "qemu-test-elf",
    feature = "qemu-test-memory",
    feature = "qemu-test-net-arp",
    feature = "qemu-test-net-ping",
    feature = "qemu-test-smp-boot"
))]
use super::*;

#[cfg(all(target_arch = "riscv64", feature = "qemu-test-elf"))]
const ELF_FIXTURE_OWNED_FRAMES: usize = 23;
#[cfg(all(target_arch = "riscv64", feature = "qemu-test-elf"))]
const ELF_DIRTY_FRAME_COUNT: usize = 64;
#[cfg(all(target_arch = "riscv64", feature = "qemu-test-elf"))]
const _: () = assert!(ELF_DIRTY_FRAME_COUNT >= ELF_FIXTURE_OWNED_FRAMES);

#[cfg(all(target_arch = "riscv64", feature = "qemu-test-fdt"))]
// FDTから発見したmachine記述をhostが期待するQEMU `virt`の値と照合する。
// 値そのものをmarkerに載せ、違いがあればMissingMarkerとして検出される。
pub(crate) fn run_fdt_test(spec: &minios_kernel::fdt::MachineSpec) {
    crate::println!(
        "[MINIOS_TEST] fdt: ram=0x{:x}..0x{:x} uart=0x{:x} timebase={}",
        spec.ram.start,
        spec.ram.end,
        spec.uart_base,
        spec.timebase_hz
    );
    crate::println!("[MINIOS_TEST] fdt: ok");
    successful_qemu_test_shutdown()
}

#[cfg(all(target_arch = "riscv64", feature = "qemu-test-virtio"))]
// FDTが報告するvirtio-mmio slotからblock deviceをprobeし、FAT32 volumeを
// mountして既知fileの内容を検証する。
pub(crate) fn run_virtio_test(
    spec: &minios_kernel::fdt::MachineSpec,
    frames: &mut dyn FrameSource,
) {
    use minios_kernel::storage::fat32::Fat32;
    use minios_kernel::storage::virtio_blk::{VirtioBlk, VirtioError};

    let mut blk = None;
    for &base in &spec.virtio_mmio[..spec.virtio_mmio_count] {
        // Safety: `base`はFDTが報告したMMIO領域で、`with_device_pages`が
        // S-mode R+Wとしてmap済み。
        let mmio = unsafe { drivers::virtio_mmio::MmioRegs::new(base) };
        let Some(frame) = frames.allocate() else {
            fatal_qemu_test(format_args!("virtio: region frame allocation failed"));
        };
        match VirtioBlk::init(mmio, VirtioRegionPage::new(frame)) {
            Ok(device) => {
                crate::println!(
                    "[MINIOS_TEST] virtio: block at 0x{base:x} capacity={} sectors",
                    device.capacity_sectors()
                );
                blk = Some(device);
                break;
            }
            // 空slot (BadMagic) かblock以外のdeviceなら次のslotを試す。
            // 失敗したregionはdropでframeがpoolへ返る。
            Err(
                VirtioError::BadMagic
                | VirtioError::NotBlockDevice(_)
                | VirtioError::UnsupportedVersion(_),
            ) => {}
            Err(error) => {
                fatal_qemu_test(format_args!("virtio: init failed at 0x{base:x}: {error:?}"));
            }
        }
    }
    let Some(blk) = blk else {
        fatal_qemu_test(format_args!("virtio: no block device found"));
    };

    let mut fs = match Fat32::mount(blk) {
        Ok(fs) => fs,
        Err(error) => fatal_qemu_test(format_args!("virtio: mount failed: {error:?}")),
    };

    let mut listed = false;
    if fs
        .for_each_root_entry(|entry| {
            if entry.name() == "HELLO.TXT" {
                listed = true;
            }
        })
        .is_err()
    {
        fatal_qemu_test(format_args!("virtio: root listing failed"));
    }
    if !listed {
        fatal_qemu_test(format_args!("virtio: HELLO.TXT not in root listing"));
    }

    // disk image builderが書き込む固定内容と照合する。
    const EXPECTED: &[u8] = b"hello from virtio\n";
    let mut collected = [0u8; 64];
    let mut len = 0usize;
    if fs
        .read_root_file("HELLO.TXT", |chunk| {
            let end = (len + chunk.len()).min(collected.len());
            collected[len..end].copy_from_slice(&chunk[..end - len]);
            len = end;
        })
        .is_err()
    {
        fatal_qemu_test(format_args!("virtio: read HELLO.TXT failed"));
    }
    if &collected[..len] != EXPECTED {
        fatal_qemu_test(format_args!("virtio: HELLO.TXT content mismatch len={len}"));
    }
    crate::println!("[MINIOS_TEST] virtio: ok");
    successful_qemu_test_shutdown()
}

#[cfg(all(
    target_arch = "riscv64",
    any(feature = "qemu-test-net-arp", feature = "qemu-test-net-ping")
))]
type NetDevice = minios_kernel::net::virtio_net::VirtioNet<
    drivers::virtio_mmio::MmioRegs,
    FrameRegion<minios_kernel::net::virtio_net::NetQueueRegion>,
    FrameRegion<minios_kernel::net::virtio_net::NetRxBufPage>,
>;

#[cfg(all(
    target_arch = "riscv64",
    any(feature = "qemu-test-net-arp", feature = "qemu-test-net-ping")
))]
// FDTが報告するvirtio-mmio slotからnet deviceをprobeし、deviceとその
// PLIC sourceを返す。queue領域とrx pageはframe poolから取り、初期化できた
// deviceを見つけるまでslotを順に試す。
fn probe_net_device(
    spec: &minios_kernel::fdt::MachineSpec,
    frames: &mut dyn FrameSource,
) -> (NetDevice, u32) {
    use minios_kernel::net::virtio_net::{NetError, NetQueueRegion, NetRxBufPage, VirtioNet};

    for (slot, &base) in spec.virtio_mmio[..spec.virtio_mmio_count]
        .iter()
        .enumerate()
    {
        // Safety: `base`はFDTが報告したMMIO領域で、`with_device_pages`が
        // S-mode R+Wとしてmap済み。
        let mmio = unsafe { drivers::virtio_mmio::MmioRegs::new(base) };
        let Some(queue_frame) = frames.allocate() else {
            fatal_qemu_test(format_args!("net: queue frame allocation failed"));
        };
        let region = FrameRegion::<NetQueueRegion>::new(queue_frame);
        let mut rx_slots = [None, None, None, None];
        for slot in &mut rx_slots {
            let Some(frame) = frames.allocate() else {
                fatal_qemu_test(format_args!("net: rx frame allocation failed"));
            };
            *slot = Some(FrameRegion::<NetRxBufPage>::new(frame));
        }
        let rx_pages = rx_slots.map(|page| page.expect("rx page slots are all populated above"));
        match VirtioNet::init(mmio, region, rx_pages) {
            Ok(device) => {
                crate::println!(
                    "[MINIOS_TEST] net: virtio-net at 0x{base:x} mac={:02x?}",
                    device.mac()
                );
                let irq = spec.virtio_mmio_irq[slot];
                if irq != 0 {
                    drivers::plic::enable(irq);
                }
                return (device, irq);
            }
            // 空slotかnet以外のdeviceなら次のslotを試す。
            Err(
                NetError::BadMagic | NetError::NotNetDevice(_) | NetError::UnsupportedVersion(_),
            ) => {}
            Err(error) => {
                fatal_qemu_test(format_args!("net: init failed at 0x{base:x}: {error:?}"));
            }
        }
    }
    fatal_qemu_test(format_args!("net: no net device found"));
}

#[cfg(all(target_arch = "riscv64", feature = "qemu-test-net-arp"))]
// FDTが報告するvirtio-mmio slotからnet deviceをprobeし、QEMU user-netの
// gateway (10.0.2.2) へARP requestを送ってreplyを受け取る。TX/RXの両
// queueが実際にframeを運んだことと、used-buffer割り込みがPLIC経由で
// claimできたことをmarkerで観測する。
pub(crate) fn run_net_arp_test(
    spec: &minios_kernel::fdt::MachineSpec,
    frames: &mut dyn FrameSource,
) {
    use minios_kernel::net::arp;

    const GATEWAY_IP: [u8; 4] = [10, 0, 2, 2];
    const GUEST_IP: [u8; 4] = [10, 0, 2, 15];

    let (mut net, net_irq) = probe_net_device(spec, frames);

    let mut request = [0u8; arp::ARP_FRAME_LEN];
    arp::encode_request(&mut request, net.mac(), GUEST_IP, GATEWAY_IP);
    if let Err(error) = net.transmit(&request) {
        fatal_qemu_test(format_args!("net: transmit failed: {error:?}"));
    }

    // replyをdeadlineまでpollする。used-buffer割り込みはPLIC claimで
    // 処理し、claim回数が割り込み経路の稼働証拠になる。
    let deadline = time::ticks() + 100;
    let mac = net.mac();
    let mut reply = None;
    while time::ticks() < deadline && reply.is_none() {
        drivers::plic::handle_external_pending();
        let mut found = None;
        match net.drain_rx(&mut |frame| {
            if let Some(candidate) = arp::parse_reply(frame, &mac) {
                found = Some(candidate);
            }
        }) {
            Ok(_) => reply = found,
            Err(error) => fatal_qemu_test(format_args!("net: drain failed: {error:?}")),
        }
    }
    let Some(reply) = reply else {
        let (rx_avail, rx_used, tx_used, queue_len, istatus) = net.debug_state();
        fatal_qemu_test(format_args!(
            "net: no ARP reply within deadline rx_avail={rx_avail} rx_used={rx_used} tx_used={tx_used} qlen={queue_len} istatus={istatus}"
        ));
    };
    crate::println!(
        "[MINIOS_TEST] net-arp: gateway {}.{}.{}.{} at {:02x?}",
        reply.sender_ip[0],
        reply.sender_ip[1],
        reply.sender_ip[2],
        reply.sender_ip[3],
        reply.sender_mac
    );
    // claimが一度も無ければdeviceのinterrupt lineが動いていない。
    if net_irq != 0 && drivers::plic::virtio_irq_claims_for(net_irq) == 0 {
        fatal_qemu_test(format_args!(
            "net: ARP reply arrived without a virtio interrupt claim"
        ));
    }
    crate::println!("[MINIOS_TEST] net-arp: ok");
    successful_qemu_test_shutdown()
}

#[cfg(all(target_arch = "riscv64", feature = "qemu-test-net-ping"))]
// virtio-netの上に最小network stackを載せ、QEMU user-netのgateway
// (10.0.2.2) へICMP echo requestを投げてreplyを受け取る。stack内の
// ARP解決・IPv4/ICMP codec・RX demuxが実deviceで動くことと、used-buffer
// 割り込みがPLIC経由でclaimできたことを観測する。
pub(crate) fn run_net_ping_test(
    spec: &minios_kernel::fdt::MachineSpec,
    frames: &mut dyn FrameSource,
) {
    use minios_kernel::net::stack::Stack;

    const GATEWAY_IP: [u8; 4] = [10, 0, 2, 2];
    const GUEST_IP: [u8; 4] = [10, 0, 2, 15];

    let (net, net_irq) = probe_net_device(spec, frames);
    let mut stack = Stack::new(net, GUEST_IP);
    if let Err(error) = stack.ping(GATEWAY_IP, 0x4d53, 1, b"minios-ping") {
        fatal_qemu_test(format_args!("net-ping: ping failed: {error:?}"));
    }
    crate::println!(
        "[MINIOS_TEST] net-ping: gateway {}.{}.{}.{} replied",
        GATEWAY_IP[0],
        GATEWAY_IP[1],
        GATEWAY_IP[2],
        GATEWAY_IP[3]
    );
    // stackの待機loopはused ringを直接pollするため、積まれたpending分を
    // 最後にまとめてclaimし、deviceのinterrupt lineが上がったことを確認する。
    drivers::plic::handle_external_pending();
    if net_irq != 0 && drivers::plic::virtio_irq_claims_for(net_irq) == 0 {
        fatal_qemu_test(format_args!(
            "net-ping: echo reply arrived without a virtio interrupt claim"
        ));
    }
    crate::println!("[MINIOS_TEST] net-ping: ok");
    successful_qemu_test_shutdown()
}

#[cfg(all(target_arch = "riscv64", feature = "qemu-test-heap"))]
// `GlobalAlloc`経由の`alloc`crate型と、ヒープ統計の整合をゲスト上で確認する。
pub(crate) fn run_heap_test() {
    use alloc::boxed::Box;
    use alloc::vec::Vec;

    // kernel address spaceの所有権台帳などがすでにheap上にあるため、
    // 検査はtest自身の割り当てが解放へ戻るかの差分で行う。基準値は
    // test自身が割り当てる前に取る。
    let baseline = KERNEL_HEAP.stats();

    // Vecの成長が再割り当てと既存内容の保持を繰り返すことを確認する。
    let mut values: Vec<u64> = Vec::new();
    for i in 0..512u64 {
        values.push(i * 3);
    }
    let sum: u64 = values.iter().sum();
    if values.len() != 512 || sum != 512 * 511 / 2 * 3 {
        fatal_qemu_test(format_args!("heap: vec contents mismatch"));
    }
    drop(values);

    // 解放後の新規割り当てが再利用と`dealloc`経路を通ることを確認する。
    let boxed = Box::new(0x5a5a_u64);
    if *boxed != 0x5a5a {
        fatal_qemu_test(format_args!("heap: box readback mismatch"));
    }
    drop(boxed);

    // VecとBoxをすべて解放したので、割り当て量は基準値へ戻っているはずである。
    let stats = KERNEL_HEAP.stats();
    if stats.total != KERNEL_HEAP_LEN || stats.free == 0 || stats.allocated != baseline.allocated {
        fatal_qemu_test(format_args!("heap: unexpected stats {stats:?}"));
    }

    // 初期領域を超える割り当てはframe poolからの成長で賄われる。
    // 台帳spanがheap下端に居座るため、要求は拡張側の空きだけで賄う。
    let frames_before = GlobalFrames.stats();
    let mut large: Vec<u8> = Vec::new();
    if large
        .try_reserve_exact(KERNEL_HEAP_LEN + PAGE_SIZE)
        .is_err()
    {
        fatal_qemu_test(format_args!("heap: growth allocation failed"));
    }
    let grown = KERNEL_HEAP.stats();
    let frames_after = GlobalFrames.stats();
    if grown.total <= KERNEL_HEAP_LEN || frames_after.allocated <= frames_before.allocated {
        fatal_qemu_test(format_args!(
            "heap: no growth: stats={grown:?} frames={frames_after:?}"
        ));
    }
    crate::println!(
        "[MINIOS_TEST] heap: grew total={} pool_free={}",
        grown.total,
        frames_after.free
    );
    drop(large);
    if KERNEL_HEAP.stats().allocated != baseline.allocated {
        fatal_qemu_test(format_args!("heap: grown allocation was not released"));
    }
    // 成長分がまとめて空くと、下端の空きpageはframe poolへ返され、
    // 領域とpoolの占有は成長前の基準へ戻る。
    let released = KERNEL_HEAP.stats();
    let frames_released = GlobalFrames.stats();
    if released.total != KERNEL_HEAP_LEN || frames_released.allocated != frames_before.allocated {
        fatal_qemu_test(format_args!(
            "heap: grown pages were not returned: stats={released:?} frames={frames_released:?}"
        ));
    }
    crate::println!(
        "[MINIOS_TEST] heap: returned total={} pool_allocated={}",
        released.total,
        frames_released.allocated
    );

    crate::println!(
        "[MINIOS_TEST] heap: total={} largest_free={} blocks={}",
        stats.total,
        stats.largest_free,
        stats.free_blocks
    );
    crate::println!("[MINIOS_TEST] heap: ok");
    successful_qemu_test_shutdown()
}

#[cfg(all(target_arch = "riscv64", feature = "qemu-test-vm"))]
pub(crate) fn run_vm_test(kernel_space: &AddressSpace, memory: &IdentityFrameStore) {
    expect_kernel_range(
        kernel_space,
        memory,
        ".text",
        core::ptr::addr_of!(__text_start) as usize,
        core::ptr::addr_of!(__text_end) as usize,
        (true, false, true, false),
    );
    expect_kernel_range(
        kernel_space,
        memory,
        ".rodata",
        core::ptr::addr_of!(__rodata_start) as usize,
        core::ptr::addr_of!(__rodata_end) as usize,
        (true, false, false, false),
    );
    expect_kernel_range(
        kernel_space,
        memory,
        ".data/.bss",
        core::ptr::addr_of!(__data_start) as usize,
        core::ptr::addr_of!(__bss_end) as usize,
        (true, true, false, false),
    );
    expect_kernel_range(
        kernel_space,
        memory,
        "boot stack",
        core::ptr::addr_of!(__boot_stack_start) as usize,
        core::ptr::addr_of!(__boot_stack_end) as usize,
        (true, true, false, false),
    );
    expect_kernel_range(
        kernel_space,
        memory,
        "UART",
        machine::spec().uart_base,
        machine::spec().uart_base + PAGE_SIZE,
        (true, true, false, false),
    );

    let payload = VirtAddr::try_new(machine::spec().payload_window().start as u64)
        .expect("payload start is an Sv39 address");
    let actual = kernel_space.translate(memory, payload);
    if !matches!(actual, Err(VmError::NotMapped)) {
        fatal_qemu_test(format_args!(
            "vm mapping payload @ {:#x}: expected NotMapped, actual {actual:?}",
            payload.as_u64()
        ));
    }

    crate::println!("[MINIOS_TEST] vm: ok");
    successful_qemu_test_shutdown()
}

#[cfg(all(target_arch = "riscv64", feature = "qemu-test-vm"))]
fn expect_kernel_range(
    kernel_space: &AddressSpace,
    memory: &IdentityFrameStore,
    region: &str,
    start: usize,
    end: usize,
    expected_flags: (bool, bool, bool, bool),
) {
    if start >= end || !start.is_multiple_of(PAGE_SIZE) || !end.is_multiple_of(PAGE_SIZE) {
        fatal_qemu_test(format_args!(
            "vm range {region}: expected nonempty page-aligned [start,end), actual [{start:#x},{end:#x})"
        ));
    }
    let expected_pages = (end - start) / PAGE_SIZE;
    let mut checked_pages = 0usize;
    for address in (start..end).step_by(PAGE_SIZE) {
        let virtual_address = VirtAddr::try_new(address as u64)
            .expect("linker and MMIO addresses are valid Sv39 values");
        let actual = kernel_space.translate(memory, virtual_address);
        let matches = matches!(
            actual,
            Ok((physical, flags))
                if physical.as_u64() == address as u64
                    && (flags.read(), flags.write(), flags.execute(), flags.user())
                        == expected_flags
        );
        if !matches {
            fatal_qemu_test(format_args!(
                "vm range {region} page {checked_pages}/{expected_pages} @ {address:#x} in [{start:#x},{end:#x}): expected identity physical={address:#x} flags(R,W,X,U)={expected_flags:?}, actual {actual:?}"
            ));
        }
        checked_pages += 1;
    }
    let observed_end = start + checked_pages * PAGE_SIZE;
    if checked_pages != expected_pages || observed_end != end {
        fatal_qemu_test(format_args!(
            "vm range {region}: expected {expected_pages} pages ending exclusively at {end:#x}, actual {checked_pages} pages ending at {observed_end:#x}"
        ));
    }
}

#[cfg(all(target_arch = "riscv64", feature = "qemu-test-elf"))]
pub(crate) fn run_elf_test(
    kernel_space: &AddressSpace,
    frames: &mut dyn FrameSource,
    memory: &mut IdentityFrameStore,
) {
    let before = frames.stats();
    dirty_reusable_elf_frames(frames, memory, before);
    if frames.stats() != before {
        fatal_qemu_test(format_args!(
            "elf dirty-frame recovery: allocator expected={before:?} actual={:?}",
            frames.stats()
        ));
    }

    let fixture = valid_riscv64_elf();
    let image = match load_image(&fixture, frames, memory) {
        Ok(image) => image,
        Err(error) => {
            fatal_qemu_test(format_args!(
                "elf load: {error:?}; allocator expected={before:?} actual={:?}",
                frames.stats()
            ));
        }
    };

    let inspection = inspect_loaded_elf(&image, kernel_space, memory);
    if let Err(error) = image.destroy(frames) {
        fatal_qemu_test(format_args!(
            "elf destroy: {:?}; inspection={inspection:?}; allocator expected={before:?} actual={:?}",
            error.frame_error(),
            frames.stats()
        ));
    }

    let after = frames.stats();
    if let Err(error) = inspection {
        fatal_qemu_test(format_args!(
            "elf inspection: {error:?}; recovery allocator expected={before:?} actual={after:?}"
        ));
    }
    if after != before {
        fatal_qemu_test(format_args!(
            "elf recovery allocator: expected={before:?}, actual={after:?}"
        ));
    }

    crate::println!("[MINIOS_TEST] elf: ok");
    successful_qemu_test_shutdown()
}

#[cfg(all(target_arch = "riscv64", feature = "qemu-test-elf"))]
fn dirty_reusable_elf_frames(
    frames: &mut dyn FrameSource,
    memory: &mut IdentityFrameStore,
    baseline: FrameStats,
) {
    let mut dirty_frames: [Option<PhysFrame>; ELF_DIRTY_FRAME_COUNT] =
        [const { None }; ELF_DIRTY_FRAME_COUNT];
    let pattern = [0xa5; 64];

    // FrameAllocator::allocate scans its bitmap from index zero. Holding the
    // lowest 64 free frames, dirtying them, and then releasing all of them
    // makes the next 23 fixture allocations reuse dirty frames before any
    // untouched frame. The host characterization test fixes that 23-frame
    // requirement (five page tables, two segments, and sixteen stack pages).
    for index in 0..ELF_DIRTY_FRAME_COUNT {
        let frame = match frames.allocate() {
            Some(frame) => frame,
            None => {
                let cleanup_error = release_dirty_elf_frames(frames, &mut dirty_frames);
                fatal_qemu_test(format_args!(
                    "elf dirty-frame allocation {index}/{ELF_DIRTY_FRAME_COUNT}: out of frames; cleanup={cleanup_error:?}; allocator expected={baseline:?} actual={:?}",
                    frames.stats()
                ));
            }
        };
        let frame_start = frame.start();
        dirty_frames[index] = Some(frame);
        for offset in (0..PAGE_SIZE).step_by(pattern.len()) {
            if let Err(error) = memory.copy_into(frame_start, offset, &pattern) {
                let cleanup_error = release_dirty_elf_frames(frames, &mut dirty_frames);
                fatal_qemu_test(format_args!(
                    "elf dirty-frame write {index}/{ELF_DIRTY_FRAME_COUNT} frame={frame_start:#x} offset={offset:#x}: {error:?}; cleanup={cleanup_error:?}; allocator expected={baseline:?} actual={:?}",
                    frames.stats()
                ));
            }
        }
    }

    if let Some(error) = release_dirty_elf_frames(frames, &mut dirty_frames) {
        fatal_qemu_test(format_args!(
            "elf dirty-frame release: {error:?}; allocator expected={baseline:?} actual={:?}",
            frames.stats()
        ));
    }
}

#[cfg(all(target_arch = "riscv64", feature = "qemu-test-elf"))]
fn release_dirty_elf_frames(
    frames: &mut dyn FrameSource,
    dirty_frames: &mut [Option<PhysFrame>; ELF_DIRTY_FRAME_COUNT],
) -> Option<FrameError> {
    let mut first_error = None;
    for slot in dirty_frames {
        if let Some(frame) = slot.take()
            && let Err(error) = frames.deallocate(frame)
            && first_error.is_none()
        {
            first_error = Some(error);
        }
    }
    first_error
}

#[cfg(all(target_arch = "riscv64", feature = "qemu-test-elf"))]
#[derive(Debug)]
enum ElfTestFailure {
    Entry {
        expected: u64,
        actual: u64,
    },
    Mapping {
        region: &'static str,
        address: u64,
        expected: PageFlags,
        actual: Result<(PhysAddr, PageFlags), VmError<IdentityFrameStoreError>>,
    },
    Read {
        region: &'static str,
        address: u64,
        error: VmError<IdentityFrameStoreError>,
    },
    Bytes {
        region: &'static str,
        address: u64,
        expected: [u8; 4],
        actual: [u8; 4],
    },
    NonZero {
        region: &'static str,
        address: u64,
        actual: u8,
    },
    Guard {
        address: u64,
        actual: Result<(PhysAddr, PageFlags), VmError<IdentityFrameStoreError>>,
    },
    UserSeparation {
        kernel: PageFlags,
        user: PageFlags,
    },
}

#[cfg(all(target_arch = "riscv64", feature = "qemu-test-elf"))]
impl core::fmt::Display for ElfTestFailure {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Entry { expected, actual } => {
                write!(
                    formatter,
                    "entry: expected={expected:#x}, actual={actual:#x}"
                )
            }
            Self::Mapping {
                region,
                address,
                expected,
                actual,
            } => write!(
                formatter,
                "{region} mapping @ {address:#x}: expected flags={expected:?}, actual={actual:?}"
            ),
            Self::Read {
                region,
                address,
                error,
            } => write!(
                formatter,
                "{region} bytes @ {address:#x}: translate/copy_out error={error:?}"
            ),
            Self::Bytes {
                region,
                address,
                expected,
                actual,
            } => write!(
                formatter,
                "{region} bytes @ {address:#x}: expected={expected:02x?}, actual={actual:02x?}"
            ),
            Self::NonZero {
                region,
                address,
                actual,
            } => write!(
                formatter,
                "{region} zero fill @ {address:#x}: expected=00, actual={actual:02x}"
            ),
            Self::Guard { address, actual } => write!(
                formatter,
                "guard @ {address:#x}: expected NotMapped, actual={actual:?}"
            ),
            Self::UserSeparation { kernel, user } => write!(
                formatter,
                "U-bit separation: expected kernel U=0/user U=1, actual kernel={kernel:?} user={user:?}"
            ),
        }
    }
}

#[cfg(all(target_arch = "riscv64", feature = "qemu-test-elf"))]
fn inspect_loaded_elf(
    image: &LoadedImage,
    kernel_space: &AddressSpace,
    memory: &IdentityFrameStore,
) -> Result<(), ElfTestFailure> {
    const ENTRY: u64 = 0x0010_0000;
    const DATA: u64 = 0x0020_0000;

    if image.entry().as_u64() != ENTRY {
        return Err(ElfTestFailure::Entry {
            expected: ENTRY,
            actual: image.entry().as_u64(),
        });
    }

    let user_rx =
        PageFlags::new(true, false, true, true).expect("fixture user text permissions are valid");
    let user_rw =
        PageFlags::new(true, true, false, true).expect("fixture user data permissions are valid");
    let text_flags = expect_elf_mapping(image, memory, "text", ENTRY, user_rx)?;
    let mut text = [0u8; 4];
    copy_virtual(image, memory, "text", ENTRY, &mut text)?;
    if text != [0x13, 0x00, 0x00, 0x00] {
        return Err(ElfTestFailure::Bytes {
            region: "text",
            address: ENTRY,
            expected: [0x13, 0x00, 0x00, 0x00],
            actual: text,
        });
    }
    expect_zero_virtual(image, memory, "text padding", ENTRY + 4, PAGE_SIZE - 4)?;

    expect_elf_mapping(image, memory, "data", DATA, user_rw)?;
    let mut data = [0u8; 4];
    copy_virtual(image, memory, "data", DATA, &mut data)?;
    if data != *b"MCB1" {
        return Err(ElfTestFailure::Bytes {
            region: "data",
            address: DATA,
            expected: *b"MCB1",
            actual: data,
        });
    }
    expect_zero_virtual(image, memory, "data/BSS", DATA + 4, PAGE_SIZE - 4)?;

    expect_elf_mapping(image, memory, "stack", USER_STACK_BOTTOM, user_rw)?;
    expect_zero_virtual(
        image,
        memory,
        "stack first page",
        USER_STACK_BOTTOM,
        PAGE_SIZE,
    )?;

    let guard = VirtAddr::try_new(USER_GUARD_BOTTOM).expect("guard address is valid Sv39");
    let actual_guard = image.address_space().translate(memory, guard);
    if !matches!(actual_guard, Err(VmError::NotMapped)) {
        return Err(ElfTestFailure::Guard {
            address: USER_GUARD_BOTTOM,
            actual: actual_guard,
        });
    }

    let kernel_text = core::ptr::addr_of!(__text_start) as usize;
    let kernel_virtual =
        VirtAddr::try_new(kernel_text as u64).expect("linker text start is a valid Sv39 address");
    let kernel_actual = kernel_space.translate(memory, kernel_virtual);
    let kernel_flags = match kernel_actual {
        Ok((physical, flags))
            if physical.as_u64() == kernel_text as u64 && flags == PageFlags::supervisor_rx() =>
        {
            flags
        }
        actual => {
            return Err(ElfTestFailure::Mapping {
                region: "kernel text",
                address: kernel_text as u64,
                expected: PageFlags::supervisor_rx(),
                actual,
            });
        }
    };
    if kernel_flags.user() || !text_flags.user() {
        return Err(ElfTestFailure::UserSeparation {
            kernel: kernel_flags,
            user: text_flags,
        });
    }

    Ok(())
}

#[cfg(all(target_arch = "riscv64", feature = "qemu-test-elf"))]
fn expect_elf_mapping(
    image: &LoadedImage,
    memory: &IdentityFrameStore,
    region: &'static str,
    address: u64,
    expected: PageFlags,
) -> Result<PageFlags, ElfTestFailure> {
    let virtual_address = VirtAddr::try_new(address).expect("fixture addresses are valid Sv39");
    let actual = image.address_space().translate(memory, virtual_address);
    match actual {
        Ok((_physical, flags)) if flags == expected => Ok(flags),
        actual => Err(ElfTestFailure::Mapping {
            region,
            address,
            expected,
            actual,
        }),
    }
}

#[cfg(all(target_arch = "riscv64", feature = "qemu-test-elf"))]
fn copy_virtual(
    image: &LoadedImage,
    memory: &IdentityFrameStore,
    region: &'static str,
    address: u64,
    output: &mut [u8],
) -> Result<(), ElfTestFailure> {
    let virtual_address = VirtAddr::try_new(address).expect("fixture addresses are valid Sv39");
    let (physical, _flags) = image
        .address_space()
        .translate(memory, virtual_address)
        .map_err(|error| ElfTestFailure::Read {
            region,
            address,
            error,
        })?;
    let physical = physical.as_u64() as usize;
    let frame_start = physical & !(PAGE_SIZE - 1);
    let offset = physical & (PAGE_SIZE - 1);
    memory
        .copy_out(frame_start, offset, output)
        .map_err(|error| ElfTestFailure::Read {
            region,
            address,
            error: VmError::Store(error),
        })
}

#[cfg(all(target_arch = "riscv64", feature = "qemu-test-elf"))]
fn expect_zero_virtual(
    image: &LoadedImage,
    memory: &IdentityFrameStore,
    region: &'static str,
    start: u64,
    len: usize,
) -> Result<(), ElfTestFailure> {
    let mut checked = 0usize;
    let mut bytes = [0u8; 64];
    while checked < len {
        let address = start + checked as u64;
        let page_remaining = PAGE_SIZE - address as usize % PAGE_SIZE;
        let chunk_len = core::cmp::min(core::cmp::min(bytes.len(), len - checked), page_remaining);
        copy_virtual(image, memory, region, address, &mut bytes[..chunk_len])?;
        if let Some((index, actual)) = bytes[..chunk_len]
            .iter()
            .copied()
            .enumerate()
            .find(|(_, byte)| *byte != 0)
        {
            return Err(ElfTestFailure::NonZero {
                region,
                address: address + index as u64,
                actual,
            });
        }
        checked += chunk_len;
    }
    Ok(())
}

#[cfg(all(target_arch = "riscv64", feature = "qemu-test-memory"))]
pub(crate) fn run_memory_test(frames: &mut dyn FrameSource) -> ! {
    let first = match frames.allocate() {
        Some(frame) => frame,
        None => fatal_memory_test(),
    };
    let first_start = first.start();
    let second = match frames.allocate() {
        Some(frame) => frame,
        None => fatal_memory_test(),
    };
    // アロケーターは4 KiB単位の物理フレームを返すため、二つのアドレスが異なり、境界にそろっていることをゲスト上で確認する。
    if first == second || first.start() % PAGE_SIZE != 0 || second.start() % PAGE_SIZE != 0 {
        fatal_memory_test();
    }
    if frames.deallocate(first).is_err() {
        fatal_memory_test();
    }
    let reused = match frames.allocate() {
        Some(frame) => frame,
        None => fatal_memory_test(),
    };
    if reused.start() != first_start {
        fatal_memory_test();
    }
    crate::println!("[MINIOS_TEST] memory: ok");
    arch::riscv64::sbi::system_reset(
        arch::riscv64::sbi::ResetType::Shutdown,
        arch::riscv64::sbi::ResetReason::NoReason,
    )
}

#[cfg(all(target_arch = "riscv64", feature = "qemu-test-memory"))]
fn fatal_memory_test() -> ! {
    crate::console::emergency_print(format_args!("MiniOS memory test failed\r\n"));
    arch::riscv64::sbi::system_reset(
        arch::riscv64::sbi::ResetType::Shutdown,
        arch::riscv64::sbi::ResetReason::SystemFailure,
    )
}

#[cfg(all(target_arch = "riscv64", feature = "qemu-test-smp-boot"))]
pub(crate) mod smp_boot {
    use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use super::*;

    /// secondary hartの専用stack。HSM entryが`sp`をここへ切り替えるまで
    /// Rustのコードは走らせられないため、bssの領域をそのまま使い切る。
    /// `u128`要素で16 byte整列を担保する。
    const SECONDARY_STACK_LEN: usize = 16 * 1024;
    static mut SECONDARY_STACK: [u128; SECONDARY_STACK_LEN / 16] = [0; SECONDARY_STACK_LEN / 16];

    static SECONDARY_READY: AtomicBool = AtomicBool::new(false);
    static SECONDARY_HART: AtomicUsize = AtomicUsize::new(usize::MAX);

    /// SBI HSM `hart_start`が呼ぶentry。`a0`=hart id、`a1`=opaqueで、
    /// opaqueにはboot側がstack topを渡す。`sp`を立ててから
    /// `secondary_main`へtail jumpする。entryの最初に`stvec`をpark
    /// loopへ向け、stack未設定の区間のfaultを無限loopで留める。
    #[unsafe(naked)]
    unsafe extern "C" fn secondary_entry() -> ! {
        core::arch::naked_asm!(
            "lla t0, 1f",
            "csrw stvec, t0",
            "mv sp, a1",
            "j {main}",
            "1: wfi",
            "j 1b",
            main = sym secondary_main
        )
    }

    /// secondary hartの起動確認だけを行い、以降はparkする。schedulerへの
    /// 参加はsmp-sched段階の仕事で、ここではS-modeへ自力到達した証拠を
    /// atomic flagとして残す。
    extern "C" fn secondary_main(hart_id: usize) -> ! {
        SECONDARY_HART.store(hart_id, Ordering::Relaxed);
        SECONDARY_READY.store(true, Ordering::Release);
        // `sie`が0なので`wfi`は何にも起こされない。spurious wakeupだけ吸収する。
        loop {
            // Safety: `wfi`は参照を持たない命令である。
            unsafe { core::arch::asm!("wfi", options(nomem, nostack)) };
        }
    }

    /// `-smp 2`環境でboot hartがSBI HSM経由でsecondary hartを起動し、
    /// secondaryがS-mode codeを実行したことをmarkerで観測する。
    pub(crate) fn run(boot_hart: usize) {
        // QEMU `virt`の`-smp 2`ではhart 0/1の二つ。boot側でない方を起こす。
        let secondary = if boot_hart == 0 { 1 } else { 0 };
        // Safety: SECONDARY_STACKはこのtest専用の静的領域で、起動される
        // secondary hartだけがstackとして使う。参照は作らずraw pointerで
        // 渡す。
        let stack_top = (&raw mut SECONDARY_STACK)
            .cast::<u8>()
            .wrapping_add(SECONDARY_STACK_LEN) as usize;
        if let Err(error) = arch::riscv64::sbi::hart_start(
            secondary,
            secondary_entry as *const () as usize,
            stack_top,
        ) {
            fatal_qemu_test(format_args!(
                "smp-boot: hart_start secondary={secondary} boot={boot_hart} entry=0x{:x} stack=0x{stack_top:x}: {error:?}",
                secondary_entry as *const () as usize
            ));
        }
        let deadline = time::ticks() + 100;
        while !SECONDARY_READY.load(Ordering::Acquire) {
            if time::ticks() >= deadline {
                fatal_qemu_test(format_args!(
                    "smp-boot: secondary hart {secondary} did not start"
                ));
            }
        }
        if SECONDARY_HART.load(Ordering::Relaxed) != secondary {
            fatal_qemu_test(format_args!("smp-boot: wrong hart id"));
        }
        crate::println!("[MINIOS_TEST] smp-boot: secondary hart={secondary}");
        crate::println!("[MINIOS_TEST] smp-boot: ok");
        successful_qemu_test_shutdown()
    }
}
