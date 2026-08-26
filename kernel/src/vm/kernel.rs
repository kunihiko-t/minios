use core::ops::Range;

use crate::memory::{KernelSections, frame::PAGE_SIZE};

use super::{PageFlags, PhysAddr, VirtPage};

const UART_START: usize = 0x1000_0000;
const UART_END: usize = UART_START + PAGE_SIZE;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KernelMapError {
    Unaligned,
    InvalidManagedRange,
}

#[derive(Clone)]
struct MappingRange {
    addresses: Range<usize>,
    flags: PageFlags,
}

pub struct KernelMapPlan {
    ranges: [MappingRange; 6],
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
            ],
        })
    }

    pub fn flags_at(&self, address: usize) -> Option<PageFlags> {
        self.ranges
            .iter()
            .find(|range| range.addresses.contains(&address))
            .map(|range| range.flags)
    }

    pub fn mappings(&self) -> impl Iterator<Item = KernelMapping> + '_ {
        self.ranges.iter().flat_map(|range| {
            range
                .addresses
                .clone()
                .step_by(PAGE_SIZE)
                .map(|address| KernelMapping {
                    page: VirtPage::from_start(address as u64)
                        .expect("validated kernel mapping ranges are page-aligned"),
                    physical: PhysAddr::try_new(address as u64)
                        .expect("kernel identity mappings fit the physical address field"),
                    flags: range.flags,
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
    use super::KernelMapPlan;
    use crate::{memory::KernelSections, vm::PageFlags};

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
}
