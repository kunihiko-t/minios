use core::ops::Range;

use crate::memory::{BOOT_PAYLOAD_END, BOOT_PAYLOAD_START, KernelSections, frame::PAGE_SIZE};

use super::{PageFlags, PhysAddr, VirtPage};

const UART_START: usize = 0x1000_0000;
const UART_END: usize = UART_START + PAGE_SIZE;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KernelMapError {
    Unaligned,
    InvalidManagedRange,
    /// payload rangeが予約窓の外、非整列、またはmanaged範囲と隣接していない。
    InvalidPayloadRange,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct MappingRange {
    addresses: Range<usize>,
    flags: PageFlags,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KernelMapPlan {
    ranges: [MappingRange; 7],
}

impl KernelMapPlan {
    pub fn new(
        sections: &KernelSections,
        managed_start: usize,
        managed_end: usize,
    ) -> Result<Self, KernelMapError> {
        if !managed_start.is_multiple_of(PAGE_SIZE) || !managed_end.is_multiple_of(PAGE_SIZE) {
            return Err(KernelMapError::Unaligned);
        }
        if managed_start != sections.kernel_end() || managed_start > managed_end {
            return Err(KernelMapError::InvalidManagedRange);
        }

        Ok(Self {
            ranges: [
                MappingRange {
                    addresses: sections.text(),
                    flags: PageFlags::supervisor_rx(),
                },
                MappingRange {
                    addresses: sections.rodata(),
                    flags: PageFlags::supervisor_r(),
                },
                MappingRange {
                    addresses: sections.writable(),
                    flags: PageFlags::supervisor_rw(),
                },
                MappingRange {
                    addresses: sections.boot_stack(),
                    flags: PageFlags::supervisor_rw(),
                },
                MappingRange {
                    addresses: managed_start..managed_end,
                    flags: PageFlags::supervisor_rw(),
                },
                MappingRange {
                    addresses: UART_START..UART_END,
                    flags: PageFlags::supervisor_rw(),
                },
                // boot payloadの予約窓は既定では1pageもmapしない。
                // with_payload_pagesが検証済みの使用pageだけをS-mode read-onlyで加える。
                MappingRange {
                    addresses: managed_end..managed_end,
                    flags: PageFlags::supervisor_r(),
                },
            ],
        })
    }

    /// boot payloadの使用page (4 KiB単位で切り上げ) だけをS-mode read-onlyで
    /// 追加する。全8 MiBを常時mapしない。
    ///
    /// payload rangeは固定予約窓の先端から始まり、予約窓の中に
    /// 収まらなければならない。
    pub fn with_payload_pages(mut self, start: usize, len: usize) -> Result<Self, KernelMapError> {
        if start != BOOT_PAYLOAD_START || !start.is_multiple_of(PAGE_SIZE) {
            return Err(KernelMapError::InvalidPayloadRange);
        }
        if len == 0 {
            return Ok(self);
        }
        if len > BOOT_PAYLOAD_END - BOOT_PAYLOAD_START {
            return Err(KernelMapError::InvalidPayloadRange);
        }
        let page_count = len
            .checked_add(PAGE_SIZE - 1)
            .ok_or(KernelMapError::InvalidPayloadRange)?
            / PAGE_SIZE;
        let rounded_len = page_count
            .checked_mul(PAGE_SIZE)
            .ok_or(KernelMapError::InvalidPayloadRange)?;
        let end = start
            .checked_add(rounded_len)
            .ok_or(KernelMapError::InvalidPayloadRange)?;
        if end > BOOT_PAYLOAD_END {
            return Err(KernelMapError::InvalidPayloadRange);
        }
        self.ranges[6] = MappingRange {
            addresses: start..end,
            flags: PageFlags::supervisor_r(),
        };
        Ok(self)
    }

    pub fn flags_at(&self, address: usize) -> Option<PageFlags> {
        self.ranges
            .iter()
            .find(|range| range.addresses.contains(&address))
            .map(|range| range.flags)
    }

    pub fn mappings(&self) -> impl Iterator<Item = KernelMapping> + '_ {
        self.ranges.iter().flat_map(|range| {
            range.addresses.clone().step_by(PAGE_SIZE).map(|address| {
                KernelMapping::new(
                    VirtPage::from_start(address as u64)
                        .expect("validated kernel mapping ranges are page-aligned"),
                    PhysAddr::try_new(address as u64)
                        .expect("kernel identity mappings fit the physical address field"),
                    range.flags,
                )
            })
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KernelMapping {
    page: VirtPage,
    physical: PhysAddr,
    flags: PageFlags,
}

impl KernelMapping {
    /// Creates a kernel mapping for crate-internal consumers that validate
    /// borrowed kernel leaves before installation.
    pub(crate) const fn new(page: VirtPage, physical: PhysAddr, flags: PageFlags) -> Self {
        Self {
            page,
            physical,
            flags,
        }
    }

    pub const fn page(self) -> VirtPage {
        self.page
    }

    pub const fn physical(self) -> PhysAddr {
        self.physical
    }

    pub const fn flags(self) -> PageFlags {
        self.flags
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::{KernelMapError, KernelMapPlan};
    use crate::{memory::KernelSections, vm::PageFlags};
    use std::{collections::BTreeSet, vec::Vec};

    fn fixture_sections() -> KernelSections {
        KernelSections::new(
            0x8020_0000..0x8020_2000,
            0x8020_2000..0x8020_3000,
            0x8020_3000..0x8020_5000,
            0x8020_5000..0x8021_5000,
            0x8021_5000,
        )
        .unwrap()
    }

    // Catches granting write access to executable/read-only sections, omitting
    // the UART mapping, or mapping the first page of the reserved payload.
    #[test]
    fn kernel_plan_uses_minimum_supervisor_permissions() {
        let sections = fixture_sections();
        let plan = KernelMapPlan::new(&sections, 0x8021_5000, 0x8780_0000).unwrap();

        assert_eq!(
            plan.flags_at(0x8020_0000).unwrap(),
            PageFlags::supervisor_rx()
        );
        assert_eq!(
            plan.flags_at(0x8020_2000).unwrap(),
            PageFlags::supervisor_r()
        );
        assert_eq!(
            plan.flags_at(0x8020_3000).unwrap(),
            PageFlags::supervisor_rw()
        );
        assert_eq!(
            plan.flags_at(0x1000_0000).unwrap(),
            PageFlags::supervisor_rw()
        );
        assert_eq!(plan.flags_at(0x8780_0000), None);
    }

    // Catches accidentally setting U on any section, managed-RAM, or UART
    // leaf while keeping the individual R/W/X bits otherwise correct.
    #[test]
    fn every_kernel_mapping_is_supervisor_only() {
        let sections = fixture_sections();
        let plan = KernelMapPlan::new(&sections, 0x8021_5000, 0x8780_0000).unwrap();

        assert!(plan.mappings().all(|mapping| !mapping.flags().user()));
    }

    // Catches skipped, duplicated, reordered, or non-identity pages anywhere
    // in the complete section/stack/managed-RAM/UART mapping sequence.
    #[test]
    fn kernel_plan_enumerates_the_exact_identity_page_sequence_once() {
        let sections = fixture_sections();
        let plan = KernelMapPlan::new(&sections, 0x8021_5000, 0x8780_0000).unwrap();
        let mappings = plan.mappings().collect::<Vec<_>>();
        let expected_pages = (0x8020_0000usize..0x8020_2000)
            .step_by(0x1000)
            .chain((0x8020_2000..0x8020_3000).step_by(0x1000))
            .chain((0x8020_3000..0x8020_5000).step_by(0x1000))
            .chain((0x8020_5000..0x8021_5000).step_by(0x1000))
            .chain((0x8021_5000..0x8780_0000).step_by(0x1000))
            .chain((0x1000_0000..0x1000_1000).step_by(0x1000))
            .map(|address| address as u64)
            .collect::<Vec<_>>();
        let actual_pages = mappings
            .iter()
            .map(|mapping| mapping.page().start().as_u64())
            .collect::<Vec<_>>();
        let unique_pages = actual_pages.iter().copied().collect::<BTreeSet<_>>();

        assert_eq!(mappings.len(), 30_209);
        assert_eq!(actual_pages, expected_pages);
        assert_eq!(unique_pages.len(), mappings.len());
        assert!(
            mappings
                .iter()
                .all(|mapping| { mapping.page().start().as_u64() == mapping.physical().as_u64() })
        );
    }

    // Catches inclusive range ends, a missing first/final page, permission
    // bleed across section boundaries, or any payload page becoming mapped.
    #[test]
    fn kernel_plan_honors_every_mapping_and_payload_boundary() {
        let sections = fixture_sections();
        let plan = KernelMapPlan::new(&sections, 0x8021_5000, 0x8780_0000).unwrap();

        for address in [0x8020_0000, 0x8020_1000] {
            assert_eq!(plan.flags_at(address), Some(PageFlags::supervisor_rx()));
        }
        assert_eq!(plan.flags_at(0x8020_2000), Some(PageFlags::supervisor_r()));
        for address in [
            0x8020_3000,
            0x8020_4000,
            0x8020_5000,
            0x8021_4000,
            0x8021_5000,
            0x877f_f000,
            0x1000_0000,
        ] {
            assert_eq!(plan.flags_at(address), Some(PageFlags::supervisor_rw()));
        }
        assert_eq!(plan.flags_at(0x8780_0000), None);
        assert_eq!(plan.flags_at(0x1000_1000), None);

        for payload_address in [0x8780_0000, 0x87c0_0000, 0x87ff_f000, 0x8800_0000] {
            assert_eq!(plan.flags_at(payload_address), None);
        }
        assert!(plan.mappings().all(|mapping| {
            let address = mapping.page().start().as_u64();
            !(0x8780_0000..0x8800_0000).contains(&address)
        }));
    }

    // Catches mapping the whole 8 MiB window, granting write or user access to
    // payload pages, rounding pages down, or letting payload pages replace the
    // identity mappings.
    #[test]
    fn payload_pages_map_exactly_the_used_pages_supervisor_read_only() {
        let sections = fixture_sections();
        let plan = KernelMapPlan::new(&sections, 0x8021_5000, 0x8780_0000)
            .unwrap()
            .with_payload_pages(0x8780_0000, 0x1234)
            .unwrap();

        // 0x1234 byteは2 pageへ切り上げされる。
        assert_eq!(plan.flags_at(0x8780_0000), Some(PageFlags::supervisor_r()));
        assert_eq!(plan.flags_at(0x8780_1000), Some(PageFlags::supervisor_r()));
        assert_eq!(plan.flags_at(0x8780_2000), None);
        for address in [0x8780_0000, 0x8780_1000] {
            let flags = plan.flags_at(address).unwrap();
            assert!(!flags.user());
            assert!(!flags.write());
            assert!(flags.read());
        }

        // 既存のidentity mappingはpayload pageに隣接したまま保たれる。
        assert_eq!(plan.flags_at(0x877f_f000), Some(PageFlags::supervisor_rw()));
        // 窓の残り (8 MiB - 8 KiB) はmapしない。
        assert_eq!(plan.flags_at(0x8790_0000), None);
        let payload_mappings = plan
            .mappings()
            .filter(|mapping| (0x8780_0000..0x8800_0000).contains(&mapping.page().start().as_u64()))
            .count();
        assert_eq!(payload_mappings, 2);
    }

    // Catches a payload range that does not start at the reserved window base
    // or that reaches past the window end.
    #[test]
    fn payload_pages_reject_ranges_outside_the_reserved_window() {
        let sections = fixture_sections();
        let base = KernelMapPlan::new(&sections, 0x8021_5000, 0x8780_0000).unwrap();

        assert_eq!(
            base.clone().with_payload_pages(0x8780_0001, 0x1000),
            Err(KernelMapError::InvalidPayloadRange)
        );
        assert_eq!(
            base.clone().with_payload_pages(0x8790_0000, 0x1000),
            Err(KernelMapError::InvalidPayloadRange)
        );
        assert_eq!(
            base.with_payload_pages(0x8780_0000, 8 * 1024 * 1024 + 1),
            Err(KernelMapError::InvalidPayloadRange)
        );
    }

    // Catches treating a plan's managed-RAM end as the payload window base,
    // which can skip the fixed reservation start and map an arbitrary window
    // suffix instead.
    #[test]
    fn payload_pages_reject_a_start_after_the_fixed_reserved_window_base() {
        let sections = fixture_sections();
        let plan = KernelMapPlan::new(&sections, 0x8021_5000, 0x8780_1000).unwrap();

        assert_eq!(
            plan.with_payload_pages(0x8780_1000, 0x1000),
            Err(KernelMapError::InvalidPayloadRange)
        );
    }

    // Catches overflowing while rounding an untrusted payload length up to a
    // page boundary before the reservation-window check can reject it.
    #[test]
    fn payload_pages_reject_an_overflowing_length_without_panicking() {
        let sections = fixture_sections();
        let plan = KernelMapPlan::new(&sections, 0x8021_5000, 0x8780_0000).unwrap();

        assert_eq!(
            plan.with_payload_pages(0x8780_0000, usize::MAX),
            Err(KernelMapError::InvalidPayloadRange)
        );
    }

    // Catches a zero-length payload range changing the plan at all.
    #[test]
    fn payload_pages_accept_an_empty_range_as_a_no_op() {
        let sections = fixture_sections();
        let plan = KernelMapPlan::new(&sections, 0x8021_5000, 0x8780_0000)
            .unwrap()
            .with_payload_pages(0x8780_0000, 0)
            .unwrap();

        assert_eq!(plan.flags_at(0x8780_0000), None);
        assert_eq!(plan.mappings().count(), 30_209);
    }
}
