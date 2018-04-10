use core;
use stm32f407;


const FLASH_SECTOR_ADDRESSES: [u32; 12] =
    [0x0800_0000, 0x0800_4000, 0x0800_8000, 0x0800_C000,
     0x0801_0000, 0x0802_0000, 0x0804_0000, 0x0806_0000,
     0x0808_0000, 0x080A_0000, 0x080C_0000, 0x080E_0000];
const FLASH_END: u32 = 0x080F_FFFF;

const FLASH_CONFIG: u32 = FLASH_SECTOR_ADDRESSES[3];
const FLASH_USER: u32   = FLASH_SECTOR_ADDRESSES[4];

const CONFIG_MAGIC: u32 = 0x67797870;


static mut FLASH: Option<stm32f407::FLASH> = None;

/// Call to move the flash peripheral into this module
pub fn init(flash: stm32f407::FLASH) {
    unsafe { FLASH = Some(flash) };
}


/// User configuration. Must live in flash at FLASH_CONFIG, 0x0800_C000.
/// `magic` must be set to 0x67797870. `checksum` must be the CRC32 of the preceeding bytes.
#[derive(Copy,Clone)]
#[repr(C,packed)]
pub struct UserConfig {
    magic: u32,
    pub mac_address: [u8; 6],
    pub ip_address: [u8; 4],
    pub ip_gateway: [u8; 4],
    pub ip_prefix: u8,
    _padding: [u8; 1],
    checksum: u32,
}

static DEFAULT_CONFIG: UserConfig = UserConfig {
    // Locally administered MAC
    magic: 0,
    mac_address: [0x02, 0x00, 0x01, 0x02, 0x03, 0x04],
    ip_address: [10, 1, 1, 10],
    ip_gateway: [10, 1, 1, 1],
    ip_prefix: 24,
    _padding: [0u8; 1],
    checksum: 0,
};

impl UserConfig {
    /// Attempt to read the UserConfig from flash sector 3 at 0x0800_C000.
    /// If a valid config cannot be read, the default one is returned instead.
    pub fn get(crc: &mut stm32f407::CRC) -> UserConfig {
        // Read config from flash
        let adr = FLASH_CONFIG as *const u32;
        let cfg = unsafe { *(FLASH_CONFIG as *const UserConfig) };

        // First check magic is correct
        if cfg.magic != CONFIG_MAGIC {
            return DEFAULT_CONFIG.clone();
        }

        // Validate checksum
        let len = core::mem::size_of::<UserConfig>() / 4;
        crc.cr.write(|w| w.reset().reset());
        for idx in 0..(len - 1) {
            let val = unsafe { *(adr.offset(idx as isize)) };
            crc.dr.write(|w| w.dr().bits(val));
        }
        let crc_computed = crc.dr.read().dr().bits();

        if crc_computed == cfg.checksum {
            cfg.clone()
        } else {
            DEFAULT_CONFIG.clone()
        }
    }
}

/// Try to determine if there is valid code in the user flash at 0x0801_0000.
/// Returns Some(u32) with the address to jump to if so, and None if not.
pub fn valid_user_code() -> Option<u32> {
    let reset_vector: u32 = unsafe { *((FLASH_USER + 4) as *const u32) };
    if reset_vector >= FLASH_USER && reset_vector <= FLASH_END {
        Some(FLASH_USER)
    } else {
        None
    }
}

/// Check if address+length is valid for read/write flash.
fn address_valid(address: u32, length: usize) -> bool {
    length % 4 == 0 &&
    address >= FLASH_CONFIG &&
    address <= (FLASH_END - length as u32 + 1)
}

/// Erase flash sectors that cover the given address and length.
pub fn erase(address: u32, length: usize) -> Option<()> {
    let address_start = address;
    if unsafe { FLASH.is_some() } && address_valid(address, length) {
        let address_end = address + length as u32;
        for (idx, sector_start) in FLASH_SECTOR_ADDRESSES.iter().enumerate() {
            let sector_start = *sector_start;
            let sector_end = match FLASH_SECTOR_ADDRESSES.get(idx + 1) {
                Some(adr) => *adr - 1,
                None => FLASH_END,
            };
            if (address_start >= sector_start && address_start <= sector_end) ||
               (address_end   >= sector_start && address_end   <= sector_end) ||
               (address_start <= sector_start && address_end   >= sector_end) {
                   erase_sector(idx as u8);
            }
        }
        Some(())
    } else {
        None
    }
}

/// Erase specified sector
fn erase_sector(sector: u8) -> Option<()> {
    if unsafe { FLASH.is_some() } && (sector as usize) < FLASH_SECTOR_ADDRESSES.len() {
        let flash = unsafe { FLASH.as_mut().unwrap() };

        // Wait for ongoing operations
        while flash.sr.read().bsy().bit_is_set() {}

        // Unlock
        flash.keyr.write(|w| w.key().bits(0x45670123));
        flash.keyr.write(|w| w.key().bits(0xCDEF89AB));

        // Erase.
        // UNSAFE: We've verified that `sector`<FLASH_SECTOR_ADDRESSES.len(), which is is
        // the number of sectors.
        unsafe {
            flash.cr.write(|w| w.lock().unlocked()
                                .ser().sector_erase()
                                .snb().bits(sector));
            flash.cr.modify(|_, w| w.strt().start());
        }

        // Wait
        while flash.sr.read().bsy().bit_is_set() {}

        // Lock flash
        flash.cr.write(|w| w.lock().locked());

        Some(())
    } else {
        None
    }
}

/// Read from flash.
/// Returns a &[u8] if the address and length are valid.
/// length must be a multiple of 4.
pub fn read(address: u32, length: usize) -> Option<&'static [u8]> {
    // Don't permit reading before FLASH_CONFIG or after FLASH_END.
    if address_valid(address, length) {
        unsafe { Some(core::slice::from_raw_parts::<'static, u8>(address as *const _, length)) }
    } else {
        None
    }
}

/// Write to flash.
/// Returns () on success, None on failure.
/// length must be a multiple of 4.
pub fn write(address: u32, length: usize, data: &[u8]) -> Option<()> {
    if unsafe { FLASH.is_some() } && address_valid(address, length) {
        let flash = unsafe { FLASH.as_mut().unwrap() };

        // Wait for ongoing operations
        while flash.sr.read().bsy().bit_is_set() {}

        // Unlock
        flash.keyr.write(|w| w.key().bits(0x45670123));
        flash.keyr.write(|w| w.key().bits(0xCDEF89AB));

        if flash.cr.read().lock().is_locked() {
            return None;
        }

        // Write in 32 bit chunks, start programming.
        // Note reset value has 1 for lock so we need to explicitly leave it blank.
        flash.cr.write(|w| w.lock().unlocked()
                            .psize().psize32()
                            .pg().program());

        for idx in 0..(length / 4) {
            let offset = idx * 4;
            let word: u32 =
                  (data[offset]   as u32)
                | (data[offset+1] as u32) << 8
                | (data[offset+2] as u32) << 16
                | (data[offset+3] as u32) << 24;
            unsafe { core::ptr::write_volatile((address + offset as u32) as *mut u32, word) };

            // Wait for write
            while flash.sr.read().bsy().bit_is_set() {}

            // Check for errors
            let sr = flash.sr.read();
            if sr.pgserr().bit_is_set() || sr.pgperr().bit_is_set() ||
               sr.pgaerr().bit_is_set() || sr.wrperr().bit_is_set() {
                // Lock flash
                flash.cr.write(|w| w.lock().locked());
                return None;
            }
        }

        // Lock flash
        flash.cr.write(|w| w.lock().locked());

        Some(())
    } else {
        None
    }
}