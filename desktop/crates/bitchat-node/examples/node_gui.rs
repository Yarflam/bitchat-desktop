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

#[derive(Clone, PartialEq)]
enum Selection {
    Broadcast,
    Peer(String),
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
}

const MAX_LOG_LINES: usize = 60;

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
                self.push_line(&peer_id, &nickname, content);
            }
            LinkEvent::BroadcastMessage { peer_id, content } => {
                let nickname = self.nickname_of(&peer_id);
                self.push_line(CHANNEL_KEY, &nickname, content);
            }
            LinkEvent::Delivered { peer_id, .. } => {
                self.log(format!("delivre a {} ✓", self.nickname_of(&peer_id)));
            }
            LinkEvent::PeerDisconnected { peer_id } => {
                self.set_session(&peer_id, false);
                self.log(format!("{} deconnecte", self.nickname_of(&peer_id)));
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

        let identity_path = PathBuf::from(
            std::env::var("BITCHATD_IDENTITY").unwrap_or_else(|_| "bitchatd.identity".to_string()),
        );

        // Terminal-style status bar: ⣿ / @nick   ·····   ⟨⟨ ⟩⟩ mesh   👥 n
        egui::TopBottomPanel::top("top_bar").show(ctx, |ui| {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                self.show_nickname_editor(ui, &identity_path);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
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

        // Hand cursor over any clickable widget.
        if ctx.is_pointer_over_area() {
            ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
        }
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
                                LinkEvent::PeerDisconnected { peer_id } => {
                                    eprintln!("[link] {peer_id} disconnected")
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
