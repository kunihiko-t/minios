//! RV32 ELFのロード範囲とリンカー由来のIMEM容量を照合する。

#[derive(Debug, PartialEq, Eq)]
struct Usage {
    used: u32,
    capacity: u32,
}

pub(crate) fn report(path: &std::path::Path) -> Result<String, String> {
    let elf = std::fs::read(path)
        .map_err(|error| format!("RV32 IMEM: {}を読めません: {error}", path.display()))?;
    let usage = inspect(&elf).map_err(|error| format!("RV32 IMEM: {}: {error}", path.display()))?;
    Ok(format!(
        "RV32 IMEM: 使用量 {} byte / 容量 {} byte、残量 {} byte（ロード領域の隙間を含む）\n",
        usage.used,
        usage.capacity,
        usage.capacity - usage.used
    ))
}

fn inspect(elf: &[u8]) -> Result<Usage, String> {
    let invalid = || "ELF32 little-endian RISC-V実行fileの構造が不正です".to_owned();
    let range = |offset: u32, length: u32| {
        let end = offset.checked_add(length).ok_or_else(invalid)?;
        elf.get(offset as usize..end as usize).ok_or_else(invalid)
    };
    let u16_at = |at: usize| -> Result<u16, String> {
        Ok(u16::from_le_bytes(
            elf.get(at..at + 2)
                .ok_or_else(invalid)?
                .try_into()
                .map_err(|_| invalid())?,
        ))
    };
    let u32_at = |at: usize| -> Result<u32, String> {
        Ok(u32::from_le_bytes(
            elf.get(at..at + 4)
                .ok_or_else(invalid)?
                .try_into()
                .map_err(|_| invalid())?,
        ))
    };
    if elf.get(..7) != Some(b"\x7fELF\x01\x01\x01")
        || u16_at(16)? != 2
        || u16_at(18)? != 243
        || u32_at(20)? != 1
    {
        return Err(invalid());
    }
    let phoff = u32_at(28)?;
    let shoff = u32_at(32)?;
    let phcount = u16_at(44)? as u32;
    let shcount = u16_at(48)? as u32;
    if u16_at(42)? != 32 || u16_at(46)? != 40 || phcount == 0 || shcount == 0 {
        return Err(invalid());
    }
    range(phoff, phcount * 32)?;
    range(shoff, shcount * 40)?;
    let mut origin = None;
    let mut capacity = None;
    for index in 0..shcount {
        let at = (shoff + index * 40) as usize;
        if u32_at(at + 4)? != 2 {
            continue;
        }
        let offset = u32_at(at + 16)?;
        let length = u32_at(at + 20)?;
        let link = u32_at(at + 24)?;
        if u32_at(at + 36)? != 16 || !length.is_multiple_of(16) || link >= shcount {
            return Err(invalid());
        }
        range(offset, length)?;
        let strings = (shoff + link * 40) as usize;
        if u32_at(strings + 4)? != 3 {
            return Err(invalid());
        }
        let names = range(u32_at(strings + 16)?, u32_at(strings + 20)?)?;
        for symbol in (0..length).step_by(16) {
            let at = (offset + symbol) as usize;
            let name = names.get(u32_at(at)? as usize..).ok_or_else(invalid)?;
            let end = name
                .iter()
                .position(|byte| *byte == 0)
                .ok_or_else(invalid)?;
            let slot = match &name[..end] {
                b"__imem_origin" => &mut origin,
                b"__imem_capacity" => &mut capacity,
                _ => continue,
            };
            if u16_at(at + 14)? != 0xfff1 || slot.replace(u32_at(at + 4)?).is_some() {
                return Err(invalid());
            }
        }
    }
    let origin = origin.ok_or("リンカーの__imem_originシンボルがありません")?;
    let capacity = capacity
        .filter(|value| *value > 0)
        .ok_or("リンカーの__imem_capacityシンボルがないか容量が0です")?;
    let limit = origin
        .checked_add(capacity)
        .ok_or("IMEM領域のアドレスが桁あふれしています")?;
    let mut high = origin;
    let mut loads = 0;
    for index in 0..phcount {
        let at = (phoff + index * 32) as usize;
        if u32_at(at)? != 1 {
            continue;
        }
        let length = u32_at(at + 16)?;
        if length == 0 {
            continue;
        } // BSSはIMEMにロードされない。
        if length > u32_at(at + 20)? {
            return Err(invalid());
        }
        range(u32_at(at + 4)?, length)?;
        let start = u32_at(at + 12)?; // .dataも仮想DMEMではなく物理ロードアドレスで数える。
        let end = start
            .checked_add(length)
            .ok_or("IMEMロード範囲のアドレスが桁あふれしています")?;
        if start < origin || end > limit {
            return Err(format!(
                "IMEMロード範囲 {start:#x}..{end:#x} がリンカー容量 {capacity} byte（{origin:#x}..{limit:#x}）を超えています"
            ));
        }
        high = high.max(end);
        loads += 1;
    }
    if loads == 0 {
        return Err("IMEMにロードするPT_LOAD領域がありません".into());
    }
    Ok(Usage {
        used: high - origin,
        capacity,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn put16(bytes: &mut [u8], at: usize, value: u16) {
        bytes[at..at + 2].copy_from_slice(&value.to_le_bytes());
    }
    fn put32(bytes: &mut [u8], at: usize, value: u32) {
        bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }
    // textとdataの間に隙間を持ち、dataの仮想アドレスはIMEM外にする。
    fn fixture() -> Vec<u8> {
        let mut elf = vec![0; 512];
        elf[..7].copy_from_slice(b"\x7fELF\x01\x01\x01");
        put16(&mut elf, 16, 2);
        put16(&mut elf, 18, 243);
        put32(&mut elf, 20, 1);
        put32(&mut elf, 28, 52);
        put16(&mut elf, 42, 32);
        put16(&mut elf, 44, 3);
        put32(&mut elf, 32, 160);
        put16(&mut elf, 46, 40);
        put16(&mut elf, 48, 3);
        for (index, paddr, filesz, vaddr) in [
            (0, 0, 8, 0),
            (1, 12, 4, 0x80000000),
            (2, 0x80000004, 0, 0x80000004),
        ] {
            let at = 52 + index * 32;
            put32(&mut elf, at, 1);
            put32(&mut elf, at + 4, 400);
            put32(&mut elf, at + 8, vaddr);
            put32(&mut elf, at + 12, paddr);
            put32(&mut elf, at + 16, filesz);
            put32(&mut elf, at + 20, filesz + 8);
        }
        put32(&mut elf, 200 + 4, 2);
        put32(&mut elf, 200 + 16, 300);
        put32(&mut elf, 200 + 20, 48);
        put32(&mut elf, 200 + 24, 2);
        put32(&mut elf, 200 + 36, 16);
        put32(&mut elf, 240 + 4, 3);
        put32(&mut elf, 240 + 16, 350);
        put32(&mut elf, 240 + 20, 36);
        let names = b"\0__imem_origin\0__imem_capacity\0";
        elf[350..350 + names.len()].copy_from_slice(names);
        put32(&mut elf, 316, 1);
        put32(&mut elf, 320, 0);
        put16(&mut elf, 330, 0xfff1);
        put32(&mut elf, 332, 15);
        put32(&mut elf, 336, 32);
        put16(&mut elf, 346, 0xfff1);
        elf
    }
    #[test]
    fn counts_data_load_gaps_and_ignores_bss() {
        assert_eq!(
            inspect(&fixture()),
            Ok(Usage {
                used: 16,
                capacity: 32
            })
        );
    }
    #[test]
    fn capacity_is_taken_from_linker_symbols() {
        let mut elf = fixture();
        put32(&mut elf, 336, 16);
        assert_eq!(
            inspect(&elf),
            Ok(Usage {
                used: 16,
                capacity: 16
            })
        );
    }
    #[test]
    fn rejects_overflow_missing_symbols_and_truncation() {
        let mut elf = fixture();
        put32(&mut elf, 336, 15);
        assert!(inspect(&elf).unwrap_err().contains("IMEM"));
        let mut elf = fixture();
        put16(&mut elf, 346, 0);
        assert!(inspect(&elf).is_err());
        for len in [0, 20, 51, 100, 320, 380, 403] {
            assert!(inspect(&fixture()[..len]).is_err(), "{len}");
        }
    }
    #[test]
    fn uses_nonzero_origin_and_rejects_address_overflow() {
        let mut elf = fixture();
        put32(&mut elf, 320, 0x1000);
        put32(&mut elf, 64, 0x1000);
        put32(&mut elf, 96, 0x100c);
        assert_eq!(
            inspect(&elf),
            Ok(Usage {
                used: 16,
                capacity: 32
            })
        );
        put32(&mut elf, 96, u32::MAX - 1);
        assert!(inspect(&elf).unwrap_err().contains("桁あふれ"));
        put32(&mut elf, 320, u32::MAX - 1);
        assert!(inspect(&elf).unwrap_err().contains("桁あふれ"));
    }
    #[test]
    fn rejects_empty_loads_and_invalid_symbol_tables() {
        let mut elf = fixture();
        put32(&mut elf, 68, 0);
        put32(&mut elf, 100, 0);
        assert!(inspect(&elf).unwrap_err().contains("PT_LOAD"));
        let mut elf = fixture();
        put32(&mut elf, 224, 3);
        assert!(inspect(&elf).is_err());
        let mut elf = fixture();
        put32(&mut elf, 316, 999);
        assert!(inspect(&elf).is_err());
    }
    #[test]
    fn rejects_wrong_arch_and_load_address() {
        let mut elf = fixture();
        put16(&mut elf, 18, 62);
        assert!(inspect(&elf).is_err());
        let mut elf = fixture();
        put32(&mut elf, 96, 0x80000000);
        assert!(inspect(&elf).is_err());
    }
}
