use super::address::PhysPageNum;

const VALID: u64 = 1 << 0;
const READ: u64 = 1 << 1;
const WRITE: u64 = 1 << 2;
const EXECUTE: u64 = 1 << 3;
const USER: u64 = 1 << 4;
const GLOBAL: u64 = 1 << 5;
const ACCESSED: u64 = 1 << 6;
const DIRTY: u64 = 1 << 7;
const RSW: u64 = 0b11 << 8;
const PPN_SHIFT: u32 = 10;
const PPN_MASK: u64 = (1 << super::address::PTE_PPN_BITS) - 1;
const HIGH_RESERVED: u64 = u64::MAX << 54;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PteError {
    WriteWithoutRead,
    WritableExecutable,
    ReservedBits,
    InvalidEntry,
    InvalidBranch,
    NotLeaf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageFlags(u8);

impl PageFlags {
    pub fn new(read: bool, write: bool, execute: bool, user: bool) -> Result<Self, PteError> {
        if write && !read {
            return Err(PteError::WriteWithoutRead);
        }
        if write && execute {
            return Err(PteError::WritableExecutable);
        }

        let mut bits = 0;
        if read {
            bits |= READ as u8;
        }
        if write {
            bits |= WRITE as u8;
        }
        if execute {
            bits |= EXECUTE as u8;
        }
        if user {
            bits |= USER as u8;
        }
        Ok(Self(bits))
    }

    pub fn supervisor_rx() -> Self {
        Self::new(true, false, true, false).expect("supervisor R+X is valid")
    }

    pub fn supervisor_r() -> Self {
        Self::new(true, false, false, false).expect("supervisor R is valid")
    }

    pub fn supervisor_rw() -> Self {
        Self::new(true, true, false, false).expect("supervisor R+W is valid")
    }

    pub const fn read(self) -> bool {
        self.0 & READ as u8 != 0
    }

    pub const fn write(self) -> bool {
        self.0 & WRITE as u8 != 0
    }

    pub const fn execute(self) -> bool {
        self.0 & EXECUTE as u8 != 0
    }

    pub const fn user(self) -> bool {
        self.0 & USER as u8 != 0
    }

    fn from_leaf_bits(bits: u64) -> Result<Self, PteError> {
        Self::new(
            bits & READ != 0,
            bits & WRITE != 0,
            bits & EXECUTE != 0,
            bits & USER != 0,
        )
    }

    const fn pte_bits(self) -> u64 {
        self.0 as u64
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageTableEntry(u64);

impl PageTableEntry {
    pub const fn invalid() -> Self {
        Self(0)
    }

    pub fn branch(ppn: PhysPageNum) -> Result<Self, PteError> {
        Ok(Self((ppn.as_u64() << PPN_SHIFT) | VALID))
    }

    pub fn leaf(ppn: PhysPageNum, flags: PageFlags) -> Result<Self, PteError> {
        Ok(Self(
            (ppn.as_u64() << PPN_SHIFT) | flags.pte_bits() | VALID | ACCESSED | DIRTY,
        ))
    }

    pub fn from_bits(bits: u64) -> Result<Self, PteError> {
        if bits & (RSW | GLOBAL | HIGH_RESERVED) != 0 {
            return Err(PteError::ReservedBits);
        }
        if bits & VALID == 0 {
            return if bits == 0 {
                Ok(Self::invalid())
            } else {
                Err(PteError::InvalidEntry)
            };
        }
        PageFlags::from_leaf_bits(bits)?;
        if bits & (READ | EXECUTE) == 0 && bits & (WRITE | USER | GLOBAL | ACCESSED | DIRTY) != 0 {
            return Err(PteError::InvalidBranch);
        }
        Ok(Self(bits))
    }

    pub const fn bits(self) -> u64 {
        self.0
    }

    pub fn ppn(self) -> Result<PhysPageNum, PteError> {
        if !self.is_valid() {
            return Err(PteError::InvalidEntry);
        }
        // The bit mask is exactly PTE_PPN_BITS wide, so this constructor is
        // infallible after extraction.
        Ok(PhysPageNum::try_new((self.0 >> PPN_SHIFT) & PPN_MASK)
            .expect("PTE PPN mask always fits PhysPageNum"))
    }

    pub fn flags(self) -> Result<PageFlags, PteError> {
        if !self.is_valid() {
            return Err(PteError::InvalidEntry);
        }
        if !self.is_leaf() {
            return Err(PteError::NotLeaf);
        }
        PageFlags::from_leaf_bits(self.0)
    }

    pub const fn is_valid(self) -> bool {
        self.0 & VALID != 0
    }

    pub const fn is_leaf(self) -> bool {
        self.is_valid() && self.0 & (READ | EXECUTE) != 0
    }
}

#[cfg(test)]
mod tests {
    use super::{PageFlags, PageTableEntry, PteError};
    use crate::vm::PhysPageNum;

    // Catches admitting a W-only or W+X leaf through the public flag
    // constructor, before those permissions reach a page table.
    #[test]
    fn pte_rejects_write_without_read_and_user_writable_executable() {
        assert_eq!(
            PageFlags::new(false, true, false, true),
            Err(PteError::WriteWithoutRead)
        );
        assert_eq!(
            PageFlags::new(true, true, true, true),
            Err(PteError::WritableExecutable)
        );
    }

    // Catches a leaf encoder or decoder that shifts PPN/permission fields
    // incorrectly, drops U, or fails to recognize a leaf.
    #[test]
    fn leaf_pte_round_trips_ppn_and_flags() {
        let flags = PageFlags::new(true, false, true, true).unwrap();
        let entry = PageTableEntry::leaf(PhysPageNum::try_new(0x12345).unwrap(), flags).unwrap();
        assert_eq!(entry.ppn().unwrap().as_u64(), 0x12345);
        assert_eq!(entry.flags().unwrap(), flags);
        assert!(entry.is_leaf());
        assert_eq!(entry.bits(), 0x0000_0000_048d_14db);
    }

    // Catches decoding reserved RSW/high bits or malformed leaf permissions,
    // which must not create executable/writable mappings from raw PTE memory.
    #[test]
    fn pte_rejects_reserved_bits_and_invalid_leaf_permissions_when_decoding() {
        assert_eq!(
            PageTableEntry::from_bits(0x0000_0000_0000_0101),
            Err(PteError::ReservedBits)
        );
        assert_eq!(
            PageTableEntry::from_bits(0x0000_0000_0000_0005),
            Err(PteError::WriteWithoutRead)
        );
        assert_eq!(
            PageTableEntry::from_bits(0x0040_0000_0000_0001),
            Err(PteError::ReservedBits)
        );
    }

    // Catches raw PTE decoding that silently reintroduces G even though this
    // VM fixes every mapping as address-space-local.
    #[test]
    fn pte_rejects_the_global_bit_when_decoding() {
        assert_eq!(
            PageTableEntry::from_bits(0x0000_0000_048d_14fb),
            Err(PteError::ReservedBits)
        );
    }

    // Catches branch encodings that accidentally gain leaf, U, G, A, or D
    // permissions and leaf creation that omits software-managed A/D.
    #[test]
    fn branch_and_leaf_encode_the_required_hardware_bits() {
        let ppn = PhysPageNum::try_new(0x12345).unwrap();
        assert_eq!(
            PageTableEntry::branch(ppn).unwrap().bits(),
            0x0000_0000_048d_1401
        );

        let leaf =
            PageTableEntry::leaf(ppn, PageFlags::new(true, true, false, false).unwrap()).unwrap();
        assert_eq!(leaf.bits(), 0x0000_0000_048d_14c7);
    }
}
