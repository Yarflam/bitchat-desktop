//! Desktop UI for the BitChat mesh node (egui), styled after the Android
//! client's retro-terminal mesh theme: monospace type, black background,
//! flat green (peers) / amber (self) containers.
//!
//! Usage:
//!   cargo run -p bitchat-node --example node_gui

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc as std_mpsc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bitchat_node::{LinkEvent, MeshLink, NodeIdentity};
use eframe::egui;

/// Key used for the mesh-wide channel in the log map.
const CHANNEL_KEY: &str = "";

// Retro-terminal palette (see docs/screenshots/readme-mesh-chat.png).
const BG: egui::Color32 = egui::Color32::from_rgb(0x00, 0x00, 0x00);
const GREEN: egui::Color32 = egui::Color32::from_rgb(0x32, 0xD7, 0x4B);
const GREEN_CONTAINER: egui::Color32 = egui::Color32::from_rgb(0x14, 0x33, 0x1B);
const GREEN_BORDER: egui::Color32 = egui::Color32::from_rgb(0x2E, 0x6B, 0x3A);
const AMBER: egui::Color32 = egui::Color32::from_rgb(0xFF, 0x9F, 0x0A);
const AMBER_CONTAINER: egui::Color32 = egui::Color32::from_rgb(0x3A, 0x2A, 0x0E);
const AMBER_BORDER: egui::Color32 = egui::Color32::from_rgb(0x8A, 0x63, 0x1A);
const BLUE: egui::Color32 = egui::Color32::from_rgb(0x0A, 0x84, 0xFF);
const TEXT: egui::Color32 = egui::Color32::from_rgb(0xEA, 0xF2, 0xEA);
const TEXT_TERTIARY: egui::Color32 = egui::Color32::from_rgb(0x8F, 0x9C, 0x8F);
const INPUT_SURFACE: egui::Color32 = egui::Color32::from_rgb(0x0B, 0x0B, 0x0B);
const INPUT_BORDER: egui::Color32 = egui::Color32::from_rgb(0x33, 0x36, 0x35);
const ROW_SELECTED: egui::Color32 = egui::Color32::from_rgb(0x10, 0x1D, 0x12);
const SURFACE: egui::Color32 = egui::Color32::from_rgb(0x0D, 0x10, 0x0D);

fn terminal_style(ctx: &egui::Context) {
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

enum UiCommand {
    SendPrivate { peer_id: String, content: String },
    SendBroadcast { content: String },
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
struct ChatLine {
    who: String,
    text: String,
    time: String,
}

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

struct BitchatApp {
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
}

const MAX_LOG_LINES: usize = 60;

/// Section label in the settings windows, like `AboutSectionLabel`.
fn section_label(ui: &mut egui::Ui, text: &str) {
    ui.add_space(10.0);
    ui.label(egui::RichText::new(text).color(GREEN).strong());
    ui.add_space(4.0);
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
    egui::Frame::new()
        .fill(SURFACE)
        .corner_radius(8)
        .inner_margin(egui::Margin::symmetric(12, 9))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                status_dot(ui, icon);
                ui.add_space(6.0);
                ui.vertical(|ui| {
                    ui.label(egui::RichText::new(title).color(title_color).strong());
                    ui.label(egui::RichText::new(subtitle).small().color(sub_color));
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let state_color = if enabled {
                        if state == "ON" || state == "✓" { GREEN } else { TEXT_TERTIARY }
                    } else {
                        egui::Color32::from_rgb(0x4A, 0x4F, 0x4A)
                    };
                    ui.label(egui::RichText::new(state).small().color(state_color));
                });
            });
        });
    ui.add_space(4.0);
}

impl BitchatApp {
    fn nickname_path(identity_path: &Path) -> PathBuf {
        identity_path.with_file_name("bitchatd.nickname")
    }

    fn load_nickname(identity_path: &Path) -> String {
        if let Ok(nick) = std::env::var("BITCHATD_NICKNAME") {
            if !nick.trim().is_empty() {
                return nick;
            }
        }
        std::fs::read_to_string(Self::nickname_path(identity_path))
            .map(|nick| nick.trim().to_string())
            .ok()
            .filter(|nick| !nick.is_empty())
            .unwrap_or_else(|| "bitchatd".to_string())
    }

    fn save_nickname(&self, identity_path: &Path) {
        let _ = std::fs::write(Self::nickname_path(identity_path), &self.nickname);
    }

    fn apply(&mut self, event: LinkEvent) {
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
                self.pending_notifs
                    .push((format!("@{} (private)", nickname), content.clone()));
                self.push_line(&peer_id, &nickname, content);
            }
            LinkEvent::BroadcastMessage { peer_id, content } => {
                let nickname = self.nickname_of(&peer_id);
                self.pending_notifs
                    .push((format!("@{} (broadcast)", nickname), content.clone()));
                self.push_line(CHANNEL_KEY, &nickname, content);
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
        self.chat.entry(key.to_string()).or_default().push(ChatLine {
            who: who.to_string(),
            text,
            time: now_hm(),
        });
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
        let own_nick = self.nickname.clone();
        match selection {
            Selection::Broadcast => {
                let _ = self
                    .cmd_out
                    .send(UiCommand::SendBroadcast { content: text.clone() });
                self.push_line(CHANNEL_KEY, &own_nick, text);
            }
            Selection::Peer(peer_id) => {
                let _ = self.cmd_out.send(UiCommand::SendPrivate {
                    peer_id: peer_id.clone(),
                    content: text.clone(),
                });
                self.push_line(&peer_id, &own_nick, text);
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
        let channel_click = ui.interact(
            channel_row.rect,
            egui::Id::new("channel_row"),
            egui::Sense::click(),
        );
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
                    let click = ui.interact(
                        row.rect,
                        egui::Id::new(("peer_row", peer.peer_id.clone())),
                        egui::Sense::click(),
                    );
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
                let lines = self.chat.get(key).cloned().unwrap_or_default();
                for line in &lines {
                    let own = line.who == self.nickname;
                    let (fill, border, name_color, time_color) = if own {
                        (
                            AMBER_CONTAINER,
                            AMBER_BORDER,
                            AMBER,
                            egui::Color32::from_rgb(0xC9, 0xA5, 0x5A),
                        )
                    } else {
                        (
                            GREEN_CONTAINER,
                            GREEN_BORDER,
                            GREEN,
                            egui::Color32::from_rgb(0x8F, 0xC9, 0x98),
                        )
                    };
                    let align = if own {
                        egui::Layout::right_to_left(egui::Align::Min)
                    } else {
                        egui::Layout::left_to_right(egui::Align::Min)
                    };
                    ui.with_layout(align, |ui| {
                        egui::Frame::new()
                            .fill(fill)
                            .stroke(egui::Stroke::new(1.0, border))
                            .corner_radius(egui::CornerRadius::same(10))
                            .inner_margin(egui::Margin::symmetric(12, 8))
                            .show(ui, |ui| {
                                ui.set_max_width(520.0);
                                ui.horizontal(|ui| {
                                    ui.label(
                                        egui::RichText::new(format!("@{}", line.who))
                                            .strong()
                                            .color(name_color),
                                    );
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            ui.label(
                                                egui::RichText::new(&line.time)
                                                    .small()
                                                    .color(time_color),
                                            );
                                        },
                                    );
                                });
                                ui.add_space(2.0);
                                ui.label(egui::RichText::new(&line.text).color(TEXT));
                            });
                        ui.add_space(6.0);
                    });
                }
                if lines.is_empty() {
                    ui.add_space(24.0);
                    ui.vertical_centered(|ui| {
                        ui.label(
                            egui::RichText::new("-- aucun message --")
                                .color(TEXT_TERTIARY),
                        );
                    });
                }
            });
    }
    /// About / Settings window mirroring the Android About sheet tabs.
    fn show_settings_window(&mut self, ctx: &egui::Context) {
        let mut open = self.show_settings;
        egui::Window::new("bitchatd · settings")
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_width(560.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    for (tab, label) in [
                        (SettingsTab::Info, "Info"),
                        (SettingsTab::Settings, "Settings"),
                    ] {
                        let selected = self.settings_tab == tab;
                        if ui
                            .selectable_label(selected, label)
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
                        let debug_row = egui::Frame::new()
                            .fill(SURFACE)
                            .corner_radius(8)
                            .inner_margin(egui::Margin::symmetric(12, 9))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    status_dot(ui, GREEN);
                                    ui.add_space(6.0);
                                    ui.vertical(|ui| {
                                        ui.label(
                                            egui::RichText::new("Debug Settings")
                                                .color(TEXT)
                                                .strong(),
                                        );
                                        ui.label(
                                            egui::RichText::new(
                                                "Diagnostics, transports, sync parameters and console",
                                            )
                                            .small()
                                            .color(TEXT_TERTIARY),
                                        );
                                    });
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            ui.label(
                                                egui::RichText::new("open ▸")
                                                    .small()
                                                    .color(GREEN),
                                            );
                                        },
                                    );
                                });
                            })
                            .response;
                        let debug_click = ui.interact(
                            debug_row.rect,
                            egui::Id::new("debug_row"),
                            egui::Sense::click(),
                        );
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
            self.apply(event);
        }
        ctx.request_repaint_after(Duration::from_millis(300));


        // Desktop notifications for incoming messages while unfocused.
        // Toasts steal OS focus, so they are rate-limited (one collapsed toast
        // per burst) and muted briefly after the window regains focus.
        if !self.pending_notifs.is_empty() {
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
                let input = egui::TextEdit::singleline(&mut self.draft)
                    .id(egui::Id::new("composer"))
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
                    // Re-grab focus in the same frame the widget exists, so
                    // typing continues right after sending or switching.
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

        // Hand cursor over any clickable widget.
        if ctx.is_pointer_over_area() {
            ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
        }
    }
}

fn parse_mac(address: &str) -> Option<[u8; 6]> {
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

fn main() -> eframe::Result<()> {
    let identity_path = PathBuf::from(
        std::env::var("BITCHATD_IDENTITY").unwrap_or_else(|_| "bitchatd.identity".to_string()),
    );
    let nickname = BitchatApp::load_nickname(&identity_path);
    let identity = NodeIdentity::load_or_generate(&identity_path)
        .expect("load or generate node identity");
    let our_id = identity.peer_id.clone();
    eprintln!("bitchatd: peer_id={our_id} nickname={nickname}");

    #[cfg(windows)]
    {
        use windows::ApplicationModel::Package;
        match Package::Current() {
            Ok(package) => {
                let id = package.Id().expect("package id");
                eprintln!(
                    "bitchatd: identity {} ({})",
                    id.Name().expect("name"),
                    id.Publisher().expect("publisher")
                );
            }
            Err(error) => eprintln!("bitchatd: NO PACKAGE IDENTITY: {error}"),
        }
    }

    // Node engine on its own thread, started lazily from the first UI frame
    // (after winit finished initializing): starting the WinRT peripheral while
    // winit is still initializing gets the publisher denied (E_ACCESSDENIED).
    let (event_tx, event_rx) = std_mpsc::channel::<LinkEvent>();
    let (cmd_tx, cmd_rx) = std_mpsc::channel::<UiCommand>();
    let node_nickname = nickname.clone();
    let (start_tx, start_rx) = std_mpsc::channel::<()>();
    thread::spawn(move || {
        let _ = start_rx.recv();
        let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
        runtime.block_on(async move {
            let mut link = match MeshLink::start(identity, &node_nickname, 256).await {
                Ok(link) => link,
                Err(error) => {
                    eprintln!("bitchatd: failed to start mesh link: {error}");
                    return;
                }
            };
            let (async_cmd_tx, mut async_cmd_rx) = tokio::sync::mpsc::channel::<UiCommand>(64);
            thread::spawn(move || {
                for command in cmd_rx {
                    if async_cmd_tx.blocking_send(command).is_err() {
                        break;
                    }
                }
            });
            loop {
                tokio::select! {
                    event = link.events.recv() => match event {
                        Some(event) => {
                            // Mirror link events into the log file for debugging.
                            match &event {
                                LinkEvent::Info(message) => eprintln!("[info] {message}"),
                                LinkEvent::PeerAnnounced { peer_id, nickname, verified, .. } => {
                                    eprintln!("[announce] {nickname} ({peer_id}) verified={verified}")
                                }
                                LinkEvent::SessionEstablished { peer_id } => {
                                    eprintln!("[session] established with {peer_id}")
                                }
                                LinkEvent::PrivateMessage { peer_id, content, .. } => {
                                    eprintln!("[chat] private from {peer_id}: {content}")
                                }
                                LinkEvent::BroadcastMessage { peer_id, content } => {
                                    eprintln!("[chat] broadcast from {peer_id}: {content}")
                                }
                                LinkEvent::Delivered { peer_id, .. } => {
                                    eprintln!("[chat] delivered -> {peer_id}")
                                }
                                LinkEvent::PeerStateSent { peer_id } => {
                                    eprintln!("[peerstate] sent to {peer_id}")
                                }
                                LinkEvent::PeerState {
                                    peer_id,
                                    capabilities,
                                    signing_public_key,
                                } => {
                                    eprintln!(
                                        "[peerstate] from {peer_id} caps={capabilities:#x} key={signing_public_key}"
                                    )
                                }
                                LinkEvent::PeerDisconnected { peer_id } => {
                                    eprintln!("[link] {peer_id} disconnected")
                                }
                                LinkEvent::PeerDiscovered { address, peer_id, rssi } => {
                                    eprintln!("[scan] {address} peer={peer_id:?} rssi={rssi:?}")
                                }
                            }
                            if event_tx.send(event).is_err() {
                                break;
                            }
                        }
                        None => break,
                    },
                    command = async_cmd_rx.recv() => match command {
                        Some(UiCommand::SendPrivate { peer_id, content }) => {
                            if let Err(error) = link.send_private_message(&peer_id, &content).await
                            {
                                eprintln!("bitchatd: send private to {peer_id} failed: {error}");
                            }
                        }
                        Some(UiCommand::SendBroadcast { content }) => {
                            if let Err(error) = link.send_broadcast_message(&content).await {
                                eprintln!("bitchatd: send broadcast failed: {error}");
                            }
                        }
                        Some(UiCommand::SetNickname { nickname }) => {
                            if let Err(error) = link.set_nickname(&nickname).await {
                                eprintln!("bitchatd: set nickname failed: {error}");
                            }
                        }
                        Some(UiCommand::Connect { address }) => {
                            let parsed = parse_mac(&address);
                            match parsed {
                                Some(mac) => {
                                    if let Err(error) = link.connect_by_address(mac).await {
                                        eprintln!("bitchatd: connect to {address} failed: {error}");
                                    }
                                }
                                None => eprintln!("bitchatd: invalid MAC address '{address}'"),
                            }
                        }
                        None => break,
                    },
                }
            }
        });
    });

    let app = BitchatApp {
        events_in: event_rx,
        cmd_out: cmd_tx,
        start_signal: Some(start_tx),
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
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1160.0, 760.0])
            .with_min_inner_size([900.0, 600.0]),
        ..Default::default()
    };
    eframe::run_native(
        "bitchatd",
        options,
        Box::new(|cc| {
            terminal_style(&cc.egui_ctx);
            Ok(Box::new(app))
        }),
    )
}
