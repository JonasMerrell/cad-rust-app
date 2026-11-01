//! The RP2040, the Raspberry Pi Pico's microcontroller: two Cortex-M0+ cores with its memory map
//! and the peripherals firmware uses most (GPIO and pads, SIO with the inter-core FIFOs, spin
//! locks and divider, the timer, PWM, ADC, UARTs, clocks, resets and the watchdog), close enough
//! to run Pico SDK firmware unchanged. PIO, DMA, USB, SPI and I2C keep their registers but move
//! no data (SPI reads 0xff and I2C transfers get no acknowledge).
//!
//! Time is in picoseconds. Each core keeps its own clock and the one behind runs next, so the
//! peripherals see the time of the access that reaches them.

use crate::mcu_cm0::{Bus, Cpu, Halt};
use crate::mcu_rom;
use std::collections::{HashMap, VecDeque};

pub const NPIN: usize = 30;
const FLASH_SIZE: usize = 2 << 20;
const SRAM_END: u32 = 0x2004_2000;
const PS: f64 = 1e12;
const XOSC_HZ: f64 = 12e6;
const ROSC_HZ: f64 = 6.5e6;

// ---------------------------------------------------------------- firmware files

#[derive(Clone, Debug, Default)]
pub struct Firmware {
    /// The flash image from offset 0 (boot2 then the program).
    pub flash: Vec<u8>,
    /// Pieces loaded straight into RAM (RAM-only builds).
    pub ram: Vec<(u32, Vec<u8>)>,
    pub entry: Option<u32>,
    /// Functions (address, size, name), sorted, for messages.
    pub syms: Vec<(u32, u32, String)>,
}

impl Firmware {
    fn put(&mut self, addr: u32, data: &[u8]) -> Result<(), String> {
        if (0x1000_0000..0x1400_0000).contains(&addr) {
            let off = (addr & 0x00ff_ffff) as usize;
            if off + data.len() > FLASH_SIZE {
                return Err(format!("the firmware doesn't fit the Pico's 2 MB flash (data at {addr:#010x})"));
            }
            if self.flash.len() < off + data.len() {
                self.flash.resize(off + data.len(), 0xff);
            }
            self.flash[off..off + data.len()].copy_from_slice(data);
        } else if (0x2000_0000..SRAM_END).contains(&addr) {
            self.ram.push((addr, data.to_vec()));
        } else {
            return Err(format!("the firmware loads data at {addr:#010x}, outside the RP2040's flash and RAM"));
        }
        Ok(())
    }

    /// The function containing `a`, as `name+0x12`.
    pub fn symbol(&self, a: u32) -> Option<String> {
        let k = self.syms.partition_point(|s| s.0 <= a);
        let (s, n, name) = self.syms.get(k.checked_sub(1)?)?;
        (a < s + n.max(&1)).then(|| if a == *s { name.clone() } else { format!("{name}+{:#x}", a - s) })
    }

}

fn u16le(b: &[u8], o: usize) -> u32 {
    b.get(o..o + 2).map(|x| u16::from_le_bytes([x[0], x[1]]) as u32).unwrap_or(0)
}

fn u32le(b: &[u8], o: usize) -> u32 {
    b.get(o..o + 4).map(|x| u32::from_le_bytes([x[0], x[1], x[2], x[3]])).unwrap_or(0)
}

/// Firmware from a UF2, ELF, Intel HEX or raw binary (a flash image) file.
pub fn load_firmware(b: &[u8]) -> Result<Firmware, String> {
    let mut fw = Firmware::default();
    if b.len() >= 512 && u32le(b, 0) == 0x0a32_4655 && u32le(b, 4) == 0x9e5d_5157 {
        for blk in b.chunks(512) {
            if blk.len() < 512 || u32le(blk, 0) != 0x0a32_4655 || u32le(blk, 508) != 0x0ab1_6f30 {
                continue;
            }
            let (flags, addr, n) = (u32le(blk, 8), u32le(blk, 12), u32le(blk, 16) as usize);
            // not-main-flash blocks, and blocks for another chip family
            if flags & 1 != 0 || (flags & 0x2000 != 0 && u32le(blk, 28) != 0xe48b_ff56) || n > 476 {
                continue;
            }
            fw.put(addr, &blk[32..32 + n])?;
        }
    } else if b.starts_with(b"\x7fELF") {
        if b.len() < 52 || b[4] != 1 || b[5] != 1 || u16le(b, 18) != 40 {
            return Err("the ELF file is not 32-bit little-endian ARM".into());
        }
        let (phoff, phent, phnum) = (u32le(b, 28) as usize, u16le(b, 42) as usize, u16le(b, 44) as usize);
        for k in 0..phnum {
            let p = phoff + k * phent;
            let (ty, off, paddr, filesz) = (u32le(b, p), u32le(b, p + 4) as usize, u32le(b, p + 12), u32le(b, p + 16) as usize);
            if ty == 1 && filesz > 0 {
                let data = b.get(off..off + filesz).ok_or("the ELF file is truncated")?;
                fw.put(paddr, data)?;
            }
        }
        fw.entry = Some(u32le(b, 24));
        // function names
        let (shoff, shent, shnum) = (u32le(b, 32) as usize, u16le(b, 46) as usize, u16le(b, 48) as usize);
        for k in 0..shnum {
            let s = shoff + k * shent;
            if u32le(b, s + 4) != 2 {
                continue;
            }
            let (off, size, link) = (u32le(b, s + 16) as usize, u32le(b, s + 20) as usize, u32le(b, s + 24) as usize);
            let strs = shoff + link * shent;
            let str_off = u32le(b, strs + 16) as usize;
            for e in (off..off + size).step_by(16) {
                if b.get(e + 12).is_some_and(|i| i & 0xf == 2) {
                    let name_at = str_off + u32le(b, e) as usize;
                    let end = b[name_at.min(b.len())..].iter().position(|c| *c == 0).map(|p| name_at + p).unwrap_or(name_at);
                    let name = String::from_utf8_lossy(&b[name_at.min(end)..end]).to_string();
                    if !name.is_empty() {
                        fw.syms.push((u32le(b, e + 4) & !1, u32le(b, e + 8), name));
                    }
                }
            }
        }
        fw.syms.sort_by_key(|s| s.0);
    } else if b.first() == Some(&b':') {
        let text = std::str::from_utf8(b).map_err(|_| "the HEX file is not text")?;
        let mut base = 0u32;
        for line in text.lines().map(str::trim).filter(|l| l.starts_with(':')) {
            let bytes: Vec<u8> = (1..line.len()).step_by(2).filter_map(|i| line.get(i..i + 2).and_then(|h| u8::from_str_radix(h, 16).ok())).collect();
            if bytes.len() < 5 || bytes.len() < 5 + bytes[0] as usize {
                return Err("a line of the HEX file is malformed".into());
            }
            let n = bytes[0] as usize;
            let off = (bytes[1] as u32) << 8 | bytes[2] as u32;
            let data = &bytes[4..4 + n];
            match bytes[3] {
                0 => fw.put(base + off, data)?,
                1 => break,
                2 if n == 2 => base = ((data[0] as u32) << 8 | data[1] as u32) << 4,
                4 if n == 2 => base = ((data[0] as u32) << 8 | data[1] as u32) << 16,
                5 if n == 4 => fw.entry = Some(u32::from_be_bytes([data[0], data[1], data[2], data[3]])),
                _ => {}
            }
        }
    } else {
        fw.put(0x1000_0000, b)?;
    }
    if fw.flash.is_empty() && fw.ram.is_empty() {
        return Err("the file holds no RP2040 firmware".into());
    }
    Ok(fw)
}

// ---------------------------------------------------------------- pins

/// How the chip drives a pin.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Drive {
    pub oe: bool,
    pub out: bool,
    pub pull_up: bool,
    pub pull_down: bool,
    /// Drive strength (2, 4, 8 or 12 mA).
    pub ma: u8,
}

impl Drive {
    /// Conductances from the pin to the supply and to ground.
    pub fn conductances(&self) -> (f64, f64) {
        let pull = 1.0 / 50e3;
        let (mut hi, mut lo) = (if self.pull_up { pull } else { 0.0 }, if self.pull_down { pull } else { 0.0 });
        if self.oe {
            // about 0.3 V drop at the rated current
            let g = self.ma.max(2) as f64 * 1e-3 / 0.3;
            if self.out {
                hi += g;
            } else {
                lo += g;
            }
        }
        (hi, lo)
    }

    pub fn describe(&self) -> &'static str {
        match (self.oe, self.out, self.pull_up, self.pull_down) {
            (true, true, ..) => "high",
            (true, false, ..) => "low",
            (false, _, true, true) => "input (bus keep)",
            (false, _, true, false) => "input, pull-up",
            (false, _, false, true) => "input, pull-down",
            _ => "input",
        }
    }
}

// ---------------------------------------------------------------- peripherals

#[derive(Clone, Copy, Debug, Default)]
struct Div {
    dividend: u32,
    divisor: u32,
    signed: bool,
    quot: u32,
    rem: u32,
    dirty: bool,
}

impl Div {
    fn calc(&mut self) {
        let (n, d) = (self.dividend, self.divisor);
        (self.quot, self.rem) = if self.signed {
            let (n, d) = (n as i32, d as i32);
            if d == 0 { (if n < 0 { 1 } else { u32::MAX }, n as u32) } else { (n.wrapping_div(d) as u32, n.wrapping_rem(d) as u32) }
        } else if d == 0 {
            (u32::MAX, n)
        } else {
            (n / d, n % d)
        };
        self.dirty = true;
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct Slice {
    csr: u32,
    div: u32,
    cc: u32,
    top: u32,
    /// Where the counter stands in its period while it doesn't count by time (stopped, gated
    /// off by its B input, or counting B's edges).
    ctr: u32,
    /// Not counting by time: DIVMODE gated with B low, or counting B's edges.
    held: bool,
    /// B edges towards the next count, in sixteenths (the divider applies to the edges).
    edges: u32,
    /// While running: the time (ps) the counter was last at 0 in its first period.
    origin: f64,
    next_wrap: f64,
}

impl Slice {
    fn tick(&self, period: f64) -> f64 {
        let int = (self.div >> 4) & 0xff;
        let d = if int == 0 { 256.0 } else { int as f64 + (self.div & 15) as f64 / 16.0 };
        period * d
    }
    fn len(&self) -> u64 {
        (self.top as u64 + 1) * if self.csr & 2 != 0 { 2 } else { 1 }
    }
    /// The slice is enabled.
    fn en(&self) -> bool {
        self.csr & 1 != 0
    }
    /// Enabled and counting by time (from `origin`).
    fn on(&self) -> bool {
        self.en() && !self.held
    }
    /// DIVMODE: 0 free running, 1 gated by B, 2 B's rising edges, 3 B's falling edges.
    fn divmode(&self) -> u32 {
        (self.csr >> 4) & 3
    }
    /// The divider in sixteenths.
    fn div16(&self) -> u32 {
        let int = (self.div >> 4) & 0xff;
        if int == 0 { 256 * 16 } else { int * 16 + (self.div & 15) }
    }
    /// Start or stop counting by time, keeping its place.
    fn set_held(&mut self, held: bool, t: f64, period: f64) {
        if held == self.held {
            return;
        }
        if self.en() {
            if held {
                self.ctr = self.pos(t, period).0 as u32;
            } else {
                let c = self.ctr;
                self.held = false;
                self.set_origin(t, c, period);
            }
        }
        self.held = held;
    }
    /// One count by an edge of B; true when it wraps.
    fn count_edge(&mut self) -> bool {
        self.edges += 16;
        let d = self.div16();
        let mut wrapped = false;
        while self.edges >= d {
            self.edges -= d;
            let n = self.len();
            let p = self.ctr as u64;
            let next = if p + 1 >= n { 0 } else { p + 1 };
            self.ctr = next as u32;
            wrapped |= next == 0;
        }
        wrapped
    }
    /// Position in the period (ticks) at time t.
    fn pos(&self, t: f64, period: f64) -> (u64, f64) {
        let tick = self.tick(period);
        let k = ((t - self.origin) / tick + 1e-9).floor().max(0.0) as u64;
        (k % self.len(), self.origin + (k - k % self.len()) as f64 * tick)
    }
    fn counter(&self, t: f64, period: f64) -> u32 {
        let top = self.top as u64;
        let p = if self.on() { self.pos(t, period).0 } else { self.ctr as u64 };
        (if p <= top || p >= self.len() { p } else { 2 * top + 1 - p }) as u32
    }
    fn level(&self, ch: usize, t: f64, period: f64) -> bool {
        let cc = (self.cc >> (16 * ch)) & 0xffff;
        let inv = self.csr & (4 << ch) != 0;
        (self.counter(t, period) < cc) != inv
    }
    /// The next time either output changes.
    fn next_edge(&self, t: f64, period: f64) -> f64 {
        if !self.on() {
            return f64::INFINITY;
        }
        let (p, base) = self.pos(t, period);
        let n = self.len();
        let tick = self.tick(period);
        let top = self.top as u64;
        let mut best = f64::INFINITY;
        for ch in 0..2 {
            let cc = ((self.cc >> (16 * ch)) & 0xffff) as u64;
            if cc == 0 || cc > top {
                continue;
            }
            let marks: [u64; 2] = if self.csr & 2 != 0 { [cc, 2 * top + 2 - cc] } else { [0, cc] };
            for m in marks {
                let q = if m > p { m } else { m + n };
                best = best.min(base + q as f64 * tick);
            }
        }
        best
    }
    fn set_origin(&mut self, t: f64, ctr: u32, period: f64) {
        self.origin = t - ctr as f64 * self.tick(period);
        let (_, base) = self.pos(t, period);
        self.next_wrap = base + self.len() as f64 * self.tick(period);
    }
}

#[derive(Clone, Debug)]
struct Uart {
    cr: u32,
    lcr: u32,
    ibrd: u32,
    fbrd: u32,
    ifls: u32,
    imsc: u32,
    dmacr: u32,
    /// Characters being sent: (byte, start, end) in ps.
    tx: VecDeque<(u8, u64, u64)>,
    rx: VecDeque<u8>,
    bit_ps: u64,
    bits: u32,
}

impl Default for Uart {
    fn default() -> Self {
        Uart { cr: 0x300, lcr: 0, ibrd: 0, fbrd: 0, ifls: 0x12, imsc: 0, dmacr: 0, tx: VecDeque::new(), rx: VecDeque::new(), bit_ps: 8_680_555, bits: 10 }
    }
}

impl Uart {
    fn depth(&self) -> usize {
        if self.lcr & 0x10 != 0 { 32 } else { 1 }
    }
    fn prune(&mut self, now: u64) {
        while self.tx.front().is_some_and(|c| c.2 <= now) {
            self.tx.pop_front();
        }
    }
    /// Characters waiting behind the one on the wire.
    fn queued(&self, now: u64) -> usize {
        self.tx.iter().rev().take_while(|c| c.1 > now).count()
    }
    fn busy(&self, now: u64) -> bool {
        self.tx.back().is_some_and(|c| c.2 > now)
    }
    fn level(&self, k: u32) -> usize {
        [4, 8, 16, 24, 28].get(k as usize).copied().unwrap_or(16) * self.depth() / 32
    }
    fn ris(&self, now: u64) -> u32 {
        let mut r = 0;
        if self.queued(now) <= self.level(self.ifls & 7) {
            r |= 1 << 5;
        }
        if !self.rx.is_empty() {
            if self.rx.len() >= self.level((self.ifls >> 3) & 7).max(1) {
                r |= 1 << 4;
            }
            r |= 1 << 6;
        }
        r
    }
    fn frame(&self, byte: u8) -> Vec<bool> {
        let wlen = 5 + ((self.lcr >> 5) & 3);
        let mut v = vec![false];
        for k in 0..wlen {
            v.push(byte >> k & 1 != 0);
        }
        if self.lcr & 2 != 0 {
            let ones = (0..wlen).filter(|k| byte >> k & 1 != 0).count() as u32;
            let even = self.lcr & 4 != 0;
            let stick = self.lcr & 0x80 != 0;
            // even parity: the bit makes the count of ones even (it is their XOR)
            v.push(if stick { !even } else { (ones % 2 == 1) == even });
        }
        v.push(true);
        if self.lcr & 8 != 0 {
            v.push(true);
        }
        v
    }
    /// The TX line at `t`.
    fn line(&self, t: u64) -> bool {
        for c in &self.tx {
            if c.1 <= t && t < c.2 {
                let k = ((t - c.1) / self.bit_ps.max(1)) as usize;
                return self.frame(c.0).get(k).copied().unwrap_or(true);
            }
        }
        true
    }
    fn next_edge(&self, t: u64) -> u64 {
        // the line only changes at bit boundaries, and idles high like the stop bits
        let level = self.line(t);
        for c in &self.tx {
            if c.2 <= t {
                continue;
            }
            for (k, b) in self.frame(c.0).iter().enumerate() {
                let at = c.1 + k as u64 * self.bit_ps;
                if at > t && *b != level {
                    return at;
                }
            }
        }
        u64::MAX
    }
}

#[derive(Clone, Debug, Default)]
struct Adc {
    cs: u32,
    result: u32,
    fcs: u32,
    fifo: VecDeque<u32>,
    div: u32,
    inte: u32,
    intf: u32,
    ready_at: u64,
    next_sample: u64,
}

/// Core 1 waiting in the boot ROM for core 0 to launch it.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Core1 {
    Running,
    /// Held in reset by the power-on state machine.
    Off,
    /// In the ROM's launch handshake: the step and the values so far.
    Wait(u8, [u32; 3]),
}

/// Words of the per-run warnings, so each is said once.
const W_PIO: u32 = 1;
const W_DMA: u32 = 2;
const W_USB: u32 = 4;
const W_I2C: u32 = 8;
const W_SPI: u32 = 16;
const W_BUS: u32 = 32;
const W_INTERP: u32 = 64;

pub struct Chip {
    rom: Vec<u8>,
    pub flash: Vec<u8>,
    sram: Vec<u8>,
    usb_ram: Vec<u8>,
    xip_sram: Vec<u8>,
    /// Picoseconds per system clock cycle.
    pub period: u64,
    /// Time of the access in progress (ps).
    pub now: u64,
    core: usize,
    regs: HashMap<u32, u32>,
    resets: u32,
    psm_off: u32,
    // GPIO
    sio_out: u32,
    sio_oe: u32,
    hi_out: u32,
    hi_oe: u32,
    ctrl: [u32; NPIN],
    pads: [u32; NPIN],
    /// Levels from outside (the circuit) on the pins in `ext`.
    pub ext_level: u32,
    pub ext: u32,
    /// Voltages on GP26..29 (ADC 0..3) and the ADC reference.
    pub analog: [f64; 4],
    pub vref: f64,
    level: u32,
    edges: [u32; 4],
    inte: [[u32; 4]; 2],
    intf: [[u32; 4]; 2],
    // SIO
    fifo: [VecDeque<u32>; 2],
    fifo_err: [u32; 2],
    spin: u32,
    div: [Div; 2],
    interp: [[u32; 32]; 2],
    // timer
    t_off: i64,
    latch: [u32; 2],
    alarm: [u32; 4],
    alarm_at: [u64; 4],
    armed: u32,
    t_intr: u32,
    t_inte: u32,
    t_intf: u32,
    pending_low: u32,
    // PWM
    pwm: [Slice; 8],
    pwm_intr: u32,
    pwm_inte: u32,
    pwm_intf: u32,
    adc: Adc,
    uart: [Uart; 2],
    // watchdog
    wd_ctrl: u32,
    wd_load: u32,
    wd_load_at: u64,
    pub scratch: [u32; 8],
    pub wd_reason: u32,
    ssi_rx: VecDeque<u32>,
    spi_rx: [u32; 2],
    i2c_abort: [bool; 2],
    rng: u32,
    pub next_due: u64,
    /// A write may have changed what the pins do.
    pub dirty: bool,
    core1: Core1,
    /// Core 1 has been launched (the run loop restarts it).
    launch: Option<(u32, u32, u32)>,
    /// Watchdog or software reset asked for.
    pub reset_req: bool,
    pub notes: Vec<String>,
    warned: u32,
    /// What each UART sent: (time in ps, byte).
    pub uart_log: [Vec<(u64, u8)>; 2],
}

const RESET_ALL: u32 = 0x01ff_ffff;

impl Chip {
    fn new(fw: &Firmware) -> Chip {
        let mut flash = vec![0xffu8; FLASH_SIZE];
        flash[..fw.flash.len()].copy_from_slice(&fw.flash);
        let mut c = Chip {
            rom: mcu_rom::image(),
            flash,
            sram: vec![0; (SRAM_END - 0x2000_0000) as usize],
            usb_ram: vec![0; 4096],
            xip_sram: vec![0; 16 << 10],
            period: (PS / ROSC_HZ) as u64,
            now: 0,
            core: 0,
            regs: HashMap::new(),
            resets: RESET_ALL,
            psm_off: 0,
            sio_out: 0,
            sio_oe: 0,
            hi_out: 0,
            hi_oe: 0,
            ctrl: [0x1f; NPIN],
            pads: [0x56; NPIN],
            ext_level: 0,
            ext: 0,
            analog: [0.0, 0.0, 0.0, 4.7 / 3.0],
            vref: 3.3,
            level: 0,
            edges: [0; 4],
            inte: [[0; 4]; 2],
            intf: [[0; 4]; 2],
            fifo: [VecDeque::new(), VecDeque::new()],
            fifo_err: [0; 2],
            spin: 0,
            div: [Div::default(); 2],
            interp: [[0; 32]; 2],
            t_off: 0,
            latch: [0; 2],
            alarm: [0; 4],
            alarm_at: [u64::MAX; 4],
            armed: 0,
            t_intr: 0,
            t_inte: 0,
            t_intf: 0,
            pending_low: 0,
            pwm: [Slice { top: 0xffff, div: 16, ..Default::default() }; 8],
            pwm_intr: 0,
            pwm_inte: 0,
            pwm_intf: 0,
            adc: Adc::default(),
            uart: [Uart::default(), Uart::default()],
            wd_ctrl: 0x0700_0000,
            wd_load: 0,
            wd_load_at: 0,
            scratch: [0; 8],
            wd_reason: 0,
            ssi_rx: VecDeque::new(),
            spi_rx: [0; 2],
            i2c_abort: [false; 2],
            rng: 0x1234_5678,
            next_due: 0,
            dirty: true,
            core1: Core1::Wait(0, [0; 3]),
            launch: None,
            reset_req: false,
            notes: vec![],
            warned: 0,
            uart_log: [vec![], vec![]],
        };
        for (a, v) in [
            (0x4000_8030, 0),       // CLK_REF_CTRL: ROSC
            (0x4000_8034, 0x100),   // CLK_REF_DIV
            (0x4000_803c, 0),       // CLK_SYS_CTRL: clk_ref
            (0x4000_8040, 0x100),   // CLK_SYS_DIV
            (0x4000_8058, 0x100),   // CLK_USB_DIV
            (0x4000_8064, 0x100),   // CLK_ADC_DIV
            (0x4000_8070, 0x100),   // CLK_RTC_DIV
            (0x4002_8000, 1),       // PLL_SYS CS
            (0x4002_8004, 0x2d),    // PLL_SYS PWR
            (0x4002_800c, 0x77000), // PLL_SYS PRIM
            (0x4002_c000, 1),
            (0x4002_c004, 0x2d),
            (0x4002_c00c, 0x77000),
            (0x4006_4000, 0xb1), // VREG
            (0x4006_4004, 0x91), // BOD
            (0x4006_0000, 0xaa0),
            (0x4005_802c, 0x200), // watchdog TICK
            (0x4001_c000, 0),
        ] {
            c.regs.insert(a, v);
        }
        for (a, d) in &fw.ram {
            let o = (a - 0x2000_0000) as usize;
            let n = d.len().min(c.sram.len().saturating_sub(o));
            c.sram[o..o + n].copy_from_slice(&d[..n]);
        }
        c.clocks_changed();
        c
    }

    fn reg(&self, a: u32) -> u32 {
        self.regs.get(&a).copied().unwrap_or(0)
    }

    fn warn(&mut self, bit: u32, msg: &str) {
        if self.warned & bit == 0 {
            self.warned |= bit;
            self.notes.push(msg.into());
        }
    }

    fn now_us(&self) -> u64 {
        ((self.now / 1_000_000) as i64 + self.t_off).max(0) as u64
    }

    // ------------------------------------------------------------ clocks

    fn pll_hz(&self, base: u32) -> f64 {
        let (cs, pwr, fb, prim) = (self.reg(base), self.reg(base + 4), self.reg(base + 8) & 0xfff, self.reg(base + 12));
        if cs & 0x100 != 0 {
            return XOSC_HZ;
        }
        if pwr & 0x21 != 0 || fb == 0 {
            return 0.0;
        }
        let refdiv = (cs & 0x3f).max(1) as f64;
        let (p1, p2) = (((prim >> 16) & 7).max(1) as f64, ((prim >> 12) & 7).max(1) as f64);
        XOSC_HZ / refdiv * fb as f64 / (p1 * p2)
    }

    fn src_hz(&self, s: &str) -> f64 {
        match s {
            "pll_sys" => self.pll_hz(0x4002_8000),
            "pll_usb" => self.pll_hz(0x4002_c000),
            "rosc" => ROSC_HZ,
            "xosc" => XOSC_HZ,
            "ref" => self.clk_hz(0),
            "sys" => self.clk_hz(1),
            _ => 0.0,
        }
    }

    /// clk_ref, clk_sys, clk_peri, clk_usb, clk_adc or clk_rtc (0..5).
    fn clk_hz(&self, k: usize) -> f64 {
        let base = 0x4000_8030 + 12 * k as u32;
        let (ctrl, div) = (self.reg(base), self.reg(base + 4));
        let aux = (ctrl >> 5) & 7;
        let int_div = |bits: u32| {
            let i = (div >> 8) & ((1 << bits) - 1);
            if i == 0 { (1u64 << bits) as f64 } else { i as f64 }
        };
        let frac_div = || {
            let i = div >> 8;
            (if i == 0 { (1u64 << 24) as f64 } else { i as f64 }) + (div & 0xff) as f64 / 256.0
        };
        let lists: [&[&str]; 6] = [
            &["pll_usb", "", ""],
            &["pll_sys", "pll_usb", "rosc", "xosc", "", ""],
            &["sys", "pll_sys", "pll_usb", "rosc", "xosc", "", ""],
            &["pll_usb", "pll_sys", "rosc", "xosc", "", ""],
            &["pll_usb", "pll_sys", "rosc", "xosc", "", ""],
            &["pll_usb", "pll_sys", "rosc", "xosc", "", ""],
        ];
        let auxf = || lists[k].get(aux as usize).map(|s| self.src_hz(s)).unwrap_or(0.0);
        match k {
            0 => {
                let f = match ctrl & 3 {
                    0 => ROSC_HZ,
                    1 => auxf(),
                    _ => XOSC_HZ,
                };
                f / int_div(2)
            }
            1 => (if ctrl & 1 == 0 { self.clk_hz(0) } else { auxf() }) / frac_div(),
            2 => {
                if ctrl & 0x800 == 0 {
                    0.0
                } else {
                    auxf()
                }
            }
            3 | 4 => auxf() / int_div(2),
            _ => auxf() / frac_div(),
        }
    }


    fn clocks_changed(&mut self) {
        let f = self.clk_hz(1);
        let f = if f > 1e3 { f } else { ROSC_HZ };
        let old = self.period;
        self.period = (PS / f).round().max(1.0) as u64;
        if old != self.period {
            // PWM counters keep their place
            let now = self.now as f64;
            let po = old as f64;
            for s in &mut self.pwm {
                if s.on() {
                    let c = s.counter(now, po);
                    s.set_origin(now, c, self.period as f64);
                }
            }
            self.uart_baud(0);
            self.uart_baud(1);
            self.next_due = 0;
        }
    }

    fn uart_baud(&mut self, u: usize) {
        let peri = self.clk_hz(2);
        let d = self.uart[u].ibrd as f64 + self.uart[u].fbrd as f64 / 64.0;
        let baud = if d > 0.0 && peri > 0.0 { peri / (16.0 * d) } else { 115200.0 };
        let x = &mut self.uart[u];
        x.bit_ps = (PS / baud).max(1.0) as u64;
        x.bits = 1 + 5 + ((x.lcr >> 5) & 3) + ((x.lcr >> 1) & 1) + 1 + ((x.lcr >> 3) & 1);
    }

    // ------------------------------------------------------------ pins

    /// What the peripheral selected on pin n asks for: (out, oe).
    fn periph(&self, n: usize) -> (bool, bool) {
        let t = self.now;
        match self.ctrl[n] & 0x1f {
            5 => (self.sio_out >> n & 1 != 0, self.sio_oe >> n & 1 != 0),
            2 => {
                let u = ((n >> 2) & 1) ^ ((n >> 3) & 1);
                match n & 3 {
                    0 => (self.uart[u].line(t), self.uart[u].cr & 0x101 == 0x101),
                    3 => (false, false),
                    _ => (false, false),
                }
            }
            4 => {
                let s = &self.pwm[(n >> 1) & 7];
                if n & 1 == 1 && (s.csr >> 4) & 3 != 0 { (false, false) } else { (s.level(n & 1, t as f64, self.period as f64), true) }
            }
            // SPI: clock and data idle low, chip select high
            1 => match n & 3 {
                0 => (false, false),
                1 => (true, true),
                _ => (false, true),
            },
            8 => (false, true),
            _ => (false, false),
        }
    }

    /// The function selected on a pin, by name.
    pub fn funcsel(&self, n: usize) -> &'static str {
        match self.ctrl[n] & 0x1f {
            1 => "SPI",
            2 => "UART",
            3 => "I2C",
            4 => "PWM",
            5 => "SIO",
            6 | 7 => "PIO",
            8 => "clock out",
            9 => "USB",
            _ => "none",
        }
    }

    pub fn drive(&self, n: usize) -> Drive {
        let (mut out, mut oe) = self.periph(n);
        let c = self.ctrl[n];
        out = match (c >> 8) & 3 {
            0 => out,
            1 => !out,
            2 => false,
            _ => true,
        };
        oe = match (c >> 12) & 3 {
            0 => oe,
            1 => !oe,
            2 => false,
            _ => true,
        };
        let p = self.pads[n];
        if p & 0x80 != 0 {
            oe = false;
        }
        Drive { oe, out, pull_up: p & 8 != 0, pull_down: p & 4 != 0, ma: [2, 4, 8, 12][((p >> 4) & 3) as usize] }
    }

    pub fn drives(&self) -> [Drive; NPIN] {
        std::array::from_fn(|n| self.drive(n))
    }

    /// The next time a peripheral changes a pin by itself (PWM, UART).
    pub fn next_edge(&self) -> u64 {
        let mut best = u64::MAX;
        let t = self.now;
        let mut slices = 0u32;
        let mut uarts = 0u32;
        for n in 0..NPIN {
            match self.ctrl[n] & 0x1f {
                4 => slices |= 1 << ((n >> 1) & 7),
                2 if n & 3 == 0 => uarts |= 1 << (((n >> 2) & 1) ^ ((n >> 3) & 1)),
                _ => {}
            }
        }
        for k in 0..8 {
            if slices >> k & 1 != 0 {
                let e = self.pwm[k].next_edge(t as f64, self.period as f64);
                if e.is_finite() {
                    best = best.min(e.ceil().max(t as f64 + 1.0) as u64);
                }
            }
        }
        for u in 0..2 {
            if uarts >> u & 1 != 0 {
                best = best.min(self.uart[u].next_edge(t));
            }
        }
        best
    }

    /// Input levels: a driven pin reads what it drives, an outside one its outside level, a
    /// floating one its pull; edges latch for the GPIO interrupts.
    fn update_levels(&mut self) {
        let mut l = 0u32;
        for n in 0..NPIN {
            let d = self.drive(n);
            let v = if d.oe {
                d.out
            } else if self.ext >> n & 1 != 0 {
                self.ext_level >> n & 1 != 0
            } else if d.pull_up != d.pull_down {
                d.pull_up
            } else {
                self.level >> n & 1 != 0
            };
            if v {
                l |= 1 << n;
            }
        }
        let old = self.level;
        let ch = l ^ old;
        if ch != 0 {
            self.pwm_inputs(old, l);
            for n in 0..NPIN {
                if ch >> n & 1 != 0 {
                    let bit = if l >> n & 1 != 0 { 8 } else { 4 };
                    self.edges[n / 8] |= bit << (4 * (n % 8));
                }
            }
        }
        self.level = l;
    }

    /// The level of slice k's B input: any pin with the PWM function on that channel.
    fn pwm_b(&self, k: usize, level: u32) -> bool {
        (0..NPIN).any(|n| n & 1 == 1 && (n >> 1) & 7 == k && self.ctrl[n] & 0x1f == 4 && level >> n & 1 != 0)
    }

    /// Slices whose DIVMODE makes their B pin an input: gate them or count its edges.
    fn pwm_inputs(&mut self, old: u32, new: u32) {
        let (now, per) = (self.now as f64, self.period as f64);
        for k in 0..8 {
            if !self.pwm[k].en() || self.pwm[k].divmode() == 0 {
                continue;
            }
            let (was, b) = (self.pwm_b(k, old), self.pwm_b(k, new));
            if was == b {
                continue;
            }
            let s = &mut self.pwm[k];
            match s.divmode() {
                1 => s.set_held(!b, now, per),
                m => {
                    if (m == 2) == b && s.count_edge() {
                        self.pwm_intr |= 1 << k;
                    }
                }
            }
            self.dirty = true;
            self.next_due = 0;
        }
    }

    fn gpio_in(&self) -> u32 {
        let mut v = self.level;
        for n in 0..NPIN {
            if self.pads[n] & 0x40 == 0 {
                v &= !(1 << n);
            }
        }
        v
    }

    fn intr_raw(&self, j: usize) -> u32 {
        let mut r = self.edges[j];
        for i in 0..8 {
            let n = 8 * j + i;
            if n < NPIN {
                r |= if self.level >> n & 1 != 0 { 2 } else { 1 } << (4 * i);
            }
        }
        r
    }

    // ------------------------------------------------------------ interrupts and time

    /// Interrupt lines for a core.
    fn lines(&self, core: usize) -> u32 {
        let mut l = (self.t_intr | self.t_intf) & self.t_inte & 0xf;
        if (self.pwm_intr | self.pwm_intf) & self.pwm_inte != 0 {
            l |= 1 << 4;
        }
        if self.inte[core] != [0; 4] && (0..4).any(|j| (self.intr_raw(j) | self.intf[core][j]) & self.inte[core][j] != 0) {
            l |= 1 << 13;
        }
        if !self.fifo[core].is_empty() || self.fifo_err[core] != 0 {
            l |= 1 << (15 + core);
        }
        for u in 0..2 {
            if self.uart[u].imsc != 0 && self.uart[u].ris(self.now) & self.uart[u].imsc != 0 {
                l |= 1 << (20 + u);
            }
        }
        if self.adc.inte != 0 && (self.adc_intr() | self.adc.intf) & self.adc.inte != 0 {
            l |= 1 << 22;
        }
        l
    }

    fn adc_intr(&self) -> u32 {
        (self.adc.fifo.len() as u32 >= ((self.adc.fcs >> 24) & 15).max(1)) as u32
    }

    /// Catch up the peripherals that act by themselves, and work out when they next will.
    fn process(&mut self) {
        let now = self.now;
        let mut due = u64::MAX;
        for k in 0..4 {
            if self.armed >> k & 1 != 0 {
                if now >= self.alarm_at[k] {
                    self.armed &= !(1 << k);
                    self.t_intr |= 1 << k;
                } else {
                    due = due.min(self.alarm_at[k]);
                }
            }
        }
        let per = self.period as f64;
        for k in 0..8 {
            let s = &mut self.pwm[k];
            if !s.on() {
                continue;
            }
            if now as f64 >= s.next_wrap {
                self.pwm_intr |= 1 << k;
                let (_, base) = s.pos(now as f64, per);
                s.next_wrap = base + s.len() as f64 * s.tick(per);
            }
            if self.pwm_inte >> k & 1 != 0 {
                due = due.min(s.next_wrap.ceil() as u64);
            }
        }
        for u in 0..2 {
            self.uart[u].prune(now);
            if self.uart[u].imsc & (1 << 5) != 0 {
                if let Some(c) = self.uart[u].tx.front() {
                    due = due.min(c.2);
                }
            }
        }
        if self.adc.cs & 8 != 0 && self.adc.cs & 1 != 0 {
            let step = self.adc_interval();
            if self.adc.next_sample == 0 {
                self.adc.next_sample = now;
            }
            let mut guard = 0;
            while self.adc.next_sample <= now && guard < 64 {
                self.convert();
                self.adc.next_sample += step;
                guard += 1;
            }
            if self.adc.next_sample <= now {
                self.adc.next_sample = now + step;
            }
            due = due.min(self.adc.next_sample);
        }
        if self.wd_ctrl & (1 << 30) != 0 {
            let at = self.wd_load_at + self.wd_load as u64 / 2 * 1_000_000;
            if now >= at {
                self.wd_reason = 1;
                self.reset_req = true;
            } else {
                due = due.min(at);
            }
        }
        self.next_due = due;
    }

    fn adc_interval(&self) -> u64 {
        let adc_hz = {
            let f = self.clk_hz(4);
            if f > 0.0 { f } else { 48e6 }
        };
        let div = (self.adc.div >> 8) as f64 + (self.adc.div & 0xff) as f64 / 256.0;
        let cycles = if div < 96.0 { 96.0 } else { div + 1.0 };
        (cycles / adc_hz * PS) as u64
    }

    fn convert(&mut self) {
        let ch = ((self.adc.cs >> 12) & 7) as usize;
        let v = match ch {
            0..=3 => self.analog[ch],
            // the temperature sensor at 27 °C
            4 if self.adc.cs & 2 != 0 => 0.706,
            _ => 0.0,
        };
        let code = (v / self.vref.max(0.1) * 4096.0).round().clamp(0.0, 4095.0) as u32;
        self.adc.result = code;
        if self.adc.fcs & 1 != 0 {
            let val = if self.adc.fcs & 2 != 0 { code >> 4 } else { code };
            if self.adc.fifo.len() < 4 {
                self.adc.fifo.push_back(val);
            } else {
                self.adc.fcs |= 1 << 11;
            }
        }
        // round robin
        let rr = (self.adc.cs >> 16) & 0x1f;
        if rr != 0 {
            let mut n = ch;
            for _ in 0..5 {
                n = (n + 1) % 5;
                if rr >> n & 1 != 0 {
                    break;
                }
            }
            self.adc.cs = (self.adc.cs & !0x7000) | (n as u32) << 12;
        }
    }

    // ------------------------------------------------------------ core 1 launch handshake

    fn park_core1(&mut self) {
        self.fifo[1].clear();
        self.push_fifo(0, 0);
        self.core1 = Core1::Wait(1, [0; 3]);
    }

    fn push_fifo(&mut self, to: usize, v: u32) {
        if self.fifo[to].len() < 8 {
            self.fifo[to].push_back(v);
        }
    }

    /// Core 1's ROM answering core 0 (as the real one does, echoing each word).
    fn core1_rom(&mut self) {
        while let Core1::Wait(step, mut vals) = self.core1 {
            let Some(w) = self.fifo[1].pop_front() else { break };
            if w == 0 {
                self.park_core1();
                continue;
            }
            self.core1 = match step {
                1 if w == 1 => {
                    self.push_fifo(0, 1);
                    Core1::Wait(2, vals)
                }
                2..=4 => {
                    vals[(step - 2).min(2) as usize] = w;
                    self.push_fifo(0, w);
                    if step == 4 {
                        self.launch = Some((vals[0], vals[1], vals[2]));
                        Core1::Running
                    } else {
                        Core1::Wait(step + 1, vals)
                    }
                }
                _ => {
                    self.fifo[1].clear();
                    self.push_fifo(0, 0);
                    Core1::Wait(1, vals)
                }
            };
        }
    }

    // ------------------------------------------------------------ resets

    fn reset_blocks(&mut self, bits: u32) {
        if bits & (1 << 0) != 0 {
            self.adc = Adc::default();
        }
        if bits & (1 << 5) != 0 {
            self.ctrl = [0x1f; NPIN];
            self.inte = [[0; 4]; 2];
            self.intf = [[0; 4]; 2];
            self.edges = [0; 4];
            self.dirty = true;
        }
        if bits & (1 << 8) != 0 {
            self.pads = [0x56; NPIN];
            self.dirty = true;
        }
        if bits & (1 << 14) != 0 {
            self.pwm = [Slice { top: 0xffff, div: 16, ..Default::default() }; 8];
            self.pwm_intr = 0;
            self.pwm_inte = 0;
            self.pwm_intf = 0;
            self.dirty = true;
        }
        if bits & (1 << 21) != 0 {
            self.t_off = -((self.now / 1_000_000) as i64);
            self.armed = 0;
            self.t_intr = 0;
            self.t_inte = 0;
            self.t_intf = 0;
        }
        for u in 0..2 {
            if bits & (1 << (22 + u)) != 0 {
                let log = std::mem::take(&mut self.uart_log[u]);
                self.uart[u] = Uart::default();
                self.uart_log[u] = log;
                self.dirty = true;
            }
        }
        self.next_due = 0;
    }

    // ------------------------------------------------------------ registers

    /// A peripheral register read (word aligned, no alias bits).
    fn reg_read(&mut self, a: u32) -> u32 {
        let off = a & 0xfff;
        match a & !0xfff {
            0x4000_0000 => match off {
                0 => 0x2000_2927,
                4 => 2,
                0x40 => 0x7a9c_b8f3,
                _ => 0,
            },
            0x4000_8000 => match off {
                0x38 => 1 << (self.reg(0x4000_8030) & 3),
                0x44 => 1 << (self.reg(0x4000_803c) & 1),
                0x08 | 0x14 | 0x20 | 0x2c | 0x50 | 0x5c | 0x68 | 0x74 => 1,
                0x98 => 0x10,
                0xb0 | 0xb4 => 0xffff_ffff,
                _ => self.reg(a),
            },
            0x4000_c000 => match off {
                0 => self.resets,
                8 => !self.resets & RESET_ALL,
                _ => self.reg(a),
            },
            0x4001_0000 => match off {
                4 => self.psm_off,
                0xc => 0x1ffff & !self.psm_off,
                _ => self.reg(a),
            },
            0x4001_4000 => {
                let o = off as usize;
                if o < 8 * NPIN {
                    let n = o / 8;
                    if o % 8 == 4 {
                        return self.ctrl[n];
                    }
                    let d = self.drive(n);
                    let (po, poe) = self.periph(n);
                    let lv = self.level >> n & 1;
                    return (po as u32) << 8 | (d.out as u32) << 9 | (poe as u32) << 12 | (d.oe as u32) << 13 | lv << 17 | lv << 19;
                }
                match o {
                    0xf0..=0xfc => self.intr_raw((o - 0xf0) / 4),
                    0x100..=0x15c => {
                        let (k, r) = ((o - 0x100) / 0x30, (o - 0x100) % 0x30);
                        let j = (r % 0x10) / 4;
                        match r / 0x10 {
                            0 => self.inte[k][j],
                            1 => self.intf[k][j],
                            _ => (self.intr_raw(j) | self.intf[k][j]) & self.inte[k][j],
                        }
                    }
                    _ => self.reg(a),
                }
            }
            0x4001_c000 => {
                let o = off as usize;
                if (4..4 + 4 * NPIN).contains(&o) { self.pads[(o - 4) / 4] } else { self.reg(a) }
            }
            0x4002_4000 => match off {
                4 => 0x8000_1000,
                0x1c => 0,
                _ => self.reg(a),
            },
            0x4002_8000 | 0x4002_c000 => match off {
                0 => self.reg(a) | 0x8000_0000,
                _ => self.reg(a),
            },
            0x4003_4000 | 0x4003_8000 => self.uart_read(((a >> 14) & 1) as usize ^ 1, off),
            0x4003_c000 | 0x4004_0000 => {
                let k = (a & !0xfff == 0x4004_0000) as usize;
                match off {
                    0x08 => {
                        self.spi_rx[k] = self.spi_rx[k].saturating_sub(1);
                        0xff
                    }
                    0x0c => 3 | ((self.spi_rx[k] > 0) as u32) << 2,
                    _ => self.reg(a),
                }
            }
            0x4004_4000 | 0x4004_8000 => {
                let k = (a & !0xfff == 0x4004_8000) as usize;
                match off {
                    0x34 => 0x10 | 0x200 | (self.i2c_abort[k] as u32) << 6,
                    0x80 => self.i2c_abort[k] as u32,
                    0x54 => {
                        self.i2c_abort[k] = false;
                        0
                    }
                    0x70 => 6,
                    0x74 | 0x78 => 0,
                    0x9c => self.reg(a & !0xfff | 0x6c) & 1,
                    _ => self.reg(a),
                }
            }
            0x4004_c000 => self.adc_read(off),
            0x4005_0000 => self.pwm_read(off),
            0x4005_4000 => self.timer_read(off),
            0x4005_8000 => match off {
                0 => {
                    let left = if self.wd_ctrl & (1 << 30) != 0 { self.wd_load.saturating_sub(((self.now - self.wd_load_at.min(self.now)) / 500_000) as u32) } else { 0 };
                    (self.wd_ctrl & !0xff_ffff) | left
                }
                8 => self.wd_reason,
                0x0c..=0x28 => self.scratch[((off - 0x0c) / 4) as usize],
                0x2c => {
                    let t = self.reg(a);
                    t | ((t >> 9) & 1) << 10
                }
                _ => self.reg(a),
            },
            0x4005_c000 => match off {
                0x0c => {
                    let c = self.reg(a);
                    (c & !2) | (c & 1) << 1
                }
                _ => self.reg(a),
            },
            0x4006_0000 => match off {
                0x18 => 0x8000_1000,
                0x1c => {
                    self.rng ^= self.rng << 13;
                    self.rng ^= self.rng >> 17;
                    self.rng ^= self.rng << 5;
                    self.rng & 1
                }
                0x20 => 0,
                _ => self.reg(a),
            },
            0x4006_c000 => match off {
                0 => 1,
                _ => self.reg(a),
            },
            0x5020_0000 | 0x5030_0000 => match off {
                // FSTAT: every FIFO empty
                4 => 0x0f00_0f00,
                _ => self.reg(a),
            },
            _ => self.reg(a),
        }
    }

    /// A peripheral register write of the bits in `mask` (alias writes touch only some).
    fn reg_write(&mut self, a: u32, val: u32, mask: u32) {
        let off = a & 0xfff;
        let old = self.reg(a);
        let m = |old: u32| (old & !mask) | (val & mask);
        let v = val & mask;
        match a & !0xfff {
            0x4000_8000 => {
                self.regs.insert(a, m(old));
                if off == 0x94 {
                    let hz = match m(old) & 0xff {
                        1 => self.src_hz("pll_sys"),
                        2 => self.src_hz("pll_usb"),
                        3 | 4 => ROSC_HZ,
                        5 => XOSC_HZ,
                        8 => self.clk_hz(0),
                        9 => self.clk_hz(1),
                        0xa => self.clk_hz(2),
                        0xb => self.clk_hz(3),
                        0xc => self.clk_hz(4),
                        0xd => self.clk_hz(5),
                        _ => 0.0,
                    };
                    let khz = hz / 1e3;
                    self.regs.insert(0x4000_809c, ((khz.floor() as u32) << 5) | ((khz.fract() * 32.0) as u32 & 31));
                }
                if off < 0x78 {
                    self.clocks_changed();
                }
            }
            0x4000_c000 if off == 0 => {
                let new = m(self.resets) & RESET_ALL;
                let into = new & !self.resets;
                self.resets = new;
                if into != 0 {
                    self.reset_blocks(into);
                }
            }
            0x4001_0000 if off == 4 => {
                let new = m(self.psm_off);
                let was = self.psm_off & (1 << 16) != 0;
                self.psm_off = new;
                let now_off = new & (1 << 16) != 0;
                if now_off && !was {
                    self.core1 = Core1::Off;
                    self.launch = None;
                } else if was && !now_off {
                    self.park_core1();
                }
            }
            0x4001_4000 => {
                let o = off as usize;
                if o < 8 * NPIN {
                    if o % 8 == 4 {
                        self.ctrl[o / 8] = m(self.ctrl[o / 8]) & 0x3303_331f;
                        self.dirty = true;
                    }
                    return;
                }
                match o {
                    0xf0..=0xfc => self.edges[(o - 0xf0) / 4] &= !(v & 0xcccc_cccc),
                    0x100..=0x15c => {
                        let (k, r) = ((o - 0x100) / 0x30, (o - 0x100) % 0x30);
                        let j = (r % 0x10) / 4;
                        match r / 0x10 {
                            0 => self.inte[k][j] = m(self.inte[k][j]),
                            1 => self.intf[k][j] = m(self.intf[k][j]),
                            _ => {}
                        }
                    }
                    _ => {
                        self.regs.insert(a, m(old));
                    }
                }
            }
            0x4001_c000 => {
                let o = off as usize;
                if (4..4 + 4 * NPIN).contains(&o) {
                    let n = (o - 4) / 4;
                    self.pads[n] = m(self.pads[n]) & 0xff;
                    self.dirty = true;
                } else {
                    self.regs.insert(a, m(old));
                }
            }
            0x4002_8000 | 0x4002_c000 => {
                self.regs.insert(a, m(old));
                self.clocks_changed();
            }
            0x4003_4000 | 0x4003_8000 => self.uart_write(((a >> 14) & 1) as usize ^ 1, off, val, mask),
            0x4003_c000 | 0x4004_0000 => {
                let k = (a & !0xfff == 0x4004_0000) as usize;
                if off == 0x08 {
                    self.spi_rx[k] = (self.spi_rx[k] + 1).min(8);
                    self.warn(W_SPI, "SPI devices are not simulated: SPI reads return 0xFF");
                } else {
                    self.regs.insert(a, m(old));
                }
            }
            0x4004_4000 | 0x4004_8000 => {
                let k = (a & !0xfff == 0x4004_8000) as usize;
                if off == 0x10 {
                    self.i2c_abort[k] = true;
                    self.warn(W_I2C, "I2C devices are not simulated: every I2C transfer gets no acknowledge");
                } else {
                    self.regs.insert(a, m(old));
                }
            }
            0x4004_c000 => self.adc_write(off, val, mask),
            0x4005_0000 => self.pwm_write(off, val, mask),
            0x4005_4000 => self.timer_write(off, val, mask),
            0x4005_8000 => match off {
                0 => {
                    let new = m(self.wd_ctrl);
                    if new & (1 << 31) != 0 {
                        self.wd_reason = 2;
                        self.reset_req = true;
                    }
                    if new & (1 << 30) != 0 && self.wd_ctrl & (1 << 30) == 0 {
                        self.wd_load_at = self.now;
                    }
                    self.wd_ctrl = new & 0x7fff_ffff;
                    self.next_due = 0;
                }
                4 => {
                    self.wd_load = v & 0xff_ffff;
                    self.wd_load_at = self.now;
                    self.next_due = 0;
                }
                0x0c..=0x28 => {
                    let k = ((off - 0x0c) / 4) as usize;
                    self.scratch[k] = m(self.scratch[k]);
                }
                _ => {
                    self.regs.insert(a, m(old));
                }
            },
            0x5000_0000 => {
                self.regs.insert(a, m(old));
                // a channel's CTRL_TRIG (or a trigger alias) with EN set
                if off < 0x400 && (off & 0x3f == 0x0c || off & 0x3f == 0x3c || off & 0x3f == 0x1c || off & 0x3f == 0x2c) && v != 0 {
                    self.warn(W_DMA, "DMA is not simulated: DMA transfers do nothing");
                }
            }
            0x5011_0000 => {
                self.regs.insert(a, m(old));
                // MAIN_CTRL: the controller switched on
                if off == 0x40 && v & 1 != 0 {
                    self.warn(W_USB, "USB is not simulated (printf over USB goes nowhere; use UART stdio to see output)");
                }
            }
            0x5020_0000 | 0x5030_0000 => {
                self.regs.insert(a, m(old));
                if off == 0 && v & 0xf != 0 {
                    self.warn(W_PIO, "PIO is not simulated: its state machines don't run");
                }
            }
            _ => {
                self.regs.insert(a, m(old));
            }
        }
    }

    fn uart_read(&mut self, u: usize, off: u32) -> u32 {
        let now = self.now;
        let x = &mut self.uart[u];
        x.prune(now);
        match off {
            0x00 => x.rx.pop_front().unwrap_or(0) as u32,
            0x18 => {
                let q = x.queued(now);
                let d = x.depth();
                ((q == 0) as u32) << 7 | ((x.rx.len() >= d) as u32) << 6 | ((q >= d) as u32) << 5 | (x.rx.is_empty() as u32) << 4 | (x.busy(now) as u32) << 3
            }
            0x24 => x.ibrd,
            0x28 => x.fbrd,
            0x2c => x.lcr,
            0x30 => x.cr,
            0x34 => x.ifls,
            0x38 => x.imsc,
            0x3c => x.ris(now),
            0x40 => x.ris(now) & x.imsc,
            0x48 => x.dmacr,
            0xfe0 => 0x11,
            0xfe4 => 0x10,
            0xfe8 => 0x34,
            0xff0 => 0x0d,
            0xff4 => 0xf0,
            0xff8 => 0x05,
            0xffc => 0xb1,
            _ => 0,
        }
    }

    fn uart_write(&mut self, u: usize, off: u32, val: u32, mask: u32) {
        let now = self.now;
        let m = |old: u32| (old & !mask) | (val & mask);
        match off {
            0x00 => {
                let x = &mut self.uart[u];
                x.prune(now);
                if x.cr & 0x101 != 0x101 {
                    return;
                }
                let byte = (val & mask) as u8;
                let start = x.tx.back().map(|c| c.2).unwrap_or(now).max(now);
                let end = start + x.bit_ps * x.frame(byte).len() as u64;
                x.tx.push_back((byte, start, end));
                self.uart_log[u].push((start, byte));
                self.dirty = true;
                self.next_due = 0;
            }
            0x24 => {
                self.uart[u].ibrd = m(self.uart[u].ibrd) & 0xffff;
            }
            0x28 => self.uart[u].fbrd = m(self.uart[u].fbrd) & 0x3f,
            0x2c => {
                self.uart[u].lcr = m(self.uart[u].lcr) & 0xff;
                self.uart_baud(u);
            }
            0x30 => {
                self.uart[u].cr = m(self.uart[u].cr) & 0xff87;
                self.dirty = true;
            }
            0x34 => self.uart[u].ifls = m(self.uart[u].ifls) & 0x3f,
            0x38 => {
                self.uart[u].imsc = m(self.uart[u].imsc) & 0x7ff;
                self.next_due = 0;
            }
            0x48 => self.uart[u].dmacr = m(self.uart[u].dmacr) & 7,
            _ => {}
        }
    }

    fn adc_read(&mut self, off: u32) -> u32 {
        if self.now >= self.next_due {
            self.process();
        }
        let a = &mut self.adc;
        match off {
            0x00 => {
                let ready = a.cs & 1 != 0 && self.now >= a.ready_at;
                (a.cs & !0x104) | (ready as u32) << 8
            }
            0x04 => a.result,
            0x08 => {
                let n = a.fifo.len() as u32;
                (a.fcs & 0x0f00_0c0f) | ((n == 0) as u32) << 8 | ((n >= 4) as u32) << 9 | n << 16
            }
            0x0c => match a.fifo.pop_front() {
                Some(v) => v,
                None => {
                    a.fcs |= 1 << 10;
                    0
                }
            },
            0x10 => a.div,
            0x14 => self.adc_intr(),
            0x18 => a.inte,
            0x1c => a.intf,
            0x20 => (self.adc_intr() | self.adc.intf) & self.adc.inte,
            _ => 0,
        }
    }

    fn adc_write(&mut self, off: u32, val: u32, mask: u32) {
        let m = |old: u32| (old & !mask) | (val & mask);
        match off {
            0x00 => {
                let new = m(self.adc.cs) & 0x001f_740f;
                let was_many = self.adc.cs & 8 != 0;
                self.adc.cs = new & !4;
                if new & 4 != 0 && new & 1 != 0 {
                    self.convert();
                    self.adc.ready_at = self.now + self.adc_interval();
                }
                if new & 8 != 0 && !was_many {
                    self.adc.next_sample = self.now + self.adc_interval();
                }
                self.next_due = 0;
            }
            0x08 => {
                let w1c = val & mask & 0xc00;
                self.adc.fcs = (m(self.adc.fcs) & 0x0f00_000f) | (self.adc.fcs & 0xc00 & !w1c);
                if self.adc.fcs & 1 == 0 {
                    self.adc.fifo.clear();
                }
            }
            0x10 => self.adc.div = m(self.adc.div) & 0xff_ffff,
            0x18 => self.adc.inte = m(self.adc.inte) & 1,
            0x1c => self.adc.intf = m(self.adc.intf) & 1,
            _ => {}
        }
    }

    fn pwm_read(&mut self, off: u32) -> u32 {
        if self.now >= self.next_due || off >= 0xa4 {
            self.process();
        }
        let (now, per) = (self.now as f64, self.period as f64);
        if off < 0xa0 {
            let s = &self.pwm[(off / 20) as usize];
            return match off % 20 {
                0 => s.csr & 0xff,
                4 => s.div,
                8 => s.counter(now, per),
                12 => s.cc,
                _ => s.top,
            };
        }
        match off {
            0xa0 => (0..8).map(|k| (self.pwm[k].en() as u32) << k).sum(),
            0xa4 => self.pwm_intr,
            0xa8 => self.pwm_inte,
            0xac => self.pwm_intf,
            0xb0 => (self.pwm_intr | self.pwm_intf) & self.pwm_inte,
            _ => 0,
        }
    }

    fn set_enable(&mut self, k: usize, on: bool) {
        let (now, per) = (self.now as f64, self.period as f64);
        let s = &mut self.pwm[k];
        if on && !s.en() {
            s.csr |= 1;
            if !s.held {
                let c = s.ctr;
                s.set_origin(now, c, per);
            }
        } else if !on && s.en() {
            if !s.held {
                s.ctr = s.pos(now, per).0 as u32;
            }
            s.csr &= !1;
        }
    }

    fn pwm_write(&mut self, off: u32, val: u32, mask: u32) {
        let m = |old: u32| (old & !mask) | (val & mask);
        let (now, per) = (self.now as f64, self.period as f64);
        self.dirty = true;
        self.next_due = 0;
        if off < 0xa0 {
            let k = (off / 20) as usize;
            match off % 20 {
                0 => {
                    let new = m(self.pwm[k].csr);
                    self.set_enable(k, new & 1 != 0);
                    let b = self.pwm_b(k, self.level);
                    let s = &mut self.pwm[k];
                    let same_mode = (new ^ s.csr) & 2 == 0;
                    // where it is in its period (which keeps a phase-correct count's direction),
                    // or its count when the mode changes
                    let mut c = match (s.on(), same_mode) {
                        (true, true) => s.pos(now, per).0,
                        (false, true) => s.ctr as u64,
                        _ => s.counter(now, per) as u64,
                    };
                    if (new >> 4) & 3 != s.divmode() {
                        s.edges = 0;
                    }
                    s.csr = (new & 0x3f & !1) | (s.csr & 1);
                    // gated by B, or counting its edges: not by time
                    s.held = match s.divmode() {
                        0 => false,
                        1 => !b,
                        _ => true,
                    };
                    if s.en() {
                        // phase advance / retard: one count forward or held back by one
                        let n = s.len();
                        if new & 0x80 != 0 {
                            c = (c + 1) % n;
                        }
                        if new & 0x40 != 0 {
                            c = (c + n - 1) % n;
                        }
                    }
                    if s.on() {
                        s.set_origin(now, c as u32, per);
                    } else {
                        s.ctr = c as u32;
                    }
                }
                4 => {
                    let s = &mut self.pwm[k];
                    let c = s.counter(now, per);
                    s.div = m(s.div) & 0xfff;
                    if s.on() {
                        s.set_origin(now, c, per);
                    }
                }
                8 => {
                    let s = &mut self.pwm[k];
                    let c = m(s.counter(now, per)) & 0xffff;
                    s.ctr = c;
                    if s.on() {
                        s.set_origin(now, c, per);
                    }
                }
                12 => self.pwm[k].cc = m(self.pwm[k].cc),
                _ => {
                    let s = &mut self.pwm[k];
                    let c = s.counter(now, per);
                    s.top = m(s.top) & 0xffff;
                    if s.on() {
                        s.set_origin(now, c.min(s.top), per);
                    }
                }
            }
            return;
        }
        match off {
            0xa0 => {
                let cur: u32 = (0..8).map(|k| (self.pwm[k].en() as u32) << k).sum();
                let new = m(cur);
                for k in 0..8 {
                    self.set_enable(k, new >> k & 1 != 0);
                }
            }
            0xa4 => self.pwm_intr &= !(val & mask),
            0xa8 => self.pwm_inte = m(self.pwm_inte) & 0xff,
            0xac => self.pwm_intf = m(self.pwm_intf) & 0xff,
            _ => {}
        }
    }

    fn timer_read(&mut self, off: u32) -> u32 {
        if self.now >= self.next_due || matches!(off, 0x20 | 0x34 | 0x40) {
            self.process();
        }
        let t = self.now_us();
        match off {
            0x08 => self.latch[self.core],
            0x0c => {
                self.latch[self.core] = (t >> 32) as u32;
                t as u32
            }
            0x10..=0x1c => self.alarm[((off - 0x10) / 4) as usize],
            0x20 => self.armed,
            0x24 => (t >> 32) as u32,
            0x28 => t as u32,
            0x34 => self.t_intr,
            0x38 => self.t_inte,
            0x3c => self.t_intf,
            0x40 => (self.t_intr | self.t_intf) & self.t_inte,
            _ => self.reg(0x4005_4000 + off),
        }
    }

    fn timer_write(&mut self, off: u32, val: u32, mask: u32) {
        let m = |old: u32| (old & !mask) | (val & mask);
        self.next_due = 0;
        match off {
            0x00 => {
                let t = ((m(0) as u64) << 32) | self.pending_low as u64;
                self.t_off = t as i64 - (self.now / 1_000_000) as i64;
            }
            0x04 => self.pending_low = m(0),
            0x10..=0x1c => {
                let k = ((off - 0x10) / 4) as usize;
                let target = m(self.alarm[k]);
                self.alarm[k] = target;
                self.armed |= 1 << k;
                let now_us = self.now_us();
                let delta = target.wrapping_sub(now_us as u32) as u64;
                let fire_us = now_us + delta;
                self.alarm_at[k] = ((fire_us as i64 - self.t_off).max(0) as u64) * 1_000_000;
            }
            0x20 => self.armed &= !(val & mask),
            0x34 => self.t_intr &= !(val & mask),
            0x38 => self.t_inte = m(self.t_inte) & 0xf,
            0x3c => self.t_intf = m(self.t_intf) & 0xf,
            _ => {
                let old = self.reg(0x4005_4000 + off);
                self.regs.insert(0x4005_4000 + off, m(old));
            }
        }
    }

    // ------------------------------------------------------------ SIO

    fn sio_read(&mut self, off: u32) -> u32 {
        let k = self.core;
        match off {
            0x00 => k as u32,
            0x04 => {
                if self.dirty {
                    self.update_levels();
                }
                self.gpio_in()
            }
            // QSPI pins: the BOOTSEL button (chip select) not pressed
            0x08 => 0x3e,
            0x10 => self.sio_out,
            0x20 => self.sio_oe,
            0x30 => self.hi_out,
            0x40 => self.hi_oe,
            0x50 => (!self.fifo[k].is_empty() as u32) | ((self.fifo[1 - k].len() < 8) as u32) << 1 | self.fifo_err[k],
            0x58 => match self.fifo[k].pop_front() {
                Some(v) => v,
                None => {
                    self.fifo_err[k] |= 8;
                    0
                }
            },
            0x5c => self.spin,
            0x60 => self.div[k].dividend,
            0x64 => self.div[k].divisor,
            0x68 => self.div[k].dividend,
            0x6c => self.div[k].divisor,
            0x70 => {
                self.div[k].dirty = false;
                self.div[k].quot
            }
            0x74 => self.div[k].rem,
            0x78 => 1 | (self.div[k].dirty as u32) << 1,
            0x80..=0xfc => {
                self.warn(W_INTERP, "the SIO interpolators are not simulated (they read back what was written)");
                self.interp[k][((off - 0x80) / 4) as usize]
            }
            0x100..=0x17c => {
                let b = 1 << ((off - 0x100) / 4);
                if self.spin & b != 0 {
                    0
                } else {
                    self.spin |= b;
                    b
                }
            }
            _ => 0,
        }
    }

    fn sio_write(&mut self, off: u32, v: u32) {
        let k = self.core;
        let pins = (1u32 << NPIN) - 1;
        match off {
            0x10..=0x2c => {
                let r = if off < 0x20 { &mut self.sio_out } else { &mut self.sio_oe };
                match off & 0xc {
                    0 => *r = v & pins,
                    4 => *r |= v & pins,
                    8 => *r &= !v,
                    _ => *r ^= v & pins,
                }
                self.dirty = true;
            }
            0x30..=0x4c => {
                let r = if off < 0x40 { &mut self.hi_out } else { &mut self.hi_oe };
                match off & 0xc {
                    0 => *r = v & 0x3f,
                    4 => *r |= v & 0x3f,
                    8 => *r &= !v,
                    _ => *r ^= v & 0x3f,
                }
            }
            0x50 => self.fifo_err[k] &= !(v & 0xc),
            0x54 => {
                if self.fifo[1 - k].len() < 8 {
                    self.fifo[1 - k].push_back(v);
                } else {
                    self.fifo_err[k] |= 4;
                }
                if k == 0 && matches!(self.core1, Core1::Wait(..)) {
                    self.core1_rom();
                }
            }
            0x60 | 0x68 => {
                self.div[k].dividend = v;
                self.div[k].signed = off == 0x68;
                self.div[k].calc();
            }
            0x64 | 0x6c => {
                self.div[k].divisor = v;
                self.div[k].signed = off == 0x6c;
                self.div[k].calc();
            }
            0x70 => {
                self.div[k].quot = v;
                self.div[k].dirty = true;
            }
            0x74 => {
                self.div[k].rem = v;
                self.div[k].dirty = true;
            }
            0x80..=0xfc => self.interp[k][((off - 0x80) / 4) as usize] = v,
            0x100..=0x17c => self.spin &= !(1 << ((off - 0x100) / 4)),
            _ => {}
        }
    }

    // ------------------------------------------------------------ the bus

    fn read_word(&mut self, a: u32) -> u32 {
        match a >> 28 {
            0x4 | 0x5 => {
                if (0x5010_0000..0x5010_1000).contains(&a) {
                    let o = (a & 0xffc) as usize;
                    return u32::from_le_bytes(self.usb_ram[o..o + 4].try_into().unwrap());
                }
                self.reg_read(a & !0x3003)
            }
            0x1 if (0x1400_0000..0x1500_0000).contains(&a) => match a & 0xff {
                // XIP STAT: FIFO empty, flush done
                0x08 => 6,
                _ => self.reg(a & !3),
            },
            0x1 if a >= 0x1800_0000 => match a & 0xff {
                0x28 => 6 | ((!self.ssi_rx.is_empty()) as u32) << 3,
                0x60 => self.ssi_rx.pop_front().unwrap_or(0),
                _ => self.reg(a & !3),
            },
            0xd => self.sio_read(a & 0x1fc),
            _ => {
                self.warn(W_BUS, &format!("the firmware read unmapped memory at {a:#010x}"));
                0
            }
        }
    }

    fn write_word(&mut self, a: u32, v: u32) {
        match a >> 28 {
            0x4 | 0x5 => {
                if (0x5010_0000..0x5010_1000).contains(&a) {
                    let o = (a & 0xffc) as usize;
                    self.usb_ram[o..o + 4].copy_from_slice(&v.to_le_bytes());
                    return;
                }
                let base = a & !0x3003;
                match (a >> 12) & 3 {
                    0 => self.reg_write(base, v, !0),
                    1 => {
                        let old = self.peek(base);
                        self.reg_write(base, old ^ v, !0)
                    }
                    2 => self.reg_write(base, !0, v),
                    _ => self.reg_write(base, 0, v),
                }
            }
            0x1 if a >= 0x1800_0000 => {
                if a & 0xff == 0x60 {
                    // flash status reads (busy clear, quad enabled)
                    if self.ssi_rx.len() < 16 {
                        self.ssi_rx.push_back(0x02);
                    }
                } else {
                    self.regs.insert(a & !3, v);
                }
            }
            0x1 if (0x1400_0000..0x1500_0000).contains(&a) => {
                self.regs.insert(a & !3, v);
            }
            0xd => self.sio_write(a & 0x1fc, v),
            _ => self.warn(W_BUS, &format!("the firmware wrote unmapped memory at {a:#010x}")),
        }
    }

    /// A register's value without read side effects (for XOR aliases).
    fn peek(&mut self, a: u32) -> u32 {
        let off = a & 0xfff;
        match a & !0xfff {
            0x4003_4000 | 0x4003_8000 if off == 0 => 0,
            0x4003_c000 | 0x4004_0000 if off == 8 => 0,
            0x4004_4000 | 0x4004_8000 if (0x40..=0x6c).contains(&off) => 0,
            0x4004_c000 if off == 0x0c => 0,
            0x4005_4000 if off == 0x0c => self.now_us() as u32,
            _ => self.reg_read(a),
        }
    }

    fn mem(&mut self, a: u32, n: usize) -> Option<&mut [u8]> {
        let (buf, o): (&mut Vec<u8>, usize) = match a >> 24 {
            0x00 => (&mut self.rom, a as usize),
            0x10..=0x13 => (&mut self.flash, (a & 0x00ff_ffff) as usize),
            0x15 => (&mut self.xip_sram, (a & 0x00ff_ffff) as usize),
            0x20 => (&mut self.sram, (a - 0x2000_0000) as usize),
            0x21 => {
                // the unstriped SRAM0..3 view
                let off = a & 0xffff;
                let bank = (a >> 16) & 3;
                (&mut self.sram, (((off >> 2) << 4) | (bank << 2) | (off & 3)) as usize)
            }
            _ => return None,
        };
        buf.get_mut(o..o + n)
    }
}

/// The chip as one core sees it.
pub struct View<'a> {
    pub chip: &'a mut Chip,
    pub core: usize,
}

impl Bus for View<'_> {
    fn read32(&mut self, a: u32) -> u32 {
        if let Some(m) = self.chip.mem(a, 4) {
            return u32::from_le_bytes([m[0], m[1], m[2], m[3]]);
        }
        self.chip.core = self.core;
        self.chip.read_word(a & !3)
    }
    fn read16(&mut self, a: u32) -> u16 {
        if let Some(m) = self.chip.mem(a, 2) {
            return u16::from_le_bytes([m[0], m[1]]);
        }
        self.chip.core = self.core;
        (self.chip.read_word(a & !3) >> ((a & 2) * 8)) as u16
    }
    fn read8(&mut self, a: u32) -> u8 {
        if let Some(m) = self.chip.mem(a, 1) {
            return m[0];
        }
        self.chip.core = self.core;
        (self.chip.read_word(a & !3) >> ((a & 3) * 8)) as u8
    }
    fn write32(&mut self, a: u32, v: u32) {
        if matches!(a >> 24, 0x15 | 0x20 | 0x21) {
            if let Some(m) = self.chip.mem(a, 4) {
                m.copy_from_slice(&v.to_le_bytes());
                return;
            }
        }
        if a < 0x1400_0000 {
            return;
        }
        self.chip.core = self.core;
        self.chip.write_word(a & !3, v)
    }
    fn write16(&mut self, a: u32, v: u16) {
        if matches!(a >> 24, 0x15 | 0x20 | 0x21) {
            if let Some(m) = self.chip.mem(a, 2) {
                m.copy_from_slice(&v.to_le_bytes());
                return;
            }
        }
        if a < 0x1400_0000 {
            return;
        }
        // narrow writes to registers repeat the value across the word
        self.chip.core = self.core;
        self.chip.write_word(a & !3, v as u32 * 0x1_0001)
    }
    fn write8(&mut self, a: u32, v: u8) {
        if matches!(a >> 24, 0x15 | 0x20 | 0x21) {
            if let Some(m) = self.chip.mem(a, 1) {
                m[0] = v;
                return;
            }
        }
        if a < 0x1400_0000 {
            return;
        }
        self.chip.core = self.core;
        self.chip.write_word(a & !3, v as u32 * 0x0101_0101)
    }
    fn irqs(&self) -> u32 {
        self.chip.lines(self.core)
    }
    fn native(&mut self, cpu: &mut Cpu, pc: u32) -> Option<u32> {
        mcu_rom::call(self, cpu, pc)
    }
}

impl mcu_rom::Host for View<'_> {
    fn flash(&mut self) -> &mut Vec<u8> {
        &mut self.chip.flash
    }
    fn park(&mut self) {
        if self.core == 1 {
            self.chip.park_core1();
        }
    }
    fn note(&mut self, s: String) {
        self.chip.notes.push(s);
    }
    fn core(&self) -> usize {
        self.core
    }
}

// ---------------------------------------------------------------- the chip with its cores

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Stop {
    /// Reached the time asked for.
    Time,
    /// A pin's drive changed.
    Changed,
    /// Both cores stopped for good.
    Halted,
}

pub struct Rp2040 {
    pub cpu: [Cpu; 2],
    pub chip: Chip,
    /// Each core's time (ps).
    pub t: [u64; 2],
    pub fw: Firmware,
    last: [Drive; NPIN],
    next_edge: u64,
    pub resets: u32,
}

impl Rp2040 {
    pub fn new(fw: Firmware) -> Rp2040 {
        let chip = Chip::new(&fw);
        let mut m = Rp2040 { cpu: [Cpu::default(), Cpu::default()], chip, t: [0; 2], fw, last: [Drive::default(); NPIN], next_edge: u64::MAX, resets: 0 };
        m.boot();
        m.last = m.chip.drives();
        m
    }

    fn boot(&mut self) {
        let s = self.chip.scratch;
        // a flash build boots through its vector table (its ELF entry re-enters the boot ROM)
        let entry = self.fw.entry.filter(|e| (0x2000_0000..SRAM_END).contains(e));
        let ram_start = self.fw.ram.iter().map(|r| r.0).min().unwrap_or(0x2000_0000);
        let has_flash = self.fw.flash.len() > 0x108;
        let mut view = View { chip: &mut self.chip, core: 0 };
        if s[4] == 0xb007_c0d3 && s[5] == s[7] ^ 0xb007_c0d3u32.wrapping_neg() && s[7] != 0 {
            // the watchdog's "reboot to this address"
            self.cpu[0].start(0x1000_0100, s[6], s[7], mcu_rom::DEAD | 1);
            view.chip.scratch[4] = 0;
        } else if let Some(e) = entry {
            let vt = if e >= 0x2000_0000 { 0x2000_0000 } else { 0x1000_0100 };
            let sp = view.read32(vt);
            let sp = if (0x2000_0000..=SRAM_END).contains(&sp) { sp } else { SRAM_END };
            self.cpu[0].start(vt, sp, e, mcu_rom::DEAD | 1);
        } else if has_flash {
            self.cpu[0].reset(&mut view, 0x1000_0100);
        } else {
            self.cpu[0].reset(&mut view, ram_start);
        }
        self.cpu[1] = Cpu::default();
        self.chip.park_core1();
    }

    /// Restart the chip (watchdog, reset request): RAM and peripherals reset, flash and the
    /// watchdog's scratch registers kept.
    fn reboot(&mut self) {
        let now = self.time_ps();
        let mut fresh = Chip::new(&self.fw);
        fresh.flash = std::mem::take(&mut self.chip.flash);
        fresh.scratch = self.chip.scratch;
        fresh.wd_reason = self.chip.wd_reason;
        fresh.notes = std::mem::take(&mut self.chip.notes);
        fresh.uart_log = std::mem::take(&mut self.chip.uart_log);
        fresh.ext = self.chip.ext;
        fresh.ext_level = self.chip.ext_level;
        fresh.analog = self.chip.analog;
        fresh.vref = self.chip.vref;
        fresh.now = now;
        fresh.t_off = -((now / 1_000_000) as i64);
        self.chip = fresh;
        self.resets += 1;
        if self.resets <= 3 {
            self.chip.notes.push(format!(
                "the chip reset at {} ({})",
                crate::circuit::si(now as f64 / PS, "s"),
                if self.chip.wd_reason & 1 != 0 { "watchdog timeout" } else { "reset requested" }
            ));
        }
        self.boot();
        self.t = [now; 2];
        self.chip.dirty = true;
    }

    pub fn time_ps(&self) -> u64 {
        self.chip.now
    }

    pub fn time(&self) -> f64 {
        self.time_ps() as f64 / PS
    }

    fn core_runs(&self, k: usize) -> bool {
        self.cpu[k].halted.is_none() && (k == 0 || self.chip.core1 == Core1::Running)
    }

    /// Where a core is, for messages.
    pub fn place(&self, k: usize) -> String {
        let pc = self.cpu[k].r[15];
        match self.fw.symbol(pc) {
            Some(s) => format!("{s} ({pc:#010x})"),
            None => format!("{pc:#010x}"),
        }
    }

    /// Outside levels and voltages: pins in `mask` read `levels`; `analog` is GP26..29.
    pub fn set_inputs(&mut self, mask: u32, levels: u32, analog: [f64; 4], vref: f64) {
        self.chip.ext = mask;
        self.chip.ext_level = levels & mask;
        self.chip.analog = analog;
        self.chip.vref = vref;
        self.chip.update_levels();
    }

    pub fn drives(&self) -> [Drive; NPIN] {
        self.last
    }

    /// Characters received on a UART.
    pub fn uart_input(&mut self, u: usize, bytes: &[u8]) {
        for b in bytes {
            if self.chip.uart[u].rx.len() < 32 {
                self.chip.uart[u].rx.push_back(*b);
            }
        }
    }

    /// Run until `until` (s), or (when `stop_on_change`) the first time a pin's drive changes.
    pub fn run(&mut self, until: f64, stop_on_change: bool) -> Stop {
        let until = (until * PS).max(0.0).ceil() as u64;
        loop {
            if self.chip.reset_req {
                self.chip.reset_req = false;
                self.reboot();
            }
            if let Some((vt, sp, pc)) = self.chip.launch.take() {
                self.cpu[1].start(vt, sp, pc, mcu_rom::DEAD | 1);
                self.t[1] = self.t[0].max(self.chip.now);
            }
            // the cores that can run now
            let mut best: Option<usize> = None;
            for k in 0..2 {
                if !self.core_runs(k) {
                    continue;
                }
                if self.cpu[k].sleeping {
                    let lines = self.chip.lines(k);
                    if !self.cpu[k].would_wake(lines) {
                        // asleep while the other core runs: its clock, and so its SysTick, keeps
                        // up with the chip (SysTick can be what wakes it)
                        let now = self.chip.now;
                        let per = self.chip.period;
                        if self.t[k] + per <= now {
                            let cycles = (now - self.t[k]) / per;
                            let t1 = self.t[k] + cycles * per;
                            let us = t1 / 1_000_000 - self.t[k] / 1_000_000;
                            self.cpu[k].idle(cycles, us as u32);
                            self.t[k] = t1;
                        }
                        if !self.cpu[k].would_wake(lines) {
                            continue;
                        }
                    }
                    self.t[k] = self.t[k].max(self.chip.now);
                }
                if best.is_none_or(|b| self.t[k] < self.t[b]) {
                    best = Some(k);
                }
            }
            let Some(k) = best else {
                if !self.core_runs(0) && !self.core_runs(1) {
                    self.chip.now = self.chip.now.max(until);
                    return Stop::Halted;
                }
                // everyone asleep: skip to the next thing that can wake them
                if self.chip.now >= self.chip.next_due {
                    self.chip.process();
                }
                let mut target = self.chip.next_due.min(until).min(self.next_edge);
                for c in 0..2 {
                    if self.core_runs(c) {
                        if let Some((ticks, core_clk)) = self.cpu[c].systick_due() {
                            let dt = if core_clk { ticks * self.chip.period } else { ticks * 1_000_000 };
                            target = target.min(self.t[c].max(self.chip.now) + dt);
                        }
                    }
                }
                let target = target.max(self.chip.now);
                for c in 0..2 {
                    if self.core_runs(c) {
                        let from = self.t[c].min(target);
                        let cycles = (target - from) / self.chip.period;
                        let us = target / 1_000_000 - from / 1_000_000;
                        self.cpu[c].idle(cycles, us as u32);
                        self.t[c] = target;
                    }
                }
                self.chip.now = target;
                self.chip.process();
                if self.check_pins(stop_on_change) {
                    return Stop::Changed;
                }
                if target >= until {
                    return Stop::Time;
                }
                continue;
            };
            let t0 = self.t[k];
            if t0 >= until {
                self.chip.now = self.chip.now.max(until);
                return Stop::Time;
            }
            self.chip.now = t0;
            if t0 >= self.chip.next_due {
                self.chip.process();
            }
            let c = {
                let mut v = View { chip: &mut self.chip, core: k };
                self.cpu[k].step(&mut v)
            };
            if c > 0 {
                let t1 = t0 + c as u64 * self.chip.period;
                self.t[k] = t1;
                let us = t1 / 1_000_000 - t0 / 1_000_000;
                self.cpu[k].tick(c, us as u32);
            }
            if self.cpu[k].sev_out {
                self.cpu[k].sev_out = false;
                self.cpu[1 - k].signal_event();
            }
            if let Some(h) = self.cpu[k].halted.clone() {
                match h {
                    Halt::Fault(why) if why == "system reset requested" => {
                        self.chip.reset_req = true;
                        continue;
                    }
                    Halt::Fault(why) => self.chip.notes.push(format!("core {k} locked up: {why} in {}", self.place(k))),
                    Halt::Breakpoint(pc) => {
                        let at = self.fw.symbol(pc).unwrap_or_else(|| format!("{pc:#010x}"));
                        self.chip.notes.push(format!("core {k} stopped at a breakpoint in {at} (the SDK's panic() ends in one)"));
                    }
                }
            }
            if (self.chip.dirty || self.t[k] >= self.next_edge) && self.check_pins(stop_on_change) {
                self.chip.now = self.chip.now.max(self.t[k]);
                return Stop::Changed;
            }
        }
    }

    /// Recompute the pins after a change; true when one changed (and the caller should stop).
    fn check_pins(&mut self, stop: bool) -> bool {
        let at = self.chip.now;
        self.chip.dirty = false;
        let d = self.chip.drives();
        self.chip.update_levels();
        self.next_edge = self.chip.next_edge();
        if self.next_edge <= at {
            self.next_edge = at + 1;
        }
        if d != self.last {
            self.last = d;
            return stop;
        }
        false
    }

    /// Power-on reset: the firmware starts over (flash kept).
    pub fn restart(&mut self) {
        let now = self.time_ps();
        let mut fresh = Chip::new(&self.fw);
        fresh.flash = std::mem::take(&mut self.chip.flash);
        fresh.notes = std::mem::take(&mut self.chip.notes);
        fresh.uart_log = std::mem::take(&mut self.chip.uart_log);
        fresh.now = now;
        fresh.t_off = -((now / 1_000_000) as i64);
        self.chip = fresh;
        self.boot();
        self.t = [now; 2];
        self.last = self.chip.drives();
        self.next_edge = u64::MAX;
    }

    /// Held in reset until `until` (s): the pins are at their reset state and nothing runs.
    pub fn hold(&mut self, until: f64) {
        self.restart();
        let t = ((until * PS).ceil() as u64).max(self.chip.now);
        self.chip.now = t;
        self.t = [t; 2];
        self.chip.t_off = -((t / 1_000_000) as i64);
    }

    /// The text a UART has sent so far.
    #[cfg(test)]
    pub fn uart_text(&self, u: usize) -> String {
        String::from_utf8_lossy(&self.chip.uart_log[u].iter().map(|c| c.1).collect::<Vec<u8>>()).replace('\r', "")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Pico SDK build from examples/firmware (sources and how to rebuild them are there).
    fn fw(name: &str) -> Option<Firmware> {
        let b = std::fs::read(format!("{}/examples/firmware/{name}", env!("CARGO_MANIFEST_DIR"))).ok()?;
        Some(load_firmware(&b).unwrap())
    }

    /// Times (s) at which a pin's drive level changes.
    fn edges(m: &mut Rp2040, pin: usize, until: f64) -> Vec<(f64, bool)> {
        let mut out = vec![];
        let mut last = m.drives()[pin];
        while m.time() < until {
            if m.run(until, true) != Stop::Changed {
                break;
            }
            let d = m.drives()[pin];
            if d != last && d.oe {
                out.push((m.time(), d.out));
            }
            last = d;
        }
        out
    }

    #[test]
    fn divider_and_spinlocks() {
        let mut c = Chip::new(&Firmware { flash: vec![0; 512], ..Default::default() });
        c.sio_write(0x68, -7i32 as u32);
        c.sio_write(0x6c, 2);
        assert_eq!(c.sio_read(0x70) as i32, -3);
        assert_eq!(c.sio_read(0x74) as i32, -1);
        assert_eq!(c.sio_read(0x104), 2);
        assert_eq!(c.sio_read(0x104), 0);
        c.sio_write(0x104, 0);
        assert_eq!(c.sio_read(0x104), 2);
    }

    #[test]
    fn pwm_levels_and_edges() {
        let mut s = Slice { csr: 1, div: 16, cc: 250, top: 999, ..Default::default() };
        s.set_origin(0.0, 0, 8000.0);
        assert!(s.level(0, 1.0, 8000.0));
        assert!(!s.level(0, 251.0 * 8000.0, 8000.0));
        assert_eq!(s.next_edge(1.0, 8000.0), 250.0 * 8000.0);
        assert_eq!(s.next_edge(250.0 * 8000.0, 8000.0), 1000.0 * 8000.0);
    }

    #[test]
    fn uart_parity_bits() {
        // 8 data bits with parity: start, 8 data, parity, stop
        let mut u = Uart { lcr: 0x60 | 2 | 4, ..Default::default() };
        // even parity: one 1 in the data, so the parity bit is 1
        assert_eq!(u.frame(0x01)[9], true);
        assert_eq!(u.frame(0x03)[9], false);
        // odd parity
        u.lcr = 0x60 | 2;
        assert_eq!(u.frame(0x01)[9], false);
        assert_eq!(u.frame(0x03)[9], true);
        // stick parity: EPS set sends 0
        u.lcr = 0x60 | 2 | 4 | 0x80;
        assert_eq!(u.frame(0x01)[9], false);
    }

    fn pwm_chip() -> Chip {
        let mut c = Chip::new(&Firmware { flash: vec![0; 512], ..Default::default() });
        c.pwm_write(0x10, 99, !0);
        c
    }

    #[test]
    fn pwm_phase_advance_and_retard() {
        let mut c = pwm_chip();
        let tick = c.period;
        c.pwm_write(0, 1, !0);
        c.now = 10 * tick + tick / 2;
        assert_eq!(c.pwm_read(8), 10);
        c.pwm_write(0, 1 | 0x80, !0);
        assert_eq!(c.pwm_read(8), 11);
        c.pwm_write(0, 1 | 0x40, !0);
        c.pwm_write(0, 1 | 0x40, !0);
        assert_eq!(c.pwm_read(8), 9);
        // and it runs on from there
        c.now += 5 * tick;
        assert_eq!(c.pwm_read(8), 14);
        // the bits don't stick
        assert_eq!(c.pwm_read(0) & 0xc0, 0);
    }

    #[test]
    fn pwm_counts_and_gates_on_its_b_input() {
        let mut c = pwm_chip();
        let tick = c.period;
        // GP1 is slice 0's B input
        c.ctrl[1] = 4;
        c.ext = 1 << 1;
        let set_b = |c: &mut Chip, hi: bool| {
            c.ext_level = if hi { 1 << 1 } else { 0 };
            c.update_levels();
        };
        // rising edges
        c.pwm_write(0, 1 | 2 << 4, !0);
        for _ in 0..3 {
            c.now += 7 * tick;
            set_b(&mut c, true);
            c.now += 7 * tick;
            set_b(&mut c, false);
        }
        assert_eq!(c.pwm_read(8), 3);
        // falling edges count one more after the next high pulse
        c.pwm_write(0, 1 | 3 << 4, !0);
        set_b(&mut c, true);
        assert_eq!(c.pwm_read(8), 3);
        set_b(&mut c, false);
        assert_eq!(c.pwm_read(8), 4);
        // gated: counts by time only while B is high
        c.pwm_write(8, 0, !0);
        c.pwm_write(0, 1 | 1 << 4, !0);
        c.now += 20 * tick;
        assert_eq!(c.pwm_read(8), 0);
        set_b(&mut c, true);
        c.now += 30 * tick + tick / 2;
        set_b(&mut c, false);
        c.now += 20 * tick;
        assert_eq!(c.pwm_read(8), 30);
        // edges wrap at TOP and raise the wrap interrupt
        c.pwm_write(0x10, 1, !0);
        c.pwm_write(8, 0, !0);
        c.pwm_write(0, 1 | 2 << 4, !0);
        for _ in 0..2 {
            set_b(&mut c, true);
            set_b(&mut c, false);
        }
        assert_eq!(c.pwm_read(8), 0);
        assert_eq!(c.pwm_read(0xa4) & 1, 1);
    }

    #[test]
    fn firmware_formats() {
        let raw = vec![1u8, 2, 3, 4];
        assert_eq!(load_firmware(&raw).unwrap().flash, raw);
        let hex = ":020000041000EA\n:0400000001020304F2\n:00000001FF\n";
        assert_eq!(load_firmware(hex.as_bytes()).unwrap().flash, raw);
        let mut uf2 = vec![0u8; 512];
        for (o, v) in [(0, 0x0a32_4655u32), (4, 0x9e5d_5157), (8, 0x2000), (12, 0x1000_0000), (16, 4), (28, 0xe48b_ff56), (508, 0x0ab1_6f30)] {
            uf2[o..o + 4].copy_from_slice(&v.to_le_bytes());
        }
        uf2[32..36].copy_from_slice(&raw);
        assert_eq!(load_firmware(&uf2).unwrap().flash, raw);
    }

    // The tests below run the Pico SDK builds in examples/firmware.

    #[test]
    fn sdk_blink() {
        let f = fw("blink.uf2").expect("examples/firmware");
        let mut m = Rp2040::new(f);
        let e = edges(&mut m, 25, 1.1);
        assert!(m.chip.notes.is_empty(), "{:?}", m.chip.notes);
        assert!(e.len() >= 4, "{e:?} at {}", m.place(0));
        // 250 ms on, 250 ms off
        let dt = e[2].0 - e[1].0;
        assert!((dt - 0.25).abs() < 1e-3, "{e:?}");
    }

    #[test]
    fn sdk_printf() {
        let f = fw("hello.uf2").expect("examples/firmware");
        let mut m = Rp2040::new(f);
        m.run(0.35, false);
        let text = m.uart_text(0);
        assert!(text.starts_with("hello 0 0.0000 0.000000 0\nhello 1 1.5000 0.033278 333333335\n"), "{text:?} at {} notes {:?}", m.place(0), m.chip.notes);
        assert!(text.contains("hello 3 2.5981 0.098507 1000000007\n"), "{text:?}");
    }

    #[test]
    fn sdk_pwm() {
        let f = fw("pwm.uf2").expect("examples/firmware");
        let mut m = Rp2040::new(f);
        let e = edges(&mut m, 0, 0.01);
        let rises: Vec<f64> = e.iter().filter(|x| x.1).map(|x| x.0).collect();
        let falls: Vec<f64> = e.iter().filter(|x| !x.1).map(|x| x.0).collect();
        assert!(rises.len() > 5, "{e:?} at {}", m.place(0));
        // 1 kHz, 25 %
        let per = rises[3] - rises[2];
        assert!((per - 1e-3).abs() < 1e-6, "{per}");
        let hi = falls.iter().find(|f| **f > rises[2]).unwrap() - rises[2];
        assert!((hi - 0.25e-3).abs() < 1e-6, "{hi}");
    }

    #[test]
    fn sdk_button() {
        let f = fw("button.uf2").expect("examples/firmware");
        let mut m = Rp2040::new(f);
        m.run(0.01, false);
        // released (pulled up): LED off; pressed (pulled to ground): on
        assert!(!m.drives()[16].out);
        m.set_inputs(1 << 15, 0, [0.0; 4], 3.3);
        m.run(0.011, false);
        assert!(m.drives()[16].out && m.drives()[16].oe);
        m.set_inputs(1 << 15, 1 << 15, [0.0; 4], 3.3);
        m.run(0.012, false);
        assert!(!m.drives()[16].out);
    }

    #[test]
    fn sdk_adc() {
        let f = fw("adc.uf2").expect("examples/firmware");
        let mut m = Rp2040::new(f);
        m.set_inputs(0, 0, [1.65, 0.0, 0.0, 0.0], 3.3);
        m.run(0.05, false);
        let text = m.uart_text(0);
        assert!(text.contains("adc 2048 1.650 V\n"), "{text:?} {:?}", m.chip.notes);
        // the PWM on GP16 follows: 2048 / 4096
        let mut hi = 0.0;
        let t0 = m.time();
        let mut last = (t0, m.drives()[16].out);
        while m.time() < t0 + 0.001 {
            if m.run(t0 + 0.001, true) == Stop::Time {
                break;
            }
            if last.1 {
                hi += m.time() - last.0;
            }
            last = (m.time(), m.drives()[16].out);
        }
        assert!((hi / 0.001 - 0.5).abs() < 0.02, "{hi}");
    }

    #[test]
    fn sdk_multicore() {
        let f = fw("multicore.uf2").expect("examples/firmware");
        let mut m = Rp2040::new(f);
        m.run(0.1, false);
        let text = m.uart_text(0);
        assert!(text.starts_with("core1 says 2\ncore1 says 4\ncore1 says 6\n"), "{text:?} {:?} core0 {} core1 {}", m.chip.notes, m.place(0), m.place(1));
    }

    #[test]
    fn sdk_interrupts() {
        let f = fw("irq.uf2").expect("examples/firmware");
        let mut m = Rp2040::new(f);
        let e = edges(&mut m, 19, 0.01);
        assert!(e.len() > 10, "{e:?} at {}", m.place(0));
        let dt = e[5].0 - e[4].0;
        assert!((dt - 500e-6).abs() < 2e-6, "{dt}");
        // a rising edge on GP14 toggles GP18
        let before = m.drives()[18].out;
        m.set_inputs(1 << 14, 1 << 14, [0.0; 4], 3.3);
        m.run(m.time() + 1e-4, false);
        assert_ne!(m.drives()[18].out, before);
    }
}
