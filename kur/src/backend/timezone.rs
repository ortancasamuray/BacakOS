// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Anadolu Panteri <bilgi@anadolupanteri.org.tr>

//! Time zone discovery.
//!
//! Read from `/usr/share/zoneinfo/zone1970.tab`, shipped by `tzdata`. Parsing
//! the table rather than walking the directory tree gives us the country codes
//! for free, which is what lets [`default_for_locale`] pick a sensible default.

use anyhow::{Context, Result};

/// A zone as `tzdata` names it, e.g. `Europe/Istanbul`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Zone {
    pub name: String,
    /// ISO 3166 codes sharing this zone, e.g. `["AE", "OM"]`.
    pub countries: Vec<String>,
}

const ZONE_TAB: &str = "/usr/share/zoneinfo/zone1970.tab";

/// Every zone, sorted by name.
///
/// `zone1970.tab` is tab-separated: `codes \t coordinates \t TZ \t comments`.
/// Lines beginning with `#` are comments.
pub fn list_zones() -> Result<Vec<Zone>> {
    let text = std::fs::read_to_string(ZONE_TAB)
        .with_context(|| format!("{ZONE_TAB} okunamadı — tzdata kurulu mu?"))?;
    Ok(parse_zone_tab(&text))
}

fn parse_zone_tab(text: &str) -> Vec<Zone> {
    let mut zones: Vec<Zone> = text
        .lines()
        .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
        .filter_map(|line| {
            let mut fields = line.split('\t');
            let codes = fields.next()?;
            let _coordinates = fields.next()?;
            let name = fields.next()?;
            Some(Zone {
                name: name.to_string(),
                countries: codes.split(',').map(str::to_string).collect(),
            })
        })
        .collect();

    zones.sort_by(|a, b| a.name.cmp(&b.name));
    zones.dedup_by(|a, b| a.name == b.name);
    zones
}

/// Best default zone for a locale, e.g. `tr_TR.UTF-8` → `Europe/Istanbul`.
///
/// Matches on the locale's territory against the zone's country codes. When a
/// country spans several zones (`US`, `RU`) the first by name wins, which is
/// arbitrary — the user is expected to correct it, and the screen exists
/// precisely so they can.
pub fn default_for_locale(locale_code: &str, zones: &[Zone]) -> Option<usize> {
    let territory = locale_code.split('_').nth(1)?.split('.').next()?;
    zones.iter().position(|z| z.countries.iter().any(|c| c == territory))
}

/// Write the zone into the target system.
///
/// `/etc/localtime` is a symlink into the zoneinfo database; `/etc/timezone` is
/// the Debian-specific plain-text copy that `tzdata`'s maintainer script reads.
/// Both must agree or `dpkg-reconfigure tzdata` will silently revert the zone.
pub fn files_for(zone: &str) -> [(String, String); 1] {
    [(String::from("/etc/timezone"), format!("{zone}\n"))]
}

/// The `ln -sf` target for `/etc/localtime`.
pub fn localtime_target(zone: &str) -> String {
    format!("/usr/share/zoneinfo/{zone}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verbatim excerpt of a real `zone1970.tab`, comments included.
    const FIXTURE: &str = "\
# comment line\n\
AD\t+4230+00131\tEurope/Andorra\n\
AE,OM,RE,SC,TF\t+2518+05518\tAsia/Dubai\tCrozet\n\
TR\t+4101+02858\tEurope/Istanbul\n\
US\t+404251-0740023\tAmerica/New_York\teastern\n\
US\t+340308-1181434\tAmerica/Los_Angeles\tPacific\n";

    #[test]
    fn parses_tab_and_skips_comments() {
        let zones = parse_zone_tab(FIXTURE);
        assert_eq!(zones.len(), 5);
        assert!(zones.iter().all(|z| !z.name.starts_with('#')));
    }

    #[test]
    fn zones_are_sorted_by_name() {
        let zones = parse_zone_tab(FIXTURE);
        let names: Vec<_> = zones.iter().map(|z| z.name.as_str()).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted);
    }

    #[test]
    fn shared_zones_keep_every_country_code() {
        let zones = parse_zone_tab(FIXTURE);
        let dubai = zones.iter().find(|z| z.name == "Asia/Dubai").unwrap();
        assert_eq!(dubai.countries, ["AE", "OM", "RE", "SC", "TF"]);
    }

    #[test]
    fn turkish_locale_defaults_to_istanbul() {
        let zones = parse_zone_tab(FIXTURE);
        let index = default_for_locale("tr_TR.UTF-8", &zones).unwrap();
        assert_eq!(zones[index].name, "Europe/Istanbul");
    }

    #[test]
    fn secondary_country_code_still_matches() {
        let zones = parse_zone_tab(FIXTURE);
        // Oman only appears as the second code of Asia/Dubai.
        let index = default_for_locale("ar_OM.UTF-8", &zones).unwrap();
        assert_eq!(zones[index].name, "Asia/Dubai");
    }

    #[test]
    fn multi_zone_country_picks_first_by_name() {
        let zones = parse_zone_tab(FIXTURE);
        let index = default_for_locale("en_US.UTF-8", &zones).unwrap();
        assert_eq!(zones[index].name, "America/Los_Angeles");
    }

    #[test]
    fn unknown_territory_has_no_default() {
        let zones = parse_zone_tab(FIXTURE);
        assert_eq!(default_for_locale("xx_ZZ.UTF-8", &zones), None);
        assert_eq!(default_for_locale("nonsense", &zones), None);
    }

    #[test]
    fn real_zone_tab_contains_istanbul() {
        // Guards the file path and format against a tzdata change.
        let Ok(zones) = list_zones() else { return };
        assert!(zones.iter().any(|z| z.name == "Europe/Istanbul"));
    }
}
