//! "Sor" card: the teacher frames part of the board, the ink inside goes
//! as a PNG to the school's YZsunucu (a local vision-language model), and
//! the step-by-step answer appears on a draggable card. Çöz / Açıkla /
//! İpucu re-ask the same image in another mode.
//!
//! The network call runs on its own thread; `poll` (from
//! `InputHandler::tick`) picks the answer up. Nothing is stored: the image
//! lives only as long as the card.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;
use std::time::Duration;

use glam::Vec2;

use crate::font_atlas::{FontAtlas, GlyphVertex};
use crate::stroke::{push_circle, push_line, push_rect, Vertex};

const WIDTH: f32 = 600.0;
const HEADER_H: f32 = 44.0;
const PADDING: f32 = 16.0;
const TEXT_PX: f32 = 18.0;
const LINE_H: f32 = 32.0;
const BUTTON_H: f32 = 52.0;
const GAP: f32 = 8.0;

const COLOR_PANEL_BG: [f32; 4] = [0.12, 0.13, 0.17, 0.97];
const COLOR_HEADER: [f32; 4] = [0.18, 0.19, 0.24, 1.0];
const COLOR_TEXT: [f32; 4] = [0.95, 0.95, 0.97, 1.0];
const COLOR_DIM: [f32; 4] = [0.62, 0.65, 0.72, 1.0];
const COLOR_ERROR: [f32; 4] = [1.0, 0.62, 0.55, 1.0];
const COLOR_RESULT: [f32; 4] = [0.55, 0.88, 0.62, 1.0];
const COLOR_KEY_IDLE: [f32; 4] = [0.22, 0.24, 0.30, 1.0];
const COLOR_KEY_ACTIVE: [f32; 4] = [0.23, 0.51, 0.96, 1.0];
const COLOR_SCROLL: [f32; 4] = [1.0, 1.0, 1.0, 0.25];

/// Server address when neither `TAHTA_YZSUNUCU` nor the config file says.
const DEFAULT_SERVER: &str = "http://yzsunucu.local:8790";
const SERVER_FILE: &str = "/etc/tahta/yzsunucu";
/// Longest side of the image sent; the model sees ~32 px per token.
const MAX_IMAGE_SIDE: f32 = 1024.0;
/// Small frames are scaled up so thin handwriting stays legible.
const MIN_IMAGE_SIDE: f32 = 640.0;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Mode {
    Solve,
    Explain,
    Hint,
}

impl Mode {
    const ALL: [Mode; 3] = [Mode::Solve, Mode::Explain, Mode::Hint];

    fn label(self) -> &'static str {
        match self {
            Mode::Solve => "Çöz",
            Mode::Explain => "Açıkla",
            Mode::Hint => "İpucu",
        }
    }

    fn query(self) -> &'static str {
        match self {
            Mode::Solve => "coz",
            Mode::Explain => "acikla",
            Mode::Hint => "ipucu",
        }
    }
}

enum State {
    Waiting { since: f64, rx: Receiver<Result<String, String>> },
    Answer(String),
    Failed(String),
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum AskPress {
    Move,
    Close,
    /// Body grabbed: drag scrolls.
    Scroll,
    Other,
}

pub struct AskCard {
    pub position: Vec2,
    height: f32,
    png: Arc<Vec<u8>>,
    mode: Mode,
    state: State,
    scroll: f32,
}

impl AskCard {
    /// Opens next to the framed region (right if it fits, else left) and
    /// sends the first question ("Çöz").
    pub fn open(png: Vec<u8>, frame: (Vec2, Vec2), screen_size: Vec2, now: f64) -> Self {
        let height = (screen_size.y - 180.0).clamp(320.0, 640.0);
        let x = if frame.1.x + 24.0 + WIDTH <= screen_size.x {
            frame.1.x + 24.0
        } else if frame.0.x - 24.0 - WIDTH >= 0.0 {
            frame.0.x - 24.0 - WIDTH
        } else {
            screen_size.x - WIDTH - 16.0
        };
        let y = frame.0.y.clamp(16.0, (screen_size.y - 140.0 - height).max(16.0));
        let mut card = Self {
            position: Vec2::new(x.max(0.0), y),
            height,
            png: Arc::new(png),
            mode: Mode::Solve,
            state: State::Failed(String::new()),
            scroll: 0.0,
        };
        card.ask(Mode::Solve, now);
        card
    }

    fn ask(&mut self, mode: Mode, now: f64) {
        self.mode = mode;
        self.scroll = 0.0;
        let (tx, rx) = mpsc::channel();
        let png = self.png.clone();
        std::thread::spawn(move || {
            let _ = tx.send(post(&server_address(), mode, &png));
        });
        // Replacing the receiver drops any older request's answer.
        self.state = State::Waiting { since: now, rx };
    }

    /// Picks up a finished answer; true while still waiting (keep redrawing
    /// for the dots).
    pub fn poll(&mut self) -> bool {
        if let State::Waiting { rx, .. } = &self.state {
            match rx.try_recv() {
                Ok(Ok(text)) => self.state = State::Answer(text),
                Ok(Err(e)) => self.state = State::Failed(e),
                Err(TryRecvError::Empty) => return true,
                Err(TryRecvError::Disconnected) => self.state = State::Failed("Yanıt alınamadı.".into()),
            }
        }
        false
    }

    pub fn is_waiting(&self) -> bool {
        matches!(self.state, State::Waiting { .. })
    }

    fn size(&self) -> Vec2 {
        Vec2::new(WIDTH, self.height)
    }

    fn close_rect(&self) -> (Vec2, Vec2) {
        (self.position + Vec2::new(WIDTH - HEADER_H, 0.0), Vec2::splat(HEADER_H))
    }

    fn body_rect(&self) -> (Vec2, Vec2) {
        let top = self.position + Vec2::new(PADDING, HEADER_H + PADDING / 2.0);
        (top, Vec2::new(WIDTH - 2.0 * PADDING, self.height - HEADER_H - PADDING * 2.0 - BUTTON_H - GAP))
    }

    fn button_rect(&self, i: usize) -> (Vec2, Vec2) {
        let w = (WIDTH - 2.0 * PADDING - 2.0 * GAP) / 3.0;
        let y = self.position.y + self.height - PADDING - BUTTON_H;
        (Vec2::new(self.position.x + PADDING + i as f32 * (w + GAP), y), Vec2::new(w, BUTTON_H))
    }

    pub fn contains(&self, p: Vec2) -> bool {
        inside((self.position, self.size()), p)
    }

    pub fn drag_to(&mut self, p: Vec2, grab_offset: Vec2) {
        self.position = p - grab_offset;
    }

    pub fn press_at(&mut self, p: Vec2, now: f64) -> AskPress {
        if inside(self.close_rect(), p) {
            return AskPress::Close;
        }
        if p.y < self.position.y + HEADER_H {
            return AskPress::Move;
        }
        for (i, mode) in Mode::ALL.into_iter().enumerate() {
            if inside(self.button_rect(i), p) {
                if mode != self.mode || matches!(self.state, State::Failed(_)) {
                    self.ask(mode, now);
                }
                return AskPress::Other;
            }
        }
        if inside(self.body_rect(), p) {
            return AskPress::Scroll;
        }
        AskPress::Other
    }

    /// Finger drag on the body: content follows the finger.
    pub fn scroll_by(&mut self, dy: f32, font_atlas: &FontAtlas) {
        let (_, body) = self.body_rect();
        let content = self.lines(font_atlas).len() as f32 * LINE_H;
        self.scroll = (self.scroll - dy).clamp(0.0, (content - body.y).max(0.0));
    }

    fn lines(&self, font_atlas: &FontAtlas) -> Vec<(String, [f32; 4])> {
        let (text, color) = match &self.state {
            State::Answer(t) => (t.as_str(), COLOR_TEXT),
            State::Failed(t) => (t.as_str(), COLOR_ERROR),
            State::Waiting { .. } => return Vec::new(),
        };
        let max_w = self.body_rect().1.x;
        let mut out = Vec::new();
        for paragraph in text.lines() {
            let c = if paragraph.starts_with("Sonuç") { COLOR_RESULT } else { color };
            for line in wrap(paragraph, max_w, |s| font_atlas.measure(s, TEXT_PX)) {
                out.push((line, c));
            }
        }
        out
    }

    pub fn render(
        &self,
        now: f64,
        font_atlas: &FontAtlas,
        out_vertices: &mut Vec<Vertex>,
        out_indices: &mut Vec<u32>,
        glyph_vertices: &mut Vec<GlyphVertex>,
        glyph_indices: &mut Vec<u32>,
    ) {
        push_rect(self.position, self.size(), COLOR_PANEL_BG, out_vertices, out_indices);
        push_rect(self.position, Vec2::new(WIDTH, HEADER_H), COLOR_HEADER, out_vertices, out_indices);
        let title = format!("Sor · {}", self.mode.label());
        font_atlas.push_text(&title, self.position + Vec2::new(PADDING, 14.0), 16.0, COLOR_TEXT, glyph_vertices, glyph_indices);
        let grip = self.position + Vec2::new(WIDTH / 2.0, HEADER_H / 2.0);
        for i in -1..=1 {
            push_circle(grip + Vec2::new(i as f32 * 10.0, 0.0), 2.0, COLOR_TEXT, 8, out_vertices, out_indices);
        }
        let (cp, cs) = self.close_rect();
        let (c, r) = (cp + cs / 2.0, cs.x * 0.2);
        push_line(c - Vec2::splat(r), c + Vec2::splat(r), 2.5, COLOR_TEXT, out_vertices, out_indices);
        push_line(c + Vec2::new(-r, r), c + Vec2::new(r, -r), 2.5, COLOR_TEXT, out_vertices, out_indices);

        let (body_pos, body_size) = self.body_rect();
        if let State::Waiting { since, .. } = &self.state {
            let dots = ((now - since) * 2.0) as usize % 4;
            let msg = format!("Düşünüyor{}", ".".repeat(dots));
            font_atlas.push_text(&msg, body_pos + Vec2::new(0.0, 8.0), TEXT_PX, COLOR_DIM, glyph_vertices, glyph_indices);
            let elapsed = (now - since) as u32;
            if elapsed >= 5 {
                let note = format!("{elapsed} sn · yanıt okulun sunucusunda hazırlanıyor");
                font_atlas.push_text(&note, body_pos + Vec2::new(0.0, 8.0 + LINE_H), 14.0, COLOR_DIM, glyph_vertices, glyph_indices);
            }
        } else {
            let lines = self.lines(font_atlas);
            for (i, (line, color)) in lines.iter().enumerate() {
                let y = body_pos.y + i as f32 * LINE_H - self.scroll;
                // Whole lines only — glyph quads aren't clipped.
                if y < body_pos.y - 1.0 || y + LINE_H > body_pos.y + body_size.y + 1.0 {
                    continue;
                }
                font_atlas.push_text(line, Vec2::new(body_pos.x, y + 6.0), TEXT_PX, *color, glyph_vertices, glyph_indices);
            }
            let content = lines.len() as f32 * LINE_H;
            if content > body_size.y {
                let track_h = body_size.y;
                let thumb_h = (track_h * body_size.y / content).max(30.0);
                let thumb_y = body_pos.y + (track_h - thumb_h) * self.scroll / (content - body_size.y);
                push_rect(Vec2::new(body_pos.x + body_size.x + 6.0, thumb_y), Vec2::new(4.0, thumb_h), COLOR_SCROLL, out_vertices, out_indices);
            }
        }

        for (i, mode) in Mode::ALL.into_iter().enumerate() {
            let (pos, size) = self.button_rect(i);
            let color = if mode == self.mode { COLOR_KEY_ACTIVE } else { COLOR_KEY_IDLE };
            push_rect(pos, size, color, out_vertices, out_indices);
            let label = mode.label();
            let w = font_atlas.measure(label, 17.0);
            font_atlas.push_text(label, pos + Vec2::new((size.x - w) / 2.0, (size.y - 17.0) / 2.0), 17.0, COLOR_TEXT, glyph_vertices, glyph_indices);
        }
    }
}

fn inside((pos, size): (Vec2, Vec2), p: Vec2) -> bool {
    p.x >= pos.x && p.y >= pos.y && p.x <= pos.x + size.x && p.y <= pos.y + size.y
}

/// Greedy word wrap; a word wider than the line is cut by characters.
fn wrap(text: &str, max_w: f32, measure: impl Fn(&str) -> f32) -> Vec<String> {
    if text.trim().is_empty() {
        return vec![String::new()];
    }
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let candidate = if line.is_empty() { word.to_string() } else { format!("{line} {word}") };
        if measure(&candidate) <= max_w {
            line = candidate;
            continue;
        }
        if !line.is_empty() {
            lines.push(std::mem::take(&mut line));
        }
        for ch in word.chars() {
            line.push(ch);
            if measure(&line) > max_w {
                line.pop();
                lines.push(std::mem::replace(&mut line, ch.to_string()));
            }
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

// --- Image -----------------------------------------------------------------

/// One stroke in screen space, for `render_png`.
pub struct InkStroke {
    pub points: Vec<Vec2>,
    pub width: f32,
    pub color: [f32; 4],
}

/// Draws the strokes inside `frame` (screen space) black-on-white-ish as a
/// PNG: light ink (chalk on a dark board) turns dark, colored ink keeps its
/// color. `None` if nothing visible lies inside.
pub fn render_png(strokes: &[InkStroke], frame: (Vec2, Vec2)) -> Option<Vec<u8>> {
    let (tl, br) = frame;
    let size = br - tl;
    let long = size.max_element();
    let scale = (MAX_IMAGE_SIDE / long).min((MIN_IMAGE_SIDE / long).max(1.0));
    let (w, h) = ((size.x * scale).ceil().max(1.0) as usize, (size.y * scale).ceil().max(1.0) as usize);
    let mut px = vec![[1.0f32; 3]; w * h];
    let mut any = false;
    for s in strokes {
        let lum = 0.299 * s.color[0] + 0.587 * s.color[1] + 0.114 * s.color[2];
        let ink = if lum > 0.6 { [0.08, 0.08, 0.1] } else { [s.color[0], s.color[1], s.color[2]] };
        let alpha = s.color[3].clamp(0.2, 1.0);
        let r = (s.width * scale / 2.0).max(1.2);
        let pts: Vec<Vec2> = s.points.iter().map(|&p| (p - tl) * scale).collect();
        let segments: Vec<(Vec2, Vec2)> = if pts.len() == 1 { vec![(pts[0], pts[0])] } else { pts.windows(2).map(|p| (p[0], p[1])).collect() };
        // Max-coverage per stroke so overlapping segments don't darken joints.
        let mut cover: std::collections::HashMap<usize, f32> = std::collections::HashMap::new();
        for (a, b) in segments {
            let lo = (a.min(b) - Vec2::splat(r + 1.0)).max(Vec2::ZERO);
            let hi = (a.max(b) + Vec2::splat(r + 1.0)).min(Vec2::new(w as f32 - 1.0, h as f32 - 1.0));
            if lo.x > hi.x || lo.y > hi.y {
                continue;
            }
            for y in lo.y as usize..=hi.y as usize {
                for x in lo.x as usize..=hi.x as usize {
                    let p = Vec2::new(x as f32 + 0.5, y as f32 + 0.5);
                    let ab = b - a;
                    let t = if ab.length_squared() > 0.0 { ((p - a).dot(ab) / ab.length_squared()).clamp(0.0, 1.0) } else { 0.0 };
                    let c = (r + 0.5 - p.distance(a + ab * t)).clamp(0.0, 1.0);
                    if c > 0.0 {
                        let e = cover.entry(y * w + x).or_insert(0.0);
                        *e = e.max(c);
                    }
                }
            }
        }
        for (i, c) in cover {
            let a = c * alpha;
            for k in 0..3 {
                px[i][k] = px[i][k] * (1.0 - a) + ink[k] * a;
            }
            any = true;
        }
    }
    if !any {
        return None;
    }
    let bytes: Vec<u8> = px.iter().flat_map(|p| p.map(|v| (v * 255.0).round() as u8)).collect();
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w as u32, h as u32);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().ok()?;
        writer.write_image_data(&bytes).ok()?;
    }
    Some(out)
}

// --- Network ---------------------------------------------------------------

fn server_address() -> String {
    if let Ok(v) = std::env::var("TAHTA_YZSUNUCU") {
        if !v.trim().is_empty() {
            return v.trim().to_string();
        }
    }
    std::fs::read_to_string(SERVER_FILE)
        .ok()
        .and_then(|t| t.lines().map(str::trim).find(|l| !l.is_empty() && !l.starts_with('#')).map(str::to_string))
        .unwrap_or_else(|| DEFAULT_SERVER.to_string())
}

/// `http://host[:port]` → `(host, port)`; plain HTTP on the school LAN.
fn parse_address(url: &str) -> Option<(String, u16)> {
    let rest = url.strip_prefix("http://").unwrap_or(url).trim_end_matches('/');
    match rest.rsplit_once(':') {
        Some((host, port)) if !host.is_empty() => Some((host.to_string(), port.parse().ok()?)),
        _ if !rest.is_empty() => Some((rest.to_string(), 80)),
        _ => None,
    }
}

fn post(url: &str, mode: Mode, png: &[u8]) -> Result<String, String> {
    let unreachable = || format!("YZsunucu'ya ulaşılamadı ({url}).\nOkul sunucusunun açık olduğundan emin olun.");
    let (host, port) = parse_address(url).ok_or_else(|| format!("Sunucu adresi geçersiz: {url}"))?;
    let addr = (host.as_str(), port).to_socket_addrs().map_err(|_| unreachable())?.next().ok_or_else(unreachable)?;
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(4)).map_err(|_| unreachable())?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(240)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(20)));
    let head = format!(
        "POST /sor?kip={} HTTP/1.1\r\nHost: {host}\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        mode.query(),
        png.len()
    );
    stream.write_all(head.as_bytes()).and_then(|_| stream.write_all(png)).map_err(|_| unreachable())?;
    let mut reply = Vec::new();
    stream.read_to_end(&mut reply).map_err(|e| {
        if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) {
            "Yanıt çok uzun sürdü, yeniden deneyin.".to_string()
        } else {
            unreachable()
        }
    })?;
    let split = reply.windows(4).position(|w| w == b"\r\n\r\n").ok_or("Sunucudan geçersiz yanıt.")?;
    let head = String::from_utf8_lossy(&reply[..split]);
    let status: u16 = head.split(' ').nth(1).and_then(|s| s.parse().ok()).ok_or("Sunucudan geçersiz yanıt.")?;
    let body = String::from_utf8_lossy(&reply[split + 4..]).trim().to_string();
    if status == 200 {
        Ok(body)
    } else if body.is_empty() {
        Err(format!("Sunucu hatası ({status})."))
    } else {
        Err(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses() {
        assert_eq!(parse_address("http://10.0.0.5:8790"), Some(("10.0.0.5".into(), 8790)));
        assert_eq!(parse_address("yzsunucu.local:8790/"), Some(("yzsunucu.local".into(), 8790)));
        assert_eq!(parse_address("http://sunucu"), Some(("sunucu".into(), 80)));
        assert_eq!(parse_address("http://"), None);
    }

    #[test]
    fn wrapping() {
        let m = |s: &str| s.chars().count() as f32;
        assert_eq!(wrap("bir iki üç dört", 7.0, m), vec!["bir iki", "üç dört"]);
        assert_eq!(wrap("abcdefghij", 4.0, m), vec!["abcd", "efgh", "ij"]);
        assert_eq!(wrap("", 4.0, m), vec![String::new()]);
    }

    #[test]
    fn png_of_chalk_is_dark_ink_and_empty_frame_is_none() {
        let s = InkStroke { points: vec![Vec2::new(10.0, 10.0), Vec2::new(90.0, 40.0)], width: 4.0, color: [1.0, 1.0, 1.0, 1.0] };
        let frame = (Vec2::ZERO, Vec2::new(100.0, 50.0));
        let png_bytes = render_png(&[s], frame).unwrap();
        let decoder = png::Decoder::new(png_bytes.as_slice());
        let mut reader = decoder.read_info().unwrap();
        let mut buf = vec![0; reader.output_buffer_size()];
        let info = reader.next_frame(&mut buf).unwrap();
        // Upscaled to MIN_IMAGE_SIDE on the long side.
        assert_eq!((info.width, info.height), (640, 320));
        assert!(buf.chunks(3).any(|p| p[0] < 40), "chalk drawn dark");
        assert!(render_png(&[], frame).is_none());
    }
}
