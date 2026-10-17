mod assembler;
mod program;
mod template;
mod tui;
mod vm;

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

use program::Program;
use template::{Fmt, Template, format_byte, parse_byte};
use vm::{REG_NAMES, RF, Vm};

/// Предохранитель от программ без HLT.
const STEP_LIMIT: u64 = 1_000_000;

const MAIN_HELP: &str = "\
Примеры:
  scpu build prog.sasm                   проверить сборку
  scpu run prog.sasm                     выполнить (ввод/вывод по шаблону)
  scpu run prog.sasm --tui               память и регистры, пошаговое выполнение
  scpu run prog.sasm --in 0xF0=5 --dump  ввод без вопросов, дамп после остановки

Подробнее: scpu run --help";

const RUN_HELP: &str = "\
Шаблон ввода/вывода — спец-комментарии в программе:
  ; scpu-in:  @0xF0 \"A: \"          спросить число и записать в ячейку 0xF0
  ; scpu-out: \"A+B = {@0xF2:s}\"    напечатать после остановки
  ; scpu-in:  @0xD0...0xDF \"числа: \"  диапазон: числа через пробел
  ; scpu-out: \"{@0xF0...0xFF:s}\"       диапазон выводится через пробел
Подстановки: {@АДРЕС:ф} — ячейка, {@А1...А2:ф} — диапазон, {r0:ф}..{r3:ф}, {rc:ф}, {rf:ф} — регистр.
Формат ф обязателен: s — со знаком, u — без знака, x — hex, b — двоичный.

Клавиши --tui:
  n, пробел  шаг              p  старт/стоп
  r          сброс            s  скорость
  e          правка (0xF0=5, r1=-3)
  q, Esc     выход

Остановка — когда RC не меняется (HLT: rc <- @HLT).
Коды выхода: 1 — ошибка сборки/шаблона, 2 — ошибка выполнения, 3 — нет HLT за 1000000 шагов.";

#[derive(Parser)]
#[command(name = "scpu", about = "Простой процессор (simplecpu.ru) в консоли", version, after_help = MAIN_HELP)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Проверить, что программа собирается
    Build {
        /// Файл программы (.sasm)
        file: PathBuf,
    },
    /// Собрать и выполнить программу (--tui — интерфейс как на сайте)
    #[command(after_help = RUN_HELP)]
    Run {
        /// Файл программы (.sasm)
        file: PathBuf,
        /// Интерфейс как на сайте: память, регистры, пошаговое выполнение
        #[arg(long)]
        tui: bool,
        /// Значение ячейки без вопроса: --in 0xF0=5 (можно несколько раз)
        #[arg(long = "in", value_name = "АДРЕС=ЗНАЧЕНИЕ", value_parser = parse_in_arg)]
        inputs: Vec<(u8, u8)>,
        /// После остановки напечатать регистры и память
        #[arg(long)]
        dump: bool,
    },
}

fn parse_in_arg(s: &str) -> Result<(u8, u8), String> {
    let (a, v) = s.split_once('=').ok_or("ожидается АДРЕС=ЗНАЧЕНИЕ, например 0xF0=5")?;
    let addr = template::parse_number(a)
        .filter(|a| (0..=255).contains(a))
        .ok_or_else(|| format!("некорректный адрес «{a}»"))? as u8;
    let val = parse_byte(v).ok_or_else(|| format!("значение «{v}» не в диапазоне -128..255"))?;
    Ok((addr, val))
}

/// Код возврата: 1 — ошибка сборки/использования, 2 — ошибка выполнения, 3 — не остановилась.
fn main() -> ExitCode {
    match real_main() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("Ошибка: {e:#}");
            ExitCode::from(1)
        }
    }
}

fn real_main() -> Result<ExitCode> {
    match Cli::parse().cmd {
        Cmd::Build { file } => {
            let src = read_source(&file)?;
            Template::parse(&src).context("шаблон ввода/вывода")?;
            Ok(match build(&src)? {
                Some(_) => {
                    println!("Сборка успешна");
                    ExitCode::SUCCESS
                }
                None => ExitCode::from(1),
            })
        }
        Cmd::Run { file, tui, inputs, dump } => {
            let src = read_source(&file)?;
            let tmpl = Template::parse(&src).context("шаблон ввода/вывода")?;
            let Some(program) = build(&src)? else { return Ok(ExitCode::from(1)) };
            let mut vm = Vm::new(&program);
            fill_inputs(&mut vm, &program, &tmpl, &inputs)?;
            vm.commit_initial();

            let (code, halted) = if tui {
                let halted = tui::run(&mut vm, &program, &src)?;
                (ExitCode::SUCCESS, halted)
            } else {
                run_batch(&mut vm, &program)
            };

            if halted {
                if tmpl.outputs.is_empty() && !dump {
                    print_regs(&vm);
                }
                for out in &tmpl.outputs {
                    println!("{}", template::render(out, &vm));
                }
            }
            if dump {
                print_regs(&vm);
                print_mem(&vm);
            }
            Ok(code)
        }
    }
}

fn read_source(file: &PathBuf) -> Result<String> {
    std::fs::read_to_string(file).with_context(|| format!("не удалось прочитать {}", file.display()))
}

/// Собрать на сервере. `None` — ошибки сборки (уже напечатаны).
fn build(src: &str) -> Result<Option<Program>> {
    match assembler::assemble(src)? {
        assembler::Outcome::Ok { bin, messages } => {
            for m in messages {
                eprintln!("{m}");
            }
            Ok(Some(Program::parse(&bin)?))
        }
        assembler::Outcome::Errors(messages) => {
            if messages.is_empty() {
                eprintln!("Сборка не удалась (сервер не сообщил причину)");
            }
            for m in messages {
                eprintln!("{m}");
            }
            Ok(None)
        }
    }
}

fn fill_inputs(vm: &mut Vm, program: &Program, tmpl: &Template, given: &[(u8, u8)]) -> Result<()> {
    let warn_code = |addr: u8| {
        if (addr as usize) < program.code_len {
            eprintln!("Внимание: ячейка 0x{addr:02X} занята кодом программы, он будет перезаписан");
        }
    };
    for &(addr, val) in given {
        warn_code(addr);
        vm.mem[addr as usize] = val;
    }
    let stdin = std::io::stdin();
    for input in &tmpl.inputs {
        let count = (input.end - input.addr) as usize + 1;
        if (input.addr..=input.end).all(|a| given.iter().any(|&(g, _)| g == a)) {
            continue;
        }
        warn_code(input.addr);
        loop {
            print!("{}", input.prompt);
            std::io::stdout().flush()?;
            let mut line = String::new();
            if stdin.lock().read_line(&mut line)? == 0 {
                bail!("ввод закончился, а значение для ячейки 0x{:02X} не получено", input.addr);
            }
            let vals: Option<Vec<u8>> = line.split_whitespace().map(parse_byte).collect();
            match vals {
                Some(v) if v.len() == count => {
                    vm.mem[input.addr as usize..=input.end as usize].copy_from_slice(&v);
                    break;
                }
                Some(v) => eprintln!("Нужно чисел: {count}, введено: {}", v.len()),
                None => eprintln!("Нужны числа от -128 до 255 через пробел (можно 0x.., 0b..)"),
            }
        }
    }
    Ok(())
}

/// Выполнить до HLT. Возвращает код выхода и признак штатной остановки.
fn run_batch(vm: &mut Vm, program: &Program) -> (ExitCode, bool) {
    for _ in 0..STEP_LIMIT {
        match vm.step() {
            Ok(info) if info.halted => return (ExitCode::SUCCESS, true),
            Ok(_) => {}
            Err(e) => {
                eprintln!(
                    "Ошибка выполнения по адресу 0x{:02X}{}: {}",
                    e.addr,
                    line_suffix(program, e.addr),
                    e.msg
                );
                return (ExitCode::from(2), false);
            }
        }
    }
    eprintln!(
        "Программа не остановилась за {STEP_LIMIT} шагов (нет HLT: rc <- @HLT?), RC = 0x{:02X}{}",
        vm.rc(),
        line_suffix(program, vm.rc())
    );
    (ExitCode::from(3), false)
}

fn line_suffix(program: &Program, addr: u8) -> String {
    match program.lines[addr as usize] {
        0 => String::new(),
        l => format!(" (строка {l})"),
    }
}

fn print_regs(vm: &Vm) {
    for (i, name) in REG_NAMES.iter().enumerate() {
        let v = vm.regs[i];
        print!("{name}: {} 0x{} {:>4} {:>3}", format_byte(v, Fmt::Bin), format_byte(v, Fmt::Hex), v as i8, v);
        if i == RF {
            print!("  C={} S={} Z={}", (v >> 2) & 1, (v >> 1) & 1, v & 1);
        }
        println!();
    }
}

fn print_mem(vm: &Vm) {
    print!("\n   ");
    for c in 0..16 {
        print!(" {c:02X}");
    }
    println!();
    for r in 0..16 {
        print!("{:02X} ", r * 16);
        for c in 0..16 {
            print!(" {:02X}", vm.mem[r * 16 + c]);
        }
        println!();
    }
}
