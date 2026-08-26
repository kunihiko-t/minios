use crate::memory::frame;

pub const PAGE_SIZE: u64 = frame::PAGE_SIZE as u64;
pub const SV39_VA_BITS: u32 = 39;
pub const PTE_PPN_BITS: u32 = 44;

const PAGE_SHIFT: u32 = 12;
const SV39_SIGN_BIT: u64 = 1 << (SV39_VA_BITS - 1);
const SV39_UPPER_BITS: u64 = u64::MAX >> SV39_VA_BITS;
const MAX_PHYS_ADDR_EXCLUSIVE: u64 = 1 << (PTE_PPN_BITS + PAGE_SHIFT);
const MAX_PPN_EXCLUSIVE: u64 = 1 << PTE_PPN_BITS;

const _: () = assert!(PAGE_SIZE == 4096);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressError {
    NonCanonical,
    Unaligned,
    PpnOutOfRange,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtAddr(u64);

impl VirtAddr {
    pub fn try_new(value: u64) -> Result<Self, AddressError> {
        let upper_bits = value >> SV39_VA_BITS;
        let sign_extended = if value & SV39_SIGN_BIT == 0 {
            upper_bits == 0
        } else {
            upper_bits == SV39_UPPER_BITS
        };
        if !sign_extended {
            return Err(AddressError::NonCanonical);
        }
        Ok(Self(value))
    }

    pub const fn as_u64(self) -> u64 {
        self.0
    }

    pub const fn vpn(self) -> [usize; 3] {
        [
            ((self.0 >> 12) & 0x1ff) as usize,
            ((self.0 >> 21) & 0x1ff) as usize,
            ((self.0 >> 30) & 0x1ff) as usize,
        ]
    }

    pub const fn page_offset(self) -> usize {
        (self.0 & (PAGE_SIZE - 1)) as usize
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhysAddr(u64);

impl PhysAddr {
    pub fn try_new(value: u64) -> Result<Self, AddressError> {
        if value >= MAX_PHYS_ADDR_EXCLUSIVE {
            return Err(AddressError::PpnOutOfRange);
        }
        Ok(Self(value))
    }

    pub const fn as_u64(self) -> u64 {
        self.0
    }

    pub const fn page_offset(self) -> usize {
        (self.0 & (PAGE_SIZE - 1)) as usize
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtPage(u64);

impl VirtPage {
    pub const fn containing(address: VirtAddr) -> Self {
        Self(address.as_u64() & !(PAGE_SIZE - 1))
    }

    pub fn from_start(start: u64) -> Result<Self, AddressError> {
        let address = VirtAddr::try_new(start)?;
        if address.page_offset() != 0 {
            return Err(AddressError::Unaligned);
        }
        Ok(Self(start))
    }

    pub const fn start(self) -> VirtAddr {
        VirtAddr(self.0)
    }

    pub const fn vpn(self) -> [usize; 3] {
        self.start().vpn()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhysPageNum(u64);

impl PhysPageNum {
    pub fn try_new(value: u64) -> Result<Self, AddressError> {
        if value >= MAX_PPN_EXCLUSIVE {
            return Err(AddressError::PpnOutOfRange);
        }
        Ok(Self(value))
    }

    pub fn from_start(start: u64) -> Result<Self, AddressError> {
        let address = PhysAddr::try_new(start)?;
        if address.page_offset() != 0 {
            return Err(AddressError::Unaligned);
        }
        Self::try_new(start >> PAGE_SHIFT)
    }

    pub const fn as_u64(self) -> u64 {
        self.0
    }

    pub const fn start(self) -> PhysAddr {
        PhysAddr(self.0 << PAGE_SHIFT)
    }
}

#[cfg(test)]
mod tests {
    use super::{AddressError, PhysAddr, PhysPageNum, VirtAddr, VirtPage};

    // Catches accepting a non-canonical bit-38 extension or extracting VPNs
    // from the wrong bit positions.
    #[test]
    fn sv39_addresses_are_canonical_and_split_into_vpns() {
        let address = VirtAddr::try_new(0x0000_003f_ffff_f000).unwrap();
        assert_eq!(address.vpn(), [0x1ff, 0x1ff, 0xff]);
        assert_eq!(address.page_offset(), 0);
        assert_eq!(
            VirtAddr::try_new(0x0000_0040_0000_0000),
            Err(AddressError::NonCanonical)
        );
        assert_eq!(
            VirtAddr::try_new(0xffff_ffc0_0000_0000).unwrap().as_u64(),
            0xffff_ffc0_0000_0000
        );
    }

    // Catches rounding an explicit page start instead of rejecting it, or
    // failing to round a containing page down to its 4 KiB boundary.
    #[test]
    fn virtual_pages_round_containing_addresses_and_reject_unaligned_starts() {
        let address = VirtAddr::try_new(0x0000_0000_0012_3456).unwrap();
        assert_eq!(
            VirtPage::containing(address).start().as_u64(),
            0x0000_0000_0012_3000
        );
        assert_eq!(
            VirtPage::from_start(0x0000_0000_0012_3456),
            Err(AddressError::Unaligned)
        );
    }

    // Catches accepting a PPN that cannot fit into Sv39 PTE bits 53:10, or
    // accepting a physical address whose page number cannot be encoded.
    #[test]
    fn physical_page_numbers_stay_within_the_pte_field() {
        assert_eq!(
            PhysPageNum::try_new(0x0000_0fff_ffff_ffff)
                .unwrap()
                .as_u64(),
            0x0000_0fff_ffff_ffff
        );
        assert_eq!(
            PhysPageNum::try_new(0x0000_1000_0000_0000),
            Err(AddressError::PpnOutOfRange)
        );
        assert_eq!(
            PhysAddr::try_new(0x0100_0000_0000_0000),
            Err(AddressError::PpnOutOfRange)
        );
    }
}
