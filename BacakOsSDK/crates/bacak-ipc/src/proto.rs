use serde::{Deserialize, Serialize};

/// One request line: a client-chosen `id` plus the request itself.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Mesaj {
    pub id: u64,
    #[serde(flatten)]
    pub istek: Istek,
}

/// What a client can ask of the compositor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "istek", rename_all = "snake_case")]
pub enum Istek {
    /// → `veri: {"protokol": u32, "compositor": "0.1.0"}`
    Surum,
    /// Shows a transient banner (toast) on the active output. Both texts are
    /// truncated to [`BILDIRIM_MAX`] characters.
    Bildirim {
        baslik: String,
        #[serde(default)]
        ayrinti: String,
    },
    /// → `veri:` [`crate::Gorunum`]
    GorunumAl,
    /// Sets dark mode desktop-wide (saved, and announced to subscribers).
    KoyuModAyarla { koyu: bool },
    /// Opens (`ac: true`), closes (`false`) or toggles (absent) the Control
    /// Center on the active output.
    KontrolMerkezi {
        #[serde(default)]
        ac: Option<bool>,
    },
    /// → `veri: [`[`Pencere`]`]`
    Pencereler,
    /// → `veri: [`[`Ekran`]`]`
    Ekranlar,
    /// Subscribes this connection to [`Olay`] events until it closes.
    Abone,
}

/// Longest notification title/body kept, in characters.
pub const BILDIRIM_MAX: usize = 200;

/// The reply to the request with the same `id`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Yanit {
    pub id: u64,
    pub tamam: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub veri: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hata: Option<String>,
}

impl Yanit {
    pub fn ok(id: u64, veri: Option<serde_json::Value>) -> Yanit {
        Yanit { id, tamam: true, veri, hata: None }
    }

    pub fn err(id: u64, hata: impl Into<String>) -> Yanit {
        Yanit { id, tamam: false, veri: None, hata: Some(hata.into()) }
    }
}

/// Pushed to subscribed connections (see [`Istek::Abone`]); has no `id`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "olay", rename_all = "snake_case")]
pub enum Olay {
    /// Desktop appearance changed (same fields as [`crate::Gorunum`]).
    Gorunum { koyu: bool },
    /// Keyboard focus moved to another window (`None`: no window focused).
    Odak { pencere: Option<u64> },
}

/// Anything the compositor writes: a reply, or an event.
#[derive(Debug, Clone, PartialEq)]
pub enum Gelen {
    Yanit(Yanit),
    Olay(Olay),
}

impl Gelen {
    /// Parses one line from the compositor. Lines with an `olay` key are
    /// events, everything else must be a reply.
    pub fn parse(line: &str) -> Result<Gelen, serde_json::Error> {
        let v: serde_json::Value = serde_json::from_str(line)?;
        if v.get("olay").is_some() {
            serde_json::from_value(v).map(Gelen::Olay)
        } else {
            serde_json::from_value(v).map(Gelen::Yanit)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Pencere {
    pub id: u64,
    /// Wayland app id / X11 class (usually the `.desktop` basename).
    pub uygulama: String,
    pub baslik: String,
    pub calisma_alani: u32,
    pub odakli: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Ekran {
    pub id: u32,
    /// Position and size in logical pixels.
    pub x: f64,
    pub y: f64,
    pub genislik: f64,
    pub yukseklik: f64,
    /// Integer render scale (1, 2 or 3).
    pub olcek: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_wire_format() {
        let m = Mesaj { id: 1, istek: Istek::Bildirim { baslik: "a".into(), ayrinti: "b".into() } };
        assert_eq!(
            serde_json::to_string(&m).unwrap(),
            r#"{"id":1,"istek":"bildirim","baslik":"a","ayrinti":"b"}"#
        );
        let m: Mesaj = serde_json::from_str(r#"{"id":4,"istek":"kontrol_merkezi"}"#).unwrap();
        assert_eq!(m.istek, Istek::KontrolMerkezi { ac: None });
        let m: Mesaj = serde_json::from_str(r#"{"id":5,"istek":"koyu_mod_ayarla","koyu":false,"yeni":1}"#).unwrap();
        assert_eq!(m.istek, Istek::KoyuModAyarla { koyu: false }, "bilinmeyen alanlar yok sayılmalı");
        assert!(serde_json::from_str::<Mesaj>(r#"{"id":6,"istek":"rm_rf"}"#).is_err());
    }

    #[test]
    fn replies_and_events_are_told_apart() {
        let y = serde_json::to_string(&Yanit::ok(2, None)).unwrap();
        assert_eq!(y, r#"{"id":2,"tamam":true}"#);
        assert_eq!(Gelen::parse(&y).unwrap(), Gelen::Yanit(Yanit::ok(2, None)));
        let o = serde_json::to_string(&Olay::Gorunum { koyu: false }).unwrap();
        assert_eq!(o, r#"{"olay":"gorunum","koyu":false}"#);
        assert_eq!(Gelen::parse(&o).unwrap(), Gelen::Olay(Olay::Gorunum { koyu: false }));
        assert_eq!(
            Gelen::parse(r#"{"id":3,"tamam":false,"hata":"x"}"#).unwrap(),
            Gelen::Yanit(Yanit::err(3, "x"))
        );
    }
}
