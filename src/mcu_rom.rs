//! The RP2040 boot ROM, re-implemented: the tables at the start of the ROM that the Pico SDK
//! reads (version, function and data lookup) and the functions they point to, run natively. The
//! real ROM is not used: its floating-point library is not under a licence this program can ship.
//!
//! Every function lives at a fixed address in the ROM; when a core reaches one, `call` does the
//! work on its registers and returns to the caller.

use crate::mcu_cm0::{Bus, Cpu};

/// What a native ROM function needs from the chip beyond memory.
pub trait Host: Bus {
    fn flash(&mut self) -> &mut Vec<u8>;
    /// The calling core parks itself in the ROM (core 1's wait for a launch).
    fn park(&mut self);
    fn note(&mut self, s: String);
    fn core(&self) -> usize;
}

pub const SIZE: usize = 16 << 10;
const FUNCS: u32 = 0x100;
const DATA: u32 = 0x200;
const LOOKUP: u32 = 0x1000;
const FN_BASE: u32 = 0x1100;
const SF: u32 = 0x700;
const SD: u32 = 0x800;
const SF_FN: u32 = 0x2000;
const SD_FN: u32 = 0x2100;
/// Where core 1 returns to if its entry function returns: it sleeps there.
pub const DEAD: u32 = 0x1ffc;
/// Core 1's launch wait (`wait_for_vector`).
pub const WAIT_VECTOR: u32 = 0x1ff8;
/// The reset vector: boot from flash as the ROM does once boot2 has set up XIP.
pub const BOOT: u32 = 0x1ff4;

fn code(s: &str) -> u16 {
    let b = s.as_bytes();
    b[0] as u16 | (b[1] as u16) << 8
}

/// The functions in the lookup table, by code, in address order from `FN_BASE`.
const FUNC_CODES: &[&str] = &["P3", "R3", "L3", "T3", "MS", "S4", "MC", "C4", "IF", "EX", "RE", "RP", "FC", "CX", "UB", "DT", "DE", "WV"];

/// The ROM image: header, lookup tables, data and a Thumb `bx lr` at every native address (so a
/// disassembler or a stray read sees something sensible).
pub fn image() -> Vec<u8> {
    let mut m = vec![0u8; SIZE];
    let w16 = |m: &mut Vec<u8>, a: u32, v: u16| m[a as usize..a as usize + 2].copy_from_slice(&v.to_le_bytes());
    let w32 = |m: &mut Vec<u8>, a: u32, v: u32| m[a as usize..a as usize + 4].copy_from_slice(&v.to_le_bytes());
    w32(&mut m, 0, 0x2004_2000);
    w32(&mut m, 4, BOOT | 1);
    m[0x10] = b'M';
    m[0x11] = b'u';
    m[0x12] = 1;
    // version 3 (RP2040 B2)
    m[0x13] = 3;
    w16(&mut m, 0x14, FUNCS as u16);
    w16(&mut m, 0x16, DATA as u16);
    w16(&mut m, 0x18, (LOOKUP | 1) as u16);
    let mut a = FUNCS;
    for (k, c) in FUNC_CODES.iter().enumerate() {
        let addr = if *c == "WV" { WAIT_VECTOR } else { FN_BASE + 4 * k as u32 };
        w16(&mut m, a, code(c));
        w16(&mut m, a + 2, (addr | 1) as u16);
        a += 4;
    }
    let copyright = b"(C) 2020 Raspberry Pi Trading Ltd (re-implemented)\0";
    let entries: [(&str, u32); 4] = [("SF", SF), ("SD", SD), ("CR", 0x600), ("GR", 0x660)];
    let mut a = DATA;
    for (c, addr) in entries {
        w16(&mut m, a, code(c));
        w16(&mut m, a + 2, addr as u16);
        a += 4;
    }
    m[0x600..0x600 + copyright.len()].copy_from_slice(copyright);
    w32(&mut m, 0x660, 0x7a9c_b8f3);
    // the float and double tables; the byte two before each is its size in words
    m[SF as usize - 2] = 32;
    m[SD as usize - 2] = 32;
    for k in 0..32 {
        w32(&mut m, SF + 4 * k, (SF_FN + 4 * k) | 1);
        w32(&mut m, SD + 4 * k, (SD_FN + 4 * k) | 1);
    }
    let mut a = LOOKUP;
    while a < 0x3000 {
        w16(&mut m, a, 0x4770);
        a += 2;
    }
    m
}

fn ftz(x: f32) -> f32 {
    if x.is_subnormal() { 0f32.copysign(x) } else { x }
}

fn dtz(x: f64) -> f64 {
    if x.is_subnormal() { 0f64.copysign(x) } else { x }
}

fn sgn(a: f64, b: f64) -> u32 {
    if a < b {
        -1i32 as u32
    } else if a > b {
        1
    } else {
        0
    }
}

/// x · 2ⁿ rounded towards −∞ and clamped, as the ROM's conversions do.
fn to_i32(x: f64, n: i32) -> u32 {
    (x * 2f64.powi(n)).floor() as i32 as u32
}
fn to_u32(x: f64, n: i32) -> u32 {
    (x * 2f64.powi(n)).floor() as u32
}
fn to_i64(x: f64, n: i32) -> u64 {
    (x * 2f64.powi(n)).floor() as i64 as u64
}
fn to_u64(x: f64, n: i32) -> u64 {
    (x * 2f64.powi(n)).floor() as u64
}

fn ret(cpu: &mut Cpu) {
    cpu.r[15] = cpu.r[14] & !1;
}

fn set64(cpu: &mut Cpu, k: usize, v: u64) {
    cpu.r[k] = v as u32;
    cpu.r[k + 1] = (v >> 32) as u32;
}

fn get64(cpu: &Cpu, k: usize) -> u64 {
    cpu.r[k] as u64 | (cpu.r[k + 1] as u64) << 32
}

/// Run the ROM function at `pc`: the cycles it takes, or None when nothing lives there.
pub fn call(h: &mut impl Host, cpu: &mut Cpu, pc: u32) -> Option<u32> {
    let r = cpu.r;
    let cycles = match pc {
        LOOKUP => {
            let (mut t, c) = (r[0], r[1] & 0xffff);
            cpu.r[0] = 0;
            for _ in 0..256 {
                let e = h.read16(t) as u32;
                if e == 0 {
                    break;
                }
                if e == c {
                    cpu.r[0] = h.read16(t + 2) as u32;
                    break;
                }
                t += 4;
            }
            40
        }
        BOOT => {
            if h.core() == 1 {
                h.park();
            } else {
                let (sp, pc) = (h.read32(0x1000_0100), h.read32(0x1000_0104));
                cpu.start(0x1000_0100, sp, pc, DEAD | 1);
            }
            return Some(100);
        }
        WAIT_VECTOR => {
            h.park();
            return Some(4);
        }
        DEAD => {
            cpu.wfe();
            return Some(1);
        }
        a if (FN_BASE..FN_BASE + 4 * FUNC_CODES.len() as u32).contains(&a) => {
            let name = FUNC_CODES[((a - FN_BASE) / 4) as usize];
            match name {
                "P3" => {
                    cpu.r[0] = r[0].count_ones();
                    10
                }
                "R3" => {
                    cpu.r[0] = r[0].reverse_bits();
                    10
                }
                "L3" => {
                    cpu.r[0] = r[0].leading_zeros();
                    10
                }
                "T3" => {
                    cpu.r[0] = r[0].trailing_zeros();
                    10
                }
                "MS" | "S4" => {
                    let (p, v, n) = (r[0], r[1] as u8, r[2]);
                    for i in 0..n {
                        h.write8(p + i, v);
                    }
                    20 + n / 2
                }
                "MC" | "C4" => {
                    let (d, s, n) = (r[0], r[1], r[2]);
                    for i in 0..n {
                        let b = h.read8(s + i);
                        h.write8(d + i, b);
                    }
                    20 + n
                }
                "IF" | "EX" | "FC" | "CX" | "DT" | "DE" => 100,
                "RE" => {
                    let (off, n) = (r[0] as usize, r[1] as usize);
                    let f = h.flash();
                    let end = (off + n).min(f.len());
                    if off < end {
                        f[off..end].fill(0xff);
                    }
                    // about 45 ms a 4 KB sector at 125 MHz
                    (n.div_ceil(4096).max(1) as u64 * 5_600_000).min(u32::MAX as u64) as u32
                }
                "RP" => {
                    let (off, src, n) = (r[0] as usize, r[1], r[2] as usize);
                    let data: Vec<u8> = (0..n as u32).map(|i| h.read8(src + i)).collect();
                    let f = h.flash();
                    for (i, b) in data.iter().enumerate() {
                        if let Some(x) = f.get_mut(off + i) {
                            // NOR flash: programming only clears bits
                            *x &= b;
                        }
                    }
                    (n.div_ceil(256).max(1) as u64 * 100_000).min(u32::MAX as u64) as u32
                }
                "UB" => {
                    h.note("the firmware rebooted into USB boot mode (BOOTSEL); it stops here".into());
                    cpu.wfe();
                    return Some(1);
                }
                _ => 10,
            }
        }
        a if (SF_FN..SF_FN + 0x80).contains(&a) => float(cpu, a - SF_FN),
        a if (SD_FN..SD_FN + 0x80).contains(&a) => double(cpu, a - SD_FN),
        _ => return None,
    };
    ret(cpu);
    Some(cycles)
}

fn float(cpu: &mut Cpu, off: u32) -> u32 {
    let r = cpu.r;
    let a = ftz(f32::from_bits(r[0]));
    let b = ftz(f32::from_bits(r[1]));
    let f = |x: f32| ftz(x).to_bits();
    let n1 = r[1] as i32;
    let n2 = r[2] as i32;
    let (v, cycles) = match off {
        0x00 => (f(a + b), 50),
        0x04 => (f(a - b), 50),
        0x08 => (f(a * b), 50),
        0x0c => (f(a / b), 70),
        0x10 | 0x54 => (sgn(a as f64, b as f64), 20),
        0x14 => {
            let s = sgn(a as f64, b as f64) as i32;
            cpu.z = s == 0;
            cpu.n = s < 0;
            cpu.c = s >= 0;
            cpu.v = false;
            (r[0], 20)
        }
        0x18 => (f(a.sqrt()), 60),
        0x1c => (to_i32(a as f64, 0), 30),
        0x20 => (to_i32(a as f64, n1), 30),
        0x24 => (to_u32(a as f64, 0), 30),
        0x28 => (to_u32(a as f64, n1), 30),
        0x2c => (f(r[0] as i32 as f32), 30),
        0x30 => (f((r[0] as i32 as f64 * 2f64.powi(-n1)) as f32), 30),
        0x34 => (f(r[0] as f32), 30),
        0x38 => (f((r[0] as f64 * 2f64.powi(-n1)) as f32), 30),
        0x3c => (f(a.cos()), 150),
        0x40 | 0x48 => {
            cpu.r[1] = f(a.cos());
            (f(a.sin()), 170)
        }
        0x44 => (f(a.tan()), 170),
        0x4c => (f(a.exp()), 120),
        0x50 => (f(a.ln()), 120),
        0x58 => (f(a.atan2(b)), 170),
        0x5c => (f(get64(cpu, 0) as i64 as f32), 40),
        0x60 => (f((get64(cpu, 0) as i64 as f64 * 2f64.powi(-n2)) as f32), 40),
        0x64 => (f(get64(cpu, 0) as f32), 40),
        0x68 => (f((get64(cpu, 0) as f64 * 2f64.powi(-n2)) as f32), 40),
        0x6c..=0x78 => {
            let n = if off == 0x70 || off == 0x78 { n1 } else { 0 };
            let x = if off <= 0x70 { to_i64(a as f64, n) } else { to_u64(a as f64, n) };
            set64(cpu, 0, x);
            return 40;
        }
        0x7c => {
            set64(cpu, 0, (a as f64).to_bits());
            return 20;
        }
        _ => (0, 10),
    };
    cpu.r[0] = v;
    cycles
}

fn double(cpu: &mut Cpu, off: u32) -> u32 {
    let r = cpu.r;
    let a = dtz(f64::from_bits(get64(cpu, 0)));
    let b = dtz(f64::from_bits(get64(cpu, 2)));
    let n2 = r[2] as i32;
    let n1 = r[1] as i32;
    let d = |cpu: &mut Cpu, x: f64| set64(cpu, 0, dtz(x).to_bits());
    let one = |cpu: &mut Cpu, x: u32| cpu.r[0] = x;
    match off {
        0x00 => d(cpu, a + b),
        0x04 => d(cpu, a - b),
        0x08 => d(cpu, a * b),
        0x0c => d(cpu, a / b),
        0x10 | 0x54 => one(cpu, sgn(a, b)),
        0x14 => {
            let s = sgn(a, b) as i32;
            cpu.z = s == 0;
            cpu.n = s < 0;
            cpu.c = s >= 0;
            cpu.v = false;
        }
        0x18 => d(cpu, a.sqrt()),
        0x1c => one(cpu, to_i32(a, 0)),
        0x20 => one(cpu, to_i32(a, n2)),
        0x24 => one(cpu, to_u32(a, 0)),
        0x28 => one(cpu, to_u32(a, n2)),
        0x2c => d(cpu, r[0] as i32 as f64),
        0x30 => d(cpu, r[0] as i32 as f64 * 2f64.powi(-n1)),
        0x34 => d(cpu, r[0] as f64),
        0x38 => d(cpu, r[0] as f64 * 2f64.powi(-n1)),
        0x3c => d(cpu, a.cos()),
        0x40 => d(cpu, a.sin()),
        0x44 => d(cpu, a.tan()),
        0x48 => {
            set64(cpu, 2, dtz(a.cos()).to_bits());
            d(cpu, a.sin());
        }
        0x4c => d(cpu, a.exp()),
        0x50 => d(cpu, a.ln()),
        0x58 => d(cpu, a.atan2(b)),
        0x5c => d(cpu, get64(cpu, 0) as i64 as f64),
        0x60 => d(cpu, get64(cpu, 0) as i64 as f64 * 2f64.powi(-n2)),
        0x64 => d(cpu, get64(cpu, 0) as f64),
        0x68 => d(cpu, get64(cpu, 0) as f64 * 2f64.powi(-n2)),
        0x6c => set64(cpu, 0, to_i64(a, 0)),
        0x70 => set64(cpu, 0, to_i64(a, n2)),
        0x74 => set64(cpu, 0, to_u64(a, 0)),
        0x78 => set64(cpu, 0, to_u64(a, n2)),
        0x7c => one(cpu, ftz(a as f32).to_bits()),
        _ => {}
    }
    match off {
        0x0c | 0x18 => 200,
        0x3c..=0x50 | 0x58 => 400,
        0x00..=0x08 => 90,
        _ => 50,
    }
}
