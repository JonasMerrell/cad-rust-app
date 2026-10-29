//! An ARM Cortex-M0+ core (ARMv6-M): the Thumb instruction set, the exception model (stacking,
//! priorities, PRIMASK, MSP/PSP), the NVIC, SysTick and the system control block. The chip
//! around it (memory and peripherals) is a `Bus`.

/// Memory and peripherals as the core sees them.
pub trait Bus {
    fn read32(&mut self, a: u32) -> u32;
    fn read16(&mut self, a: u32) -> u16;
    fn read8(&mut self, a: u32) -> u8;
    fn write32(&mut self, a: u32, v: u32);
    fn write16(&mut self, a: u32, v: u16);
    fn write8(&mut self, a: u32, v: u8);
    /// Interrupt lines (IRQ 0..31) that are asserted.
    fn irqs(&self) -> u32;
    /// Run a function the chip implements natively at `pc` (its ROM); the cycles it took, or None.
    fn native(&mut self, cpu: &mut Cpu, pc: u32) -> Option<u32>;
}

#[derive(Clone, Debug, PartialEq)]
pub enum Halt {
    /// A fault the firmware has no way out of (lockup).
    Fault(String),
    /// A BKPT instruction (the SDK's panic() ends in one).
    Breakpoint(u32),
}

const HARDFAULT: u32 = 3;
const SVCALL: u32 = 11;
const PENDSV: u32 = 14;
const SYSTICK: u32 = 15;

#[derive(Clone, Debug)]
pub struct Cpu {
    pub r: [u32; 16],
    pub n: bool,
    pub z: bool,
    pub c: bool,
    pub v: bool,
    /// Exception number being handled (0 in thread mode).
    pub ipsr: u32,
    pub primask: bool,
    /// CONTROL: bit 0 nPRIV, bit 1 SPSEL.
    pub control: u32,
    msp: u32,
    psp: u32,
    /// Exceptions pending and active, by number (bit n).
    pending: u64,
    active: u64,
    nvic_en: u32,
    nvic_prio: [u8; 32],
    shpr2: u32,
    shpr3: u32,
    pub vtor: u32,
    scr: u32,
    event: bool,
    /// Sleeping in WFI or WFE.
    pub sleeping: bool,
    sleep_wfe: bool,
    /// Set by SEV, for the chip to pass to the other core.
    pub sev_out: bool,
    st_csr: u32,
    st_rvr: u32,
    st_cvr: u32,
    pub halted: Option<Halt>,
    /// The last fault, for the user.
    pub fault: Option<String>,
    pub cycles: u64,
    pub instructions: u64,
}

impl Default for Cpu {
    fn default() -> Self {
        Cpu {
            r: [0; 16],
            n: false,
            z: false,
            c: false,
            v: false,
            ipsr: 0,
            primask: false,
            control: 0,
            msp: 0,
            psp: 0,
            pending: 0,
            active: 0,
            nvic_en: 0,
            nvic_prio: [0; 32],
            shpr2: 0,
            shpr3: 0,
            vtor: 0,
            scr: 0,
            event: false,
            sleeping: false,
            sleep_wfe: false,
            sev_out: false,
            st_csr: 0,
            st_rvr: 0,
            st_cvr: 0,
            halted: None,
            fault: None,
            cycles: 0,
            instructions: 0,
        }
    }
}

fn add_c(x: u32, y: u32, c: bool) -> (u32, bool, bool) {
    let r = x as u64 + y as u64 + c as u64;
    let res = r as u32;
    (res, r > 0xffff_ffff, ((x ^ res) & (y ^ res)) & 0x8000_0000 != 0)
}

fn sx(v: u32, bits: u32) -> u32 {
    let s = 32 - bits;
    (((v << s) as i32) >> s) as u32
}

impl Cpu {
    /// Start from a vector table: SP and PC from its first two words.
    pub fn reset(&mut self, bus: &mut impl Bus, vtor: u32) {
        *self = Cpu { vtor, ..Default::default() };
        self.msp = bus.read32(vtor) & !3;
        self.r[13] = self.msp;
        self.r[15] = bus.read32(vtor + 4) & !1;
    }

    /// Start at `pc` with the main stack at `sp` and the vector table at `vtor`, as a boot ROM
    /// hands over.
    pub fn start(&mut self, vtor: u32, sp: u32, pc: u32, lr: u32) {
        *self = Cpu { vtor, ..Default::default() };
        self.msp = sp & !3;
        self.r[13] = self.msp;
        self.r[14] = lr;
        self.r[15] = pc & !1;
    }

    fn set_nz(&mut self, v: u32) {
        self.n = v & 0x8000_0000 != 0;
        self.z = v == 0;
    }

    pub fn xpsr(&self) -> u32 {
        (self.n as u32) << 31 | (self.z as u32) << 30 | (self.c as u32) << 29 | (self.v as u32) << 28 | 1 << 24 | self.ipsr
    }

    fn set_apsr(&mut self, v: u32) {
        self.n = v & 1 << 31 != 0;
        self.z = v & 1 << 30 != 0;
        self.c = v & 1 << 29 != 0;
        self.v = v & 1 << 28 != 0;
    }

    /// SP of the current mode is in r13; the other one is banked.
    fn using_psp(&self) -> bool {
        self.ipsr == 0 && self.control & 2 != 0
    }

    fn bank_sp(&mut self) {
        if self.using_psp() {
            self.psp = self.r[13];
        } else {
            self.msp = self.r[13];
        }
    }

    fn unbank_sp(&mut self) {
        self.r[13] = if self.using_psp() { self.psp } else { self.msp };
    }

    fn prio(&self, n: u32) -> i32 {
        match n {
            0..=3 => -1,
            SVCALL => (self.shpr2 >> 30) as i32,
            PENDSV => ((self.shpr3 >> 22) & 3) as i32,
            SYSTICK => (self.shpr3 >> 30) as i32,
            n if n >= 16 => (self.nvic_prio[(n - 16) as usize] >> 6) as i32,
            _ => 4,
        }
    }

    fn exec_prio(&self) -> i32 {
        let mut p = 4;
        let mut a = self.active;
        while a != 0 {
            let n = a.trailing_zeros();
            p = p.min(self.prio(n));
            a &= a - 1;
        }
        if self.primask { p.min(0) } else { p }
    }

    /// The exception to take now, if any.
    fn next_exception(&mut self, lines: u32) -> Option<u32> {
        // asserted lines are pending, enabled or not
        self.pending |= (lines as u64) << 16;
        let mut cand = self.pending & !self.active;
        // disabled IRQs are not taken
        cand &= !((!self.nvic_en as u64) << 16);
        if cand == 0 {
            return None;
        }
        let ep = self.exec_prio();
        let mut best: Option<(i32, u32)> = None;
        while cand != 0 {
            let n = cand.trailing_zeros();
            cand &= cand - 1;
            let p = self.prio(n);
            if p < ep && best.is_none_or(|b| p < b.0) {
                best = Some((p, n));
            }
        }
        best.map(|b| b.1)
    }

    /// Whether a pending interrupt wakes the core from WFI/WFE (even with PRIMASK set).
    fn wake(&self, lines: u32) -> bool {
        let pend = (self.pending | ((lines as u64) << 16)) & !self.active;
        if pend & (0xfffc | ((self.nvic_en as u64) << 16)) != 0 {
            return true;
        }
        // SEVONPEND: any pending interrupt wakes WFE
        self.sleep_wfe && self.scr & 0x10 != 0 && pend >> 16 != 0
    }

    /// Sleep as WFE does with no event pending (for natively implemented ROM code).
    pub fn wfe(&mut self) {
        self.sleeping = true;
        self.sleep_wfe = true;
    }

    /// An event from outside (the other core's SEV): wakes a WFE.
    pub fn signal_event(&mut self) {
        self.event = true;
    }

    /// Whether the core would wake now.
    pub fn would_wake(&self, lines: u32) -> bool {
        self.wake(lines) || (self.sleep_wfe && self.event)
    }

    fn enter(&mut self, bus: &mut impl Bus, n: u32, ret: u32) {
        let xpsr = self.xpsr();
        let mut sp = self.r[13];
        let pad = sp & 4 != 0;
        if pad {
            sp -= 4;
        }
        sp -= 32;
        let frame = [self.r[0], self.r[1], self.r[2], self.r[3], self.r[12], self.r[14], ret, xpsr | (pad as u32) << 9];
        for (k, w) in frame.iter().enumerate() {
            bus.write32(sp + 4 * k as u32, *w);
        }
        self.r[13] = sp;
        let lr = if self.ipsr != 0 {
            0xffff_fff1
        } else if self.control & 2 != 0 {
            0xffff_fffd
        } else {
            0xffff_fff9
        };
        self.bank_sp();
        self.ipsr = n;
        self.unbank_sp();
        self.r[14] = lr;
        self.pending &= !(1u64 << n);
        self.active |= 1u64 << n;
        self.r[15] = bus.read32(self.vtor + 4 * n) & !1;
        self.sleeping = false;
        self.event = true;
    }

    fn exc_return(&mut self, bus: &mut impl Bus, exc: u32) {
        let n = self.ipsr;
        self.active &= !(1u64 << n);
        self.bank_sp();
        let to_psp = exc & 0xf == 0xd;
        let to_thread = exc & 0xf != 0x1;
        if to_thread {
            self.control = (self.control & !2) | if to_psp { 2 } else { 0 };
        }
        let mut sp = if to_psp { self.psp } else { self.msp };
        let mut f = [0u32; 8];
        for (k, w) in f.iter_mut().enumerate() {
            *w = bus.read32(sp + 4 * k as u32);
        }
        sp += 32;
        if f[7] & 1 << 9 != 0 {
            sp += 4;
        }
        if to_psp {
            self.psp = sp;
        } else {
            self.msp = sp;
        }
        self.r[0] = f[0];
        self.r[1] = f[1];
        self.r[2] = f[2];
        self.r[3] = f[3];
        self.r[12] = f[4];
        self.r[14] = f[5];
        self.r[15] = f[6] & !1;
        self.set_apsr(f[7]);
        self.ipsr = if to_thread { 0 } else { f[7] & 0x3f };
        self.unbank_sp();
        // returning wakes a WFE
        self.event = true;
    }

    /// A branch that may switch instruction set or return from an exception.
    fn bx(&mut self, bus: &mut impl Bus, t: u32) {
        if t >= 0xf000_0000 && self.ipsr != 0 {
            self.exc_return(bus, t);
        } else if t & 1 == 0 {
            self.fault(bus, format!("branch to ARM state at {t:#010x}"));
        } else {
            self.r[15] = t & !1;
        }
    }

    fn fault(&mut self, bus: &mut impl Bus, why: String) {
        let at = self.r[15];
        self.fault = Some(format!("{why} (pc {at:#010x})"));
        if self.active & (1 << HARDFAULT) != 0 || self.read_vec(bus, HARDFAULT) == 0 {
            self.halted = Some(Halt::Fault(self.fault.clone().unwrap()));
            return;
        }
        self.pending |= 1 << HARDFAULT;
    }

    fn read_vec(&self, bus: &mut impl Bus, n: u32) -> u32 {
        bus.read32(self.vtor + 4 * n)
    }

    // ------------------------------------------------------------ PPB (SCS) registers

    fn ppb_read(&mut self, a: u32) -> u32 {
        match a {
            0xe000_e010 => {
                let v = self.st_csr;
                self.st_csr &= !(1 << 16);
                v
            }
            0xe000_e014 => self.st_rvr,
            0xe000_e018 => self.st_cvr,
            0xe000_e01c => 0x4000_0000 | 9999,
            0xe000_e100 | 0xe000_e180 => self.nvic_en,
            0xe000_e200 | 0xe000_e280 => (self.pending >> 16) as u32,
            0xe000_e400..=0xe000_e41f => {
                let k = (a - 0xe000_e400) as usize;
                u32::from_le_bytes([self.nvic_prio[k], self.nvic_prio[k + 1], self.nvic_prio[k + 2], self.nvic_prio[k + 3]])
            }
            0xe000_ed00 => 0x410c_c601,
            0xe000_ed04 => {
                let pend = self.pending & !self.active;
                let vp = if pend != 0 { pend.trailing_zeros() } else { 0 };
                (self.pending >> PENDSV & 1) as u32 * (1 << 28) | (self.pending >> SYSTICK & 1) as u32 * (1 << 26) | (vp & 0x1ff) << 12 | self.ipsr
            }
            0xe000_ed08 => self.vtor,
            0xe000_ed0c => 0xfa05_0000,
            0xe000_ed10 => self.scr,
            0xe000_ed14 => 0x204,
            0xe000_ed1c => self.shpr2,
            0xe000_ed20 => self.shpr3,
            0xe000_ed24 => ((self.pending >> SVCALL & 1) as u32) << 15,
            _ => 0,
        }
    }

    fn ppb_write(&mut self, a: u32, v: u32) {
        match a {
            0xe000_e010 => self.st_csr = (self.st_csr & (1 << 16)) | (v & 7),
            0xe000_e014 => self.st_rvr = v & 0xff_ffff,
            0xe000_e018 => {
                self.st_cvr = 0;
                self.st_csr &= !(1 << 16);
            }
            0xe000_e100 => self.nvic_en |= v,
            0xe000_e180 => self.nvic_en &= !v,
            0xe000_e200 => self.pending |= (v as u64) << 16,
            0xe000_e280 => self.pending &= !((v as u64) << 16),
            0xe000_e400..=0xe000_e41f => {
                let k = (a - 0xe000_e400) as usize;
                for (i, b) in v.to_le_bytes().iter().enumerate() {
                    self.nvic_prio[k + i] = b & 0xc0;
                }
            }
            0xe000_ed04 => {
                if v & 1 << 28 != 0 {
                    self.pending |= 1 << PENDSV;
                }
                if v & 1 << 27 != 0 {
                    self.pending &= !(1 << PENDSV);
                }
                if v & 1 << 26 != 0 {
                    self.pending |= 1 << SYSTICK;
                }
                if v & 1 << 25 != 0 {
                    self.pending &= !(1 << SYSTICK);
                }
            }
            0xe000_ed08 => self.vtor = v & !0xff,
            0xe000_ed0c => {
                if v >> 16 == 0x05fa && v & 4 != 0 {
                    self.halted = Some(Halt::Fault("system reset requested".into()));
                }
            }
            0xe000_ed10 => self.scr = v & 0x16,
            0xe000_ed1c => self.shpr2 = v & 0xc000_0000,
            0xe000_ed20 => self.shpr3 = v & 0xc0c0_0000,
            _ => {}
        }
    }

    fn rd32(&mut self, bus: &mut impl Bus, a: u32) -> u32 {
        if a >= 0xe000_0000 { self.ppb_read(a & !3) } else { bus.read32(a) }
    }
    fn wr32(&mut self, bus: &mut impl Bus, a: u32, v: u32) {
        if a >= 0xe000_0000 { self.ppb_write(a & !3, v) } else { bus.write32(a, v) }
    }
    fn rd16(&mut self, bus: &mut impl Bus, a: u32) -> u32 {
        if a >= 0xe000_0000 { self.ppb_read(a & !3) >> ((a & 2) * 8) & 0xffff } else { bus.read16(a) as u32 }
    }
    fn rd8(&mut self, bus: &mut impl Bus, a: u32) -> u32 {
        if a >= 0xe000_0000 { self.ppb_read(a & !3) >> ((a & 3) * 8) & 0xff } else { bus.read8(a) as u32 }
    }
    fn wr16(&mut self, bus: &mut impl Bus, a: u32, v: u32) {
        if a >= 0xe000_0000 { self.ppb_write(a & !3, v & 0xffff) } else { bus.write16(a, v as u16) }
    }
    fn wr8(&mut self, bus: &mut impl Bus, a: u32, v: u32) {
        if a >= 0xe000_0000 {
            // byte writes to the priority registers
            if (0xe000_e400..=0xe000_e41f).contains(&a) {
                self.nvic_prio[(a - 0xe000_e400) as usize] = v as u8 & 0xc0;
            } else if (0xe000_ed1c..=0xe000_ed23).contains(&a) {
                let k = (a - 0xe000_ed1c) * 8;
                let mut w = (self.shpr2 as u64) | (self.shpr3 as u64) << 32;
                w = (w & !(0xffu64 << k)) | ((v as u64 & 0xff) << k);
                self.shpr2 = w as u32 & 0xc000_0000;
                self.shpr3 = (w >> 32) as u32 & 0xc0c0_0000;
            } else {
                self.ppb_write(a & !3, v & 0xff);
            }
        } else {
            bus.write8(a, v as u8)
        }
    }

    /// SysTick counts down with the core clock (or the 1 µs reference with CLKSOURCE 0).
    pub fn tick(&mut self, cycles: u32, ref_ticks: u32) {
        if self.st_csr & 1 == 0 {
            return;
        }
        let mut d = if self.st_csr & 4 != 0 { cycles } else { ref_ticks };
        while d > 0 {
            if self.st_cvr == 0 {
                self.st_cvr = self.st_rvr;
                if self.st_rvr == 0 {
                    return;
                }
                d -= 1;
                continue;
            }
            if d < self.st_cvr {
                self.st_cvr -= d;
                break;
            }
            d -= self.st_cvr;
            self.st_cvr = 0;
            self.st_csr |= 1 << 16;
            if self.st_csr & 2 != 0 {
                self.pending |= 1 << SYSTICK;
            }
        }
    }

    /// Ticks until SysTick next interrupts, and whether they are core cycles (else 1 µs ticks).
    pub fn systick_due(&self) -> Option<(u64, bool)> {
        if self.st_csr & 3 != 3 { None } else { Some((self.st_cvr.max(1) as u64, self.st_csr & 4 != 0)) }
    }

    /// Advance the clock by sleeping `cycles` (SysTick still counts).
    pub fn idle(&mut self, cycles: u64, ref_ticks: u32) {
        self.cycles += cycles;
        self.tick(cycles.min(u32::MAX as u64) as u32, ref_ticks);
    }

    /// Execute one instruction (or take an exception); the cycles used. 0 when halted or asleep.
    pub fn step(&mut self, bus: &mut impl Bus) -> u32 {
        if self.halted.is_some() {
            return 0;
        }
        let lines = bus.irqs();
        if let Some(n) = self.next_exception(lines) {
            let ret = self.r[15];
            self.enter(bus, n, ret);
            self.cycles += 15;
            return 15;
        }
        if self.sleeping {
            if self.wake(lines) {
                self.sleeping = false;
            } else if self.sleep_wfe && self.event {
                self.event = false;
                self.sleeping = false;
            } else {
                return 0;
            }
        }
        let pc = self.r[15];
        if pc < 0x4000 {
            if let Some(c) = bus.native(self, pc) {
                self.cycles += c as u64;
                self.instructions += 1;
                return c;
            }
        }
        let op = bus.read16(pc) as u32;
        self.r[15] = pc + 2;
        let c = self.exec(bus, op, pc);
        self.cycles += c as u64;
        self.instructions += 1;
        c
    }

    fn cond(&self, c: u32) -> bool {
        match c {
            0 => self.z,
            1 => !self.z,
            2 => self.c,
            3 => !self.c,
            4 => self.n,
            5 => !self.n,
            6 => self.v,
            7 => !self.v,
            8 => self.c && !self.z,
            9 => !self.c || self.z,
            10 => self.n == self.v,
            11 => self.n != self.v,
            12 => !self.z && self.n == self.v,
            13 => self.z || self.n != self.v,
            _ => true,
        }
    }

    /// Register value as an operand (PC reads as the instruction address + 4).
    fn reg(&self, k: usize, pc: u32) -> u32 {
        if k == 15 { pc + 4 } else { self.r[k] }
    }

    fn exec(&mut self, bus: &mut impl Bus, op: u32, pc: u32) -> u32 {
        let rd = (op & 7) as usize;
        let rn = ((op >> 3) & 7) as usize;
        let rm = ((op >> 6) & 7) as usize;
        match op >> 11 {
            // shifts by immediate
            0b00000 => {
                let imm = (op >> 6) & 31;
                let x = self.r[rn];
                let r = if imm == 0 {
                    x
                } else {
                    self.c = (x >> (32 - imm)) & 1 != 0;
                    x << imm
                };
                self.r[rd] = r;
                self.set_nz(r);
                1
            }
            0b00001 => {
                let imm = (op >> 6) & 31;
                let x = self.r[rn];
                let r = if imm == 0 {
                    self.c = x >> 31 != 0;
                    0
                } else {
                    self.c = (x >> (imm - 1)) & 1 != 0;
                    x >> imm
                };
                self.r[rd] = r;
                self.set_nz(r);
                1
            }
            0b00010 => {
                let imm = (op >> 6) & 31;
                let x = self.r[rn];
                let r = if imm == 0 {
                    self.c = x >> 31 != 0;
                    ((x as i32) >> 31) as u32
                } else {
                    self.c = (x >> (imm - 1)) & 1 != 0;
                    ((x as i32) >> imm) as u32
                };
                self.r[rd] = r;
                self.set_nz(r);
                1
            }
            0b00011 => {
                // add/sub register or 3-bit immediate
                let x = self.r[rn];
                let y = if op & 0x400 != 0 { (op >> 6) & 7 } else { self.r[rm] };
                let (r, c, v) = if op & 0x200 != 0 { add_c(x, !y, true) } else { add_c(x, y, false) };
                self.r[rd] = r;
                self.set_nz(r);
                self.c = c;
                self.v = v;
                1
            }
            0b00100..=0b00111 => {
                let d = ((op >> 8) & 7) as usize;
                let imm = op & 0xff;
                match (op >> 11) & 3 {
                    0 => {
                        self.r[d] = imm;
                        self.set_nz(imm);
                    }
                    1 => {
                        let (r, c, v) = add_c(self.r[d], !imm, true);
                        self.set_nz(r);
                        self.c = c;
                        self.v = v;
                    }
                    2 => {
                        let (r, c, v) = add_c(self.r[d], imm, false);
                        self.r[d] = r;
                        self.set_nz(r);
                        self.c = c;
                        self.v = v;
                    }
                    _ => {
                        let (r, c, v) = add_c(self.r[d], !imm, true);
                        self.r[d] = r;
                        self.set_nz(r);
                        self.c = c;
                        self.v = v;
                    }
                }
                1
            }
            0b01000 => {
                if op & 0x400 == 0 {
                    self.alu(op);
                    1
                } else {
                    self.hi(bus, op, pc)
                }
            }
            0b01001 => {
                let d = ((op >> 8) & 7) as usize;
                let a = ((pc + 4) & !3) + ((op & 0xff) << 2);
                self.r[d] = self.rd32(bus, a);
                2
            }
            0b01010 | 0b01011 => {
                let a = self.r[rn].wrapping_add(self.r[rm]);
                match (op >> 9) & 7 {
                    0 => self.wr32(bus, a, self.r[rd]),
                    1 => self.wr16(bus, a, self.r[rd]),
                    2 => self.wr8(bus, a, self.r[rd]),
                    3 => self.r[rd] = sx(self.rd8(bus, a), 8),
                    4 => self.r[rd] = self.rd32(bus, a),
                    5 => self.r[rd] = self.rd16(bus, a),
                    6 => self.r[rd] = self.rd8(bus, a),
                    _ => self.r[rd] = sx(self.rd16(bus, a), 16),
                }
                2
            }
            0b01100 => {
                let a = self.r[rn].wrapping_add(((op >> 6) & 31) << 2);
                self.wr32(bus, a, self.r[rd]);
                2
            }
            0b01101 => {
                let a = self.r[rn].wrapping_add(((op >> 6) & 31) << 2);
                self.r[rd] = self.rd32(bus, a);
                2
            }
            0b01110 => {
                let a = self.r[rn].wrapping_add((op >> 6) & 31);
                self.wr8(bus, a, self.r[rd]);
                2
            }
            0b01111 => {
                let a = self.r[rn].wrapping_add((op >> 6) & 31);
                self.r[rd] = self.rd8(bus, a);
                2
            }
            0b10000 => {
                let a = self.r[rn].wrapping_add(((op >> 6) & 31) << 1);
                self.wr16(bus, a, self.r[rd]);
                2
            }
            0b10001 => {
                let a = self.r[rn].wrapping_add(((op >> 6) & 31) << 1);
                self.r[rd] = self.rd16(bus, a);
                2
            }
            0b10010 => {
                let d = ((op >> 8) & 7) as usize;
                let a = self.r[13].wrapping_add((op & 0xff) << 2);
                self.wr32(bus, a, self.r[d]);
                2
            }
            0b10011 => {
                let d = ((op >> 8) & 7) as usize;
                let a = self.r[13].wrapping_add((op & 0xff) << 2);
                self.r[d] = self.rd32(bus, a);
                2
            }
            0b10100 => {
                let d = ((op >> 8) & 7) as usize;
                self.r[d] = ((pc + 4) & !3) + ((op & 0xff) << 2);
                1
            }
            0b10101 => {
                let d = ((op >> 8) & 7) as usize;
                self.r[d] = self.r[13].wrapping_add((op & 0xff) << 2);
                1
            }
            0b10110 | 0b10111 => self.misc(bus, op, pc),
            0b11000 => {
                // STM Rn!, {list}
                let n = ((op >> 8) & 7) as usize;
                let mut a = self.r[n];
                let mut k = 0;
                for i in 0..8 {
                    if op & (1 << i) != 0 {
                        self.wr32(bus, a, self.r[i]);
                        a = a.wrapping_add(4);
                        k += 1;
                    }
                }
                self.r[n] = a;
                1 + k
            }
            0b11001 => {
                let n = ((op >> 8) & 7) as usize;
                let mut a = self.r[n];
                let mut k = 0;
                for i in 0..8 {
                    if op & (1 << i) != 0 {
                        self.r[i] = self.rd32(bus, a);
                        a = a.wrapping_add(4);
                        k += 1;
                    }
                }
                if op & (1 << n) == 0 {
                    self.r[n] = a;
                }
                1 + k
            }
            0b11010 | 0b11011 => {
                let c = (op >> 8) & 15;
                match c {
                    14 => {
                        self.fault(bus, format!("undefined instruction {op:#06x}"));
                        1
                    }
                    15 => {
                        self.pending |= 1 << SVCALL;
                        1
                    }
                    _ => {
                        if self.cond(c) {
                            self.r[15] = (pc + 4).wrapping_add(sx(op & 0xff, 8) << 1);
                            2
                        } else {
                            1
                        }
                    }
                }
            }
            0b11100 => {
                self.r[15] = (pc + 4).wrapping_add(sx(op & 0x7ff, 11) << 1);
                2
            }
            _ => {
                let op2 = bus.read16(pc + 2) as u32;
                self.r[15] = pc + 4;
                self.wide(bus, op, op2, pc)
            }
        }
    }

    fn alu(&mut self, op: u32) {
        let d = (op & 7) as usize;
        let m = ((op >> 3) & 7) as usize;
        let x = self.r[d];
        let y = self.r[m];
        let shift_by = y & 0xff;
        match (op >> 6) & 15 {
            0 => {
                let r = x & y;
                self.r[d] = r;
                self.set_nz(r);
            }
            1 => {
                let r = x ^ y;
                self.r[d] = r;
                self.set_nz(r);
            }
            2 => {
                let n = shift_by;
                let r = if n == 0 {
                    x
                } else if n < 32 {
                    self.c = (x >> (32 - n)) & 1 != 0;
                    x << n
                } else {
                    self.c = n == 32 && x & 1 != 0;
                    0
                };
                self.r[d] = r;
                self.set_nz(r);
            }
            3 => {
                let n = shift_by;
                let r = if n == 0 {
                    x
                } else if n < 32 {
                    self.c = (x >> (n - 1)) & 1 != 0;
                    x >> n
                } else {
                    self.c = n == 32 && x >> 31 != 0;
                    0
                };
                self.r[d] = r;
                self.set_nz(r);
            }
            4 => {
                let n = shift_by;
                let r = if n == 0 {
                    x
                } else if n < 32 {
                    self.c = (x >> (n - 1)) & 1 != 0;
                    ((x as i32) >> n) as u32
                } else {
                    self.c = x >> 31 != 0;
                    ((x as i32) >> 31) as u32
                };
                self.r[d] = r;
                self.set_nz(r);
            }
            5 => {
                let (r, c, v) = add_c(x, y, self.c);
                self.r[d] = r;
                self.set_nz(r);
                self.c = c;
                self.v = v;
            }
            6 => {
                let (r, c, v) = add_c(x, !y, self.c);
                self.r[d] = r;
                self.set_nz(r);
                self.c = c;
                self.v = v;
            }
            7 => {
                let n = shift_by;
                let r = if n == 0 {
                    x
                } else {
                    let r = x.rotate_right(n & 31);
                    self.c = r >> 31 != 0;
                    r
                };
                self.r[d] = r;
                self.set_nz(r);
            }
            8 => self.set_nz(x & y),
            9 => {
                let (r, c, v) = add_c(!y, 0, true);
                self.r[d] = r;
                self.set_nz(r);
                self.c = c;
                self.v = v;
            }
            10 => {
                let (r, c, v) = add_c(x, !y, true);
                self.set_nz(r);
                self.c = c;
                self.v = v;
            }
            11 => {
                let (r, c, v) = add_c(x, y, false);
                self.set_nz(r);
                self.c = c;
                self.v = v;
            }
            12 => {
                let r = x | y;
                self.r[d] = r;
                self.set_nz(r);
            }
            13 => {
                let r = x.wrapping_mul(y);
                self.r[d] = r;
                self.set_nz(r);
            }
            14 => {
                let r = x & !y;
                self.r[d] = r;
                self.set_nz(r);
            }
            _ => {
                let r = !y;
                self.r[d] = r;
                self.set_nz(r);
            }
        }
    }

    /// High register operations and BX/BLX.
    fn hi(&mut self, bus: &mut impl Bus, op: u32, pc: u32) -> u32 {
        let d = ((op & 7) | ((op >> 4) & 8)) as usize;
        let m = ((op >> 3) & 15) as usize;
        match (op >> 8) & 3 {
            0 => {
                let r = self.reg(d, pc).wrapping_add(self.reg(m, pc));
                if d == 15 {
                    self.r[15] = r & !1;
                    2
                } else {
                    self.r[d] = r;
                    1
                }
            }
            1 => {
                let (r, c, v) = add_c(self.reg(d, pc), !self.reg(m, pc), true);
                self.set_nz(r);
                self.c = c;
                self.v = v;
                1
            }
            2 => {
                let r = self.reg(m, pc);
                if d == 15 {
                    self.r[15] = r & !1;
                    2
                } else {
                    self.r[d] = r;
                    1
                }
            }
            _ => {
                let t = self.reg(m, pc);
                if op & 0x80 != 0 {
                    self.r[14] = (pc + 2) | 1;
                }
                self.bx(bus, t);
                2
            }
        }
    }

    fn misc(&mut self, bus: &mut impl Bus, op: u32, pc: u32) -> u32 {
        match (op >> 8) & 15 {
            0 => {
                let imm = (op & 0x7f) << 2;
                self.r[13] = if op & 0x80 != 0 { self.r[13].wrapping_sub(imm) } else { self.r[13].wrapping_add(imm) };
                1
            }
            2 => {
                let d = (op & 7) as usize;
                let m = self.r[((op >> 3) & 7) as usize];
                self.r[d] = match (op >> 6) & 3 {
                    0 => sx(m & 0xffff, 16),
                    1 => sx(m & 0xff, 8),
                    2 => m & 0xffff,
                    _ => m & 0xff,
                };
                1
            }
            4 | 5 => {
                // PUSH {list, lr}
                let k = (op & 0x1ff).count_ones();
                let mut a = self.r[13].wrapping_sub(4 * k);
                self.r[13] = a;
                for i in 0..8 {
                    if op & (1 << i) != 0 {
                        self.wr32(bus, a, self.r[i]);
                        a += 4;
                    }
                }
                if op & 0x100 != 0 {
                    self.wr32(bus, a, self.r[14]);
                }
                1 + k
            }
            6 => {
                // CPS
                if op & 0xffef == 0xb662 {
                    self.primask = op & 0x10 != 0;
                }
                1
            }
            10 => {
                let d = (op & 7) as usize;
                let m = self.r[((op >> 3) & 7) as usize];
                self.r[d] = match (op >> 6) & 3 {
                    0 => m.swap_bytes(),
                    1 => ((m & 0x00ff_00ff) << 8) | ((m >> 8) & 0x00ff_00ff),
                    3 => sx(((m & 0xff) << 8) | ((m >> 8) & 0xff), 16),
                    _ => {
                        self.fault(bus, format!("undefined instruction {op:#06x}"));
                        m
                    }
                };
                1
            }
            12 | 13 => {
                let mut a = self.r[13];
                let mut k = 0;
                for i in 0..8 {
                    if op & (1 << i) != 0 {
                        self.r[i] = self.rd32(bus, a);
                        a = a.wrapping_add(4);
                        k += 1;
                    }
                }
                if op & 0x100 != 0 {
                    let t = self.rd32(bus, a);
                    self.r[13] = a.wrapping_add(4);
                    self.bx(bus, t);
                    return 4 + k;
                }
                self.r[13] = a;
                1 + k
            }
            14 => {
                self.halted = Some(Halt::Breakpoint(pc));
                1
            }
            15 => {
                match (op >> 4) & 15 {
                    2 => {
                        // WFE
                        if self.event {
                            self.event = false;
                        } else {
                            self.sleeping = true;
                            self.sleep_wfe = true;
                        }
                    }
                    3 => {
                        self.sleeping = true;
                        self.sleep_wfe = false;
                    }
                    4 => {
                        self.sev_out = true;
                        self.event = true;
                    }
                    _ => {}
                }
                1
            }
            _ => {
                self.fault(bus, format!("undefined instruction {op:#06x}"));
                1
            }
        }
    }

    fn wide(&mut self, bus: &mut impl Bus, op: u32, op2: u32, pc: u32) -> u32 {
        if op & 0xf800 == 0xf000 && op2 & 0xd000 == 0xd000 {
            // BL
            let s = (op >> 10) & 1;
            let j1 = (op2 >> 13) & 1;
            let j2 = (op2 >> 11) & 1;
            let i1 = !(j1 ^ s) & 1;
            let i2 = !(j2 ^ s) & 1;
            let imm = (s << 24) | (i1 << 23) | (i2 << 22) | ((op & 0x3ff) << 12) | ((op2 & 0x7ff) << 1);
            self.r[14] = (pc + 4) | 1;
            self.r[15] = (pc + 4).wrapping_add(sx(imm, 25));
            return 3;
        }
        if op & 0xfff0 == 0xf380 && op2 & 0xff00 == 0x8800 {
            // MSR
            let v = self.r[(op & 15) as usize];
            match op2 & 0xff {
                0..=3 => self.set_apsr(v),
                8 => {
                    if self.using_psp() {
                        self.msp = v & !3;
                    } else {
                        self.r[13] = v & !3;
                    }
                }
                9 => {
                    if self.using_psp() {
                        self.r[13] = v & !3;
                    } else {
                        self.psp = v & !3;
                    }
                }
                16 => self.primask = v & 1 != 0,
                20 => {
                    if self.ipsr == 0 {
                        self.bank_sp();
                        self.control = v & 3;
                        self.unbank_sp();
                    } else {
                        self.control = (self.control & 2) | (v & 1);
                    }
                }
                _ => {}
            }
            return 4;
        }
        if op == 0xf3ef && op2 & 0xf000 == 0x8000 {
            // MRS
            let d = ((op2 >> 8) & 15) as usize;
            self.r[d] = match op2 & 0xff {
                0 | 2 => self.xpsr() & 0xf000_0000,
                1 | 3 => self.xpsr() & 0xf000_003f,
                5 | 7 => self.ipsr,
                6 => 0,
                8 => {
                    if self.using_psp() {
                        self.msp
                    } else {
                        self.r[13]
                    }
                }
                9 => {
                    if self.using_psp() {
                        self.r[13]
                    } else {
                        self.psp
                    }
                }
                16 => self.primask as u32,
                20 => self.control,
                _ => 0,
            };
            return 4;
        }
        if op == 0xf3bf && op2 & 0xff00 == 0x8f00 {
            // DSB, DMB, ISB
            return 3;
        }
        self.fault(bus, format!("undefined instruction {op:#06x} {op2:#06x}"));
        1
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// 64 KB of RAM at 0 for testing the core.
    pub struct Ram(pub Vec<u8>);

    impl Bus for Ram {
        fn read32(&mut self, a: u32) -> u32 {
            let a = a as usize;
            u32::from_le_bytes(self.0[a..a + 4].try_into().unwrap())
        }
        fn read16(&mut self, a: u32) -> u16 {
            let a = a as usize;
            u16::from_le_bytes(self.0[a..a + 2].try_into().unwrap())
        }
        fn read8(&mut self, a: u32) -> u8 {
            self.0[a as usize]
        }
        fn write32(&mut self, a: u32, v: u32) {
            let a = a as usize;
            self.0[a..a + 4].copy_from_slice(&v.to_le_bytes());
        }
        fn write16(&mut self, a: u32, v: u16) {
            let a = a as usize;
            self.0[a..a + 2].copy_from_slice(&v.to_le_bytes());
        }
        fn write8(&mut self, a: u32, v: u8) {
            self.0[a as usize] = v;
        }
        fn irqs(&self) -> u32 {
            0
        }
        fn native(&mut self, _: &mut Cpu, _: u32) -> Option<u32> {
            None
        }
    }

    /// Run Thumb code placed at 0x100 with the stack at 0x8000 until it reaches a BKPT.
    pub fn run(code: &[u16]) -> (Cpu, Ram) {
        let mut ram = Ram(vec![0; 0x10000]);
        ram.write32(0, 0x8000);
        ram.write32(4, 0x101);
        for (k, h) in code.iter().enumerate() {
            ram.write16(0x100 + 2 * k as u32, *h);
        }
        let mut cpu = Cpu::default();
        cpu.reset(&mut ram, 0);
        for _ in 0..100_000 {
            cpu.step(&mut ram);
            if cpu.halted.is_some() {
                break;
            }
        }
        (cpu, ram)
    }

    #[test]
    fn arithmetic_and_flags() {
        // movs r0, #200; movs r1, #100; subs r2, r0, r1; adds r3, r0, r1; cmp r1, r0; bkpt
        let (c, _) = run(&[0x20c8, 0x2164, 0x1a42, 0x1843, 0x4281, 0xbe00]);
        assert_eq!(c.r[2], 100);
        assert_eq!(c.r[3], 300);
        assert!(c.n && !c.z && !c.c);
        assert_eq!(c.halted, Some(Halt::Breakpoint(0x10a)));
    }

    #[test]
    fn loop_and_memory() {
        // sum 1..10 into r1, store at [r2] and load back
        //   movs r0, #10; movs r1, #0
        // l: adds r1, r1, r0; subs r0, #1; bne l
        //   movs r2, #0x80; lsls r2, r2, #4  (0x800)
        //   str r1, [r2]; ldr r3, [r2]; push {r1, r3}; pop {r4, r5}; bkpt
        let (c, mut ram) = run(&[0x200a, 0x2100, 0x1809, 0x3801, 0xd1fc, 0x2280, 0x0112, 0x6011, 0x6813, 0xb40a, 0xbc30, 0xbe00]);
        assert_eq!(c.r[1], 55);
        assert_eq!(ram.read32(0x800), 55);
        assert_eq!((c.r[3], c.r[4], c.r[5]), (55, 55, 55));
        assert_eq!(c.r[13], 0x8000);
    }

    #[test]
    fn bl_and_return() {
        // bl f; bkpt; f: movs r0, #7; bx lr
        let (c, _) = run(&[0xf000, 0xf801, 0xbe00, 0x2007, 0x4770]);
        assert_eq!(c.r[0], 7);
        assert_eq!(c.halted, Some(Halt::Breakpoint(0x104)));
    }

    #[test]
    fn exception_entry_and_return() {
        // SysTick handler at vector 15 increments r4 (in RAM it can't keep across stacking? r4 is not stacked)
        let mut ram = Ram(vec![0; 0x10000]);
        ram.write32(0, 0x8000);
        ram.write32(4, 0x101);
        ram.write32(15 * 4, 0x201);
        // main: movs r0, #1; b . (wait)
        ram.write16(0x100, 0x2001);
        ram.write16(0x102, 0xe7fe);
        // handler: adds r4, #1; bx lr
        ram.write16(0x200, 0x3401);
        ram.write16(0x202, 0x4770);
        let mut cpu = Cpu::default();
        cpu.reset(&mut ram, 0);
        cpu.ppb_write(0xe000_ed04, 1 << 26);
        for _ in 0..20 {
            cpu.step(&mut ram);
        }
        assert_eq!(cpu.r[4], 1);
        assert_eq!(cpu.r[0], 1);
        assert_eq!(cpu.ipsr, 0);
        assert_eq!(cpu.r[13], 0x8000);
    }
}
