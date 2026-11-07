use crate::device::Device;
use crate::{sha256_hex, ElabError, Machine, Result};
use object::{Object, ObjectSegment};
use std::path::Path;

/// Bytes of an ELF file at a virtual address, from a PT_LOAD segment.
/// The `object` crate handles ELF32/ELF64 and both byte orders.
pub fn elf_bytes_at(elf: &Path, va: u64, n: usize) -> Result<Vec<u8>> {
    let data = std::fs::read(elf)?;
    let file = object::File::parse(&*data)
        .map_err(|e| ElabError::msg(format!("cannot parse ELF: {e}")))?;
    for seg in file.segments() {
        let addr = seg.address();
        let size = seg.size();
        if addr <= va && va < addr.saturating_add(size) {
            let (off, _) = seg.file_range();
            let start = off + (va - addr);
            let end = start + n as u64;
            if end > data.len() as u64 {
                return Err(ElabError::msg("VA range outside the file"));
            }
            return Ok(data[start as usize..end as usize].to_vec());
        }
    }
    Err(ElabError::msg(format!(
        "VA 0x{va:x} is not in a PT_LOAD segment"
    )))
}

pub fn check_image(device: &Device, elf: &Path, machine: &mut dyn Machine) -> Result<()> {
    if let Some(want) = &device.image.elf_sha256 {
        let bytes = std::fs::read(elf)?;
        let got = sha256_hex(&bytes);
        if &got != want {
            return Err(ElabError::msg(format!(
                "ELF digest {got} != {want} (wrong image or file)"
            )));
        }
    }
    for v in &device.image.verify {
        let addr = v.addr.resolve(machine)?;
        let live = machine.read_memory(addr, v.n)?;
        let file = elf_bytes_at(elf, addr, v.n)?;
        if live != file {
            return Err(ElabError::msg(format!(
                "live memory at 0x{addr:x} does not match the ELF (wrong image?)"
            )));
        }
    }
    if let Some(m) = &device.image.model {
        let addr = m.addr.resolve(machine)?;
        let live = machine.read_memory(addr, m.size)?;
        let got = sha256_hex(&live);
        if got != m.sha256 {
            return Err(ElabError::msg(format!(
                "model digest {got} != {} (model changed)",
                m.sha256
            )));
        }
    }
    Ok(())
}
