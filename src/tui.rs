//! Интерфейс «как на сайте»: таблица памяти, регистры, исходник, пошаговое выполнение.

use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use ratatui::{DefaultTerminal, Frame};

use crate::program::Program;
use crate::template::{parse_byte, parse_number};
use crate::vm::{REG_NAMES, RF, Vm, instr_len};

const SPEEDS: usize = 4;

struct App<'a> {
    vm: &'a mut Vm,
    program: &'a Program,
    src: Vec<&'a str>,
    /// Регистры до последнего шага — для подсветки изменённых битов.
    prev_regs: [u8; 6],
    changed_mem: Option<u8>,
    running: bool,
    speed: usize,
    halted: bool,
    steps: u64,
    status: String,
    /// Строка ввода в режиме правки.
    edit: Option<String>,
}

/// Запустить интерфейс. Возвращает `true`, если машина остановилась на HLT.
pub fn run(vm: &mut Vm, program: &Program, src: &str) -> Result<bool> {
    let mut app = App {
        prev_regs: vm.regs,
        vm,
        program,
        src: src.lines().collect(),
        changed_mem: None,
        running: false,
        speed: 0,
        halted: false,
        steps: 0,
        status: String::new(),
        edit: None,
    };
    let mut terminal = ratatui::init();
    let res = app.event_loop(&mut terminal);
    ratatui::restore();
    res.map(|_| app.halted)
}

impl App<'_> {
    fn interval(&self) -> Duration {
        // Как на сайте: 500 - 100 * (скорость + 1) мс.
        Duration::from_millis(400 - 100 * self.speed as u64)
    }

    fn event_loop(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        let mut last_step = Instant::now();
        loop {
            terminal.draw(|f| self.draw(f))?;
            let timeout = if self.running {
                self.interval().saturating_sub(last_step.elapsed())
            } else {
                Duration::from_millis(500)
            };
            if event::poll(timeout)?
                && let Event::Key(key) = event::read()?
                && key.kind == KeyEventKind::Press
                && self.on_key(key)
            {
                return Ok(());
            }
            if self.running && last_step.elapsed() >= self.interval() {
                self.step();
                last_step = Instant::now();
            }
        }
    }

    /// Обработать клавишу. `true` — выход.
    fn on_key(&mut self, key: KeyEvent) -> bool {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return true;
        }
        if let Some(buf) = &mut self.edit {
            match key.code {
                KeyCode::Esc => self.edit = None,
                KeyCode::Enter => {
                    let input = std::mem::take(buf);
                    self.edit = None;
                    self.apply_edit(&input);
                }
                KeyCode::Backspace => {
                    buf.pop();
                }
                KeyCode::Char(c) => buf.push(c),
                _ => {}
            }
            return false;
        }
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return true,
            KeyCode::Char('p') => {
                self.running = !self.running;
                self.status = if self.running { "Выполнение...".into() } else { "Пауза".into() };
            }
            _ if self.running => {}
            KeyCode::Char('n') | KeyCode::Char(' ') | KeyCode::Right => self.step(),
            KeyCode::Char('r') => {
                self.vm.reset();
                self.prev_regs = self.vm.regs;
                self.changed_mem = None;
                self.halted = false;
                self.steps = 0;
                self.status = "Сброс".into();
            }
            KeyCode::Char('s') => self.speed = (self.speed + 1) % SPEEDS,
            KeyCode::Char('e') => self.edit = Some(String::new()),
            _ => {}
        }
        false
    }

    fn step(&mut self) {
        self.prev_regs = self.vm.regs;
        match self.vm.step() {
            Ok(info) => {
                self.steps += 1;
                self.changed_mem = info.changed_mem;
                self.halted = info.halted;
                if info.halted {
                    self.running = false;
                    self.status = "Остановка (HLT)".into();
                } else if !self.running {
                    self.status.clear();
                }
            }
            Err(e) => {
                self.running = false;
                self.status = format!("Ошибочка по адресу 0x{:02X}: {}", e.addr, e.msg);
            }
        }
    }

    /// `0xF0=5`, `@240=-3`, `r1=0x10`, `rc=0`.
    fn apply_edit(&mut self, input: &str) {
        let Some((target, value)) = input.split_once('=') else {
            self.status = "Формат: АДРЕС=ЗНАЧЕНИЕ или РЕГИСТР=ЗНАЧЕНИЕ".into();
            return;
        };
        let Some(value) = parse_byte(value) else {
            self.status = format!("Значение «{}» не в диапазоне -128..255", value.trim());
            return;
        };
        let target = target.trim().to_ascii_uppercase();
        self.prev_regs = self.vm.regs;
        self.changed_mem = None;
        if let Some(r) = REG_NAMES.iter().position(|&n| n == target) {
            self.vm.regs[r] = value;
            self.status = format!("{target} = {value:02X}");
        } else if let Some(a) =
            parse_number(target.trim_start_matches('@')).filter(|a| (0..=255).contains(a))
        {
            self.vm.mem[a as usize] = value;
            self.changed_mem = Some(a as u8);
            self.status = format!("@{a:02X} = {value:02X}");
        } else {
            self.status = format!("«{target}» — не адрес и не регистр");
        }
    }

    fn draw(&self, f: &mut Frame) {
        let [main, status] = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(f.area());
        let [left, right] = Layout::horizontal([Constraint::Length(53), Constraint::Min(30)]).areas(main);
        let [mem, help] = Layout::vertical([Constraint::Length(19), Constraint::Min(0)]).areas(left);
        let [regs, src] = Layout::vertical([Constraint::Length(8), Constraint::Min(0)]).areas(right);

        self.draw_mem(f, mem);
        self.draw_regs(f, regs);
        self.draw_src(f, src);
        self.draw_help(f, help);
        self.draw_status(f, status);
    }

    fn draw_mem(&self, f: &mut Frame, area: Rect) {
        let rc = self.vm.rc() as usize;
        let cur_len = instr_len(self.vm.mem[rc]);
        let is_current = |addr: usize| (addr + 256 - rc) % 256 < cur_len;
        let header = Style::new().fg(Color::DarkGray);

        let mut lines = vec![Line::from(
            std::iter::once(Span::styled("  ", header))
                .chain((0..16).map(|c| Span::styled(format!(" {c:02X}"), header)))
                .collect::<Vec<_>>(),
        )];
        for r in 0..16 {
            let mut spans = vec![Span::styled(format!("{:02X}", r * 16), header)];
            for c in 0..16 {
                let addr = r * 16 + c;
                let mut style = Style::new();
                if self.changed_mem == Some(addr as u8) {
                    style = style.fg(Color::Red);
                }
                if is_current(addr) {
                    style = style.add_modifier(Modifier::BOLD | Modifier::REVERSED);
                }
                spans.push(Span::raw(" "));
                spans.push(Span::styled(format!("{:02X}", self.vm.mem[addr]), style));
            }
            lines.push(Line::from(spans));
        }
        f.render_widget(Paragraph::new(lines).block(Block::bordered().title(" Память ")), area);
    }

    fn draw_regs(&self, f: &mut Frame, area: Rect) {
        let lines: Vec<Line> = (0..6)
            .map(|i| {
                let v = self.vm.regs[i];
                let diff = v ^ self.prev_regs[i];
                let mut spans = vec![Span::raw(format!("{:<3} ", REG_NAMES[i]))];
                for bit in (0..8).rev() {
                    let style = if diff >> bit & 1 == 1 { Style::new().fg(Color::Red) } else { Style::new() };
                    spans.push(Span::styled(format!("{}", v >> bit & 1), style));
                }
                spans.push(Span::styled(
                    format!("  {v:02X} {:>4} {v:>3}", v as i8),
                    Style::new().fg(Color::DarkGray),
                ));
                if i == RF {
                    spans.push(Span::raw("  "));
                    for (name, bit) in [("C", 2), ("S", 1), ("Z", 0)] {
                        let on = v >> bit & 1 == 1;
                        let style = if on { Style::new().bold() } else { Style::new().fg(Color::DarkGray) };
                        spans.push(Span::styled(format!("{name} "), style));
                    }
                }
                Line::from(spans)
            })
            .collect();
        f.render_widget(Paragraph::new(lines).block(Block::bordered().title(" Регистры ")), area);
    }

    fn draw_src(&self, f: &mut Frame, area: Rect) {
        let cur = self.program.lines[self.vm.rc() as usize];
        let height = area.height.saturating_sub(2) as usize;
        let offset = cur.saturating_sub(height / 2 + 1).min(self.src.len().saturating_sub(height));
        let lines: Vec<Line> = self
            .src
            .iter()
            .enumerate()
            .map(|(i, text)| {
                let n = i + 1;
                let line = Line::from(vec![
                    Span::styled(format!("{n:>3} "), Style::new().fg(Color::DarkGray)),
                    Span::raw(text.replace('\t', "    ")),
                ]);
                if n == cur { line.style(Style::new().reversed()) } else { line }
            })
            .collect();
        f.render_widget(
            Paragraph::new(lines).scroll((offset as u16, 0)).block(Block::bordered().title(" Исходник ")),
            area,
        );
    }

    fn draw_help(&self, f: &mut Frame, area: Rect) {
        let speed = "▉".repeat(self.speed + 1);
        let text = vec![
            Line::from("n/пробел шаг   p старт/стоп   r сброс"),
            Line::from(format!("s скорость {speed:<4}  e правка   q выход")),
        ];
        f.render_widget(Paragraph::new(text).fg(Color::DarkGray), area);
    }

    fn draw_status(&self, f: &mut Frame, area: Rect) {
        let line = match &self.edit {
            Some(buf) => Line::from(vec![
                Span::styled("Изменить (0xF0=5, r1=-3): ", Style::new().bold()),
                Span::raw(format!("{buf}▏")),
            ]),
            None => Line::from(format!("Шагов: {}   {}", self.steps, self.status)),
        };
        f.render_widget(Paragraph::new(line), area);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::KeyEvent;

    fn key(app: &mut App, c: char) {
        app.on_key(KeyEvent::from(KeyCode::Char(c)));
    }

    #[test]
    fn renders_and_steps() {
        // r0 <- @0xF0; r1 <- @0xF1; r0 <- r0 + r1; @0xF2 <- r0; HLT
        let program = Program::parse("10f0:1;14f1:2;41:3;20f2:4;c007:5;").unwrap();
        let src = "r0 <- @0xF0\nr1 <- @0xF1\nr0 <- r0 + r1\n@0xF2 <- r0\nHLT: rc <- @HLT";
        let mut vm = Vm::new(&program);
        vm.mem[0xF0] = 5;
        vm.mem[0xF1] = 0xFE;
        let mut app = App {
            prev_regs: vm.regs,
            vm: &mut vm,
            program: &program,
            src: src.lines().collect(),
            changed_mem: None,
            running: false,
            speed: 0,
            halted: false,
            steps: 0,
            status: String::new(),
            edit: None,
        };
        for _ in 0..5 {
            key(&mut app, 'n');
        }
        assert!(app.halted);
        assert_eq!(app.vm.mem[0xF2], 3);

        key(&mut app, 'e');
        for c in "r2=-5".chars() {
            key(&mut app, c);
        }
        app.on_key(KeyEvent::from(KeyCode::Enter));
        assert_eq!(app.vm.regs[2], 0xFB);

        let mut term = Terminal::new(TestBackend::new(100, 24)).unwrap();
        term.draw(|f| app.draw(f)).unwrap();
        let screen: String = term
            .backend()
            .buffer()
            .content
            .chunks(100)
            .map(|row| row.iter().map(|c| c.symbol()).collect::<String>() + "\n")
            .collect();
        println!("{screen}");
        assert!(screen.contains("F0 05 FE 03"));
        assert!(screen.contains("R2  11111011"));

        key(&mut app, 'r');
        assert_eq!(app.vm.mem[0xF2], 0);
        assert!(!app.halted);
    }
}
