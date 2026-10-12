extern crate std;

use super::{
    DTB_WORKSPACE, Error, Fdt, LINUX_FIT_LIMIT, SBI_LIMIT, STAGING_START, Segment, inspect, layout,
    validate_linux_fit,
};
use std::{vec, vec::Vec};

const PRODUCT: &str = "k1-x_MUSE-Pi-Pro";
const KERNEL_ADDRESS: u64 = 0x0100_0000;
const FOOTPRINT: u64 = 0x0040_0000;
const DTB_ADDRESS: u64 = 0x0300_0000;
const INITRD_ADDRESS: u64 = 0x0340_0000;
const INITRD: &[u8] = b"initramfs bytes";

// Self-contained version-17 trees; host tests never access physical memory.
#[derive(Default)]
struct Tree {
    structure: Vec<u8>,
    strings: Vec<u8>,
}
impl Tree {
    fn token(&mut self, value: u32) {
        self.structure.extend_from_slice(&value.to_be_bytes());
    }
    fn pad(&mut self) {
        while !self.structure.len().is_multiple_of(4) {
            self.structure.push(0);
        }
    }
    fn begin(&mut self, name: &str) {
        self.token(1);
        self.structure.extend_from_slice(name.as_bytes());
        self.structure.push(0);
        self.pad();
    }
    fn end(&mut self) {
        self.token(2);
    }
    fn property(&mut self, name: &str, value: &[u8]) {
        self.token(3);
        self.token(value.len() as u32);
        self.token(self.strings.len() as u32);
        self.strings.extend_from_slice(name.as_bytes());
        self.strings.push(0);
        self.structure.extend_from_slice(value);
        self.pad();
    }
    fn text(&mut self, name: &str, value: &str) {
        let mut b = value.as_bytes().to_vec();
        b.push(0);
        self.property(name, &b);
    }
    fn finish(mut self) -> Vec<u8> {
        self.token(9);
        let strings_offset = 56 + self.structure.len();
        let total = strings_offset + self.strings.len();
        let mut bytes = Vec::with_capacity(total);
        for field in [
            0xd00d_feed,
            total as u32,
            56,
            strings_offset as u32,
            40,
            17,
            16,
            0,
            self.strings.len() as u32,
            self.structure.len() as u32,
        ] {
            bytes.extend_from_slice(&field.to_be_bytes());
        }
        bytes.extend_from_slice(&[0; 16]);
        bytes.extend_from_slice(&self.structure);
        bytes.extend_from_slice(&self.strings);
        bytes
    }
}

fn reg(address: u64, size: u64) -> Vec<u8> {
    [address.to_be_bytes(), size.to_be_bytes()].concat()
}
fn board_dtb(compatible: &str, initrd: Option<Segment>) -> Vec<u8> {
    let mut t = Tree::default();
    t.begin("");
    t.text("compatible", compatible);
    t.property("#address-cells", &2u32.to_be_bytes());
    t.property("#size-cells", &2u32.to_be_bytes());
    t.begin("memory@0");
    t.text("device_type", "memory");
    t.property("reg", &reg(0, 0x0800_0000));
    t.end();
    t.begin("chosen");
    t.text("bootargs", "console=ttyS0,115200 earlycon");
    if let Some(s) = initrd {
        t.property("linux,initrd-start", &(s.address as u64).to_be_bytes());
        t.property("linux,initrd-end", &(s.end().unwrap() as u64).to_be_bytes());
    }
    t.end();
    t.begin("reserved-memory");
    t.property("#address-cells", &2u32.to_be_bytes());
    t.property("#size-cells", &2u32.to_be_bytes());
    t.property("ranges", &[]);
    t.begin("sbi@0");
    t.property("reg", &reg(0, 0x0020_0000));
    t.property("no-map", &[]);
    t.end();
    t.begin("rcpu@100000");
    t.text("status", "disabled");
    t.property("reg", &reg(0x0010_0000, 0x0050_0000));
    t.property("no-map", &[]);
    t.end();
    t.end();
    t.end();
    t.finish()
}

fn image(
    t: &mut Tree,
    name: &str,
    kind: &str,
    os: Option<&str>,
    data: &[u8],
    load: Option<u64>,
    entry: Option<u64>,
) {
    t.begin(name);
    t.text("type", kind);
    t.text("arch", "riscv");
    t.text("compression", "none");
    if let Some(os) = os {
        t.text("os", os);
    }
    if let Some(a) = load {
        t.property("load", &a.to_be_bytes());
    }
    if let Some(a) = entry {
        t.property("entry", &a.to_be_bytes());
    }
    t.property("data", data);
    t.begin("hash-1");
    t.text("algo", "crc32");
    t.property("value", &layout::crc32(data).to_be_bytes());
    t.end();
    t.end();
}
fn sbi_fit() -> Vec<u8> {
    let mut t = Tree::default();
    t.begin("");
    t.begin("images");
    image(
        &mut t,
        "sbi",
        "firmware",
        Some("opensbi"),
        &[0x13, 0, 0, 0, 0x73, 0, 0, 0],
        Some(0),
        Some(0),
    );
    t.end();
    t.begin("configurations");
    t.text("default", "conf-sbi");
    t.begin("conf-sbi");
    t.text("firmware", "sbi");
    t.end();
    t.end();
    t.end();
    t.finish()
}
fn kernel_image(size: u64) -> Vec<u8> {
    let mut bytes = vec![0; 256];
    bytes[..4].copy_from_slice(&0x0000_006fu32.to_le_bytes());
    bytes[8..16].copy_from_slice(&0x0020_0000u64.to_le_bytes());
    bytes[16..24].copy_from_slice(&size.to_le_bytes());
    bytes[32..36].copy_from_slice(&2u32.to_le_bytes());
    bytes[48..56].copy_from_slice(b"RISCV\0\0\0");
    bytes[56..60].copy_from_slice(b"RSC\x05");
    bytes
}
fn linux_fit() -> Vec<u8> {
    fit_with(
        &kernel_image(FOOTPRINT),
        Some(DTB_ADDRESS),
        None,
        "kernel",
        "linux",
        "kernel",
    )
}
fn fit_with(
    kernel: &[u8],
    dtb_load: Option<u64>,
    initrd: Option<(u64, &[u8])>,
    kind: &str,
    os: &str,
    reference: &str,
) -> Vec<u8> {
    let mut t = Tree::default();
    t.begin("");
    t.begin("images");
    image(
        &mut t,
        "kernel",
        kind,
        Some(os),
        kernel,
        Some(KERNEL_ADDRESS),
        Some(KERNEL_ADDRESS),
    );
    let range = initrd.map(|(address, data)| Segment {
        address: address as usize,
        size: data.len(),
    });
    for (name, compatible) in [
        ("fdt-bpi", "bananapi,bpi-f3"),
        ("fdt-card", "spacemit,muse-card"),
        ("fdt-pro", "spacemit,musepi-pro"),
    ] {
        image(
            &mut t,
            name,
            "flat_dt",
            None,
            &board_dtb(compatible, range),
            dtb_load,
            None,
        );
    }
    if let Some((a, data)) = initrd {
        image(
            &mut t,
            "initrd",
            "ramdisk",
            Some("linux"),
            data,
            Some(a),
            None,
        );
    }
    t.end();
    t.begin("configurations");
    t.text("default", "conf-bpi");
    for (name, product, dtb) in [
        ("conf-bpi", "k1-x_deb1", "fdt-bpi"),
        ("conf-card", "k1-x_MUSE-Card", "fdt-card"),
        ("conf-pro", PRODUCT, "fdt-pro"),
    ] {
        t.begin(name);
        t.text("description", product);
        t.text(reference, "kernel");
        t.text("fdt", dtb);
        if initrd.is_some() {
            t.text("ramdisk", "initrd");
        }
        t.end();
    }
    t.end();
    t.end();
    t.finish()
}
fn property_offset(bytes: &[u8], path: &[&str], property: &str) -> usize {
    let mut node = Fdt::root(bytes).unwrap();
    for name in path {
        node = node.child(name).unwrap();
    }
    node.required(property).unwrap().as_ptr() as usize - bytes.as_ptr() as usize
}
fn set_address(bytes: &mut [u8], name: &str, property: &str, address: u64) {
    let o = property_offset(bytes, &["images", name], property);
    bytes[o..o + 8].copy_from_slice(&address.to_be_bytes());
}
fn error(fit: &[u8]) -> Option<Error> {
    inspect(&sbi_fit(), fit, Some(PRODUCT)).err()
}

#[test]
fn linux_relocates_above_vendor_reservations_with_distinct_runtime_footprint() {
    let sbi = sbi_fit();
    let fit = linux_fit();
    let images = inspect(&sbi, &fit, Some(PRODUCT)).unwrap();
    let d = images.destinations();
    assert_eq!(
        d.sbi,
        Segment {
            address: 0,
            size: 8
        }
    );
    assert_eq!(d.sbi_entry, 0);
    assert_eq!(
        d.kernel,
        Segment {
            address: KERNEL_ADDRESS as usize,
            size: 256
        }
    );
    assert_eq!(d.kernel_footprint.size, FOOTPRINT as usize);
    assert_eq!(d.kernel_entry, KERNEL_ADDRESS as usize);
    assert_eq!(d.dtb.address, DTB_ADDRESS as usize);
    assert_eq!(d.initrd, None);
    assert_eq!(images.kernel.data, kernel_image(FOOTPRINT));
    assert!(validate_linux_fit(&fit, PRODUCT).is_ok());
}
#[test]
fn board_identity_is_required_and_overrides_fit_default() {
    let sbi = sbi_fit();
    let fit = linux_fit();
    for (product, compatible) in [
        ("k1-x_deb1", "bananapi,bpi-f3"),
        ("k1-x_MUSE-Card", "spacemit,muse-card"),
        (PRODUCT, "spacemit,musepi-pro"),
    ] {
        let images = inspect(&sbi, &fit, Some(product)).unwrap();
        assert_eq!(images.dtb.data, board_dtb(compatible, None));
    }
    for product in [None, Some(""), Some("unknown-board")] {
        assert_eq!(inspect(&sbi, &fit, product).err(), Some(Error::Missing));
    }
}
#[test]
fn optional_initrd_is_hashed_and_matches_chosen() {
    let sbi = sbi_fit();
    let fit = fit_with(
        &kernel_image(FOOTPRINT),
        Some(DTB_ADDRESS),
        Some((INITRD_ADDRESS, INITRD)),
        "kernel",
        "linux",
        "kernel",
    );
    let images = inspect(&sbi, &fit, Some(PRODUCT)).unwrap();
    assert_eq!(
        images.destinations().initrd,
        Some(Segment {
            address: INITRD_ADDRESS as usize,
            size: INITRD.len()
        })
    );
    assert_eq!(images.initrd.unwrap().data, INITRD);
    let mut corrupt = fit;
    let o = property_offset(&corrupt, &["images", "initrd"], "data");
    corrupt[o] ^= 1;
    assert_eq!(error(&corrupt), Some(Error::Integrity));
}
#[test]
fn changed_data_and_crc_are_rejected() {
    let original = linux_fit();
    for (path, property) in [
        (&["images", "kernel"][..], "data"),
        (&["images", "kernel", "hash-1"][..], "value"),
        (&["images", "fdt-pro"][..], "data"),
    ] {
        let mut fit = original.clone();
        let o = property_offset(&fit, path, property);
        fit[o] ^= 1;
        assert_eq!(error(&fit), Some(Error::Integrity));
    }
    let mut sbi = sbi_fit();
    let o = property_offset(&sbi, &["images", "sbi"], "data");
    sbi[o] ^= 1;
    assert_eq!(
        inspect(&sbi, &original, Some(PRODUCT)).err(),
        Some(Error::Integrity)
    );
}
#[test]
fn uboot_standalone_and_old_loadables_are_rejected() {
    for (kind, os, reference) in [
        ("standalone", "U-Boot", "kernel"),
        ("kernel", "U-Boot", "kernel"),
        ("standalone", "U-Boot", "loadables"),
    ] {
        assert_eq!(
            error(&fit_with(
                &kernel_image(FOOTPRINT),
                Some(DTB_ADDRESS),
                None,
                kind,
                os,
                reference
            )),
            Some(Error::Unsupported)
        );
    }
}
#[test]
fn compressed_and_elf_payloads_are_rejected() {
    let mut fit = linux_fit();
    let o = property_offset(&fit, &["images", "kernel"], "compression");
    fit[o..o + 5].copy_from_slice(b"gzip\0");
    assert_eq!(error(&fit), Some(Error::Unsupported));
    for prefix in [&b"\x7fELF"[..], &b"\x1f\x8b"[..]] {
        let mut k = kernel_image(FOOTPRINT);
        k[..prefix.len()].copy_from_slice(prefix);
        assert_eq!(
            error(&fit_with(
                &k,
                Some(DTB_ADDRESS),
                None,
                "kernel",
                "linux",
                "kernel"
            )),
            Some(Error::Unsupported)
        );
    }
}
#[test]
fn linux_header_magic_flags_version_offset_and_effective_size_are_checked() {
    for (offset, bytes, expected) in [
        (56, vec![0; 4], Error::Format),
        (32, 1u32.to_le_bytes().to_vec(), Error::Unsupported),
        (24, 1u64.to_le_bytes().to_vec(), Error::Unsupported),
        (8, 0u64.to_le_bytes().to_vec(), Error::Address),
        (16, 0u64.to_le_bytes().to_vec(), Error::Bounds),
        (16, 255u64.to_le_bytes().to_vec(), Error::Bounds),
        (16, u64::MAX.to_le_bytes().to_vec(), Error::Bounds),
        (36, vec![1], Error::Format),
    ] {
        let mut k = kernel_image(FOOTPRINT);
        k[offset..offset + bytes.len()].copy_from_slice(&bytes);
        assert_eq!(
            error(&fit_with(
                &k,
                Some(DTB_ADDRESS),
                None,
                "kernel",
                "linux",
                "kernel"
            )),
            Some(expected),
            "header offset {offset}"
        );
    }
    let k = kernel_image(FOOTPRINT);
    for size in 0..64 {
        assert_eq!(
            error(&fit_with(
                &k[..size],
                Some(DTB_ADDRESS),
                None,
                "kernel",
                "linux",
                "kernel"
            )),
            Some(Error::Format)
        );
    }
}
#[test]
fn kernel_bss_cannot_overlap_dtb_or_initrd() {
    let inside = KERNEL_ADDRESS + FOOTPRINT / 2;
    assert_eq!(
        error(&fit_with(
            &kernel_image(FOOTPRINT),
            Some(inside),
            None,
            "kernel",
            "linux",
            "kernel"
        )),
        Some(Error::Overlap)
    );
    assert_eq!(
        error(&fit_with(
            &kernel_image(FOOTPRINT),
            Some(DTB_ADDRESS),
            Some((inside, INITRD)),
            "kernel",
            "linux",
            "kernel"
        )),
        Some(Error::Overlap)
    );
    assert!(
        error(&fit_with(
            &kernel_image(DTB_ADDRESS - KERNEL_ADDRESS),
            Some(DTB_ADDRESS),
            None,
            "kernel",
            "linux",
            "kernel"
        ))
        .is_none()
    );
}
#[test]
fn dtb_and_initrd_cannot_overlap() {
    assert_eq!(
        error(&fit_with(
            &kernel_image(FOOTPRINT),
            Some(DTB_ADDRESS),
            Some((DTB_ADDRESS, INITRD)),
            "kernel",
            "linux",
            "kernel"
        )),
        Some(Error::Overlap)
    );
}
#[test]
fn dtb_growth_workspace_cannot_cover_initrd_or_staging() {
    let raw_size = board_dtb(
        "spacemit,musepi-pro",
        Some(Segment {
            address: INITRD_ADDRESS as usize,
            size: INITRD.len(),
        }),
    )
    .len();
    let immediately_after_raw_dtb = DTB_ADDRESS + raw_size as u64;
    assert_eq!(
        error(&fit_with(
            &kernel_image(FOOTPRINT),
            Some(DTB_ADDRESS),
            Some((immediately_after_raw_dtb, INITRD)),
            "kernel",
            "linux",
            "kernel"
        )),
        Some(Error::Overlap)
    );
    assert!(
        error(&fit_with(
            &kernel_image(FOOTPRINT),
            Some(DTB_ADDRESS),
            Some((DTB_ADDRESS + DTB_WORKSPACE as u64, INITRD)),
            "kernel",
            "linux",
            "kernel"
        ))
        .is_none()
    );
    assert_eq!(
        error(&fit_with(
            &kernel_image(FOOTPRINT),
            Some(STAGING_START as u64 - 0x10000),
            None,
            "kernel",
            "linux",
            "kernel"
        )),
        Some(Error::Address)
    );
}
#[test]
fn dtb_load_is_explicit_and_staging_is_not_a_destination() {
    assert_eq!(
        error(&fit_with(
            &kernel_image(FOOTPRINT),
            None,
            None,
            "kernel",
            "linux",
            "kernel"
        )),
        Some(Error::Missing)
    );
    for address in [
        0,
        0x0020_0000 - 8,
        STAGING_START as u64 - 8,
        STAGING_START as u64,
    ] {
        assert_eq!(
            error(&fit_with(
                &kernel_image(FOOTPRINT),
                Some(address),
                None,
                "kernel",
                "linux",
                "kernel"
            )),
            Some(Error::Address)
        );
    }
    assert_eq!(
        error(&fit_with(
            &kernel_image(STAGING_START as u64),
            Some(DTB_ADDRESS),
            None,
            "kernel",
            "linux",
            "kernel"
        )),
        Some(Error::Address)
    );
    assert_eq!(
        error(&fit_with(
            &kernel_image(FOOTPRINT),
            Some(DTB_ADDRESS),
            Some((STAGING_START as u64, INITRD)),
            "kernel",
            "linux",
            "kernel"
        )),
        Some(Error::Address)
    );
}
#[test]
fn kernel_entry_matches_its_aligned_load_and_other_aligned_bases_work() {
    for address in [
        0,
        KERNEL_ADDRESS + 1,
        KERNEL_ADDRESS + 2,
        STAGING_START as u64,
    ] {
        let mut fit = linux_fit();
        set_address(&mut fit, "kernel", "load", address);
        set_address(&mut fit, "kernel", "entry", address);
        assert_eq!(error(&fit), Some(Error::Address));
    }
    let mut fit = linux_fit();
    set_address(&mut fit, "kernel", "entry", KERNEL_ADDRESS + 2);
    assert_eq!(error(&fit), Some(Error::Address));
    for address in [0x0080_0000, 0x0200_0000] {
        let mut fit = linux_fit();
        set_address(&mut fit, "kernel", "load", address);
        set_address(&mut fit, "kernel", "entry", address);
        assert!(error(&fit).is_none());
    }
}
#[test]
fn sbi_entry_cannot_escape_loaded_firmware() {
    let fit = linux_fit();
    for entry in [1, 8] {
        let mut sbi = sbi_fit();
        set_address(&mut sbi, "sbi", "entry", entry);
        assert_eq!(
            inspect(&sbi, &fit, Some(PRODUCT)).err(),
            Some(Error::Address)
        );
    }
}
#[test]
fn overflowing_dtb_destination_is_rejected() {
    let mut fit = linux_fit();
    set_address(&mut fit, "fdt-pro", "load", u64::MAX - 7);
    assert_eq!(error(&fit), Some(Error::Bounds));
}
#[test]
fn truncated_fits_are_rejected_without_panicking() {
    let sbi = sbi_fit();
    let fit = linux_fit();
    for n in 0..sbi.len() {
        assert!(inspect(&sbi[..n], &fit, Some(PRODUCT)).is_err());
    }
    for n in 0..fit.len() {
        assert!(inspect(&sbi, &fit[..n], Some(PRODUCT)).is_err());
    }
}
#[test]
fn forged_fdt_sizes_and_offsets_are_rejected() {
    let original = linux_fit();
    for field in [4, 8, 12, 16, 32, 36] {
        let mut fit = original.clone();
        fit[field..field + 4].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(error(&fit).is_some());
    }
    let mut fit = original;
    let o = property_offset(&fit, &["images", "kernel"], "data");
    fit[o - 8..o - 4].copy_from_slice(&u32::MAX.to_be_bytes());
    assert_eq!(error(&fit), Some(Error::Bounds));
}
#[test]
fn fit_limits_are_checked_before_parsing() {
    assert_eq!(
        inspect(&vec![0; SBI_LIMIT + 1], &[], Some(PRODUCT)).err(),
        Some(Error::Bounds)
    );
    assert_eq!(
        validate_linux_fit(&vec![0; LINUX_FIT_LIMIT + 1], PRODUCT).err(),
        Some(Error::Bounds)
    );
}

const PARTITIONS: &str = "128K(bootinfo),256K(fsbl),64K(env),512K(opensbi),8M(linux),-(rootfs)";
const CAPACITY: u32 = 16 * 1024 * 1024;
fn environment(size: usize, parts: &str) -> Vec<u8> {
    let mut bytes = vec![0; size];
    let entry = std::format!("mtdparts=mtdparts=d420c000.spi-0:{parts}");
    bytes[4..4 + entry.len()].copy_from_slice(entry.as_bytes());
    let crc = layout::crc32(&bytes[4..]);
    bytes[..4].copy_from_slice(&crc.to_le_bytes());
    bytes
}
#[test]
fn crc32_standard_vector() {
    assert_eq!(layout::crc32(b"123456789"), 0xcbf4_3926);
}
#[test]
fn both_environment_sizes_require_linux_partition() {
    for size in [layout::ENV_SIZE, layout::ENV_MAX_SIZE] {
        let env = environment(size, PARTITIONS);
        let parsed = layout::Layout::from_env(&env, CAPACITY).unwrap();
        assert_eq!(parsed.env_size, size);
        assert_eq!(parsed.fsbl.offset, 0x20000);
        assert_eq!(parsed.sbi.offset, 0x70000);
        assert_eq!(parsed.kernel.offset, 0xf0000);
        assert_eq!(parsed.kernel.size, 0x800000);
        let mut full = env;
        full.resize(layout::ENV_MAX_SIZE, 0xff);
        assert_eq!(layout::Layout::from_env(&full, CAPACITY).unwrap(), parsed);
        let old = environment(size, &PARTITIONS.replace("(linux)", "(uboot)"));
        assert_eq!(
            layout::Layout::from_env(&old, CAPACITY),
            Err(layout::Error::Missing)
        );
    }
}
#[test]
fn environment_crc_and_partition_bounds_are_enforced() {
    for size in [layout::ENV_SIZE, layout::ENV_MAX_SIZE] {
        let mut env = environment(size, PARTITIONS);
        env[size - 1] ^= 1;
        assert_eq!(
            layout::Layout::from_env(&env, CAPACITY),
            Err(layout::Error::Integrity)
        );
    }
    for (parts, expected) in [
        (
            "128K(bootinfo),256K(fsbl),64K(env),512K(opensbi),2M@0xe0000(linux)",
            layout::Error::Overlap,
        ),
        (
            "128K(bootinfo),256K(fsbl),64K(env),512K(opensbi),4G(linux)",
            layout::Error::Bounds,
        ),
    ] {
        assert_eq!(
            layout::Layout::from_env(&environment(layout::ENV_SIZE, parts), CAPACITY),
            Err(expected)
        );
    }
}
