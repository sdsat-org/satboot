//! Compile-time board selection and pure EEPROM compatibility checks.

use crate::eeprom;

/// DDR configuration qualified for one board family.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DdrSpec {
    /// Memory technology accepted from EEPROM.
    pub memory_type: &'static str,
    /// Populated chip selects.
    pub chip_selects: u32,
    /// Transfer rate in MT/s.
    pub data_rate: u32,
    /// Transmit termination resistance in ohms.
    pub tx_odt: u8,
}

/// Hardware identity and boot parameters selected when building the image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BoardSpec {
    /// Stable command-line and artifact identifier.
    pub id: &'static str,
    /// Human-readable board name.
    pub name: &'static str,
    /// Exact vendor FIT configuration product name.
    pub product_name: &'static str,
    /// Qualified DDR training parameters.
    pub ddr: DdrSpec,
}

// Vendor arch/riscv/dts/k1-x_spl.dts; these are board settings, not EEPROM defaults.
const LPDDR4X: DdrSpec = DdrSpec {
    memory_type: "LPDDR4X",
    chip_selects: 2,
    data_rate: 2400,
    tx_odt: 80,
};

/// Banana Pi BPI-F3 with SpacemiT K1.
pub const BPI_F3: BoardSpec = BoardSpec {
    id: "bpi-f3",
    name: "Banana Pi BPI-F3 (K1)",
    product_name: "k1-x_deb1",
    ddr: LPDDR4X,
};

/// MUSE Card with SpacemiT M1.
pub const MUSE_CARD: BoardSpec = BoardSpec {
    id: "muse-card",
    name: "MUSE Card (M1)",
    product_name: "k1-x_MUSE-Card",
    ddr: LPDDR4X,
};

/// MUSE Pi Pro with SpacemiT M1.
pub const MUSE_PI_PRO: BoardSpec = BoardSpec {
    id: "muse-pi-pro",
    name: "MUSE Pi Pro (M1)",
    product_name: "k1-x_MUSE-Pi-Pro",
    ddr: LPDDR4X,
};

/// Invalid compile-time board selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionError {
    /// No board feature was enabled.
    Missing,
    /// More than one board feature was enabled.
    Multiple,
}

/// Resolves exactly one board choice without accessing hardware.
pub const fn select(
    bpi_f3: bool,
    muse_card: bool,
    muse_pi_pro: bool,
) -> Result<&'static BoardSpec, SelectionError> {
    match bpi_f3 as u8 + muse_card as u8 + muse_pi_pro as u8 {
        0 => Err(SelectionError::Missing),
        1 => Ok(if bpi_f3 {
            &BPI_F3
        } else if muse_card {
            &MUSE_CARD
        } else {
            &MUSE_PI_PRO
        }),
        _ => Err(SelectionError::Multiple),
    }
}

const SELECTION: Result<&BoardSpec, SelectionError> = select(
    cfg!(feature = "board-bpi-f3"),
    cfg!(feature = "board-muse-card"),
    cfg!(feature = "board-muse-pi-pro"),
);

#[cfg(all(target_arch = "riscv64", target_os = "none"))]
const _: () = assert!(
    SELECTION.is_ok(),
    "select exactly one board feature: board-bpi-f3, board-muse-card or board-muse-pi-pro"
);

/// Board selected for this firmware; host tools can default to MUSE Pi Pro.
pub const SELECTED: &BoardSpec = match SELECTION {
    Ok(board) => board,
    // Target builds reject this path above. Host tools can inspect every board
    // with no hardware features or Cargo's additive --all-features selection.
    Err(_) => &MUSE_PI_PRO,
};

/// EEPROM identity does not establish compatibility with the selected board.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdentityError {
    /// The board requires a product name that the EEPROM did not provide.
    MissingProduct,
    /// The EEPROM describes a different product.
    ProductMismatch,
}

/// A recorded DDR field conflicts with this board's qualified configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DdrMismatch {
    /// Different memory technology.
    MemoryType,
    /// Different populated chip-select count.
    ChipSelects,
    /// Different transfer rate.
    DataRate,
    /// Different transmit termination.
    TxOdt,
}

impl BoardSpec {
    /// Checks an EEPROM product and returns the selected board's exact FIT product.
    ///
    /// BPI-F3 EEPROMs can omit the product name. Only that board may use its
    /// explicit configured product in the absence of a recorded identity.
    /// This compatibility check is not cryptographic device authentication.
    pub fn validate_identity(
        &self,
        recorded_product: Option<&str>,
    ) -> Result<&'static str, IdentityError> {
        match recorded_product {
            Some(product) if product == self.product_name => Ok(self.product_name),
            Some(_) => Err(IdentityError::ProductMismatch),
            None if *self == BPI_F3 => Ok(self.product_name),
            None => Err(IdentityError::MissingProduct),
        }
    }

    /// Validates recorded fields without filling absent EEPROM fields or touching DRAM.
    pub fn validate_ddr(
        &self,
        recorded: Option<&eeprom::DdrConfig<'_>>,
    ) -> Result<(), DdrMismatch> {
        let Some(info) = recorded else {
            return Ok(());
        };
        if info
            .memory_type
            .is_some_and(|value| !value.eq_ignore_ascii_case(self.ddr.memory_type))
        {
            return Err(DdrMismatch::MemoryType);
        }
        if info
            .chip_selects
            .is_some_and(|value| u32::from(value) != self.ddr.chip_selects)
        {
            return Err(DdrMismatch::ChipSelects);
        }
        if info
            .data_rate
            .is_some_and(|value| u32::from(value) != self.ddr.data_rate)
        {
            return Err(DdrMismatch::DataRate);
        }
        if info.tx_odt.is_some_and(|value| value != self.ddr.tx_odt) {
            return Err(DdrMismatch::TxOdt);
        }
        Ok(())
    }
}
