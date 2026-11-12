//! Minimal backend-side UI strings (tray menu / tooltip), following the
//! `language` setting ("ru" | "en") from config.json. The tray menu is
//! rebuilt on every poll, so a language change in settings applies there
//! within one poll interval.

use tauri::{AppHandle, Manager};

/// Current UI language, "ru" (default) or "en".
pub fn lang(app: &AppHandle) -> &'static str {
    let l = app
        .try_state::<crate::snapshot::AppState>()
        .map(|s| s.settings.inner.read().language.clone())
        .unwrap_or_default();
    if l == "en" { "en" } else { "ru" }
}

/// Pick a string by language: tr(lang, russian, english).
pub fn tr(lang: &str, ru: &'static str, en: &'static str) -> &'static str {
    if lang == "en" { en } else { ru }
}
