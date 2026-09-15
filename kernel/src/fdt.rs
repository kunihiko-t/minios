//! Flattened Device Tree (FDT/DTB) の読み取り。
//!
//! OpenSBIは`a1`へDTBの物理addressを渡す。QEMU `virt`はFDTをDRAM上端から
//! 2 MiB下の2 MiB整列境界へ置くため、MiniOSはRAM最後の2 MiBを予約し、
//! boot payload窓をその直下へ置く。
//!
//! パーサーは`&[u8]`だけを対象とする純粋ロジックであり、ホストテストで
//! 検証する。`kernel_main`が一度だけ呼び、結果を[`MachineSpec`]として保持する。

use core::ops::Range;

use minios_abi::boot::BUNDLE_MAX_LEN;

/// QEMUがFDTを配置するDRAM上端の予約長。payload窓はこの直下に置く。
pub const FDT_RESERVED_LEN: usize = 2 * 1024 * 1024;

const FDT_MAGIC: u32 = 0xd00d_feed;
const FDT_BEGIN_NODE: u32 = 1;
const FDT_END_NODE: u32 = 2;
const FDT_PROP: u32 = 3;
const FDT_NOP: u32 = 4;
const FDT_END: u32 = 9;

/// QEMU `virt`のnode階層は深さ4まで。学習対象のmachine記述に必要な
/// 深さだけを許容し、深い構造は探索しない。
const MAX_DEPTH: usize = 8;
/// node名とproperty名の読み取り上限。QEMUが出す名前はすべてこれより短い。
const MAX_NAME: usize = 64;

const HEADER_LEN: usize = 40;

/// DTBから取り出したmachine記述。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineSpec {
    /// `/memory`のreg第1エントリが示す物理RAM全体。
    pub ram: Range<usize>,
    /// `ns16550a`/`ns16550`互換の先頭nodeのregベース。
    pub uart_base: usize,
    /// `/cpus`の`timebase-frequency`。
    pub timebase_hz: u64,
    /// `compatible="virtio,mmio"`の各nodeのregベース（発見順、最大
    /// [`VIRTIO_MMIO_MAX`]個）。
    pub virtio_mmio: [usize; VIRTIO_MMIO_MAX],
    /// `virtio_mmio`の有効個数。
    pub virtio_mmio_count: usize,
}

/// QEMU virtが提供するvirtio-mmio transportのslot数。
pub const VIRTIO_MMIO_MAX: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FdtError {
    BadMagic,
    Truncated,
    UnsupportedVersion,
    BadStructure,
    MissingMemory,
    MissingUart,
    MissingTimebase,
    /// DTBは読めたが、MiniOSの配置契約に合わないRAM構成だった。
    UnsupportedMachine,
}

/// 走査中のnodeひとつ分の状態。`reg`の解読には親nodeのcellsを使う。
#[derive(Default)]
struct NodeScan {
    name_is_memory: bool,
    name_is_cpus: bool,
    device_is_memory: bool,
    has_ns16550: bool,
    is_virtio_mmio: bool,
    reg_cells: (u32, u32),
    reg_first: Option<(u64, u64)>,
    timebase: Option<u32>,
}

impl MachineSpec {
    /// DTBを検証し、MiniOSが必要とするmachine記述を取り出す。
    ///
    /// 構造と必要nodeの両方を確認する。不正なblobや、RAM・UART・
    /// timebaseを欠くmachine記述は`Err`で拒否する。
    pub fn from_dtb(bytes: &[u8]) -> Result<Self, FdtError> {
        let spec = parse_machine_spec(bytes)?;
        // RAM範囲がpayload窓とFDT予約を収められないmachineは受け付けない。
        let minimum = FDT_RESERVED_LEN + BUNDLE_MAX_LEN as usize;
        let aligned = spec.ram.start % 4096 == 0 && spec.ram.end % 4096 == 0;
        if !aligned || spec.ram.len() <= minimum || spec.uart_base == 0 {
            return Err(FdtError::UnsupportedMachine);
        }
        Ok(spec)
    }

    /// boot payloadの予約窓。FDT予約領域の直下の`BUNDLE_MAX_LEN`だけ。
    pub fn payload_window(&self) -> Range<usize> {
        let end = self.ram.end - FDT_RESERVED_LEN;
        end - BUNDLE_MAX_LEN as usize..end
    }

    /// フレームアロケーターが管理する物理RAMの上端。
    pub fn managed_end(&self) -> usize {
        self.payload_window().start
    }

    /// FDTが置かれるRAM最後の予約領域。
    pub fn fdt_region(&self) -> Range<usize> {
        self.ram.end - FDT_RESERVED_LEN..self.ram.end
    }
}

fn read_be32(bytes: &[u8], offset: usize) -> Result<u32, FdtError> {
    let field = bytes.get(offset..offset + 4).ok_or(FdtError::Truncated)?;
    Ok(u32::from_be_bytes(
        field.try_into().map_err(|_| FdtError::Truncated)?,
    ))
}

fn parse_machine_spec(bytes: &[u8]) -> Result<MachineSpec, FdtError> {
    if bytes.len() < HEADER_LEN {
        return Err(FdtError::Truncated);
    }
    if read_be32(bytes, 0)? != FDT_MAGIC {
        return Err(FdtError::BadMagic);
    }
    let totalsize = read_be32(bytes, 4)? as usize;
    if totalsize < HEADER_LEN || totalsize > bytes.len() {
        return Err(FdtError::Truncated);
    }
    let dtb = &bytes[..totalsize];
    let off_struct = read_be32(dtb, 8)? as usize;
    let off_strings = read_be32(dtb, 12)? as usize;
    let version = read_be32(dtb, 20)?;
    // version 16でsize_dt_struct等の長さfieldが揃う。QEMUは17を出す。
    if version < 16 {
        return Err(FdtError::UnsupportedVersion);
    }
    let size_strings = read_be32(dtb, 32)? as usize;
    let size_struct = read_be32(dtb, 36)? as usize;
    let structure = dtb
        .get(off_struct..off_struct + size_struct)
        .ok_or(FdtError::Truncated)?;
    let strings = dtb
        .get(off_strings..off_strings + size_strings)
        .ok_or(FdtError::Truncated)?;

    walk_structure(structure, strings)
}

fn prop_name(strings: &[u8], offset: usize) -> Result<&str, FdtError> {
    let tail = strings.get(offset..).ok_or(FdtError::BadStructure)?;
    let end = tail
        .iter()
        .position(|byte| *byte == 0)
        .ok_or(FdtError::BadStructure)?;
    if end > MAX_NAME {
        return Err(FdtError::BadStructure);
    }
    core::str::from_utf8(&tail[..end]).map_err(|_| FdtError::BadStructure)
}

/// `reg`の先頭エントリを`#address-cells`/`#size-cells`で解読する。
/// cellsは1または2 (32/64ビット) だけを受け付ける。
fn decode_reg(value: &[u8], cells: (u32, u32)) -> Result<(u64, u64), FdtError> {
    fn cell_bytes(value: &[u8], cursor: &mut usize, cells: u32) -> Result<u64, FdtError> {
        match cells {
            // `#size-cells = <0>`はsize fieldそのものを持たないことを表す
            // (`/cpus`以下の`cpu@n`の`reg`がこの形を取る)。
            0 => Ok(0),
            1 => {
                let number = read_be32(value, *cursor).map_err(|_| FdtError::BadStructure)?;
                *cursor += 4;
                Ok(u64::from(number))
            }
            2 => {
                let high = read_be32(value, *cursor).map_err(|_| FdtError::BadStructure)?;
                let low = read_be32(value, *cursor + 4).map_err(|_| FdtError::BadStructure)?;
                *cursor += 8;
                Ok((u64::from(high) << 32) | u64::from(low))
            }
            _ => Err(FdtError::BadStructure),
        }
    }
    let mut cursor = 0;
    let address = cell_bytes(value, &mut cursor, cells.0)?;
    let size = cell_bytes(value, &mut cursor, cells.1)?;
    Ok((address, size))
}

fn compatible_has_virtio_mmio(value: &[u8]) -> bool {
    value
        .split(|byte| *byte == 0)
        .any(|entry| entry == b"virtio,mmio")
}

fn compatible_has_ns16550(value: &[u8]) -> bool {
    value
        .split(|byte| *byte == 0)
        .any(|entry| entry == b"ns16550a" || entry == b"ns16550")
}

fn device_type_is_memory(value: &[u8]) -> bool {
    value == b"memory" || value == b"memory\0"
}

fn walk_structure(structure: &[u8], strings: &[u8]) -> Result<MachineSpec, FdtError> {
    let mut cursor = 0usize;
    let mut depth = 0usize;
    // cells[depth]はその深さのnodeが子に与えるcells。nodeの`reg`自体は
    // 親のcellsで解読するため、BEGIN時点の親cellsを`reg_cells`へ写す。
    let mut cells = [(2u32, 2u32); MAX_DEPTH];
    let mut scans: [Option<NodeScan>; MAX_DEPTH] = Default::default();
    let mut ram: Option<(u64, u64)> = None;
    let mut uart: Option<u64> = None;
    let mut timebase: Option<u32> = None;
    let mut virtio_mmio = [0usize; VIRTIO_MMIO_MAX];
    let mut virtio_mmio_count = 0usize;

    loop {
        let token = read_be32(structure, cursor).map_err(|_| FdtError::BadStructure)?;
        cursor += 4;
        match token {
            FDT_BEGIN_NODE => {
                if depth >= MAX_DEPTH {
                    return Err(FdtError::BadStructure);
                }
                let name_end = structure[cursor..]
                    .iter()
                    .position(|byte| *byte == 0)
                    .ok_or(FdtError::BadStructure)?;
                if name_end > MAX_NAME {
                    return Err(FdtError::BadStructure);
                }
                let name = core::str::from_utf8(&structure[cursor..cursor + name_end])
                    .map_err(|_| FdtError::BadStructure)?;
                cursor = (cursor + name_end + 1 + 3) & !3;
                let parent_cells = if depth == 0 { (2, 2) } else { cells[depth - 1] };
                scans[depth] = Some(NodeScan {
                    name_is_memory: name.starts_with("memory"),
                    name_is_cpus: name == "cpus",
                    reg_cells: parent_cells,
                    ..NodeScan::default()
                });
                cells[depth] = (2, 2);
                depth += 1;
            }
            FDT_END_NODE => {
                if depth == 0 {
                    return Err(FdtError::BadStructure);
                }
                depth -= 1;
                let scan = scans[depth].take().unwrap_or_default();
                if depth == 0 {
                    // root node自体のproperty評価は不要である。
                    continue;
                }
                if scan.name_is_memory && scan.device_is_memory && ram.is_none() {
                    ram = scan.reg_first;
                }
                if scan.name_is_cpus && scan.timebase.is_some() {
                    timebase = scan.timebase;
                }
                if scan.has_ns16550 && uart.is_none() {
                    uart = scan.reg_first.map(|(address, _)| address);
                }
                if scan.is_virtio_mmio
                    && virtio_mmio_count < VIRTIO_MMIO_MAX
                    && let Some((base, _)) = scan.reg_first
                    && let Ok(base) = usize::try_from(base)
                {
                    virtio_mmio[virtio_mmio_count] = base;
                    virtio_mmio_count += 1;
                }
            }
            FDT_PROP => {
                if depth == 0 {
                    return Err(FdtError::BadStructure);
                }
                let len = read_be32(structure, cursor)? as usize;
                let name_offset = read_be32(structure, cursor + 4)? as usize;
                cursor += 8;
                let value = structure
                    .get(cursor..cursor + len)
                    .ok_or(FdtError::BadStructure)?;
                cursor = (cursor + len + 3) & !3;
                let name = prop_name(strings, name_offset)?;
                let scan = scans[depth - 1].as_mut().ok_or(FdtError::BadStructure)?;
                match name {
                    "#address-cells" => cells[depth - 1].0 = read_be32(value, 0)?,
                    "#size-cells" => cells[depth - 1].1 = read_be32(value, 0)?,
                    "reg" => {
                        scan.reg_first = Some(decode_reg(value, scan.reg_cells)?);
                    }
                    "device_type" => scan.device_is_memory = device_type_is_memory(value),
                    "compatible" => {
                        scan.has_ns16550 = compatible_has_ns16550(value);
                        scan.is_virtio_mmio = compatible_has_virtio_mmio(value);
                    }
                    "timebase-frequency" => scan.timebase = Some(read_be32(value, 0)?),
                    _ => {}
                }
            }
            FDT_NOP => {}
            FDT_END => {
                if depth != 0 {
                    return Err(FdtError::BadStructure);
                }
                break;
            }
            _ => return Err(FdtError::BadStructure),
        }
    }

    let (ram_start, ram_len) = ram.ok_or(FdtError::MissingMemory)?;
    let ram_end = ram_start
        .checked_add(ram_len)
        .ok_or(FdtError::BadStructure)?;
    Ok(MachineSpec {
        ram: usize::try_from(ram_start).map_err(|_| FdtError::BadStructure)?
            ..usize::try_from(ram_end).map_err(|_| FdtError::BadStructure)?,
        uart_base: usize::try_from(uart.ok_or(FdtError::MissingUart)?)
            .map_err(|_| FdtError::BadStructure)?,
        timebase_hz: u64::from(timebase.ok_or(FdtError::MissingTimebase)?),
        virtio_mmio,
        virtio_mmio_count,
    })
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::vec::Vec;

    use super::{FDT_RESERVED_LEN, FdtError, MachineSpec};
    use minios_abi::boot::BUNDLE_MAX_LEN;

    struct Prop {
        name: &'static str,
        value: Vec<u8>,
    }

    struct Node {
        name: &'static str,
        props: Vec<Prop>,
        children: Vec<Node>,
    }

    fn prop(name: &'static str, value: Vec<u8>) -> Prop {
        Prop { name, value }
    }

    fn be32(value: u32) -> Vec<u8> {
        value.to_be_bytes().to_vec()
    }

    fn reg(cells: (u32, u32), address: u64, size: u64) -> Vec<u8> {
        let mut value = Vec::new();
        let mut push = |cells: u32, number: u64| {
            if cells == 2 {
                value.extend_from_slice(&((number >> 32) as u32).to_be_bytes());
            }
            value.extend_from_slice(&(number as u32).to_be_bytes());
        };
        push(cells.0, address);
        push(cells.1, size);
        value
    }

    /// QEMU `virt` `-m 128M`が渡すDTBと同じ形を組み立てる。
    /// `timebase`と`uart`を外せるのは、欠落経路の再現のためである。
    fn qemu_virt_dtb() -> Vec<u8> {
        qemu_virt_dtb_with(true, true)
    }

    fn qemu_virt_dtb_with(with_timebase: bool, with_uart: bool) -> Vec<u8> {
        let mut cpus_props = std::vec![
            prop("#address-cells", be32(1)),
            prop("#size-cells", be32(0)),
        ];
        if with_timebase {
            cpus_props.push(prop("timebase-frequency", be32(10_000_000)));
        }
        let mut soc_children = std::vec![];
        soc_children.push(Node {
            name: "virtio_mmio@10001000",
            props: std::vec![
                prop("reg", reg((2, 2), 0x1000_1000, 0x1000)),
                prop("compatible", b"virtio,mmio\0".to_vec()),
            ],
            children: std::vec![],
        });
        if with_uart {
            soc_children.push(Node {
                name: "serial@10000000",
                props: std::vec![
                    prop("reg", reg((2, 2), 0x1000_0000, 0x100)),
                    prop("compatible", b"ns16550a\0".to_vec()),
                ],
                children: std::vec![],
            });
        }
        build_dtb(&Node {
            name: "",
            props: std::vec![
                prop("#address-cells", be32(2)),
                prop("#size-cells", be32(2)),
                prop("compatible", b"riscv-virtio\0".to_vec()),
            ],
            children: std::vec![
                Node {
                    name: "memory@80000000",
                    props: std::vec![
                        prop("device_type", b"memory\0".to_vec()),
                        prop("reg", reg((2, 2), 0x8000_0000, 0x0800_0000)),
                    ],
                    children: std::vec![],
                },
                Node {
                    name: "cpus",
                    props: cpus_props,
                    children: std::vec![Node {
                        name: "cpu@0",
                        props: std::vec![prop("device_type", b"cpu\0".to_vec())],
                        children: std::vec![],
                    }],
                },
                Node {
                    name: "soc",
                    props: std::vec![
                        prop("#address-cells", be32(2)),
                        prop("#size-cells", be32(2)),
                        prop("compatible", b"simple-bus\0".to_vec()),
                    ],
                    children: soc_children,
                },
            ],
        })
    }

    const FDT_BEGIN_NODE: u32 = 1;
    const FDT_END_NODE: u32 = 2;
    const FDT_PROP: u32 = 3;
    const FDT_END: u32 = 9;

    fn build_dtb(root: &Node) -> Vec<u8> {
        let mut strings: Vec<u8> = Vec::new();
        let mut offsets: Vec<(&'static str, u32)> = Vec::new();
        let mut structure: Vec<u8> = Vec::new();

        fn string_offset(
            name: &'static str,
            strings: &mut Vec<u8>,
            offsets: &mut Vec<(&'static str, u32)>,
        ) -> u32 {
            if let Some((_, offset)) = offsets.iter().find(|(seen, _)| *seen == name) {
                return *offset;
            }
            let offset = strings.len() as u32;
            strings.extend_from_slice(name.as_bytes());
            strings.push(0);
            offsets.push((name, offset));
            offset
        }

        fn emit(
            node: &Node,
            structure: &mut Vec<u8>,
            strings: &mut Vec<u8>,
            offsets: &mut Vec<(&'static str, u32)>,
        ) {
            structure.extend_from_slice(&FDT_BEGIN_NODE.to_be_bytes());
            structure.extend_from_slice(node.name.as_bytes());
            structure.push(0);
            while structure.len() % 4 != 0 {
                structure.push(0);
            }
            for entry in &node.props {
                structure.extend_from_slice(&FDT_PROP.to_be_bytes());
                structure.extend_from_slice(&(entry.value.len() as u32).to_be_bytes());
                let offset = string_offset(entry.name, strings, offsets);
                structure.extend_from_slice(&offset.to_be_bytes());
                structure.extend_from_slice(&entry.value);
                while structure.len() % 4 != 0 {
                    structure.push(0);
                }
            }
            for child in &node.children {
                emit(child, structure, strings, offsets);
            }
            structure.extend_from_slice(&FDT_END_NODE.to_be_bytes());
        }

        emit(root, &mut structure, &mut strings, &mut offsets);
        structure.extend_from_slice(&FDT_END.to_be_bytes());

        let header_len = 40;
        let reserve_len = 16;
        let off_struct = header_len + reserve_len;
        let off_strings = off_struct + structure.len();
        let total = off_strings + strings.len();

        let mut dtb = Vec::with_capacity(total);
        let mut push = |value: u32| dtb.extend_from_slice(&value.to_be_bytes());
        push(0xd00d_feed);
        push(total as u32);
        push(off_struct as u32);
        push(off_strings as u32);
        push(header_len as u32);
        push(17);
        push(16);
        push(0);
        push(strings.len() as u32);
        push(structure.len() as u32);
        dtb.extend_from_slice(&[0; 16]);
        dtb.extend_from_slice(&structure);
        dtb.extend_from_slice(&strings);
        dtb
    }

    #[test]
    fn parses_the_qemu_virt_machine_spec() {
        let spec = MachineSpec::from_dtb(&qemu_virt_dtb()).unwrap();

        assert_eq!(spec.ram, 0x8000_0000..0x8800_0000);
        assert_eq!(spec.uart_base, 0x1000_0000);
        assert_eq!(spec.timebase_hz, 10_000_000);
        assert_eq!(spec.virtio_mmio[..spec.virtio_mmio_count], [0x1000_1000]);
    }

    #[test]
    fn derived_layout_keeps_the_fdt_reserve_clear_of_the_payload() {
        let spec = MachineSpec::from_dtb(&qemu_virt_dtb()).unwrap();

        assert_eq!(spec.managed_end(), 0x8780_0000);
        assert_eq!(spec.payload_window(), 0x8780_0000..0x87e0_0000);
        assert_eq!(spec.fdt_region(), 0x87e0_0000..0x8800_0000);
        assert_eq!(spec.payload_window().len(), BUNDLE_MAX_LEN as usize);
        assert_eq!(spec.fdt_region().len(), FDT_RESERVED_LEN);
    }

    #[test]
    fn rejects_a_bad_magic() {
        let mut dtb = qemu_virt_dtb();
        dtb[0] = 0xff;
        assert_eq!(MachineSpec::from_dtb(&dtb), Err(FdtError::BadMagic));
    }

    #[test]
    fn rejects_a_truncated_blob() {
        let dtb = qemu_virt_dtb();
        let cut = &dtb[..dtb.len() - 200];
        assert_eq!(MachineSpec::from_dtb(cut), Err(FdtError::Truncated));
    }

    #[test]
    fn rejects_a_dtb_without_the_required_nodes() {
        assert_eq!(
            MachineSpec::from_dtb(&qemu_virt_dtb_with(false, true)),
            Err(FdtError::MissingTimebase)
        );
        assert_eq!(
            MachineSpec::from_dtb(&qemu_virt_dtb_with(true, false)),
            Err(FdtError::MissingUart)
        );
    }

    #[test]
    fn rejects_a_dtb_without_memory() {
        let dtb = build_dtb(&Node {
            name: "",
            props: std::vec![
                prop("#address-cells", be32(2)),
                prop("#size-cells", be32(2)),
            ],
            children: std::vec![Node {
                name: "cpus",
                props: std::vec![prop("timebase-frequency", be32(10_000_000))],
                children: std::vec![],
            }],
        });
        assert_eq!(MachineSpec::from_dtb(&dtb), Err(FdtError::MissingMemory));
    }

    #[test]
    fn rejects_a_ram_too_small_for_the_mini_os_layout() {
        let mut dtb = qemu_virt_dtb();
        // memory regのsize (0x0800_0000) を1 MiBへ書き換える。
        let needle = 0x0800_0000_u32.to_be_bytes();
        let index = dtb.windows(4).position(|window| window == needle).unwrap();
        dtb[index..index + 4].copy_from_slice(&0x0010_0000_u32.to_be_bytes());
        assert_eq!(
            MachineSpec::from_dtb(&dtb),
            Err(FdtError::UnsupportedMachine)
        );
    }

    #[test]
    fn rejects_an_unterminated_structure_block() {
        let mut dtb = qemu_virt_dtb();
        // FDT_END tokenをNOPへ置き換え、構造が終わらないblobにする。
        let end_offset = u32::from_be_bytes(dtb[8..12].try_into().unwrap()) as usize
            + u32::from_be_bytes(dtb[36..40].try_into().unwrap()) as usize
            - 4;
        dtb[end_offset..end_offset + 4].copy_from_slice(&4_u32.to_be_bytes());
        assert_eq!(MachineSpec::from_dtb(&dtb), Err(FdtError::BadStructure));
    }

    #[test]
    fn rejects_an_old_format_version() {
        let mut dtb = qemu_virt_dtb();
        dtb[20..24].copy_from_slice(&3_u32.to_be_bytes());
        assert_eq!(
            MachineSpec::from_dtb(&dtb),
            Err(FdtError::UnsupportedVersion)
        );
    }
}
