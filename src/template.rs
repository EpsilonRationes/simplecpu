//! Шаблоны ввода/вывода в спец-комментариях исходника:
//!
//! ```text
//! ; scpu-in:  @0xF0 "A: "
//! ; scpu-out: "A+B = {@0xF2:s}, hex {@0xF2:x}, R0 = {r0:u}"
//! ```

use anyhow::{Result, anyhow, bail};

use crate::vm::Vm;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Fmt {
    Signed,
    Unsigned,
    Hex,
    Bin,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Piece {
    Text(String),
    Cell(u8, Fmt),
    Reg(usize, Fmt),
    /// Диапазон ячеек `@0xD0...0xDF`, значения через пробел.
    Range(u8, u8, Fmt),
}

#[derive(Debug)]
pub struct Input {
    pub addr: u8,
    /// Последняя ячейка диапазона (равна `addr` для одной ячейки).
    pub end: u8,
    pub prompt: String,
}

#[derive(Debug, Default)]
pub struct Template {
    pub inputs: Vec<Input>,
    pub outputs: Vec<Vec<Piece>>,
}

/// Число в записи простого ассемблера: `12`, `-3`, `0x1F`, `0b101`.
pub fn parse_number(s: &str) -> Option<i64> {
    let s = s.trim();
    let (neg, s) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s),
    };
    let lower = s.to_ascii_lowercase();
    let v = if let Some(h) = lower.strip_prefix("0x") {
        i64::from_str_radix(h, 16).ok()?
    } else if let Some(b) = lower.strip_prefix("0b") {
        i64::from_str_radix(b, 2).ok()?
    } else {
        lower.parse().ok()?
    };
    Some(if neg { -v } else { v })
}

/// Значение байта: -128..=255, отрицательные — в дополнительном коде.
pub fn parse_byte(s: &str) -> Option<u8> {
    let v = parse_number(s)?;
    (-128..=255).contains(&v).then_some(v as u8)
}

fn parse_addr(s: &str) -> Option<u8> {
    let v = parse_number(s)?;
    (0..=255).contains(&v).then_some(v as u8)
}

impl Template {
    pub fn parse(src: &str) -> Result<Template> {
        let mut t = Template::default();
        for (i, line) in src.lines().enumerate() {
            let Some(directive) = directive(line) else { continue };
            let res = match directive {
                Directive::In(rest) => parse_in(rest).map(|inp| t.inputs.push(inp)),
                Directive::Out(rest) => parse_out(rest).map(|out| t.outputs.push(out)),
            };
            res.map_err(|e| anyhow!("строка {}: {e}", i + 1))?;
        }
        Ok(t)
    }
}

enum Directive<'a> {
    In(&'a str),
    Out(&'a str),
}

fn directive(line: &str) -> Option<Directive<'_>> {
    let (_, comment) = line.split_once(';')?;
    let comment = comment.trim_start();
    if let Some(rest) = comment.strip_prefix("scpu-in:") {
        Some(Directive::In(rest))
    } else {
        comment.strip_prefix("scpu-out:").map(Directive::Out)
    }
}

/// Строка исходника с директивой шаблона? (такие строки не отправляются на сервер)
pub fn is_directive(line: &str) -> bool {
    directive(line).is_some()
}

/// Строка в кавычках с экранированием `\n \t \" \\`. Возвращает содержимое и остаток.
fn parse_quoted(s: &str) -> Result<(String, &str)> {
    let s = s.trim_start();
    let Some(body) = s.strip_prefix('"') else { bail!("ожидается строка в кавычках") };
    let mut out = String::new();
    let mut chars = body.char_indices();
    while let Some((i, c)) = chars.next() {
        match c {
            '"' => return Ok((out, &body[i + 1..])),
            '\\' => match chars.next() {
                Some((_, 'n')) => out.push('\n'),
                Some((_, 't')) => out.push('\t'),
                Some((_, c @ ('"' | '\\'))) => out.push(c),
                Some((_, c)) => bail!("неизвестная escape-последовательность \\{c}"),
                None => break,
            },
            c => out.push(c),
        }
    }
    bail!("незакрытая кавычка")
}

/// `0xD0` или диапазон `0xD0...0xDF` (допустимы также `..` и `…`).
fn parse_addr_range(s: &str) -> Option<(u8, u8)> {
    let s = s.trim();
    let Some(i) = s.find("..").or_else(|| s.find('…')) else {
        let a = parse_addr(s)?;
        return Some((a, a));
    };
    let tail = s[i..].trim_start_matches(['.', '…']);
    let (a, b) = (parse_addr(&s[..i])?, parse_addr(tail)?);
    (a <= b).then_some((a, b))
}

fn parse_in(rest: &str) -> Result<Input> {
    let rest = rest.trim();
    let Some(rest) = rest.strip_prefix('@') else { bail!("ожидается адрес ячейки: @0xF0") };
    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    let (addr, last) = parse_addr_range(&rest[..end])
        .ok_or_else(|| anyhow!("некорректный адрес ячейки или диапазон «{}»", &rest[..end]))?;
    let rest = rest[end..].trim();
    let prompt = if rest.is_empty() && addr == last {
        format!("@0x{addr:02X}: ")
    } else if rest.is_empty() {
        format!("@0x{addr:02X}...0x{last:02X}: ")
    } else {
        let (p, tail) = parse_quoted(rest)?;
        if !tail.trim().is_empty() {
            bail!("лишний текст после строки: «{}»", tail.trim());
        }
        p
    };
    Ok(Input { addr, end: last, prompt })
}

fn parse_out(rest: &str) -> Result<Vec<Piece>> {
    let (text, tail) = parse_quoted(rest)?;
    if !tail.trim().is_empty() {
        bail!("лишний текст после строки: «{}»", tail.trim());
    }
    let mut pieces = Vec::new();
    let mut lit = String::new();
    let mut rest = text.as_str();
    while let Some(i) = rest.find(['{', '}']) {
        lit.push_str(&rest[..i]);
        let c = rest.as_bytes()[i];
        rest = &rest[i + 1..];
        if rest.as_bytes().first() == Some(&c) {
            // {{ или }}
            lit.push(c as char);
            rest = &rest[1..];
            continue;
        }
        if c == b'}' {
            bail!("непарная «}}» (для символа пишите «}}}}»)");
        }
        let close = rest.find('}').ok_or_else(|| anyhow!("незакрытая «{{»"))?;
        let placeholder = &rest[..close];
        rest = &rest[close + 1..];
        if !lit.is_empty() {
            pieces.push(Piece::Text(std::mem::take(&mut lit)));
        }
        pieces.push(parse_placeholder(placeholder)?);
    }
    lit.push_str(rest);
    if !lit.is_empty() {
        pieces.push(Piece::Text(lit));
    }
    Ok(pieces)
}

fn parse_placeholder(p: &str) -> Result<Piece> {
    let Some((what, fmt)) = p.split_once(':') else {
        bail!("в «{{{p}}}» не указан формат: {{{p}:s}} — со знаком, {{{p}:u}} — без знака, :x — hex, :b — двоичный");
    };
    let fmt = match fmt.trim() {
        "s" => Fmt::Signed,
        "u" => Fmt::Unsigned,
        "x" => Fmt::Hex,
        "b" => Fmt::Bin,
        f => bail!("неизвестный формат «{f}» (s, u, x, b)"),
    };
    let what = what.trim();
    if let Some(addr) = what.strip_prefix('@') {
        let (a, b) = parse_addr_range(addr)
            .ok_or_else(|| anyhow!("некорректный адрес ячейки или диапазон «{addr}»"))?;
        return Ok(if a == b { Piece::Cell(a, fmt) } else { Piece::Range(a, b, fmt) });
    }
    let reg = match what.to_ascii_lowercase().as_str() {
        "r0" => 0,
        "r1" => 1,
        "r2" => 2,
        "r3" => 3,
        "rc" => 4,
        "rf" => 5,
        _ => bail!("«{what}» — не ячейка (@адрес) и не регистр (r0-r3, rc, rf)"),
    };
    Ok(Piece::Reg(reg, fmt))
}

pub fn format_byte(v: u8, fmt: Fmt) -> String {
    match fmt {
        Fmt::Signed => (v as i8).to_string(),
        Fmt::Unsigned => v.to_string(),
        Fmt::Hex => format!("{v:02X}"),
        Fmt::Bin => format!("{v:08b}"),
    }
}

pub fn render(pieces: &[Piece], vm: &Vm) -> String {
    pieces
        .iter()
        .map(|p| match p {
            Piece::Text(s) => s.clone(),
            Piece::Cell(a, f) => format_byte(vm.mem[*a as usize], *f),
            Piece::Reg(r, f) => format_byte(vm.regs[*r], *f),
            Piece::Range(a, b, f) => vm.mem[*a as usize..=*b as usize]
                .iter()
                .map(|&v| format_byte(v, *f))
                .collect::<Vec<_>>()
                .join(" "),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers() {
        assert_eq!(parse_byte("5"), Some(5));
        assert_eq!(parse_byte("-2"), Some(0xFE));
        assert_eq!(parse_byte("0x1f"), Some(0x1F));
        assert_eq!(parse_byte("0B101"), Some(5));
        assert_eq!(parse_byte("256"), None);
        assert_eq!(parse_byte("-129"), None);
        assert_eq!(parse_byte("abc"), None);
    }

    #[test]
    fn parses_template() {
        let src = "; scpu-in:  @0xF0 \"A в ячейку 0xF0: \"\n\
                   ;scpu-in: @241\n\
                   r0 <- @0xF0 ; обычный комментарий\n\
                   ; scpu-out: \"A+B = {@0xF2:s} {{x}} {R0:x}\\n\"\n";
        let t = Template::parse(src).unwrap();
        assert_eq!(t.inputs.len(), 2);
        assert_eq!(t.inputs[0].addr, 0xF0);
        assert_eq!(t.inputs[0].prompt, "A в ячейку 0xF0: ");
        assert_eq!(t.inputs[1].addr, 241);
        assert_eq!(t.inputs[1].prompt, "@0xF1: ");
        assert_eq!(
            t.outputs[0],
            vec![
                Piece::Text("A+B = ".into()),
                Piece::Cell(0xF2, Fmt::Signed),
                Piece::Text(" {x} ".into()),
                Piece::Reg(0, Fmt::Hex),
                Piece::Text("\n".into()),
            ]
        );
    }

    #[test]
    fn ranges() {
        let t = Template::parse(
            "; scpu-in: @0xD0...0xDF \"числа: \"\n; scpu-out: \"{@0xF0...0xF3:s}\"",
        )
        .unwrap();
        assert_eq!((t.inputs[0].addr, t.inputs[0].end), (0xD0, 0xDF));
        assert_eq!(t.outputs[0], vec![Piece::Range(0xF0, 0xF3, Fmt::Signed)]);
        assert!(Template::parse("; scpu-in: @0xDF...0xD0").is_err());
    }

    #[test]
    fn format_is_required() {
        let err = Template::parse("\n; scpu-out: \"{@3}\"").unwrap_err().to_string();
        assert!(err.starts_with("строка 2:"), "{err}");
        assert!(err.contains("не указан формат"), "{err}");
    }

    #[test]
    fn bad_templates() {
        assert!(Template::parse("; scpu-out: \"{@3:q}\"").is_err());
        assert!(Template::parse("; scpu-out: \"{r9:s}\"").is_err());
        assert!(Template::parse("; scpu-out: \"{@3:s\"").is_err());
        assert!(Template::parse("; scpu-out: \"x").is_err());
        assert!(Template::parse("; scpu-in: 0xF0").is_err());
        assert!(Template::parse("; scpu-in: @0x100").is_err());
    }

    #[test]
    fn formats() {
        assert_eq!(format_byte(0xFE, Fmt::Signed), "-2");
        assert_eq!(format_byte(0xFE, Fmt::Unsigned), "254");
        assert_eq!(format_byte(0x0A, Fmt::Hex), "0A");
        assert_eq!(format_byte(5, Fmt::Bin), "00000101");
    }
}
