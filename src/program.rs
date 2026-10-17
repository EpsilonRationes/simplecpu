//! Разбор машинного кода, который возвращает сервер: `HEX:LINE;HEX:LINE;...`
//! (порт `SimpleCPUVM.load()` из site/scpu.min.js).

use anyhow::{Result, bail};

pub const MEM_SIZE: usize = 256;

#[derive(Clone)]
pub struct Program {
    /// Начальное содержимое памяти.
    pub mem: [u8; MEM_SIZE],
    /// Номер строки исходника для каждого байта (0 — не код).
    pub lines: [usize; MEM_SIZE],
    /// Сколько байтов занимает код.
    pub code_len: usize,
}

impl Program {
    pub fn parse(bin: &str) -> Result<Program> {
        let mut p = Program { mem: [0; MEM_SIZE], lines: [0; MEM_SIZE], code_len: 0 };
        for instr in bin.split(';').filter(|s| !s.is_empty()) {
            let Some((hex, line)) = instr.split_once(':') else {
                bail!("некорректный формат кода: {instr}");
            };
            if hex.is_empty() || hex.len() % 2 != 0 {
                bail!("некорректный формат кода: {instr}");
            }
            let line: usize = line
                .parse()
                .map_err(|_| anyhow::anyhow!("некорректный номер строки: {line}"))?;
            for i in (0..hex.len()).step_by(2) {
                let byte = u8::from_str_radix(&hex[i..i + 2], 16)
                    .map_err(|_| anyhow::anyhow!("некорректная инструкция: {instr}"))?;
                if p.code_len >= MEM_SIZE {
                    bail!("программа не помещается в 256 байт");
                }
                p.mem[p.code_len] = byte;
                p.lines[p.code_len] = line;
                p.code_len += 1;
            }
        }
        if p.code_len == 0 {
            bail!("нет кода для загрузки");
        }
        Ok(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_server_output() {
        let p = Program::parse("3005:1;e4:3;21f2:4;c005:5;").unwrap();
        assert_eq!(p.code_len, 7);
        assert_eq!(&p.mem[..7], &[0x30, 0x05, 0xe4, 0x21, 0xf2, 0xc0, 0x05]);
        assert_eq!(&p.lines[..7], &[1, 1, 3, 4, 4, 5, 5]);
    }

    #[test]
    fn rejects_garbage() {
        assert!(Program::parse("").is_err());
        assert!(Program::parse("3:1;").is_err());
        assert!(Program::parse("zz:1;").is_err());
        assert!(Program::parse("30").is_err());
    }
}
