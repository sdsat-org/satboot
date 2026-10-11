#![no_std]
#![no_main]

use satboot::{Board, eeprom, entry, eprintln, println};

static EEPROM_BYTES: spin::Mutex<[u8; 256]> = spin::Mutex::new([0; 256]);

#[entry]
fn main(mut b: Board) {
    println!("satboot {} - {}", env!("CARGO_PKG_VERSION"), b.spec.name);
    let mut bytes = EEPROM_BYTES.lock();
    if let Err(error) = b.eeprom.read_data(0, &mut *bytes) {
        eprintln!("EEPROM read failed: {:?}", error);
        return;
    }
    let mut info = eeprom::EepromInfo::default();
    if let Err(error) = eeprom::parse_into(&*bytes, &mut info) {
        eprintln!("EEPROM parse failed: {:?}", error);
        return;
    }
    let product = match b.spec.validate_identity(info.product_name) {
        Ok(product) => product,
        Err(error) => {
            eprintln!("Board identity mismatch: {:?}", error);
            return;
        }
    };

    println!("EEPROM:");
    satboot::io::print_eeprom(&info);

    println!("DDR: training board memory...");
    // SAFETY: Cold K1/M1 boot; no payload, DMA or secondary hart uses DRAM.
    let firmware_status = match unsafe { b.init_ddr(info.ddr.as_ref()) } {
        Ok(status) => status,
        Err(error) => {
            eprintln!("DDR initialization failed: {:?}", error);
            return;
        }
    };
    println!("DDR firmware returned: {}", firmware_status);
    println!("NOR: loading SBI firmware, next stage and DTB...");
    // SAFETY: Training succeeded; DRAM is unused and all other harts and DMA remain stopped.
    match unsafe { b.load_images(Some(product)) } {
        Ok(images) => {
            println!(
                "Loaded: SBI {} bytes, next stage {} bytes, DTB {} bytes",
                images.sbi.size, images.payload.size, images.dtb.size
            );
            // SAFETY: Verified images, sole boot hart, completed PIO and no further Rust execution.
            unsafe { satboot::handoff::enter(images) };
        }
        Err(error) => eprintln!("NOR image loading failed: {:?}", error),
    }
}
