//! Простой процессор: точный порт `SimpleCPUVM.execute_current_instruction()`
//! из site/scpu.min.js.

use crate::program::{MEM_SIZE, Program};

pub const RC: usize = 4;
pub const RF: usize = 5;
pub const REG_NAMES: [&str; 6] = ["R0", "R1", "R2", "R3", "RC", "RF"];

pub const FLAG_Z: u8 = 1;
pub const FLAG_S: u8 = 2;
pub const FLAG_C: u8 = 4;

/// Длина инструкции в байтах по старшему полубайту.
pub fn instr_len(byte: u8) -> usize {
    match byte >> 4 {
        1 | 2 | 3 | 9 | 10 | 11 | 12 => 2,
        _ => 1,
    }
}

#[derive(Debug)]
pub struct VmError {
    /// Адрес инструкции, на которой произошла ошибка.
    pub addr: u8,
    pub msg: String,
}

#[derive(Debug)]
pub struct StepInfo {
    /// RC не изменился — программа «стоит» на `HLT: rc <- @HLT`.
    pub halted: bool,
    pub changed_mem: Option<u8>,
}

pub struct Vm {
    pub regs: [u8; 6],
    pub mem: [u8; MEM_SIZE],
    initial_mem: [u8; MEM_SIZE],
    rng: u64,
}

impl Vm {
    pub fn new(program: &Program) -> Vm {
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x9E3779B97F4A7C15);
        Vm { regs: [0; 6], mem: program.mem, initial_mem: program.mem, rng: seed | 1 }
    }

    /// Запомнить текущую память как начальную (после ввода данных), чтобы `reset` её восстанавливал.
    pub fn commit_initial(&mut self) {
        self.initial_mem = self.mem;
    }

    pub fn reset(&mut self) {
        self.regs = [0; 6];
        self.mem = self.initial_mem;
    }

    pub fn rc(&self) -> u8 {
        self.regs[RC]
    }

    fn fetch_next(&mut self) -> u8 {
        self.regs[RC] = self.regs[RC].wrapping_add(1);
        self.mem[self.regs[RC] as usize]
    }

    fn set_flags(&mut self, res: u32) {
        let mut f = 0;
        if res & 0xFF == 0 {
            f |= FLAG_Z;
        }
        if res & 0x80 != 0 {
            f |= FLAG_S;
        }
        if res & 0x100 != 0 {
            f |= FLAG_C;
        }
        self.regs[RF] = f;
    }

    fn random(&mut self) -> u8 {
        // xorshift64
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        (self.rng >> 24) as u8
    }

    pub fn step(&mut self) -> Result<StepInfo, VmError> {
        let old_mem = self.mem;
        let start = self.regs[RC];

        let cmd = self.mem[start as usize];
        let op = cmd >> 4;
        let dd = ((cmd >> 2) & 3) as usize;
        let ss = (cmd & 3) as usize;
        let mut jumped = false;

        match op {
            0x0 => self.regs[dd] = self.regs[ss],
            0x1 => {
                let a = self.fetch_next();
                self.regs[dd] = self.mem[a as usize];
            }
            0x2 => {
                let a = self.fetch_next();
                self.mem[a as usize] = self.regs[ss];
            }
            0x3 => self.regs[dd] = self.fetch_next(),
            0x4..=0x7 => {
                let d = self.regs[dd] as u32;
                let s = self.regs[ss] as u32;
                let res = match op {
                    0x4 => d + s,
                    0x5 => d + (s.wrapping_neg() & 0xFF),
                    0x6 => d * s,
                    _ => {
                        if s == 0 {
                            return Err(VmError { addr: start, msg: "Деление на ноль!".into() });
                        }
                        d / s
                    }
                };
                self.set_flags(res);
                self.regs[dd] = res as u8;
            }
            0x8 => {
                let d = self.regs[dd] as u32;
                let s = self.regs[ss] as u32;
                self.set_flags(d + (s.wrapping_neg() & 0xFF));
            }
            0x9..=0xC => {
                let target = self.fetch_next();
                let take = match op {
                    0x9 => self.regs[RF] & FLAG_C != 0,
                    0xA => self.regs[RF] & FLAG_S != 0,
                    0xB => self.regs[RF] & FLAG_Z != 0,
                    _ => true,
                };
                if take {
                    self.regs[RC] = target;
                    jumped = true;
                }
            }
            0xD => self.regs[dd] = self.random(),
            0xE => {
                let res = self.regs[ss] as u32 + 1;
                self.set_flags(res);
                self.regs[dd] = res as u8;
            }
            _ => {
                return Err(VmError {
                    addr: start,
                    msg: format!("Недопустимый код операции: '{op:x}'"),
                });
            }
        }
        if !jumped {
            self.regs[RC] = self.regs[RC].wrapping_add(1);
        }

        Ok(StepInfo {
            halted: self.regs[RC] == start,
            changed_mem: (0..MEM_SIZE).find(|&i| self.mem[i] != old_mem[i]).map(|i| i as u8),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vm(bin: &str) -> Vm {
        Vm::new(&Program::parse(bin).unwrap())
    }

    fn run(vm: &mut Vm) {
        for _ in 0..1000 {
            if vm.step().unwrap().halted {
                return;
            }
        }
        panic!("не остановилась");
    }

    #[test]
    fn load_store_add() {
        // r0 <- 5; r1 <- 0xFE (=-2); r0 <- r0 + r1; @0xF2 <- r0; HLT
        let mut m = vm("3005:1;34fe:2;41:3;20f2:4;c007:5;");
        run(&mut m);
        assert_eq!(m.mem[0xF2], 3);
        assert_eq!(m.regs[RF], FLAG_C); // 5 + 254 = 259 -> перенос
        assert_eq!(m.rc(), 7);
    }

    #[test]
    fn sub_and_flags() {
        // r0 <- 3; r1 <- 5; r0 <- r0 - r1  => -2, S=1, C=0
        let mut m = vm("3003:1;3405:2;51:3;");
        for _ in 0..3 {
            m.step().unwrap();
        }
        assert_eq!(m.regs[0], 0xFE);
        assert_eq!(m.regs[RF], FLAG_S);
        // 5 - 5 = 0: Z и C (как в JS: 5 + 251 = 256)
        let mut m = vm("3005:1;3405:2;51:3;");
        for _ in 0..3 {
            m.step().unwrap();
        }
        assert_eq!(m.regs[0], 0);
        assert_eq!(m.regs[RF], FLAG_Z | FLAG_C);
    }

    #[test]
    fn compare_does_not_store() {
        let mut m = vm("3007:1;3407:2;81:3;");
        for _ in 0..3 {
            m.step().unwrap();
        }
        assert_eq!(m.regs[0], 7);
        assert_ne!(m.regs[RF] & FLAG_Z, 0);
    }

    #[test]
    fn conditional_jump() {
        // 0: r0 <- 0   2: r1 <- 0   4: r0 ~ r1   5: jz 9   7: r2 <- 1(не выполняется)  9: HLT
        let mut m = vm("3000:1;3400:2;81:3;b009:4;3801:5;c009:6;");
        run(&mut m);
        assert_eq!(m.regs[2], 0);
        assert_eq!(m.rc(), 9);
    }

    #[test]
    fn not_taken_jump_skips_address() {
        // флаги 0 -> jz не срабатывает, RC = 2
        let mut m = vm("b0ff:1;");
        m.step().unwrap();
        assert_eq!(m.rc(), 2);
    }

    #[test]
    fn mul_div_inc() {
        // r0 <- 20; r1 <- 20; r0 <- r0*r1 (400 = 0x190 -> 0x90, C и S); r2 <- r1++; r1 <- r1 / r2? 20/21 = 0
        let mut m = vm("3014:1;3414:2;61:3;e9:4;76:5;");
        for _ in 0..3 {
            m.step().unwrap();
        }
        assert_eq!(m.regs[0], 144);
        assert_eq!(m.regs[RF], FLAG_C | FLAG_S);
        m.step().unwrap();
        assert_eq!(m.regs[2], 21);
        m.step().unwrap();
        assert_eq!(m.regs[1], 0);
        assert_eq!(m.regs[RF], FLAG_Z);
    }

    #[test]
    fn div_by_zero_and_bad_opcode() {
        let mut m = vm("71:1;");
        assert!(m.step().unwrap_err().msg.contains("ноль"));
        let mut m = vm("f0:1;");
        assert_eq!(m.step().unwrap_err().addr, 0);
    }

    #[test]
    fn reset_restores_memory() {
        let mut m = vm("3007:1;20f0:2;");
        m.step().unwrap();
        let info = m.step().unwrap();
        assert_eq!(info.changed_mem, Some(0xF0));
        m.reset();
        assert_eq!(m.mem[0xF0], 0);
        assert_eq!(m.regs, [0; 6]);
    }
}
