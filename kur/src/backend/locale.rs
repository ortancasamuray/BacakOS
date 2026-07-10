// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Anadolu Panteri <bilgi@anadolupanteri.org.tr>

//! Locale and keyboard layout discovery.
//!
//! Everything here reads from files shipped by `locales` and `xkb-data`, both of
//! which are present in any Debian live image. Nothing shells out, so this
//! module works identically in the installer and in unit tests.

use std::collections::BTreeMap;

use anyhow::Result;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Locale {
    /// e.g. `tr_TR.UTF-8`
    pub code: String,
    /// e.g. `Türkçe (Türkiye)`
    pub label: String,
}

/// Locales we surface by default, with names written in their own language —
/// a user who cannot read the current UI language must still find their own.
///
/// The full `/usr/share/i18n/SUPPORTED` list runs to ~500 entries; showing all
/// of them is hostile. [`list_locales`] intersects this table with what the
/// system actually supports.
const COMMON: &[(&str, &str)] = &[
    ("tr_TR.UTF-8", "Türkçe (Türkiye)"),
    ("en_US.UTF-8", "English (United States)"),
    ("en_GB.UTF-8", "English (United Kingdom)"),
    ("de_DE.UTF-8", "Deutsch (Deutschland)"),
    ("fr_FR.UTF-8", "Français (France)"),
    ("es_ES.UTF-8", "Español (España)"),
    ("it_IT.UTF-8", "Italiano (Italia)"),
    ("pt_BR.UTF-8", "Português (Brasil)"),
    ("ru_RU.UTF-8", "Русский (Россия)"),
    ("ar_SA.UTF-8", "العربية (السعودية)"),
    ("zh_CN.UTF-8", "中文 (简体)"),
    ("ja_JP.UTF-8", "日本語 (日本)"),
];

/// Locale codes the running system can generate, read from
/// `/usr/share/i18n/SUPPORTED` (format: `tr_TR.UTF-8 UTF-8`).
fn supported_codes() -> Option<Vec<String>> {
    let text = std::fs::read_to_string("/usr/share/i18n/SUPPORTED").ok()?;
    Some(
        text.lines()
            .filter_map(|line| line.split_whitespace().next())
            .map(str::to_string)
            .collect(),
    )
}

/// The locales to offer, in [`COMMON`] order.
///
/// Falls back to the full [`COMMON`] table when `/usr/share/i18n/SUPPORTED` is
/// missing — better to offer a locale that later fails to generate than to
/// present the user an empty list.
pub fn list_locales() -> Vec<Locale> {
    let supported = supported_codes();

    COMMON
        .iter()
        .filter(|(code, _)| supported.as_ref().is_none_or(|s| s.iter().any(|c| c == code)))
        .map(|(code, label)| Locale { code: (*code).into(), label: (*label).into() })
        .collect()
}

/// Index of the best default in the list returned by [`list_locales`].
///
/// Honours `$LANG` from the live session (the boot menu sets it), else Turkish,
/// else the first entry.
pub fn default_locale_index(locales: &[Locale]) -> usize {
    std::env::var("LANG")
        .ok()
        .and_then(|lang| locales.iter().position(|l| l.code == lang))
        .or_else(|| locales.iter().position(|l| l.code == "tr_TR.UTF-8"))
        .unwrap_or(0)
}

/// XKB layout codes (`tr`, `us`, `de`, …) parsed from `/usr/share/X11/xkb/rules/base.lst`.
///
/// The file has `! layout` / `! variant` sections; we take the layout section
/// only. Returns a de-duplicated, alphabetically sorted list.
pub fn list_keymaps() -> Result<Vec<String>> {
    let Ok(text) = std::fs::read_to_string("/usr/share/X11/xkb/rules/base.lst") else {
        // xkb-data absent: a minimal set still lets the user proceed.
        return Ok(["tr", "us", "de", "fr", "gb"].map(String::from).to_vec());
    };

    let mut layouts = BTreeMap::new();
    let mut in_layout_section = false;

    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(section) = trimmed.strip_prefix('!') {
            in_layout_section = section.trim() == "layout";
            continue;
        }
        if !in_layout_section || trimmed.is_empty() {
            continue;
        }
        // "tr              Turkish"
        if let Some((code, description)) = trimmed.split_once(char::is_whitespace) {
            layouts.insert(code.to_string(), description.trim().to_string());
        }
    }

    Ok(layouts.into_keys().collect())
}

/// The keymap that matches a locale, e.g. `tr_TR.UTF-8` → `tr`.
///
/// A locale's territory is a better keymap hint than its language for the cases
/// where they differ (`en_GB` → `gb`, not `en`), so try territory first.
pub fn keymap_for_locale(locale_code: &str, available: &[String]) -> Option<usize> {
    let language = locale_code.split('_').next()?;
    let territory = locale_code
        .split('_')
        .nth(1)
        .and_then(|t| t.split('.').next())
        .map(str::to_lowercase);

    territory
        .and_then(|t| available.iter().position(|k| *k == t))
        .or_else(|| available.iter().position(|k| k == language))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keymaps() -> Vec<String> {
        ["de", "fr", "gb", "tr", "us"].map(String::from).to_vec()
    }

    #[test]
    fn territory_beats_language_for_en_gb() {
        let km = keymaps();
        assert_eq!(keymap_for_locale("en_GB.UTF-8", &km), Some(2)); // "gb"
    }

    #[test]
    fn falls_back_to_language_when_territory_absent() {
        let km = keymaps();
        // "us" is not a territory match for fr_CA, but "fr" is a language match.
        assert_eq!(keymap_for_locale("fr_CA.UTF-8", &km), Some(1)); // "fr"
    }

    #[test]
    fn turkish_maps_to_tr() {
        let km = keymaps();
        assert_eq!(keymap_for_locale("tr_TR.UTF-8", &km), Some(3));
    }

    #[test]
    fn unknown_locale_maps_to_nothing() {
        assert_eq!(keymap_for_locale("xx_YY.UTF-8", &keymaps()), None);
    }

    #[test]
    fn default_index_is_in_bounds() {
        let locales = list_locales();
        assert!(!locales.is_empty());
        assert!(default_locale_index(&locales) < locales.len());
    }
}
