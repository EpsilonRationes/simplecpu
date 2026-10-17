//! Сборка на сервере simplecpu.ru (`POST /assemble`), с кэшем удачных сборок.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::template;

const URL: &str = "https://simplecpu.ru/assemble";
/// Ограничение длины исходника, как на сайте.
const MAX_LEN: usize = 4096;

#[derive(Deserialize)]
struct Response {
    sasm_result: String,
    #[serde(default)]
    sasm_messages: Vec<(i64, i64, String)>,
    #[serde(default)]
    sasm_bin: String,
}

pub enum Outcome {
    /// Машинный код в формате сервера.
    Ok { bin: String, messages: Vec<String> },
    Errors(Vec<String>),
}

/// Строки с директивами `scpu-in`/`scpu-out` заменяются пустыми,
/// чтобы номера строк в сообщениях сервера не сдвигались.
fn prepare(src: &str) -> String {
    src.lines()
        .map(|l| if template::is_directive(l) { "" } else { l })
        .collect::<Vec<_>>()
        .join("\n")
}

fn cache_path(text: &str) -> Option<PathBuf> {
    let hash = Sha256::digest(text.as_bytes());
    let hex: String = hash.iter().map(|b| format!("{b:02x}")).collect();
    Some(dirs::cache_dir()?.join("scpu").join(format!("{hex}.bin")))
}

pub fn assemble(src: &str) -> Result<Outcome> {
    let text = prepare(src);
    if text.trim().is_empty() {
        return Ok(Outcome::Errors(vec!["Маловато будет! (пустая программа)".into()]));
    }
    if text.chars().count() > MAX_LEN {
        return Ok(Outcome::Errors(vec![format!(
            "Многовато будет! (больше {MAX_LEN} символов)"
        )]));
    }

    let cache = cache_path(&text);
    if let Some(bin) = cache.as_ref().and_then(|p| std::fs::read_to_string(p).ok()) {
        return Ok(Outcome::Ok { bin, messages: vec![] });
    }

    let body = reqwest::blocking::Client::new()
        .post(URL)
        .form(&[("text", text.as_str())])
        .send()
        .and_then(|r| r.error_for_status())
        .and_then(|r| r.text())
        .context("сервер simplecpu.ru недоступен")?;
    let resp: Response = serde_json::from_str(&body)
        .with_context(|| format!("сервер прислал какую-то ерунду: {body}"))?;
    let messages = resp.sasm_messages.into_iter().map(|(_, _, m)| m).collect();

    match resp.sasm_result.as_str() {
        "OK" if !resp.sasm_bin.is_empty() => {
            if let Some(p) = cache {
                // Кэш — не критично, ошибки записи игнорируем.
                let _ = std::fs::create_dir_all(p.parent().unwrap())
                    .and_then(|_| std::fs::write(&p, &resp.sasm_bin));
            }
            Ok(Outcome::Ok { bin: resp.sasm_bin, messages })
        }
        "OK" => bail!("сервер не вернул машинный код"),
        _ => Ok(Outcome::Errors(messages)),
    }
}
