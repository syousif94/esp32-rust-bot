//! WiFi configuration storage in flash memory.
//!
//! Stores WiFi SSID and password in a dedicated flash region so credentials
//! can be configured via BLE and persist across reboots.

use esp_storage::FlashStorage;
use embedded_storage::nor_flash::{ReadNorFlash, NorFlash};
use esp_println::println;

/// Magic bytes to identify valid stored credentials
const MAGIC: [u8; 4] = [0xCA, 0xFE, 0xBE, 0xEF];

/// Flash offset for WiFi config (use a safe area in NVS partition region)
/// ESP32 flash layout: 0x9000 is typically NVS start, we use an offset within
/// the application data area. Using 0x3F0000 which is in the upper flash region.
const FLASH_OFFSET: u32 = 0x3F0000;

/// Maximum SSID length (WiFi spec allows 32 bytes)
pub const MAX_SSID_LEN: usize = 32;

/// Maximum password length (WPA2 allows 63 characters)
pub const MAX_PASSWORD_LEN: usize = 64;

/// Total storage size: 4 (magic) + 1 (ssid_len) + 32 (ssid) + 1 (pass_len) + 64 (pass) = 102 bytes
/// Round up to 4096 (one flash sector) for alignment - flash must be erased in sectors
const STORAGE_SIZE: usize = 4096;

/// WiFi credentials structure
#[derive(Debug, Clone)]
pub struct WifiCredentials {
    pub ssid: heapless::String<MAX_SSID_LEN>,
    pub password: heapless::String<MAX_PASSWORD_LEN>,
}

impl WifiCredentials {
    pub fn new(ssid: &str, password: &str) -> Option<Self> {
        let ssid_str = heapless::String::try_from(ssid).ok()?;
        let password_str = heapless::String::try_from(password).ok()?;
        Some(Self {
            ssid: ssid_str,
            password: password_str,
        })
    }
}

/// Read WiFi credentials from flash storage
pub fn read_wifi_credentials(flash: &mut FlashStorage<'_>) -> Option<WifiCredentials> {
    let mut buffer = [0u8; 128]; // Only read what we need
    
    if let Err(e) = flash.read(FLASH_OFFSET, &mut buffer) {
        println!("[WiFi Config] Flash read error: {:?}", e);
        return None;
    }
    
    // Check magic bytes
    if buffer[0..4] != MAGIC {
        println!("[WiFi Config] No valid credentials found (magic mismatch)");
        return None;
    }
    
    // Read SSID
    let ssid_len = buffer[4] as usize;
    if ssid_len == 0 || ssid_len > MAX_SSID_LEN {
        println!("[WiFi Config] Invalid SSID length: {}", ssid_len);
        return None;
    }
    
    let ssid_bytes = &buffer[5..5 + ssid_len];
    let ssid = core::str::from_utf8(ssid_bytes).ok()?;
    
    // Read password
    let pass_offset = 5 + MAX_SSID_LEN;
    let pass_len = buffer[pass_offset] as usize;
    if pass_len > MAX_PASSWORD_LEN {
        println!("[WiFi Config] Invalid password length: {}", pass_len);
        return None;
    }
    
    let pass_bytes = &buffer[pass_offset + 1..pass_offset + 1 + pass_len];
    let password = core::str::from_utf8(pass_bytes).ok()?;
    
    println!("[WiFi Config] Loaded credentials for SSID: {}", ssid);
    WifiCredentials::new(ssid, password)
}

/// Write WiFi credentials to flash storage
pub fn write_wifi_credentials(flash: &mut FlashStorage<'_>, ssid: &str, password: &str) -> Result<(), &'static str> {
    if ssid.is_empty() || ssid.len() > MAX_SSID_LEN {
        return Err("SSID must be 1-32 bytes");
    }
    if password.len() > MAX_PASSWORD_LEN {
        return Err("Password must be 0-64 bytes");
    }
    
    let mut buffer = [0xFFu8; STORAGE_SIZE]; // 0xFF is erased flash state
    
    // Write magic
    buffer[0..4].copy_from_slice(&MAGIC);
    
    // Write SSID
    buffer[4] = ssid.len() as u8;
    buffer[5..5 + ssid.len()].copy_from_slice(ssid.as_bytes());
    
    // Write password
    let pass_offset = 5 + MAX_SSID_LEN;
    buffer[pass_offset] = password.len() as u8;
    buffer[pass_offset + 1..pass_offset + 1 + password.len()].copy_from_slice(password.as_bytes());
    
    // Erase the sector first (ESP32 requires 4KB sector erase)
    if let Err(e) = flash.erase(FLASH_OFFSET, FLASH_OFFSET + STORAGE_SIZE as u32) {
        println!("[WiFi Config] Flash erase error: {:?}", e);
        return Err("Flash erase failed");
    }
    
    // Write the data
    if let Err(e) = flash.write(FLASH_OFFSET, &buffer) {
        println!("[WiFi Config] Flash write error: {:?}", e);
        return Err("Flash write failed");
    }
    
    println!("[WiFi Config] Saved credentials for SSID: {}", ssid);
    Ok(())
}

/// Clear stored WiFi credentials
pub fn clear_wifi_credentials(flash: &mut FlashStorage<'_>) -> Result<(), &'static str> {
    // Just erase the sector
    if let Err(e) = flash.erase(FLASH_OFFSET, FLASH_OFFSET + STORAGE_SIZE as u32) {
        println!("[WiFi Config] Flash erase error: {:?}", e);
        return Err("Flash erase failed");
    }
    
    println!("[WiFi Config] Credentials cleared");
    Ok(())
}
