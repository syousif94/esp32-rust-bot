//! ST3215 serial bus servo driver (Feetech `SMS_STS` protocol).
//!
//! Half-duplex UART at 1 Mbps, 8N1. On the Waveshare General Driver for
//! Robots the bus is wired as RX = GPIO18, TX = GPIO19. Depending on the
//! transceiver path, transmitted bytes may echo on RX; receive parsing treats
//! echoes as optional and scans for a valid status packet.
//!
//! Only the STS series (LSB-first register encoding) is supported here; the
//! older SCS / SCSCL MSB-first variant is *not* handled.

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_sync::once_lock::OnceLock;
use embassy_time::{Duration, Instant, Timer};
use esp_hal::Blocking;
use esp_hal::uart::Uart;
use heapless::Vec;

// -- Protocol constants ----------------------------------------------------

pub const BROADCAST_ID: u8 = 0xFE;

// Instructions
const INST_PING: u8 = 0x01;
const INST_READ: u8 = 0x02;
const INST_WRITE: u8 = 0x03;
const INST_SYNC_WRITE: u8 = 0x83;

// SRAM / EEPROM register addresses (ST series)
const REG_ID: u8 = 5;
const REG_LOCK: u8 = 55;
const REG_TORQUE_ENABLE: u8 = 40;
const REG_ACC: u8 = 41; // 41..=47 covers acc, goal_pos, goal_time, goal_speed
const REG_PRESENT_POS: u8 = 56; // 56..=63 covers pos, speed, load, voltage, temp

/// Maximum number of servos we track on the bus. Plenty for the ~5-servo
/// current limit of the General Driver for Robots board.
pub const MAX_SERVOS: usize = 16;

pub type ServoList = Vec<u8, MAX_SERVOS>;
pub type SharedBus = Mutex<CriticalSectionRawMutex, St3215Bus<'static>>;
pub type SharedList = Mutex<CriticalSectionRawMutex, ServoList>;

/// Globals so HTTP / BLE / serial tasks can reach the bus and discovered
/// list without threading references through every spawn.
pub static SHARED_BUS: OnceLock<&'static SharedBus> = OnceLock::new();
pub static SHARED_LIST: OnceLock<&'static SharedList> = OnceLock::new();

// -- Errors ----------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
pub enum St3215Error {
    Timeout,
    BadHeader,
    BadChecksum,
    BadLength,
    UartTx,
    UartRx,
    /// Status packet's error byte was non-zero.
    Status(u8),
}

// -- Bulk-read state -------------------------------------------------------

#[derive(Debug, Clone, Copy, Default)]
pub struct ServoState {
    pub pos: i16,
    pub speed: i16,
    pub load: i16,
    pub voltage: u8, // deci-volts (0.1 V)
    pub temp: u8,    // degrees C
}

// -- Packet framing --------------------------------------------------------

fn checksum(id: u8, len: u8, instr: u8, params: &[u8]) -> u8 {
    let mut sum: u32 = id as u32 + len as u32 + instr as u32;
    for &p in params {
        sum = sum.wrapping_add(p as u32);
    }
    !(sum as u8)
}

/// Build a packet into `buf` and return its length.
/// Layout: `FF FF id len instr params… checksum`. `len = params.len() + 2`.
fn build_packet(buf: &mut [u8], id: u8, instr: u8, params: &[u8]) -> usize {
    let len = (params.len() as u8) + 2;
    buf[0] = 0xFF;
    buf[1] = 0xFF;
    buf[2] = id;
    buf[3] = len;
    buf[4] = instr;
    buf[5..5 + params.len()].copy_from_slice(params);
    buf[5 + params.len()] = checksum(id, len, instr, params);
    6 + params.len()
}

/// Feetech "TO_HOST" conversion: bit 15 acts as a sign bit for some
/// 12-bit registers (position, speed).
fn to_signed_15(raw: u16) -> i16 {
    if raw & 0x8000 != 0 {
        -((raw & 0x7FFF) as i16)
    } else {
        raw as i16
    }
}

/// Load is encoded with bit 10 as the sign (low 10 bits = ‰).
fn to_signed_10(raw: u16) -> i16 {
    let mag = (raw & 0x03FF) as i16;
    if raw & 0x0400 != 0 { -mag } else { mag }
}

// -- Bus driver ------------------------------------------------------------

pub struct St3215Bus<'d> {
    uart: Uart<'d, Blocking>,
}

impl<'d> St3215Bus<'d> {
    pub fn new(uart: Uart<'d, Blocking>) -> Self {
        Self { uart }
    }

    /// Drain any stale bytes sitting in the RX FIFO.
    fn drain_rx(&mut self) {
        let mut scratch = [0u8; 32];
        while self.uart.read_ready() {
            match self.uart.read_buffered(&mut scratch) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
        }
    }

    /// Read one byte with an absolute deadline.
    async fn read_byte_until(&mut self, deadline: Instant) -> Result<u8, St3215Error> {
        let mut byte = [0u8; 1];
        loop {
            if self.uart.read_ready() {
                match self.uart.read_buffered(&mut byte) {
                    Ok(1) => return Ok(byte[0]),
                    Ok(_) => Timer::after(Duration::from_micros(100)).await,
                    Err(_) => return Err(St3215Error::UartRx),
                }
            } else if Instant::now() >= deadline {
                return Err(St3215Error::Timeout);
            } else {
                Timer::after(Duration::from_micros(150)).await;
            }
        }
    }

    /// Read exactly `n` bytes into `buf[..n]` with an absolute deadline.
    async fn read_exact_until(
        &mut self,
        buf: &mut [u8],
        n: usize,
        deadline: Instant,
    ) -> Result<(), St3215Error> {
        let mut filled = 0;
        while filled < n {
            if self.uart.read_ready() {
                match self.uart.read_buffered(&mut buf[filled..n]) {
                    Ok(0) => Timer::after(Duration::from_micros(100)).await,
                    Ok(got) => filled += got,
                    Err(_) => return Err(St3215Error::UartRx),
                }
            } else if Instant::now() >= deadline {
                return Err(St3215Error::Timeout);
            } else {
                Timer::after(Duration::from_micros(150)).await;
            }
        }
        Ok(())
    }

    /// Read the next well-formed status packet, skipping optional command echo.
    async fn read_status_packet(
        &mut self,
        sent_id: u8,
        sent_len: u8,
        sent_instr: u8,
        expected_params: usize,
        timeout: Duration,
        out_params: &mut [u8],
    ) -> Result<usize, St3215Error> {
        let deadline = Instant::now() + timeout;

        loop {
            // Match SCServo::checkHead(): scan the stream for FF FF instead
            // of assuming echo/no-echo behavior.
            let mut prev = 0u8;
            loop {
                let byte = self.read_byte_until(deadline).await?;
                if prev == 0xFF && byte == 0xFF {
                    break;
                }
                prev = byte;
            }

            let mut head = [0u8; 3]; // id, len, error-or-instruction
            self.read_exact_until(&mut head, 3, deadline).await?;
            let packet_id = head[0];
            let packet_len = head[1];
            let code = head[2];
            if packet_len < 2 {
                return Err(St3215Error::BadLength);
            }

            let body_len = (packet_len as usize) - 2;
            let mut body = [0u8; 256];
            self.read_exact_until(&mut body, body_len + 1, deadline)
                .await?;

            let mut sum: u32 = packet_id as u32 + packet_len as u32 + code as u32;
            for &byte in &body[..body_len] {
                sum = sum.wrapping_add(byte as u32);
            }
            if body[body_len] != !(sum as u8) {
                continue;
            }

            // If the bus echoes TX, the echoed command is itself a valid packet.
            // Ignore it and keep scanning for the servo's status packet.
            if packet_id == sent_id && packet_len == sent_len && code == sent_instr {
                continue;
            }

            if packet_id != sent_id && sent_id != BROADCAST_ID {
                continue;
            }
            if body_len != expected_params {
                return Err(St3215Error::BadLength);
            }
            if code != 0 {
                return Err(St3215Error::Status(code));
            }
            out_params[..expected_params].copy_from_slice(&body[..expected_params]);
            return Ok(expected_params);
        }
    }

    /// Write a packet and (optionally) read back a status response.
    /// `response_params` is the number of parameter bytes expected in the
    /// status packet (status length = response_params + 2).
    async fn transact(
        &mut self,
        id: u8,
        instr: u8,
        params: &[u8],
        response_params: Option<usize>,
        timeout: Duration,
        out_params: &mut [u8],
    ) -> Result<usize, St3215Error> {
        let mut tx = [0u8; 256];
        let tx_len = build_packet(&mut tx, id, instr, params);

        // Drain stale bytes, send the packet, then scan for a status packet.
        // Some adapters echo TX and some don't; `read_status_packet` handles
        // both by ignoring a valid echoed command packet if it appears.
        self.drain_rx();
        self.uart
            .write(&tx[..tx_len])
            .map_err(|_| St3215Error::UartTx)?;
        let _ = self.uart.flush();

        let Some(n_params) = response_params else {
            self.drain_rx();
            return Ok(0);
        };

        self.read_status_packet(id, tx[3], instr, n_params, timeout, out_params)
            .await
    }

    /// Ping a single servo. Short timeout so scans stay fast.
    pub async fn ping(&mut self, id: u8) -> Result<(), St3215Error> {
        let mut scratch = [0u8; 8];
        self.transact(
            id,
            INST_PING,
            &[],
            Some(0),
            Duration::from_millis(50),
            &mut scratch,
        )
        .await?;
        Ok(())
    }

    /// Scan a range of IDs and collect responders into `out` (cleared first).
    pub async fn scan(&mut self, from: u8, to: u8, out: &mut ServoList) {
        out.clear();
        for id in from..=to {
            if id == BROADCAST_ID {
                continue;
            }
            if self.ping(id).await.is_ok() {
                let _ = out.push(id);
                if out.is_full() {
                    return;
                }
            }
        }
    }

    /// Atomically write [acc, pos_l, pos_h, time_l, time_h, speed_l, speed_h]
    /// to registers 41..=47. Move starts immediately.
    pub async fn write_pos(
        &mut self,
        id: u8,
        pos: u16,
        speed: u16,
        acc: u8,
    ) -> Result<(), St3215Error> {
        let params: [u8; 8] = [
            REG_ACC,
            acc,
            (pos & 0xFF) as u8,
            (pos >> 8) as u8,
            0, // goal time L
            0, // goal time H
            (speed & 0xFF) as u8,
            (speed >> 8) as u8,
        ];
        let mut scratch = [0u8; 8];
        self.transact(
            id,
            INST_WRITE,
            &params,
            Some(0),
            Duration::from_millis(15),
            &mut scratch,
        )
        .await?;
        Ok(())
    }

    /// SYNC_WRITE the same (pos, speed, acc) layout to many servos in one
    /// broadcast packet. Order-agnostic. No status response.
    pub async fn sync_write_pos(
        &mut self,
        moves: &[(u8, u16, u16, u8)],
    ) -> Result<(), St3215Error> {
        if moves.is_empty() {
            return Ok(());
        }
        const PER_SERVO: usize = 1 /*id*/ + 7 /*acc+pos+time+speed*/;
        let mut params: Vec<u8, { 2 + MAX_SERVOS * PER_SERVO }> = Vec::new();
        let _ = params.push(REG_ACC); // start address
        let _ = params.push(7); // bytes per servo
        for &(id, pos, speed, acc) in moves.iter().take(MAX_SERVOS) {
            let _ = params.push(id);
            let _ = params.push(acc);
            let _ = params.push((pos & 0xFF) as u8);
            let _ = params.push((pos >> 8) as u8);
            let _ = params.push(0);
            let _ = params.push(0);
            let _ = params.push((speed & 0xFF) as u8);
            let _ = params.push((speed >> 8) as u8);
        }
        let mut scratch = [0u8; 8];
        // SYNC_WRITE addressed to BROADCAST_ID; no status response from any servo.
        self.transact(
            BROADCAST_ID,
            INST_SYNC_WRITE,
            &params,
            None,
            Duration::from_millis(0),
            &mut scratch,
        )
        .await
        .map(|_| ())
    }

    pub async fn set_torque(&mut self, id: u8, on: bool) -> Result<(), St3215Error> {
        let params = [REG_TORQUE_ENABLE, on as u8];
        let mut scratch = [0u8; 8];
        self.transact(
            id,
            INST_WRITE,
            &params,
            Some(0),
            Duration::from_millis(15),
            &mut scratch,
        )
        .await?;
        Ok(())
    }

    /// Change a servo's ID. Unlocks EEPROM (reg 55 = 0), writes new ID
    /// (reg 5), re-locks EEPROM (reg 55 = 1).
    pub async fn write_id(&mut self, current: u8, new: u8) -> Result<(), St3215Error> {
        let mut scratch = [0u8; 8];
        // Unlock EEPROM
        self.transact(
            current,
            INST_WRITE,
            &[REG_LOCK, 0],
            Some(0),
            Duration::from_millis(15),
            &mut scratch,
        )
        .await?;
        // Write new ID — response (if any) comes back on the new ID, so don't expect it.
        self.transact(
            current,
            INST_WRITE,
            &[REG_ID, new],
            None,
            Duration::from_millis(15),
            &mut scratch,
        )
        .await?;
        Timer::after(Duration::from_millis(20)).await;
        // Re-lock using the new ID
        self.transact(
            new,
            INST_WRITE,
            &[REG_LOCK, 1],
            Some(0),
            Duration::from_millis(15),
            &mut scratch,
        )
        .await?;
        Ok(())
    }

    /// Bulk read of [pos, speed, load, voltage, temp] in one READ.
    pub async fn read_state(&mut self, id: u8) -> Result<ServoState, St3215Error> {
        let params = [REG_PRESENT_POS, 8];
        let mut out = [0u8; 16];
        let n = self
            .transact(
                id,
                INST_READ,
                &params,
                Some(8),
                Duration::from_millis(15),
                &mut out,
            )
            .await?;
        if n < 8 {
            return Err(St3215Error::BadLength);
        }
        let pos = u16::from_le_bytes([out[0], out[1]]);
        let speed = u16::from_le_bytes([out[2], out[3]]);
        let load = u16::from_le_bytes([out[4], out[5]]);
        Ok(ServoState {
            pos: to_signed_15(pos),
            speed: to_signed_15(speed),
            load: to_signed_10(load),
            voltage: out[6],
            temp: out[7],
        })
    }
}
