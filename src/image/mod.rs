//! K1 NOR loading of dynamic SBI firmware and an uncompressed Linux Image.

mod bootdt;
mod fdt;
pub mod layout;
mod linux;
mod memory;

use embedded_storage::nor_flash::ReadNorFlash;
use fdt::{Fdt, Node, integer, text, word};
use layout::{ENV_MAX_SIZE, ENV_OFFSET, Layout, crc32};

const SBI_LIMIT: usize = 0x0008_0000;
const LINUX_FIT_LIMIT: usize = 0x0200_0000;
// OpenSBI may grow the board DTB before handing it to Linux. Its final extent
// must remain inside this reserved destination, including all applied fixups.
pub(super) const DTB_WORKSPACE: usize = 0x0010_0000;
const STAGING_START: usize = 0x0400_0000;
const STAGING_SIZE: usize = SBI_LIMIT + LINUX_FIT_LIMIT;
const _: () = assert!(STAGING_START + STAGING_SIZE <= 0x0800_0000);

type Result<T> = core::result::Result<T, Error>;

/// NOR initialization or image loading failed.
#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    /// The NOR environment or partition layout is invalid.
    Environment(layout::Error),
    /// QSPI initialization failed.
    Qspi(spacemit_hal::qspi::Error),
    /// A NOR operation failed.
    Nor(qspi_nor::Error<spacemit_hal::qspi::Error>),
    /// An image or FDT structure is malformed.
    Format,
    /// A required image, property or board configuration is missing.
    Missing,
    /// The image encoding, compression or hash algorithm is unsupported.
    Unsupported,
    /// A size or offset exceeds its permitted region.
    Bounds,
    /// A payload load or entry address is invalid.
    Address,
    /// Loaded images overlap.
    Overlap,
    /// A CRC32 integrity check failed.
    Integrity,
    /// DRAM readback differs from the verified image.
    Readback,
    /// Image loading exhausted the application stack guard.
    Stack,
}

impl From<spacemit_hal::qspi::Error> for Error {
    fn from(error: spacemit_hal::qspi::Error) -> Self {
        Self::Qspi(error)
    }
}

impl From<qspi_nor::Error<spacemit_hal::qspi::Error>> for Error {
    fn from(error: qspi_nor::Error<spacemit_hal::qspi::Error>) -> Self {
        Self::Nor(error)
    }
}

/// A validated physical image destination.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Segment {
    /// Physical DRAM load address.
    pub address: usize,
    /// Image length in bytes.
    pub size: usize,
}

impl Segment {
    fn end(self) -> Result<usize> {
        self.address.checked_add(self.size).ok_or(Error::Bounds)
    }

    fn within(self, start: usize, end: usize) -> Result<()> {
        if self.size == 0 || self.address < start || self.end()? > end {
            return Err(Error::Address);
        }
        Ok(())
    }
}

/// Verified SBI, Linux and DTB destinations without a firmware handoff.
#[derive(Debug)]
pub struct LoadedImages {
    /// SBI code and data.
    pub sbi: Segment,
    /// Linux Image bytes copied from the FIT.
    pub kernel: Segment,
    /// Complete Linux runtime region, including BSS declared by its Image header.
    pub kernel_footprint: Segment,
    /// Board device tree bytes, with a separate 1 MiB destination reserved for SBI fixups.
    pub dtb: Segment,
    /// Optional initramfs bytes, matching the DTB's chosen initrd range.
    pub initrd: Option<Segment>,
    /// SBI entry address.
    pub sbi_entry: usize,
    /// Linux Image entry address.
    pub kernel_entry: usize,
}

struct Image<'a> {
    data: &'a [u8],
    destination: Segment,
    entry: usize,
}

/// Validated image bytes retained until copying finishes.
pub struct Images<'a> {
    sbi: Image<'a>,
    kernel: Image<'a>,
    kernel_footprint: Segment,
    dtb: Image<'a>,
    initrd: Option<Image<'a>>,
}

impl Images<'_> {
    /// Returns physical destinations without accessing hardware.
    pub fn destinations(&self) -> LoadedImages {
        LoadedImages {
            sbi: self.sbi.destination,
            kernel: self.kernel.destination,
            kernel_footprint: self.kernel_footprint,
            dtb: self.dtb.destination,
            initrd: self.initrd.as_ref().map(|image| image.destination),
            sbi_entry: self.sbi.entry,
            kernel_entry: self.kernel.entry,
        }
    }
}

/// Validates SBI and Linux FITs for an explicitly resolved board product.
///
/// The Linux FIT must contain one configuration whose description exactly matches
/// the selected board. Its default configuration never overrides board identity.
pub fn inspect<'a>(
    sbi: &'a [u8],
    linux_fit: &'a [u8],
    product_name: Option<&str>,
) -> Result<Images<'a>> {
    if sbi.len() > SBI_LIMIT {
        return Err(Error::Bounds);
    }
    let sbi_root = Fdt::root(sbi)?;
    let configurations = sbi_root.child("configurations")?;
    let selected = configurations.child(text(configurations.required("default")?)?)?;
    let sbi_node = sbi_root
        .child("images")?
        .child(text(selected.required("firmware")?)?)?;
    check(sbi_node, "type", "firmware")?;
    // Vendor SPL uses this tag for the dynamic SBI handoff, including compatible RustSBI firmware.
    check(sbi_node, "os", "opensbi")?;
    check(sbi_node, "arch", "riscv")?;
    let sbi = read_image(sbi_node)?;
    // Reserve the complete low 2 MiB for SBI firmware, including its own BSS/scratch.
    sbi.destination.within(0, 0x0020_0000)?;
    check_entry(&sbi)?;

    let linux = inspect_linux(linux_fit, product_name.ok_or(Error::Missing)?)?;
    Ok(Images {
        sbi,
        kernel: linux.kernel,
        kernel_footprint: linux.kernel_footprint,
        dtb: linux.dtb,
        initrd: linux.initrd,
    })
}

/// Validates a complete Linux FIT without reading firmware or touching hardware.
///
/// This uses the same header, board selection, destination and DTB checks as the
/// boot path and can be used by a host-side image packer.
pub fn validate_linux_fit(bytes: &[u8], product_name: &str) -> Result<()> {
    inspect_linux(bytes, product_name).map(|_| ())
}

struct LinuxImages<'a> {
    kernel: Image<'a>,
    kernel_footprint: Segment,
    dtb: Image<'a>,
    initrd: Option<Image<'a>>,
}

fn inspect_linux<'a>(bytes: &'a [u8], product_name: &str) -> Result<LinuxImages<'a>> {
    if bytes.len() > LINUX_FIT_LIMIT {
        return Err(Error::Bounds);
    }
    if product_name.is_empty() {
        return Err(Error::Missing);
    }
    let root = Fdt::root(bytes)?;
    let configurations = root.child("configurations")?;
    let mut selected = None;
    for node in configurations.children() {
        let node = node?;
        if node.property("description")?.map(text).transpose()? == Some(product_name)
            && selected.replace(node).is_some()
        {
            return Err(Error::Format);
        }
    }
    let configuration = selected.ok_or(Error::Missing)?;
    // A Linux configuration has no executable loadables or a separate firmware stage.
    for property in ["loadables", "firmware"] {
        if configuration.property(property)?.is_some() {
            return Err(Error::Unsupported);
        }
    }
    let images = root.child("images")?;
    let kernel_node = images.child(text(configuration.required("kernel")?)?)?;
    check(kernel_node, "type", "kernel")?;
    check(kernel_node, "os", "linux")?;
    check(kernel_node, "arch", "riscv")?;
    let kernel = read_image(kernel_node)?;
    let kernel_footprint = linux::validate(kernel.data, kernel.destination.address, kernel.entry)?;
    kernel_footprint.within(0x0020_0000, STAGING_START)?;

    let dtb_node = images.child(text(configuration.required("fdt")?)?)?;
    check(dtb_node, "type", "flat_dt")?;
    let dtb = read_image(dtb_node)?;
    dtb.destination.within(0x0020_0000, STAGING_START)?;
    if !dtb.destination.address.is_multiple_of(8)
        || dtb.data.len() > 0x40000
        || Fdt::size(dtb.data)? != dtb.data.len()
    {
        return Err(Error::Bounds);
    }
    let dtb_workspace = Segment {
        address: dtb.destination.address,
        size: DTB_WORKSPACE,
    };
    dtb_workspace.within(0x0020_0000, STAGING_START)?;
    Fdt::root(dtb.data)?;
    let initrd = configuration
        .property("ramdisk")?
        .map(|name| {
            let node = images.child(text(name)?)?;
            check(node, "type", "ramdisk")?;
            check(node, "os", "linux")?;
            check(node, "arch", "riscv")?;
            let image = read_image(node)?;
            image.destination.within(0x0020_0000, STAGING_START)?;
            Ok::<_, Error>(image)
        })
        .transpose()?;
    let segments = [
        Some(kernel_footprint),
        Some(dtb_workspace),
        initrd.as_ref().map(|image: &Image<'_>| image.destination),
    ];
    for (index, first) in segments.iter().enumerate() {
        let Some(first) = first else { continue };
        for second in segments[index + 1..].iter().flatten() {
            if first.address < second.end()? && second.address < first.end()? {
                return Err(Error::Overlap);
            }
        }
    }
    bootdt::validate(dtb.data, kernel_footprint, dtb.destination, segments[2])?;
    Ok(LinuxImages {
        kernel,
        kernel_footprint,
        dtb,
        initrd,
    })
}

fn check(node: Node<'_>, name: &str, expected: &str) -> Result<()> {
    if text(node.required(name)?)? != expected {
        return Err(Error::Unsupported);
    }
    Ok(())
}

fn check_entry(image: &Image<'_>) -> Result<()> {
    if !image.entry.is_multiple_of(2)
        || image.entry < image.destination.address
        || image.entry >= image.destination.end()?
    {
        return Err(Error::Address);
    }
    Ok(())
}

fn read_image(node: Node<'_>) -> Result<Image<'_>> {
    check(node, "compression", "none")?;
    // The inspected official FITs use inline data, not external data or overlays.
    for name in ["data-offset", "data-position", "data-size"] {
        if node.property(name)?.is_some() {
            return Err(Error::Unsupported);
        }
    }
    let data = node.required("data")?;
    let mut verified = false;
    for hash in node.children() {
        let hash = hash?;
        if hash.name.starts_with("hash") {
            check(hash, "algo", "crc32")?;
            if word(hash.required("value")?)? != crc32(data) {
                return Err(Error::Integrity);
            }
            verified = true;
        }
    }
    if !verified {
        return Err(Error::Missing);
    }
    let address = integer(node.required("load")?)?;
    let entry = node
        .property("entry")?
        .map(integer)
        .transpose()?
        .unwrap_or(address);
    Ok(Image {
        data,
        destination: Segment {
            address,
            size: data.len(),
        },
        entry,
    })
}

// Caller reserves trained DRAM 0..128 MiB and excludes all external users.
pub(crate) unsafe fn load(
    flash: &mut impl ReadNorFlash<Error = qspi_nor::Error<spacemit_hal::qspi::Error>>,
    product_name: Option<&str>,
) -> Result<LoadedImages> {
    // SAFETY: Staging is disjoint from validated destinations and all SRAM allocations.
    let staging = unsafe {
        core::ptr::write_bytes(STAGING_START as *mut u8, 0, STAGING_SIZE);
        core::slice::from_raw_parts_mut(STAGING_START as *mut u8, STAGING_SIZE)
    };
    flash.read(ENV_OFFSET, &mut staging[..ENV_MAX_SIZE])?;
    let layout = Layout::from_env(&staging[..ENV_MAX_SIZE], flash.capacity() as u32)
        .map_err(Error::Environment)?;
    crate::eprintln!(
        "NOR environment: {:#x} bytes, CRC32 verified",
        layout.env_size
    );
    crate::eprintln!(
        "NOR partitions: SBI {:#x}+{:#x}, Linux {:#x}+{:#x}",
        layout.sbi.offset,
        layout.sbi.size,
        layout.kernel.offset,
        layout.kernel.size
    );
    let (sbi, kernel) = staging.split_at_mut(SBI_LIMIT);
    let sbi = read_fit(
        flash,
        layout.sbi.offset,
        &mut sbi[..SBI_LIMIT.min(layout.sbi.size as usize)],
    )?;
    let kernel = read_fit(
        flash,
        layout.kernel.offset,
        &mut kernel[..LINUX_FIT_LIMIT.min(layout.kernel.size as usize)],
    )?;
    let images = inspect(sbi, kernel, product_name)?;
    for image in [&images.sbi, &images.kernel, &images.dtb]
        .into_iter()
        .chain(images.initrd.iter())
    {
        // SAFETY: All destinations were bounded, checked for overlap and verified before any copy.
        unsafe { memory::copy(image.destination.address, image.data)? };
    }
    Ok(images.destinations())
}

// The caller owns 256 initialized bytes at the stack's lower boundary.
pub(crate) unsafe fn check_stack_guard(bottom: usize) -> Result<()> {
    for offset in 0..256 {
        // SAFETY: The caller reserves this initialized guard, without concurrent writers.
        if unsafe { core::ptr::read_volatile((bottom + offset) as *const u8) } != 0xa5 {
            return Err(Error::Stack);
        }
    }
    Ok(())
}

fn read_fit<'a>(
    flash: &mut impl ReadNorFlash<Error = qspi_nor::Error<spacemit_hal::qspi::Error>>,
    offset: u32,
    buffer: &'a mut [u8],
) -> Result<&'a [u8]> {
    let available = flash
        .capacity()
        .checked_sub(offset as usize)
        .ok_or(Error::Bounds)?;
    if available < 40 || buffer.len() < 40 {
        return Err(Error::Bounds);
    }
    flash.read(offset, &mut buffer[..40])?;
    let size = Fdt::size(&buffer[..40])?;
    if size > available || size > buffer.len() {
        return Err(Error::Bounds);
    }
    flash.read(offset + 40, &mut buffer[40..size])?;
    Ok(&buffer[..size])
}

#[cfg(test)]
mod tests;
