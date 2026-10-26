//! Desktop UI for the BitChat mesh node (egui), styled after the Android
//! client's retro-terminal mesh theme: monospace type, black background,
//! flat green (peers) / amber (self) containers.
//!
//! Built as a library module so egui_kittest can drive it headlessly in
//! tests (widget queries and rect assertions catch layout regressions
//! without a human in the loop).

//! Desktop UI for the BitChat mesh node (egui), styled after the Android
//! client's retro-terminal mesh theme: monospace type, black background,
//! flat green (peers) / amber (self) containers.
//!
//! Usage:
//!   cargo run -p bitchat-node --example node_gui

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc as std_mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::link::MAX_FILE_CONTENT_BYTES;

use crate::config::{ChatTheme, NodeConfig};
use crate::link::LinkEvent;
use bitchat_wire::file_packet::FilePacket;
use chrono::TimeZone;

/// Decoded image ready for egui, cached per chat line.
struct LoadedImage {
    texture: egui::TextureHandle,
    size: [usize; 2],
}
pub use eframe::egui;

/// Key used for the mesh-wide channel in the log map.
const CHANNEL_KEY: &str = "";

// Retro-terminal palette (see docs/screenshots/readme-mesh-chat.png).
const BG: egui::Color32 = egui::Color32::from_rgb(0x00, 0x00, 0x00);
const GREEN: egui::Color32 = egui::Color32::from_rgb(0x32, 0xD7, 0x4B);
const GREEN_CONTAINER: egui::Color32 = egui::Color32::from_rgb(0x14, 0x33, 0x1B);
const AMBER: egui::Color32 = egui::Color32::from_rgb(0xFF, 0x9F, 0x0A);
const BLUE: egui::Color32 = egui::Color32::from_rgb(0x0A, 0x84, 0xFF);
const TEXT: egui::Color32 = egui::Color32::from_rgb(0xEA, 0xF2, 0xEA);
const TEXT_TERTIARY: egui::Color32 = egui::Color32::from_rgb(0x8F, 0x9C, 0x8F);
const INPUT_SURFACE: egui::Color32 = egui::Color32::from_rgb(0x0B, 0x0B, 0x0B);
const INPUT_BORDER: egui::Color32 = egui::Color32::from_rgb(0x33, 0x36, 0x35);
const ROW_SELECTED: egui::Color32 = egui::Color32::from_rgb(0x10, 0x1D, 0x12);
const SURFACE: egui::Color32 = egui::Color32::from_rgb(0x0D, 0x10, 0x0D);

pub fn terminal_style(ctx: &egui::Context) {
    use eframe::egui::{FontFamily, FontId, TextStyle};
    let monospace = |size: f32| FontId::new(size, FontFamily::Monospace);
    let mut style = (*ctx.style()).clone();
    style.text_styles = [
        (TextStyle::Heading, monospace(18.0)),
        (TextStyle::Body, monospace(14.5)),
        (TextStyle::Small, monospace(11.5)),
        (TextStyle::Button, monospace(13.5)),
        (TextStyle::Monospace, monospace(14.5)),
    ]
    .into();
    style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    style.spacing.button_padding = egui::vec2(12.0, 8.0);
    style.spacing.interact_size = egui::vec2(8.0, 32.0);
    let visuals = &mut style.visuals;
    visuals.panel_fill = BG;
    visuals.window_fill = BG;
    visuals.extreme_bg_color = INPUT_SURFACE;
    visuals.faint_bg_color = GREEN_CONTAINER;
    visuals.override_text_color = Some(TEXT);
    visuals.hyperlink_color = BLUE;
    visuals.selection.bg_fill = GREEN_CONTAINER;
    visuals.selection.stroke.color = GREEN;
    // No focus rectangle around text fields: our composer draws its own
    // rounded frame, and egui's default would paint a square on top of it.
    visuals.selection.stroke = egui::Stroke::NONE;
    visuals.widgets.noninteractive.bg_fill = GREEN_CONTAINER;
    visuals.widgets.noninteractive.corner_radius = egui::CornerRadius::same(10);
    visuals.widgets.inactive.bg_fill = GREEN_CONTAINER;
    visuals.widgets.inactive.weak_bg_fill = GREEN_CONTAINER;
    visuals.widgets.hovered.bg_fill = ROW_SELECTED;
    visuals.widgets.hovered.weak_bg_fill = ROW_SELECTED;
    visuals.widgets.active.bg_fill = GREEN;
    visuals.widgets.active.fg_stroke.color = BG;
    ctx.set_style(style);
}

/// Radio-waves glyph painted with primitives (default fonts lack the unicode).
fn radio_icon(ui: &mut egui::Ui, color: egui::Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(20.0, 16.0), egui::Sense::hover());
    let center = egui::pos2(rect.center().x, rect.bottom() - 2.0);
    ui.painter().circle_filled(center, 2.0, color);
    for radius in [5.5f32, 9.0, 12.5] {
        let points: Vec<egui::Pos2> = (0..=10)
            .map(|k| {
                let angle = std::f32::consts::PI * (0.2 + 0.6 * k as f32 / 10.0);
                egui::pos2(center.x - radius * angle.cos(), center.y - radius * angle.sin())
            })
            .collect();
        ui.painter()
            .add(egui::Shape::line(points, egui::Stroke::new(1.6, color)));
    }
}

fn uuid_line_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Strip path components so a peer-supplied name cannot escape downloads/.
fn sanitize_filename(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c => c,
        })
        .collect()
}

/// Guess a MIME type from the extension, same coarse mapping the phone uses
/// when the platform resolver fails (image/jpeg, video/mp4, octet-stream…).
fn mime_of(path: &std::path::Path) -> String {
    let ext = path
        .extension()
        .map(|ext| ext.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "jpg" | "jpeg" => "image/jpeg".to_string(),
        "png" => "image/png".to_string(),
        "gif" => "image/gif".to_string(),
        "webp" => "image/webp".to_string(),
        "bmp" => "image/bmp".to_string(),
        "mp4" => "video/mp4".to_string(),
        "mov" => "video/quicktime".to_string(),
        "webm" => "video/webm".to_string(),
        "3gp" => "video/3gpp".to_string(),
        "mp3" => "audio/mpeg".to_string(),
        "m4a" => "audio/mp4".to_string(),
        "aac" => "audio/aac".to_string(),
        "wav" => "audio/wav".to_string(),
        "pdf" => "application/pdf".to_string(),
        "zip" => "application/zip".to_string(),
        "txt" | "log" | "md" => "text/plain".to_string(),
        _ => "application/octet-stream".to_string(),
    }
}

/// Small status dot.
fn status_dot(ui: &mut egui::Ui, color: egui::Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(12.0, 16.0), egui::Sense::hover());
    ui.painter()
        .circle_filled(egui::pos2(rect.center().x, rect.center().y), 4.0, color);
}

/// Green block logo, like the app's pixel robot glyph.
fn block_logo(ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(14.0, 20.0), egui::Sense::hover());
    ui.painter().rect_filled(
        rect.shrink2(egui::vec2(1.0, 4.0)),
        egui::CornerRadius::same(2),
        GREEN,
    );
}

fn now_hm() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() % 86_400)
        .unwrap_or(0);
    format!("{:02}:{:02}", (seconds / 3600) % 24, (seconds / 60) % 60)
}

pub enum UiCommand {
    SendPrivate { peer_id: String, content: String },
    SendBroadcast { content: String },
    SendPrivateFile { peer_id: String, file: FilePacket },
    SendBroadcastFile { file: FilePacket },
    SetNickname { nickname: String },
    /// Force an outgoing connection to a scanned MAC address.
    Connect { address: String },
}

#[derive(Clone)]
struct PeerInfo {
    peer_id: String,
    nickname: String,
    verified: bool,
    has_session: bool,
}

#[derive(Clone)]
enum ChatContent {
    Text(String),
    /// A file transfer (photo, video, document…). Received files are
    /// auto-saved under `bitchatd_downloads/`; `saved` holds that path.
    File {
        name: String,
        size: i64,
        mime: String,
        bytes: Vec<u8>,
        saved: Option<PathBuf>,
    },
}

#[derive(Clone)]
struct ChatLine {
    id: String,
    /// Display name (`@who`).
    who: String,
    /// Author peer id (`me` for our own lines): stable colour + click target.
    author_id: String,
    mine: bool,
    content: ChatContent,
    /// Epoch milliseconds; rendered `HH:mm` today, `dd/MM/yyyy HH:mm` older.
    ts_millis: i64,
}

/// Dynamic chat timestamp, mirroring the mobile app: compact `HH:mm` for
/// today, `dd/MM/yyyy HH:mm` for older messages.
fn format_chat_time(ts_millis: i64) -> String {
    let time = chrono::Local.timestamp_millis_opt(ts_millis).single();
    let Some(time) = time else {
        return String::new();
    };
    let now = chrono::Local::now();
    if time.date_naive() == now.date_naive() {
        time.format("%H:%M").to_string()
    } else {
        time.format("%d/%m/%Y %H:%M").to_string()
    }
}

/// Stable author colour, byte-identical port of Android `colorForPeer`:
/// djb2 over `noise:{peer_id}` gives the hue (orange reserved for self),
/// saturation 0.55 / value 0.82 keep it readable on near-black surfaces.
fn peer_color(author_id: &str) -> egui::Color32 {
    let key = format!("noise:{}", author_id.to_lowercase());
    let mut hash: u64 = 5381;
    for byte in key.as_bytes() {
        hash = hash.wrapping_shl(5).wrapping_add(hash).wrapping_add(*byte as u64);
    }
    let mut hue = (hash % 360) as f32;
    if (hue - 30.0).abs() < 18.0 {
        hue = (hue + 43.2) % 360.0;
    }
    hsv_to_rgb(hue / 360.0, 0.55, 0.82)
}

fn hsv_to_rgb(h: f32, s: f32, v: f32) -> egui::Color32 {
    let i = (h * 6.0).floor();
    let f = h * 6.0 - i;
    let p = v * (1.0 - s);
    let q = v * (1.0 - f * s);
    let t = v * (1.0 - (1.0 - f) * s);
    let (r, g, b) = match (i as i32).rem_euclid(6) {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };
    egui::Color32::from_rgb(
        (r * 255.0) as u8,
        (g * 255.0) as u8,
        (b * 255.0) as u8,
    )
}

/// Body font actually in effect (the terminal theme sets monospace); all
/// width measurements must use this family or the bubble under-sizes and
/// the timestamp bleeds past the right edge.
fn body_font(ui: &egui::Ui) -> egui::FontId {
    ui.style()
        .text_styles
        .get(&egui::TextStyle::Body)
        .cloned()
        .unwrap_or_else(|| egui::FontId::proportional(14.0))
}

/// `.small()` resolves to 0.8× the body size, same family.
fn small_font(ui: &egui::Ui) -> egui::FontId {
    let body = body_font(ui);
    egui::FontId::new(body.size * 0.8, body.family)
}

struct ContentSize {
    /// Widest extent of the body (for hugging the bubble to the content).
    width: f32,
    /// Total height of the body.
    height: f32,
    /// Right edge of the last line (for the on-line timestamp decision).
    last_right: f32,
}

/// Human-readable byte size, French locale (`12 Ko`, `1,4 Mo`).
fn human_size(bytes: i64) -> String {
    if bytes < 1024 {
        format!("{bytes} o")
    } else if bytes < 1024 * 1024 {
        format!("{} Ko", bytes / 1024)
    } else {
        format!("{:.1} Mo", bytes as f32 / (1024.0 * 1024.0))
    }
}

/// Long messages keep the timestamp on its own line: this is the minimum
/// last-line width that still hosts the time cluster to the right of text.
const TIME_GAP: f32 = 8.0;
/// Bubble geometry, ported from `ChatVisualTokens`.
const BUBBLE_MAX_FRACTION: f32 = 0.80;
const BUBBLE_PAD_H: f32 = 12.0;
const BUBBLE_PAD_V: f32 = 8.0;

#[derive(Clone)]
struct ScanEntry {
    address: String,
    peer_id: Option<String>,
    rssi: Option<i16>,
    last_seen: String,
}

#[derive(Clone, PartialEq)]
enum Selection {
    Broadcast,
    Peer(String),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SettingsTab {
    Info,
    Settings,
}

pub struct BitchatApp {
    events_in: std_mpsc::Receiver<LinkEvent>,
    cmd_out: std_mpsc::Sender<UiCommand>,
    our_id: String,
    nickname: String,
    nick_draft: String,
    editing_nick: bool,
    status: String,
    logs: std::collections::VecDeque<String>,
    peers: Vec<PeerInfo>,
    chat: HashMap<String, Vec<ChatLine>>,
    selected: Selection,
    draft: String,
    start_signal: Option<std_mpsc::Sender<()>>,
    show_settings: bool,
    settings_tab: SettingsTab,
    show_debug: bool,
    /// Re-grab the composer after sending or switching conversation.
    focus_composer: bool,
    /// Incoming messages waiting for a focus check before desktop notification.
    pending_notifs: Vec<(String, String)>,
    /// Recently scanned devices (like the mobile debug scan list).
    scan: Vec<ScanEntry>,
    /// Last desktop notification shown (rate-limit: toasts steal OS focus).
    last_notif: Option<std::time::Instant>,
    /// Short grace period after the window regains focus during which no
    /// notification may fire, so a toast can never interrupt typing.
    notif_mute_until: Option<std::time::Instant>,
    /// Desktop notifications are opt-in (BITCHATD_NOTIFY=1): Windows toasts
    /// steal OS focus and would fight the composer while typing.
    notifications_enabled: bool,
    /// Chat transcript layout (Bubbles or Matrix), persisted in bitchatd.toml.
    theme: ChatTheme,
    /// Decoded images for inline display, keyed by chat line id.
    image_cache: HashMap<String, LoadedImage>,
}

const MAX_LOG_LINES: usize = 60;

/// Section label in the settings windows, like `AboutSectionLabel`.
fn section_label(ui: &mut egui::Ui, text: &str) {
    ui.add_space(10.0);
    ui.label(egui::RichText::new(text).color(GREEN).strong());
    ui.add_space(4.0);
}

/// Shared full-width settings-row container (identical geometry for every
/// row, including the interactive Chat Layout toggle).
/// Row boxes keep a constant width no matter what the window does: egui
/// windows keep auto-sizing around their content even with
/// `resizable(false)`, and a wide row grows the window mid-frame, which
/// staggered the boxes of the rows laid out before it. A fixed width makes
/// every row identical by construction.
/// Returns the painted box so rows can use it as a click target.
///
/// Rows fill whatever width the window currently has. They can no longer
/// overflow it (fixed-zone geometry with wrapping subtitles), so the window
/// no longer grows mid-frame and the boxes cannot stagger.
fn settings_row_frame(ui: &mut egui::Ui, add_contents: impl FnOnce(&mut egui::Ui)) -> egui::Rect {
    let width = ui.available_width().max(120.0);
    let start = ui.cursor().min;
    let child = ui.allocate_ui_with_layout(
        egui::vec2(width, 0.0),
        egui::Layout::top_down(egui::Align::LEFT),
        |ui| {
            // The sizing pass allocates rows at their natural (wide) width
            // and that painted shape survives into the final paint, which is
            // what staggered the boxes. During sizing, run the same geometry
            // with a transparent fill; only the real pass paints.
            let frame = egui::Frame::new()
                .fill(if ui.is_sizing_pass() {
                    egui::Color32::TRANSPARENT
                } else {
                    SURFACE
                })
                .corner_radius(8)
                .inner_margin(egui::Margin::symmetric(12, 9));
            frame.show(ui, |ui| {
                ui.horizontal(add_contents);
            });
        },
    );
    egui::Rect::from_min_size(start, egui::vec2(width, child.response.rect.height()))
}

/// A settings row with a leading status-dot "icon", title, subtitle and a
/// trailing state label. Disabled rows render dimmed (Android grise l'option).
fn settings_row(
    ui: &mut egui::Ui,
    icon: egui::Color32,
    title: &str,
    subtitle: &str,
    state: &str,
    enabled: bool,
) {
    let (title_color, sub_color) = if enabled {
        (TEXT, TEXT_TERTIARY)
    } else {
        (
            egui::Color32::from_rgb(0x6B, 0x70, 0x6B),
            egui::Color32::from_rgb(0x4A, 0x4F, 0x4A),
        )
    };
    settings_row_frame(ui, |ui| {
        let state_color = if enabled {
            if state == "ON" || state == "✓" { GREEN } else { TEXT_TERTIARY }
        } else {
            egui::Color32::from_rgb(0x4A, 0x4F, 0x4A)
        };
        // Fixed-zone geometry: Sides expands the row when the natural text
        // widths exceed the box (its overflow clause), and labels inside a
        // horizontal layout Extend instead of wrapping. Measure the state,
        // give the rest to the (wrapping) text block, push the state to the
        // right edge by consuming the exact remaining gap.
        let state_font = small_font(ui);
        let right_w = ui.fonts(|f| {
            f.layout_no_wrap(state.to_string(), state_font, state_color).size().x
        });
        let gap = 10.0;
        let left_w = (ui.available_width() - right_w - gap).max(60.0);
        ui.horizontal(|ui| {
            status_dot(ui, icon);
            ui.add_space(6.0);
            ui.allocate_ui_with_layout(
                egui::vec2(left_w, 0.0),
                egui::Layout::top_down(egui::Align::LEFT),
                |ui| {
                    ui.label(egui::RichText::new(title).color(title_color).strong());
                    ui.add(
                        egui::Label::new(egui::RichText::new(subtitle).small().color(sub_color))
                            .wrap(),
                    );
                },
            );
            let remaining = ui.available_width();
            ui.allocate_space(egui::vec2((remaining - right_w).max(0.0), 1.0));
            ui.label(egui::RichText::new(state).small().color(state_color));
        });
    });
    // padding-top of the next element: keep the boxes visually separated
    ui.add_space(8.0);
}

impl BitchatApp {
    pub fn load_nickname(identity_path: &Path) -> String {
        if let Ok(nick) = std::env::var("BITCHATD_NICKNAME") {
            if !nick.trim().is_empty() {
                return nick;
            }
        }
        NodeConfig::load(identity_path).nickname_or_default()
    }

    fn save_nickname(&self, identity_path: &Path) {
        let mut config = NodeConfig::load(identity_path);
        config.node.nickname = Some(self.nickname.clone());
        let _ = config.save(identity_path);
    }

    fn apply(&mut self, ctx: &egui::Context, event: LinkEvent) {
        match event {
            LinkEvent::PeerAnnounced {
                peer_id,
                nickname,
                verified,
                ..
            } => {
                let has_session = self
                    .peers
                    .iter()
                    .find(|peer| peer.peer_id == peer_id)
                    .is_some_and(|peer| peer.has_session);
                self.upsert_peer(PeerInfo {
                    peer_id,
                    nickname,
                    verified,
                    has_session,
                });
            }
            LinkEvent::SessionEstablished { peer_id } => {
                self.set_session(&peer_id, true);
                self.log(format!(
                    "session chiffree etablie avec {}",
                    self.nickname_of(&peer_id)
                ));
            }
            LinkEvent::PrivateMessage {
                peer_id, content, ..
            } => {
                let nickname = self.nickname_of(&peer_id);
                if self.notifications_enabled {
                    self.pending_notifs
                        .push((format!("@{} (private)", nickname), content.clone()));
                }
                self.push_line(&peer_id, &nickname, content);
            }
            LinkEvent::BroadcastMessage { peer_id, content } => {
                let nickname = self.nickname_of(&peer_id);
                if self.notifications_enabled {
                    self.pending_notifs
                        .push((format!("@{} (broadcast)", nickname), content.clone()));
                }
                self.push_line(CHANNEL_KEY, &nickname, content);
            }
            LinkEvent::PrivateFile { peer_id, file } => {
                let nickname = self.nickname_of(&peer_id);
                if self.notifications_enabled {
                    self.pending_notifs.push((
                        format!("@{} (fichier)", nickname),
                        format!("📎 {} ({} o)", file.file_name, file.file_size),
                    ));
                }
                self.push_file_line(ctx, &peer_id, &nickname, &peer_id, file);
            }
            LinkEvent::BroadcastFile { peer_id, file } => {
                let nickname = self.nickname_of(&peer_id);
                if self.notifications_enabled {
                    self.pending_notifs.push((
                        format!("@{} (fichier, canal)", nickname),
                        format!("📎 {} ({} o)", file.file_name, file.file_size),
                    ));
                }
                self.push_file_line(ctx, CHANNEL_KEY, &nickname, &peer_id, file);
            }
            LinkEvent::Delivered { peer_id, .. } => {
                self.log(format!("delivre a {} ✓", self.nickname_of(&peer_id)));
            }
            LinkEvent::PeerStateSent { peer_id } => {
                self.log(format!(
                    "etat authentifie envoye a {}",
                    self.nickname_of(&peer_id)
                ));
            }
            LinkEvent::PeerState {
                peer_id,
                capabilities,
                signing_public_key,
            } => {
                self.log(format!(
                    "etat authentifie de {} caps={:#x} cle={}",
                    self.nickname_of(&peer_id),
                    capabilities,
                    &signing_public_key[..8.min(signing_public_key.len())]
                ));
            }
            LinkEvent::PeerDisconnected { peer_id } => {
                self.set_session(&peer_id, false);
                self.log(format!("{} deconnecte", self.nickname_of(&peer_id)));
            }
            LinkEvent::PeerDiscovered {
                address,
                peer_id,
                rssi,
            } => {
                if let Some(entry) = self.scan.iter_mut().find(|e| e.address == address) {
                    entry.peer_id = peer_id.or_else(|| entry.peer_id.clone());
                    entry.rssi = rssi.or(entry.rssi);
                    entry.last_seen = now_hm();
                } else {
                    self.scan.insert(
                        0,
                        ScanEntry {
                            address,
                            peer_id,
                            rssi,
                            last_seen: now_hm(),
                        },
                    );
                    self.scan.truncate(20);
                }
            }
            LinkEvent::Info(message) => {
                self.log(message);
            }
        }
    }

    fn upsert_peer(&mut self, info: PeerInfo) {
        if let Some(peer) = self
            .peers
            .iter_mut()
            .find(|peer| peer.peer_id == info.peer_id)
        {
            *peer = info;
        } else {
            self.status = format!("peer annonce: {} ({})", info.nickname, info.peer_id);
            self.peers.push(info);
        }
    }

    fn set_session(&mut self, peer_id: &str, has_session: bool) {
        if let Some(peer) = self.peers.iter_mut().find(|p| p.peer_id == peer_id) {
            peer.has_session = has_session;
        }
    }

    fn nickname_of(&self, peer_id: &str) -> String {
        self.peers
            .iter()
            .find(|peer| peer.peer_id == peer_id)
            .map_or_else(
                || peer_id[..peer_id.len().min(8)].to_string(),
                |p| p.nickname.clone(),
            )
    }

    fn push_line(&mut self, key: &str, who: &str, text: String) {
        let author_id = self
            .peers
            .iter()
            .find(|peer| peer.nickname == who)
            .map(|peer| peer.peer_id.clone())
            .unwrap_or_else(|| who.to_string());
        self.chat.entry(key.to_string()).or_default().push(ChatLine {
            id: uuid_line_id(),
            who: who.to_string(),
            author_id,
            mine: false,
            content: ChatContent::Text(text),
            ts_millis: now_millis(),
        });
    }

    fn push_own_line(&mut self, key: &str, content: ChatContent) {
        self.chat.entry(key.to_string()).or_default().push(ChatLine {
            id: uuid_line_id(),
            who: self.nickname.clone(),
            author_id: "me".to_string(),
            mine: true,
            content,
            ts_millis: now_millis(),
        });
    }

    /// Convert a received file into a chat line (auto-saved to downloads).
    fn push_file_line(
        &mut self,
        ctx: &egui::Context,
        key: &str,
        who: &str,
        author_id: &str,
        file: FilePacket,
    ) {
        let saved = self.save_download(&file.file_name, &file.content);
        if saved.is_none() {
            self.log(format!("échec de l'enregistrement de {}", file.file_name));
        }
        let id = uuid_line_id();
        self.try_cache_image(ctx, &id, &file.mime_type, &file.content);
        self.chat.entry(key.to_string()).or_default().push(ChatLine {
            id,
            who: who.to_string(),
            author_id: author_id.to_string(),
            mine: false,
            content: ChatContent::File {
                name: file.file_name,
                size: file.file_size,
                mime: file.mime_type,
                bytes: file.content,
                saved,
            },
            ts_millis: now_millis(),
        });
    }

    /// Decode an image payload into the texture cache so bubbles can render
    /// it inline. No-op for non-image MIME types and undecodable data.
    fn try_cache_image(&mut self, ctx: &egui::Context, id: &str, mime: &str, bytes: &[u8]) {
        if !mime.starts_with("image/") || self.image_cache.contains_key(id) {
            return;
        }
        let Ok(decoded) = image::load_from_memory(bytes) else {
            return;
        };
        let rgba = decoded.to_rgba8();
        let size = [rgba.width() as usize, rgba.height() as usize];
        if size[0] == 0 || size[1] == 0 {
            return;
        }
        let color_image = egui::ColorImage::from_rgba_unmultiplied(size, &rgba);
        let texture = ctx.load_texture(
            format!("img-{id}"),
            color_image,
            egui::TextureOptions::LINEAR,
        );
        self.image_cache.insert(id.to_string(), LoadedImage { texture, size });
    }

    /// Downloads directory beside the identity file; created on demand.
    fn downloads_dir(&self) -> PathBuf {
        let identity_path = PathBuf::from(
            std::env::var("BITCHATD_IDENTITY").unwrap_or_else(|_| "bitchatd.identity".to_string()),
        );
        let dir = identity_path
            .parent()
            .map(|parent| parent.join("bitchatd_downloads"))
            .unwrap_or_else(|| PathBuf::from("bitchatd_downloads"));
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    /// Persist received file bytes under the downloads directory.
    /// Deduplicates by content hash suffix when the name already exists.
    fn save_download(&self, name: &str, bytes: &[u8]) -> Option<PathBuf> {
        let dir = self.downloads_dir();
        let mut path = dir.join(sanitize_filename(name));
        if path.exists() {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            bytes.hash(&mut hasher);
            let stem = path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            let ext = path
                .extension()
                .map(|e| format!(".{}", e.to_string_lossy()))
                .unwrap_or_default();
            path = dir.join(format!("{stem}-{:x}{ext}", hasher.finish() & 0xFFFF));
        }
        std::fs::write(&path, bytes).ok().map(|()| path)
    }

    /// Open a file with its default Windows application.
    fn open_path(path: &std::path::Path) {
        let _ = std::process::Command::new("cmd")
            .args(["/c", "start", "", &path.to_string_lossy()])
            .spawn();
    }

    /// Post a visible error line in the current conversation.
    fn push_system_line(&mut self, text: String) {
        let key = match &self.selected {
            Selection::Broadcast => CHANNEL_KEY.to_string(),
            Selection::Peer(peer_id) => peer_id.clone(),
        };
        self.push_own_line(&key, ChatContent::Text(text));
    }

    /// Re-encode an image so it fits the mesh v1 wire budget: shrinking
    /// thumbnail (never upscaling) + JPEG quality steps, mirroring the phone
    /// media pipeline's compression. Returns None when it cannot fit.
    fn compress_image_to_budget(img: &image::DynamicImage) -> Option<Vec<u8>> {
        use image::codecs::jpeg::JpegEncoder;
        for (max_dim, quality) in [(1024u32, 78u8), (640, 72), (448, 66)] {
            let thumb = img.thumbnail(max_dim, max_dim);
            let mut out = std::io::Cursor::new(Vec::new());
            let mut encoder = JpegEncoder::new_with_quality(&mut out, quality);
            if encoder.encode_image(&thumb).is_err() {
                continue;
            }
            let jpeg = out.into_inner();
            if jpeg.len() as u64 <= MAX_FILE_CONTENT_BYTES {
                return Some(jpeg);
            }
        }
        None
    }

    /// Pick a file from disk and send it to the current conversation.
    /// Wire budget: mesh v1 fragmentation caps a packet near ~118 KB, so the
    /// file content is capped under that (same spirit as the phone's media
    /// picker warning, tighter because the phone's 10 MiB cap only works on
    /// transports that bypass BLE fragmentation). Images are recompressed to
    /// fit, like the phone does; rejections are posted in the conversation.
    fn pick_and_send_file(&mut self, ctx: &egui::Context) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("images", &["jpg", "jpeg", "png", "gif", "webp", "bmp"])
            .add_filter("vidéos", &["mp4", "mov", "webm", "3gp"])
            .add_filter("documents", &["pdf", "txt", "md", "zip"])
            .pick_file()
        else {
            return;
        };
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "fichier".to_string());
        let Ok(bytes) = std::fs::read(&path) else {
            self.log(format!("lecture impossible: {name}"));
            return;
        };
        let mime = mime_of(&path);

        // Images: transcode down to the wire budget (phone photos are
        // several MB raw and would be rejected outright otherwise).
        let (bytes, mime, name) = if mime.starts_with("image/") {
            match image::load_from_memory(&bytes) {
                Ok(img) => match Self::compress_image_to_budget(&img) {
                    Some(jpeg) => {
                        let jpg_name = Path::new(&name)
                            .with_extension("jpg")
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or(name);
                        (jpeg, "image/jpeg".to_string(), jpg_name)
                    }
                    None => {
                        self.push_system_line(format!(
                            "❌ {name}: impossible de réduire l'image sous {} (mesh v1)",
                            human_size(MAX_FILE_CONTENT_BYTES as i64)
                        ));
                        return;
                    }
                },
                // Undecodable formats (gif/webp/bmp without the codec
                // feature): send the raw bytes when they already fit.
                Err(_) => (bytes, mime, name),
            }
        } else {
            (bytes, mime, name)
        };

        if bytes.len() as u64 > MAX_FILE_CONTENT_BYTES {
            self.push_system_line(format!(
                "❌ {name}: trop gros pour le mesh v1 ({} > {}). Essayez un document plus petit.",
                human_size(bytes.len() as i64),
                human_size(MAX_FILE_CONTENT_BYTES as i64)
            ));
            return;
        }

        let file = FilePacket::new(name.clone(), bytes.len() as i64, mime.clone(), bytes);
        let (key, command) = match &self.selected {
            Selection::Broadcast => (
                CHANNEL_KEY.to_string(),
                UiCommand::SendBroadcastFile { file: file.clone() },
            ),
            Selection::Peer(peer_id) => (
                peer_id.clone(),
                UiCommand::SendPrivateFile {
                    peer_id: peer_id.clone(),
                    file: file.clone(),
                },
            ),
        };
        let _ = self.cmd_out.send(command);
        // Local echo: same rendering path as a received file.
        let saved = self.save_download(&name, &file.content);
        let id = uuid_line_id();
        self.try_cache_image(ctx, &id, &mime, &file.content);
        self.chat.entry(key).or_default().push(ChatLine {
            id,
            who: self.nickname.clone(),
            author_id: "me".to_string(),
            mine: true,
            content: ChatContent::File {
                name,
                size: file.file_size,
                mime,
                bytes: file.content.clone(),
                saved,
            },
            ts_millis: now_millis(),
        });
        self.log(format!("envoi de {}…", file.file_name));
        self.focus_composer = true;
    }

    fn load_theme(identity_path: &Path) -> ChatTheme {
        NodeConfig::load(identity_path)
            .ui
            .theme
            .unwrap_or_default()
    }

    fn save_theme(&self, identity_path: &Path) {
        let mut config = NodeConfig::load(identity_path);
        config.ui.theme = Some(self.theme);
        let _ = config.save(identity_path);
    }

    fn log(&mut self, message: impl Into<String>) {
        self.status = message.into();
        self.logs.push_back(self.status.clone());
        if self.logs.len() > MAX_LOG_LINES {
            self.logs.pop_front();
        }
    }

    fn send_draft(&mut self) {
        let text = self.draft.trim().to_string();
        if text.is_empty() {
            return;
        }
        let selection = self.selected.clone();
        match selection {
            Selection::Broadcast => {
                let _ = self
                    .cmd_out
                    .send(UiCommand::SendBroadcast { content: text.clone() });
                self.push_own_line(CHANNEL_KEY, ChatContent::Text(text));
            }
            Selection::Peer(peer_id) => {
                let _ = self.cmd_out.send(UiCommand::SendPrivate {
                    peer_id: peer_id.clone(),
                    content: text.clone(),
                });
                self.push_own_line(&peer_id, ChatContent::Text(text));
            }
        }
        self.draft.clear();
        self.focus_composer = true;
    }

    fn show_nickname_editor(&mut self, ui: &mut egui::Ui, identity_path: &Path) {
        if self.editing_nick {
            let response = ui.add(
                egui::TextEdit::singleline(&mut self.nick_draft)
                    .desired_width(170.0)
                    .font(egui::TextStyle::Heading),
            );
            if ui
                .button("ok")
                .on_hover_text("appliquer le pseudo")
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .clicked()
                || (response.lost_focus()
                    && ui.input(|input| input.key_pressed(egui::Key::Enter)))
            {
                let nickname = self.nick_draft.trim().to_string();
                if !nickname.is_empty() {
                    self.nickname = nickname.clone();
                    self.save_nickname(identity_path);
                    let _ = self.cmd_out.send(UiCommand::SetNickname { nickname });
                }
                self.editing_nick = false;
            }
        } else {
            block_logo(ui);
            if ui
                .add(
                    egui::Button::new(
                        egui::RichText::new(format!("/ @{}", self.nickname))
                            .heading()
                            .color(GREEN),
                    )
                    .fill(BG)
                    .corner_radius(6),
                )
                .on_hover_text("changer le pseudo")
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .clicked()
            {
                self.editing_nick = true;
                self.nick_draft = self.nickname.clone();
            }
        }
    }

    fn show_peer_list(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        let channel_selected = self.selected == Selection::Broadcast;
        let channel_row = egui::Frame::new()
            .fill(if channel_selected { ROW_SELECTED } else { BG })
            .corner_radius(8)
            .inner_margin(egui::Margin::symmetric(10, 8))
            .show(ui, |ui| {
                ui.label(
                    egui::RichText::new(format!(
                        "{} / {}",
                        if channel_selected { ">" } else { " " },
                        "canal commun"
                    ))
                    .color(if channel_selected { GREEN } else { TEXT }),
                );
            })
            .response;
        let channel_click = ui
            .interact(
                channel_row.rect,
                egui::Id::new("channel_row"),
                egui::Sense::click(),
            )
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        if channel_click.clicked() {
            self.selected = Selection::Broadcast;
            self.focus_composer = true;
        }
        ui.add_space(6.0);
        ui.label(egui::RichText::new("PEERS").small().color(TEXT_TERTIARY));
        ui.add_space(2.0);

        let mut pick: Option<Selection> = None;
        egui::ScrollArea::vertical()
            .id_salt("peers")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for peer in &self.peers {
                    let selected = self.selected == Selection::Peer(peer.peer_id.clone());
                    let dot = if peer.has_session {
                        GREEN
                    } else if peer.verified {
                        AMBER
                    } else {
                        TEXT_TERTIARY
                    };
                    let row = egui::Frame::new()
                        .fill(if selected { ROW_SELECTED } else { BG })
                        .corner_radius(8)
                        .inner_margin(egui::Margin::symmetric(10, 8))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                status_dot(ui, dot);
                                ui.label(
                                    egui::RichText::new(format!("@{}", peer.nickname))
                                        .color(if selected { GREEN } else { TEXT }),
                                );
                                if peer.has_session {
                                    ui.label(
                                        egui::RichText::new("[tls]")
                                            .small()
                                            .color(TEXT_TERTIARY),
                                    );
                                }
                            });
                        })
                        .response;
                    let click = ui
                        .interact(
                            row.rect,
                            egui::Id::new(("peer_row", peer.peer_id.clone())),
                            egui::Sense::click(),
                        )
                        .on_hover_cursor(egui::CursorIcon::PointingHand);
                    if click.clicked() {
                        pick = Some(Selection::Peer(peer.peer_id.clone()));
                    }
                    row.on_hover_text(peer.peer_id.clone());
                }
            });
        if let Some(selection) = pick {
            self.selected = selection;
            self.focus_composer = true;
        }

        ui.separator();
        egui::CollapsingHeader::new(egui::RichText::new("logs").small().color(TEXT_TERTIARY))
            .default_open(false)
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("logs")
                    .max_height(140.0)
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        for line in &self.logs {
                            ui.label(
                                egui::RichText::new(format!("# {line}"))
                                    .small()
                                    .color(TEXT_TERTIARY),
                            );
                        }
                    });
            });
    }

    fn show_messages(&self, ui: &mut egui::Ui, key: &str) {
        egui::ScrollArea::vertical()
            .stick_to_bottom(true)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let Some(lines) = self.chat.get(key) else {
                    ui.add_space(24.0);
                    ui.vertical_centered(|ui| {
                        ui.label(egui::RichText::new("-- aucun message --").color(TEXT_TERTIARY));
                    });
                    return;
                };
                if lines.is_empty() {
                    ui.add_space(24.0);
                    ui.vertical_centered(|ui| {
                        ui.label(egui::RichText::new("-- aucun message --").color(TEXT_TERTIARY));
                    });
                }
                for line in lines {
                    match self.theme {
                        ChatTheme::Bubbles => self.show_bubble_line(ui, line),
                        ChatTheme::Matrix => self.show_matrix_line(ui, line),
                    }
                    ui.add_space(2.0);
                }
            });
    }

    /// Message body width for the meta-time placement calculation.
    fn content_galley(&self, ui: &egui::Ui, line: &ChatLine, wrap_width: f32) -> ContentSize {
        match &line.content {
            ChatContent::Text(text) => {
                let galley = ui.fonts(|f| f.layout(text.clone(), body_font(ui), TEXT, wrap_width));
                let last_right = galley
                    .rows
                    .last()
                    .map(|row| row.rect.right())
                    .unwrap_or(0.0);
                ContentSize {
                    width: galley.size().x,
                    height: galley.size().y,
                    last_right,
                }
            }
            ChatContent::File {
                name, size, mime, ..
            } => {
                if let Some(image) = self.image_cache.get(&line.id) {
                    // Same scaling rule as show_content, so the hug width
                    // matches the rendered image.
                    let [w, h] = image.size;
                    let scale = (360.0f32 / w as f32)
                        .min(260.0f32 / h as f32)
                        .min(1.0);
                    let width = (w as f32 * scale).min(wrap_width);
                    let height = h as f32 * scale;
                    ContentSize {
                        width,
                        height,
                        last_right: width,
                    }
                } else {
                    let text = format!("📎 {name} · {} · {mime}", human_size(*size));
                    let galley = ui.fonts(|f| f.layout(text, body_font(ui), TEXT, wrap_width));
                    // Chip + the trailing "ouvrir" button.
                    let width = galley.size().x + 64.0;
                    ContentSize {
                        width,
                        height: galley.size().y,
                        last_right: width,
                    }
                }
            }
        }
    }

    /// Bubbles layout, mirroring the mobile `BubbleTextMessageLayout`: own
    /// messages right (no name), others left with their `@nickname` on its
    /// own line, capped at 80% of the row (the opposite side keeps a 20%
    /// breathing margin), with a greyed dynamic timestamp riding the last
    /// text line bottom-aligned when there is room, right-aligned on its own
    /// line otherwise.
    fn show_bubble_line(&self, ui: &mut egui::Ui, line: &ChatLine) {
        let author_color = if line.mine {
            AMBER
        } else {
            peer_color(&line.author_id)
        };
        let fill = author_color.gamma_multiply(0.18);
        let border = author_color.gamma_multiply(0.38);
        let time = format_chat_time(line.ts_millis);
        let time_color = TEXT_TERTIARY;

        let row_width = ui.available_width();
        let max_bubble = (row_width * BUBBLE_MAX_FRACTION).max(140.0);
        let inner_max = (max_bubble - BUBBLE_PAD_H * 2.0).max(60.0);
        let time_font = small_font(ui);
        let time_w = ui.fonts(|f| f.layout_no_wrap(time.clone(), time_font, time_color).size().x);
        let body = self.content_galley(ui, line, inner_max);
        let same_line = body.last_right + TIME_GAP + time_w <= inner_max;
        // The nickname heads other users' bubbles and can be wider than the
        // body — include it so a long handle never wraps mid-bubble.
        let nick_w = if line.mine {
            0.0
        } else {
            ui.fonts(|f| f.layout_no_wrap(format!("@{}", line.who), body_font(ui), TEXT).size().x)
        };
        // Hug the bubble to its content (capped at the 80% row fraction),
        // like the mobile layout — the Frame would otherwise stretch to the
        // full max width in a top-down layout.
        let content_w = if same_line {
            body.last_right.max(nick_w) + TIME_GAP + time_w
        } else {
            body.width.max(nick_w).max(time_w)
        };
        // Slack: measured galleys under-report rendered text by a few px;
        // without it the timestamp could bleed past the bubble's right edge.
        let bubble_w = (content_w + BUBBLE_PAD_H * 2.0 + 10.0)
            .max(time_w + BUBBLE_PAD_H * 2.0 + 10.0)
            .min(max_bubble);

        // Full-width row; the bubble hugs the speaker's edge (top_down keeps
        // the Frame's contents vertical — nick and body on separate lines).
        let cross = if line.mine {
            egui::Align::RIGHT
        } else {
            egui::Align::LEFT
        };
        ui.with_layout(egui::Layout::top_down(cross), |ui| {
            ui.set_max_width(bubble_w);
            egui::Frame::new()
                .fill(fill)
                .stroke(egui::Stroke::new(1.0, border))
                .corner_radius(egui::CornerRadius::same(16))
                .inner_margin(egui::Margin::symmetric(
                    BUBBLE_PAD_H as i8,
                    BUBBLE_PAD_V as i8,
                ))
                .show(ui, |ui| {
                    ui.set_max_width((bubble_w - BUBBLE_PAD_H * 2.0).max(60.0));
                    // Own bubbles never show a name — the end side is
                    // attribution enough (Android parity).
                    if !line.mine {
                        ui.label(
                            egui::RichText::new(format!("@{}", line.who))
                                .strong()
                                .color(author_color),
                        );
                        ui.add_space(2.0);
                    }
                    if same_line {
                        // Timestamp rides the last body line: explicit
                        // fixed-size left-to-right band (an inherited
                        // right-aligned layout would flip the order).
                        let band_w = (body.last_right + TIME_GAP + time_w + 8.0)
                            .min((bubble_w - BUBBLE_PAD_H * 2.0 - 2.0).max(60.0));
                        ui.allocate_ui_with_layout(
                            egui::vec2(band_w, body.height.max(14.0)),
                            egui::Layout::left_to_right(egui::Align::BOTTOM),
                            |ui| {
                                self.show_content(ui, line);
                                ui.add_space(TIME_GAP);
                                ui.label(
                                    egui::RichText::new(&time).small().color(time_color),
                                );
                            },
                        );
                    } else {
                        self.show_content(ui, line);
                        // Fixed-size row: right-aligned timestamp without the
                        // vertical stretch of `with_layout`.
                        ui.allocate_ui_with_layout(
                            egui::vec2(
                                (bubble_w - BUBBLE_PAD_H * 2.0).max(60.0),
                                14.0,
                            ),
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| {
                                ui.label(egui::RichText::new(&time).small().color(time_color));
                            },
                        );
                    }
                });
        });
    }

    /// Matrix layout, mirroring the mobile terminal-style transcript:
    /// `@nickname` then the message below, everything left-aligned, with the
    /// timestamp greyed and smaller immediately right of the text block,
    /// bottom-aligned. The row is allocated at its measured size: a plain
    /// `with_layout` band would stretch across the viewport.
    fn show_matrix_line(&self, ui: &mut egui::Ui, line: &ChatLine) {
        let author_color = if line.mine {
            AMBER
        } else {
            peer_color(&line.author_id)
        };
        let time = format_chat_time(line.ts_millis);
        let (time_w, time_h) = ui.fonts(|f| {
            let galley = f.layout_no_wrap(time.clone(), small_font(ui), TEXT_TERTIARY);
            (galley.size().x, galley.size().y)
        });
        let body = self.content_galley(ui, line, f32::INFINITY);
        let nick_text = format!("@{}", line.who);
        let (nick_w, nick_h) = ui.fonts(|f| {
            let galley = f.layout_no_wrap(nick_text.clone(), body_font(ui), TEXT);
            (galley.size().x, galley.size().y)
        });
        let column_w = nick_w.max(body.width);
        let column_h = nick_h + 1.0 + body.height;
        let row_w = 4.0 + column_w + 10.0 + time_w;
        let row_h = column_h.max(time_h);
        ui.allocate_ui_with_layout(
            egui::vec2(row_w, row_h),
            egui::Layout::left_to_right(egui::Align::BOTTOM),
            |ui| {
                ui.add_space(4.0);
                ui.vertical(|ui| {
                    ui.label(egui::RichText::new(nick_text).strong().color(author_color));
                    ui.add_space(1.0);
                    self.show_content(ui, line);
                });
                ui.add_space(10.0);
                ui.label(egui::RichText::new(time).small().color(TEXT_TERTIARY));
            },
        );
    }

    /// Render a message body: plain text, inline image, or file chip.
    fn show_content(&self, ui: &mut egui::Ui, line: &ChatLine) {
        match &line.content {
            ChatContent::Text(text) => {
                // Force wrapping: in a horizontal row (timestamp riding the
                // last line) egui defaults labels to `Extend`, which would
                // grow a single line past the window instead of wrapping.
                ui.add(
                    egui::Label::new(egui::RichText::new(text).color(TEXT)).wrap(),
                );
            }
            ChatContent::File {
                name,
                size,
                mime,
                bytes,
                saved,
            } => {
                let open_target = saved.clone().or_else(|| {
                    // Fallback for lines saved before the path was tracked:
                    // persist a copy now, then open it.
                    self.save_download(name, bytes)
                });
                if let Some(image) = self.image_cache.get(&line.id) {
                    let max_w = 360.0f32;
                    let max_h = 260.0f32;
                    let [w, h] = image.size;
                    let scale = (max_w / w as f32).min(max_h / h as f32).min(1.0);
                    let display = egui::vec2(w as f32 * scale, h as f32 * scale);
                    let sized =
                        egui::load::SizedTexture::new(image.texture.id(), display);
                    let response = ui
                        .add(egui::Image::new(sized).corner_radius(egui::CornerRadius::same(8)))
                        .on_hover_text(format!("{name} — cliquer pour ouvrir"))
                        .on_hover_cursor(egui::CursorIcon::PointingHand);
                    if response.clicked() {
                        if let Some(path) = open_target {
                            Self::open_path(&path);
                        }
                    }
                } else {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("📎").size(16.0));
                        ui.vertical(|ui| {
                            ui.label(egui::RichText::new(name).color(TEXT));
                            ui.label(
                                egui::RichText::new(format!("{} · {mime}", human_size(*size)))
                                    .small()
                                    .color(TEXT_TERTIARY),
                            );
                        });
                        if ui
                            .small_button("ouvrir")
                            .on_hover_cursor(egui::CursorIcon::PointingHand)
                            .clicked()
                        {
                            if let Some(path) = open_target.clone() {
                                Self::open_path(&path);
                            }
                        }
                    });
                }
            }
        }
    }
    /// About / Settings window mirroring the Android About sheet tabs.
    /// About / Settings window mirroring the Android About sheet tabs.
    fn show_settings_window(&mut self, ctx: &egui::Context) {
        let identity_path = PathBuf::from(
            std::env::var("BITCHATD_IDENTITY").unwrap_or_else(|_| "bitchatd.identity".to_string()),
        );
        let mut open = self.show_settings;
        egui::Window::new("bitchatd · settings")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(600.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    for (tab, label) in [
                        (SettingsTab::Info, "Info"),
                        (SettingsTab::Settings, "Settings"),
                    ] {
                        let selected = self.settings_tab == tab;
                        if ui
                            .selectable_label(selected, label)
                            .on_hover_cursor(egui::CursorIcon::PointingHand)
                            .clicked()
                            && !selected
                        {
                            self.settings_tab = tab;
                        }
                    }
                });
                ui.separator();

                match self.settings_tab {
                    SettingsTab::Info => {
                        section_label(ui, "About");
                        settings_row(
                            ui,
                            GREEN,
                            "Offline Communication",
                            "Chat over a Bluetooth mesh with no internet and no servers. \
                             Messages relay through nearby devices to extend range.",
                            "",
                            true,
                        );
                        settings_row(
                            ui,
                            GREEN,
                            "End-to-End Encryption",
                            "Private messages are encrypted. Channel messages are public.",
                            "",
                            true,
                        );
                        settings_row(
                            ui,
                            TEXT_TERTIARY,
                            "Online Geohash",
                            "Location-based channels over Nostr relays. Not available on desktop.",
                            "unsupported",
                            false,
                        );

                        section_label(ui, "How To Use");
                        for hint in [
                            "Set your nickname by clicking it in the header",
                            "Switch between the common channel and private messages in the list",
                            "Type a message and press Enter to send",
                            "Broadcasts reach the whole mesh; private messages use a Noise session",
                        ] {
                            ui.horizontal(|ui| {
                                status_dot(ui, TEXT_TERTIARY);
                                ui.add_space(6.0);
                                ui.label(egui::RichText::new(hint).small().color(TEXT));
                            });
                            ui.add_space(4.0);
                        }
                    }
                    SettingsTab::Settings => {
                        section_label(ui, "Settings");
                        // Chat layout toggle, persisted in bitchatd.toml — the
                        // same Bubbles/Matrix choice the mobile app offers.
                        settings_row_frame(ui, |ui| {
                            // Same fixed-zone geometry as settings_row: the
                            // segmented toggle occupies a measured right
                            // zone, the text block the rest (see settings_row).
                            let font = body_font(ui);
                            let pad = ui.spacing().button_padding.x;
                            let seg = ui.fonts(|f| {
                                ["Bubbles", "Matrix"].iter().map(|t| f.layout_no_wrap(t.to_string(), font.clone(), TEXT).size().x).sum::<f32>()
                                    + 4.0 * pad
                                    + ui.spacing().item_spacing.x
                            });
                            let gap = 10.0;
                            let left_w = (ui.available_width() - seg - gap).max(60.0);
                            ui.horizontal(|ui| {
                                status_dot(ui, GREEN);
                                ui.add_space(6.0);
                                ui.allocate_ui_with_layout(
                                    egui::vec2(left_w, 0.0),
                                    egui::Layout::top_down(egui::Align::LEFT),
                                    |ui| {
                                        ui.label(
                                            egui::RichText::new("Chat Layout").color(TEXT).strong(),
                                        );
                                        ui.add(
                                            egui::Label::new(
                                                egui::RichText::new(
                                                    "Messenger bubbles, or the flat terminal transcript.",
                                                )
                                                .small()
                                                .color(TEXT_TERTIARY),
                                            )
                                            .wrap(),
                                        );
                                    },
                                );
                                let remaining = ui.available_width();
                                ui.allocate_space(egui::vec2((remaining - seg).max(0.0), 1.0));
                                let mut theme = self.theme;
                                for (label, value) in [
                                    ("Bubbles", ChatTheme::Bubbles),
                                    ("Matrix", ChatTheme::Matrix),
                                ] {
                                    if ui
                                        .selectable_label(theme == value, label)
                                        .on_hover_cursor(egui::CursorIcon::PointingHand)
                                        .clicked()
                                        && theme != value
                                    {
                                        theme = value;
                                    }
                                }
                                if theme != self.theme {
                                    self.theme = theme;
                                    self.save_theme(&identity_path);
                                }
                            });
                        });
                        ui.add_space(8.0);
                        settings_row(
                            ui,
                            BLUE,
                            "Run in Background",
                            "Keep the mesh node running when the window is closed (detached process).",
                            "ON",
                            false,
                        );
                        settings_row(
                            ui,
                            AMBER,
                            "Live push-to-talk",
                            "Play voice bursts live on the mesh; voice notes are always sent on release.",
                            "not available on desktop",
                            false,
                        );
                        settings_row(
                            ui,
                            AMBER,
                            "Proof of Work",
                            "Add proof of work to geohash messages for spam deterrence.",
                            "not available on desktop",
                            false,
                        );
                        settings_row(
                            ui,
                            TEXT_TERTIARY,
                            "Tor Network",
                            "Route internet over Tor for enhanced privacy.",
                            "not available on desktop",
                            false,
                        );
                        ui.add_space(8.0);
                        ui.separator();
                        // Same fixed-zone geometry as settings_row (see
                        // settings_row); the whole row is the click target.
                        let state = "open ▸";
                        let state_font = small_font(ui);
                        let right_w = ui.fonts(|f| {
                            f.layout_no_wrap(state.to_string(), state_font, GREEN).size().x
                        });
                        let gap = 10.0;
                        let debug_row = settings_row_frame(ui, |ui| {
                            let left_w = (ui.available_width() - right_w - gap).max(60.0);
                            ui.horizontal(|ui| {
                                status_dot(ui, GREEN);
                                ui.add_space(6.0);
                                ui.allocate_ui_with_layout(
                                    egui::vec2(left_w, 0.0),
                                    egui::Layout::top_down(egui::Align::LEFT),
                                    |ui| {
                                        ui.label(
                                            egui::RichText::new("Debug Settings")
                                                .color(TEXT)
                                                .strong(),
                                        );
                                        ui.add(
                                            egui::Label::new(
                                                egui::RichText::new(
                                                    "Diagnostics, transports, sync parameters and console",
                                                )
                                                .small()
                                                .color(TEXT_TERTIARY),
                                            )
                                            .wrap(),
                                        );
                                    },
                                );
                                let remaining = ui.available_width();
                                ui.allocate_space(egui::vec2((remaining - right_w).max(0.0), 1.0));
                                ui.label(egui::RichText::new(state).small().color(GREEN));
                            });
                        });
                        let debug_click = ui
                            .interact(
                                debug_row,
                                egui::Id::new("debug_row"),
                                egui::Sense::click(),
                            )
                            .on_hover_cursor(egui::CursorIcon::PointingHand);
                        if debug_click.clicked() {
                            self.show_debug = true;
                        }
                        debug_click.on_hover_text("open the debug settings");
                    }
                }
            });
        self.show_settings = open;
    }

    /// Debug sheet mirroring the Android Debug Settings sections.
    fn show_debug_window(&mut self, ctx: &egui::Context) {
        let mut open = self.show_debug;
        egui::Window::new("bitchatd · debug settings")
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_width(620.0)
            .default_height(520.0)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    section_label(ui, "Logging");
                    settings_row(
                        ui,
                        BLUE,
                        "Verbose logging",
                        "Hex packet dumps; enabled by the launcher (BITCHATD_DEBUG_HEX).",
                        "ON",
                        false,
                    );

                    section_label(ui, "Mesh topology");
                    let sessions = self
                        .peers
                        .iter()
                        .filter(|peer| peer.has_session)
                        .count();
                    let verified = self.peers.iter().filter(|peer| peer.verified).count();
                    settings_row(
                        ui,
                        GREEN,
                        "Gossip",
                        if self.peers.is_empty() {
                            "No gossip yet".to_string()
                        } else {
                            format!(
                                "{} peer(s) · {} verified · {} encrypted session(s)",
                                self.peers.len(),
                                verified,
                                sessions
                            )
                        }
                        .as_str(),
                        "",
                        !self.peers.is_empty(),
                    );

                    section_label(ui, "Bluetooth roles");
                    settings_row(
                        ui,
                        BLUE,
                        "GATT server",
                        "Advertises the mesh service and accepts incoming BLE connections.",
                        "ON",
                        false,
                    );
                    settings_row(
                        ui,
                        BLUE,
                        "GATT client",
                        format!("Auto-connects to discovered peers ({} known).", self.peers.len())
                            .as_str(),
                        "ON",
                        false,
                    );

                    section_label(ui, "Transports");
                    settings_row(ui, GREEN, "BLE", "Bluetooth Low Energy mesh transport.", "ON", false);
                    settings_row(
                        ui,
                        TEXT_TERTIARY,
                        "Wi-Fi Aware",
                        "Wi-Fi Aware is not supported on this platform.",
                        "unsupported",
                        false,
                    );

                    section_label(ui, "Packet relay");
                    settings_row(
                        ui,
                        GREEN,
                        "Relay",
                        "Verified public packets are relayed to extend the mesh.",
                        "ON",
                        false,
                    );

                    section_label(ui, "Sync settings");
                    settings_row(
                        ui,
                        BLUE,
                        "GCS filter",
                        format!(
                            "seen capacity {} · filter {} bytes · target fpr {:.1}%",
                            bitchat_wire::gcs::SEEN_CAPACITY,
                            bitchat_wire::gcs::DEFAULT_FILTER_BYTES,
                            bitchat_wire::gcs::DEFAULT_FPR * 100.0
                        )
                        .as_str(),
                        "",
                        true,
                    );

                    section_label(ui, "Connected devices");
                    if self.peers.is_empty() {
                        ui.label(
                            egui::RichText::new("None")
                                .small()
                                .color(TEXT_TERTIARY),
                        );
                    }
                    for peer in &self.peers {
                        let dot = if peer.has_session {
                            GREEN
                        } else if peer.verified {
                            AMBER
                        } else {
                            TEXT_TERTIARY
                        };
                        settings_row(
                            ui,
                            dot,
                            format!("@{} · {}", peer.nickname, &peer.peer_id[..8]).as_str(),
                            if peer.has_session {
                                "session chiffree [tls]"
                            } else if peer.verified {
                                "verified, handshake pending"
                            } else {
                                "unverified announce"
                            },
                            "",
                            true,
                        );
                    }

                    section_label(ui, "Recent scan results");
                    if self.scan.is_empty() {
                        ui.label(
                            egui::RichText::new("None").small().color(TEXT_TERTIARY),
                        );
                    }
                    for entry in self.scan.clone() {
                        let known = self.peers.iter().any(|peer| {
                            entry
                                .peer_id
                                .as_deref()
                                .is_some_and(|id| peer.peer_id == id)
                        });
                        let title = format!(
                            "{} • {}",
                            entry.peer_id.as_deref().unwrap_or("unknown"),
                            entry.address
                        );
                        let subtitle = format!(
                            "RSSI: {} · seen {}",
                            entry
                                .rssi
                                .map_or("?".to_string(), |value| value.to_string()),
                            entry.last_seen
                        );
                        egui::Frame::new()
                            .fill(SURFACE)
                            .corner_radius(8)
                            .inner_margin(egui::Margin::symmetric(12, 8))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    status_dot(
                                        ui,
                                        if known { GREEN } else { TEXT_TERTIARY },
                                    );
                                    ui.add_space(6.0);
                                    ui.vertical(|ui| {
                                        ui.label(
                                            egui::RichText::new(title)
                                                .color(TEXT)
                                                .strong(),
                                        );
                                        ui.label(
                                            egui::RichText::new(subtitle)
                                                .small()
                                                .color(TEXT_TERTIARY),
                                        );
                                    });
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            if ui
                                                .add(
                                                    egui::Button::new(
                                                        egui::RichText::new("Connect")
                                                            .small()
                                                            .color(BG),
                                                    )
                                                    .fill(if known {
                                                        TEXT_TERTIARY
                                                    } else {
                                                        GREEN
                                                    })
                                                    .corner_radius(6),
                                                )
                                                .on_hover_text(if known {
                                                    "already connected"
                                                } else {
                                                    "force an outgoing connection (like the mobile debug Connect)"
                                                })
                                                .on_hover_cursor(egui::CursorIcon::PointingHand)
                                                .clicked()
                                                && !known
                                            {
                                                let _ = self.cmd_out.send(UiCommand::Connect {
                                                    address: entry.address.clone(),
                                                });
                                                self.log(format!(
                                                    "connexion forcee vers {}",
                                                    entry.address
                                                ));
                                            }
                                        },
                                    );
                                });
                            });
                        ui.add_space(4.0);
                    }

                    section_label(ui, "Debug console");
                    egui::Frame::new()
                        .fill(SURFACE)
                        .corner_radius(8)
                        .inner_margin(egui::Margin::symmetric(10, 8))
                        .show(ui, |ui| {
                            egui::ScrollArea::vertical()
                                .id_salt("debug_console")
                                .max_height(200.0)
                                .stick_to_bottom(true)
                                .show(ui, |ui| {
                                    for line in &self.logs {
                                        ui.label(
                                            egui::RichText::new(format!("# {line}"))
                                                .small()
                                                .color(TEXT_TERTIARY),
                                        );
                                    }
                                });
                        });
                });
            });
        self.show_debug = open;
    }
}

impl eframe::App for BitchatApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.set_cursor_icon(egui::CursorIcon::Default);
        if let Some(signal) = self.start_signal.take() {
            // winit is fully initialized now; bring the mesh engine up.
            let _ = signal.send(());
        }
        while let Ok(event) = self.events_in.try_recv() {
            self.apply(ctx, event);
        }
        ctx.request_repaint_after(Duration::from_millis(300));


        // Desktop notifications for incoming messages while unfocused (opt-in:
        // toasts steal OS focus, so they are rate-limited and muted briefly
        // after the window regains focus).
        if self.notifications_enabled && !self.pending_notifs.is_empty() {
            let focused = ctx.input(|input| input.focused);
            let now = std::time::Instant::now();
            if focused {
                self.pending_notifs.clear();
                self.notif_mute_until = Some(now + Duration::from_secs(5));
            } else {
                let muted = self
                    .notif_mute_until
                    .is_some_and(|until| now < until);
                let due = self
                    .last_notif
                    .is_none_or(|last| last.elapsed() >= Duration::from_secs(15));
                if !muted && due {
                    let count = self.pending_notifs.len();
                    let (who, text) = self.pending_notifs.last().cloned().unwrap();
                    self.pending_notifs.clear();
                    let body = if count > 1 {
                        format!(
                            "{count} new messages · last {who}: {}",
                            truncate_chars(&text, 90)
                        )
                    } else {
                        format!("{who}: {}", truncate_chars(&text, 120))
                    };
                    notify_rust::Notification::new()
                        .summary("bitchatd")
                        .body(&body)
                        .show()
                        .ok();
                    self.last_notif = Some(now);
                }
            }
        }

        let identity_path = PathBuf::from(
            std::env::var("BITCHATD_IDENTITY").unwrap_or_else(|_| "bitchatd.identity".to_string()),
        );

        // Terminal-style status bar: ⣿ / @nick   ·····   ⟨⟨ ⟩⟩ mesh   👥 n
        egui::TopBottomPanel::top("top_bar").show(ctx, |ui| {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                self.show_nickname_editor(ui, &identity_path);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add(
                            egui::Button::new(
                                egui::RichText::new("⚙").heading().color(TEXT),
                            )
                            .fill(BG)
                            .corner_radius(6),
                        )
                        .on_hover_text("settings & about")
                        .on_hover_cursor(egui::CursorIcon::PointingHand)
                        .clicked()
                    {
                        self.show_settings = true;
                    }
                    ui.add_space(10.0);
                    ui.label(
                        egui::RichText::new(format!("👥 {}", self.peers.len()))
                            .heading()
                            .color(BLUE),
                    );
                    ui.add_space(14.0);
                    ui.label(egui::RichText::new("mesh").heading().color(BLUE));
                    radio_icon(ui, BLUE);
                    ui.add_space(12.0);
                    ui.label(
                        egui::RichText::new(format!("#{}", &self.our_id[..8]))
                            .small()
                            .color(TEXT_TERTIARY),
                    );
                    ui.add_space(16.0);
                });
            });
            ui.add_space(4.0);
            ui.separator();
        });

        egui::SidePanel::left("peers")
            .resizable(true)
            .default_width(250.0)
            .show(ctx, |ui| {
                self.show_peer_list(ui);
            });

        egui::TopBottomPanel::bottom("composer").show(ctx, |ui| {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui
                    .add(
                        egui::Button::new(egui::RichText::new("📎").size(17.0))
                            .fill(INPUT_SURFACE)
                            .corner_radius(20)
                            .min_size(egui::vec2(42.0, 42.0)),
                    )
                    .on_hover_text("envoyer une image, une vidéo ou un document (≤ 96 Ko sur le mesh v1)")
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .clicked()
                {
                    self.pick_and_send_file(ctx);
                }
                let input = egui::TextEdit::singleline(&mut self.draft)
                    .frame(false) // rounded Frame drawn below
                    .hint_text(
                        egui::RichText::new("Type a message…").color(TEXT_TERTIARY),
                    )
                    .desired_width(f32::INFINITY)
                    .min_size(egui::vec2(0.0, 34.0))
                    .margin(egui::vec2(12.0, 9.0));
                let response = egui::Frame::new()
                    .fill(INPUT_SURFACE)
                    .stroke(egui::Stroke::new(1.0, INPUT_BORDER))
                    .corner_radius(20)
                    .inner_margin(egui::Margin::symmetric(6, 2))
                    .show(ui, |ui| ui.add(input))
                    .inner;
                if self.focus_composer {
                    // Re-grab focus on the frame right after sending or
                    // switching conversation, so typing can continue.
                    self.focus_composer = false;
                    response.request_focus();
                }
                let enter = response.lost_focus()
                    && ui.input(|input| input.key_pressed(egui::Key::Enter));
                let send = ui
                    .add(
                        egui::Button::new(egui::RichText::new("➤").size(17.0))
                            .fill(GREEN)
                            .corner_radius(20)
                            .min_size(egui::vec2(46.0, 42.0)),
                    )
                    .on_hover_text("envoyer (Entree)")
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .clicked();
                if (send || enter) && !self.draft.trim().is_empty() {
                    self.send_draft();
                }
            });
            ui.add_space(8.0);
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            let (key, title, subtitle) = match &self.selected {
                Selection::Broadcast => (
                    CHANNEL_KEY.to_string(),
                    "canal commun".to_string(),
                    "mesh-wide · non chiffre".to_string(),
                ),
                Selection::Peer(peer_id) => {
                    let session = self
                        .peers
                        .iter()
                        .any(|peer| peer.peer_id == *peer_id && peer.has_session);
                    (
                        peer_id.clone(),
                        format!("@{}", self.nickname_of(peer_id)),
                        if session {
                            format!("{} · session chiffree [tls]", &peer_id[..8])
                        } else {
                            format!("{} · en attente de session…", &peer_id[..8])
                        },
                    )
                }
            };
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(title).heading().color(GREEN));
                ui.label(
                    egui::RichText::new(format!("· {subtitle}"))
                        .small()
                        .color(TEXT_TERTIARY),
                );
            });
            ui.separator();
            ui.add_space(2.0);
            self.show_messages(ui, &key);
        });

        if self.show_settings {
            self.show_settings_window(ctx);
        }
        if self.show_debug {
            self.show_debug_window(ctx);
        }

        // No global cursor override: egui already shows the hand over real
        // click targets; `is_pointer_over_area` is true for the whole window
        // (every panel is an "area"), which painted a pointer everywhere.
    }
}

pub fn parse_mac(address: &str) -> Option<[u8; 6]> {
    let parts: Vec<&str> = address.split(':').collect();
    if parts.len() != 6 {
        return None;
    }
    let mut mac = [0u8; 6];
    for (index, part) in parts.iter().enumerate() {
        mac[index] = u8::from_str_radix(part, 16).ok()?;
    }
    Some(mac)
}

fn truncate_chars(text: &str, max: usize) -> String {
    let mut chars = text.chars();
    let truncated: String = chars.by_ref().take(max).collect();
    if chars.next().is_some() {
        format!("{truncated}…")
    } else {
        truncated
    }
}

impl BitchatApp {
    /// Construct the app shell (used by the `node_gui` example and by
    /// headless egui_kittest tests).
    pub fn new(
        events_in: std_mpsc::Receiver<LinkEvent>,
        cmd_out: std_mpsc::Sender<UiCommand>,
        start_signal: Option<std_mpsc::Sender<()>>,
        our_id: String,
        nickname: String,
        identity_path: &Path,
    ) -> Self {
        Self {
            events_in,
            cmd_out,
            start_signal,
            our_id,
            nickname: nickname.clone(),
            nick_draft: nickname,
            editing_nick: false,
            status: "scan BLE en cours…".to_string(),
            peers: Vec::new(),
            logs: std::collections::VecDeque::new(),
            chat: HashMap::new(),
            selected: Selection::Broadcast,
            draft: String::new(),
            show_settings: false,
            settings_tab: SettingsTab::Info,
            show_debug: false,
            focus_composer: false,
            pending_notifs: Vec::new(),
            scan: Vec::new(),
            last_notif: None,
            notif_mute_until: None,
            notifications_enabled: std::env::var_os("BITCHATD_NOTIFY").is_some(),
            theme: Self::load_theme(identity_path),
            image_cache: HashMap::new(),
        }
    }
}

#[cfg(test)]
mod tests;
