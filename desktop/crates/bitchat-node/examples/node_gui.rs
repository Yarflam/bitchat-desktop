//! Desktop UI for the BitChat mesh node (egui), styled after the Android
//! client's dark theme: message bubbles, peer colors, editable nickname.
//!
//! Usage:
//!   cargo run -p bitchat-node --example node_gui

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc as std_mpsc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bitchat_node::{LinkEvent, MeshLink, NodeIdentity};
use eframe::egui;

/// Key used for the mesh-wide channel in the log map.
const CHANNEL_KEY: &str = "";

// Dark theme lifted from the Android app (BitchatPalette / Theme.kt).
const BG: egui::Color32 = egui::Color32::from_rgb(0x00, 0x00, 0x00);
const SURFACE: egui::Color32 = egui::Color32::from_rgb(0x0E, 0x15, 0x0E);
const SURFACE_VARIANT: egui::Color32 = egui::Color32::from_rgb(0x18, 0x21, 0x18);
const INPUT_SURFACE: egui::Color32 = egui::Color32::from_rgb(0x0B, 0x0B, 0x0B);
const GREEN: egui::Color32 = egui::Color32::from_rgb(0x32, 0xD7, 0x4B);
const GREEN_CONTAINER: egui::Color32 = egui::Color32::from_rgb(0x16, 0x3D, 0x1D);
const ON_GREEN_CONTAINER: egui::Color32 = egui::Color32::from_rgb(0xB8, 0xF5, 0xC1);
const TEXT: egui::Color32 = egui::Color32::from_rgb(0xE8, 0xF2, 0xE8);
const TEXT_TERTIARY: egui::Color32 = egui::Color32::from_rgb(0x6B, 0x77, 0x6B);
const ORANGE: egui::Color32 = egui::Color32::from_rgb(0xFF, 0x9F, 0x0A);

fn bitchat_visuals() -> egui::Visuals {
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = SURFACE;
    visuals.window_fill = BG;
    visuals.extreme_bg_color = INPUT_SURFACE;
    visuals.faint_bg_color = SURFACE_VARIANT;
    visuals.override_text_color = Some(TEXT);
    visuals.selection.bg_fill = GREEN_CONTAINER;
    visuals.selection.stroke.color = ON_GREEN_CONTAINER;
    visuals.widgets.noninteractive.bg_fill = SURFACE_VARIANT;
    visuals.widgets.noninteractive.corner_radius = egui::CornerRadius::same(8);
    visuals.widgets.inactive.bg_fill = SURFACE_VARIANT;
    visuals.widgets.hovered.bg_fill = GREEN_CONTAINER;
    visuals.widgets.hovered.weak_bg_fill = GREEN_CONTAINER;
    visuals.widgets.active.bg_fill = GREEN;
    visuals.widgets.active.fg_stroke.color = BG;
    visuals.hyperlink_color = ORANGE;
    visuals
}

/// Stable per-peer color, the same idea as the app's `PeerColors`.
fn peer_color(peer_id: &str) -> egui::Color32 {
    let hue = u32::from_str_radix(&peer_id[..4], 16).unwrap_or(0) as f32 / 65535.0;
    egui::epaint::Hsva {
        h: hue,
        s: 0.55,
        v: 0.95,
        a: 1.0,
    }
    .into()
}

fn now_hms() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() % 86_400)
        .unwrap_or(0);
    format!(
        "{:02}:{:02}:{:02}",
        seconds / 3600,
        (seconds / 60) % 60,
        seconds % 60
    )
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
    peers: Vec<PeerInfo>,
    logs: HashMap<String, Vec<ChatLine>>,
    selected: Selection,
    draft: String,
}

impl BitchatApp {
    fn nickname_path(identity_path: &std::path::Path) -> PathBuf {
        identity_path.with_file_name("bitchatd.nickname")
    }

    fn load_nickname(identity_path: &std::path::Path) -> String {
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

    fn save_nickname(&self, identity_path: &std::path::Path) {
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
                self.status = format!("session chiffree etablie avec {peer_id}");
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
            LinkEvent::Delivered {
                peer_id,
                message_id,
            } => {
                self.status = format!("delivre: {} -> {peer_id}", &message_id[..8]);
            }
            LinkEvent::PeerDisconnected { peer_id } => {
                self.set_session(&peer_id, false);
                self.status = format!("{} deconnecte", self.nickname_of(&peer_id));
            }
            LinkEvent::Info(message) => {
                self.status = message;
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
        self.logs.entry(key.to_string()).or_default().push(ChatLine {
            who: who.to_string(),
            text,
            time: now_hms(),
        });
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
                self.push_line(CHANNEL_KEY, &self.nickname.clone(), text);
            }
            Selection::Peer(peer_id) => {
                let _ = self.cmd_out.send(UiCommand::SendPrivate {
                    peer_id: peer_id.clone(),
                    content: text.clone(),
                });
                self.push_line(&peer_id, &self.nickname.clone(), text);
            }
        }
        self.draft.clear();
    }

    fn show_nickname_editor(&mut self, ui: &mut egui::Ui, identity_path: &std::path::Path) {
        if self.editing_nick {
            ui.horizontal(|ui| {
                let response = ui.add(
                    egui::TextEdit::singleline(&mut self.nick_draft)
                        .desired_width(140.0)
                        .hint_text("pseudo"),
                );
                let enter = response.lost_focus()
                    && ui.input(|input| input.key_pressed(egui::Key::Enter));
                let apply = ui.button("✓").clicked() || enter;
                if ui.button("✕").clicked() {
                    self.editing_nick = false;
                    self.nick_draft = self.nickname.clone();
                }
                if apply {
                    let nickname = self.nick_draft.trim().to_string();
                    if !nickname.is_empty() {
                        self.nickname = nickname.clone();
                        self.save_nickname(identity_path);
                        let _ = self
                            .cmd_out
                            .send(UiCommand::SetNickname { nickname });
                    }
                    self.editing_nick = false;
                }
                response.request_focus();
            });
        } else {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(&self.nickname)
                        .strong()
                        .color(ORANGE),
                );
                if ui
                    .button(egui::RichText::new("✏").small())
                    .on_hover_text("changer le pseudo")
                    .clicked()
                {
                    self.editing_nick = true;
                    self.nick_draft = self.nickname.clone();
                }
            });
        }
    }

    fn show_peer_list(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        let channel_selected = self.selected == Selection::Broadcast;
        let channel_title = "🌐 Canal commun".to_string();
        if ui
            .selectable_label(channel_selected, egui::RichText::new(&channel_title).strong())
            .clicked()
        {
            self.selected = Selection::Broadcast;
        }
        ui.separator();
        ui.label(egui::RichText::new("PEERS").small().color(TEXT_TERTIARY));
        let mut pick: Option<Selection> = None;
        for peer in &self.peers {
            let color = peer_color(&peer.peer_id);
            let selected = self.selected == Selection::Peer(peer.peer_id.clone());
            let title = egui::RichText::new(format!("  {}", peer.nickname.chars().next().unwrap_or('?'))).color(BG).strong();
            let response = ui.horizontal(|ui| {
                let bubble = ui.add(
                    egui::Label::new(title)
                        .selectable(false)
                        .sense(egui::Sense::click()),
                );
                ui.painter().rect_filled(
                    bubble.rect,
                    egui::CornerRadius::same(10),
                    color,
                );
                // Redraw text on top of the colored disc.
                ui.painter().text(
                    bubble.rect.center(),
                    egui::Align2::CENTER_CENTER,
                    peer.nickname.chars().next().unwrap_or('?').to_string(),
                    egui::FontId::proportional(14.0),
                    BG,
                );
                let label = egui::RichText::new(if peer.has_session {
                    format!("{} 🔒", peer.nickname)
                } else {
                    peer.nickname.clone()
                })
                .color(if selected { ON_GREEN_CONTAINER } else { TEXT });
                ui.label(label);
            });
            let row = response.response;
            if row.clicked() {
                pick = Some(Selection::Peer(peer.peer_id.clone()));
            }
            row.on_hover_text(format!(
                "{}\nsignature verifiee: {}\nsession chiffree: {}",
                peer.peer_id, peer.verified, peer.has_session
            ));
            if !peer.verified {
                ui.label(
                    egui::RichText::new("   signature non verifiee")
                        .small()
                        .color(TEXT_TERTIARY),
                );
            }
        }
        if let Some(selection) = pick {
            self.selected = selection;
        }
    }

    fn show_messages(&self, ui: &mut egui::Ui, key: &str, title: &str, subtitle: &str) {
        ui.label(egui::RichText::new(title).heading().strong());
        ui.label(egui::RichText::new(subtitle).small().color(TEXT_TERTIARY));
        ui.add_space(4.0);
        ui.separator();
        egui::ScrollArea::vertical()
            .stick_to_bottom(true)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let lines = self.logs.get(key).cloned().unwrap_or_default();
                for line in &lines {
                    let own = line.who == self.nickname;
                    let align = if own {
                        egui::Layout::right_to_left(egui::Align::Min)
                    } else {
                        egui::Layout::left_to_right(egui::Align::Min)
                    };
                    ui.with_layout(align, |ui| {
                        let (fill, text_color) = if own {
                            (GREEN_CONTAINER, ON_GREEN_CONTAINER)
                        } else {
                            (SURFACE_VARIANT, TEXT)
                        };
                        let who_color = if own {
                            ORANGE
                        } else {
                            peer_color_for_name(key, &line.who, &self.peers)
                        };
                        egui::Frame::new()
                            .fill(fill)
                            .corner_radius(egui::CornerRadius::same(10))
                            .inner_margin(egui::Margin::symmetric(10, 6))
                            .show(ui, |ui| {
                                ui.label(
                                    egui::RichText::new(format!("{}  ", line.who))
                                        .small()
                                        .strong()
                                        .color(who_color),
                                );
                                ui.label(egui::RichText::new(&line.text).color(text_color));
                                ui.label(
                                    egui::RichText::new(format!("  {}", line.time))
                                        .small()
                                        .color(TEXT_TERTIARY),
                                );
                            });
                        ui.add_space(2.0);
                    });
                }
                if lines.is_empty() {
                    ui.label(
                        egui::RichText::new("Aucun message pour le moment.")
                            .color(TEXT_TERTIARY),
                    );
                }
            });
    }
}

fn peer_color_for_name(key: &str, who: &str, peers: &[PeerInfo]) -> egui::Color32 {
    let peer_id = peers
        .iter()
        .find(|peer| peer.nickname == who)
        .map(|peer| peer.peer_id.as_str());
    match peer_id {
        Some(peer_id) => peer_color(peer_id),
        None if key == CHANNEL_KEY => TEXT_TERTIARY,
        None => TEXT,
    }
}

impl eframe::App for BitchatApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        while let Ok(event) = self.events_in.try_recv() {
            self.apply(event);
        }
        ctx.request_repaint_after(Duration::from_millis(300));

        let identity_path = PathBuf::from(
            std::env::var("BITCHATD_IDENTITY").unwrap_or_else(|_| "bitchatd.identity".to_string()),
        );

        egui::TopBottomPanel::top("top_bar").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("bitchatd").strong().color(GREEN));
                ui.separator();
                self.show_nickname_editor(ui, &identity_path);
                ui.separator();
                ui.label(
                    egui::RichText::new(format!("peer {}", &self.our_id[..8]))
                        .small()
                        .color(TEXT_TERTIARY),
                );
            });
            ui.add_space(4.0);
        });

        egui::SidePanel::left("peers")
            .resizable(true)
            .default_width(230.0)
            .show(ctx, |ui| {
                self.show_peer_list(ui);
            });

        egui::TopBottomPanel::bottom("composer").show(ctx, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let response = ui.add(
                    egui::TextEdit::singleline(&mut self.draft)
                        .hint_text("Message…")
                        .desired_width(f32::INFINITY),
                );
                let enter = response.lost_focus()
                    && ui.input(|input| input.key_pressed(egui::Key::Enter));
                let send = ui
                    .add(egui::Button::new(egui::RichText::new("Envoyer").strong()).fill(GREEN))
                    .clicked();
                if (send || enter) && !self.draft.trim().is_empty() {
                    self.send_draft();
                }
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(&self.status).small().color(TEXT_TERTIARY));
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            let (key, title, subtitle) = match &self.selected {
                Selection::Broadcast => (
                    CHANNEL_KEY.to_string(),
                    "Canal commun".to_string(),
                    "mesh-wide, non chiffre".to_string(),
                ),
                Selection::Peer(peer_id) => (
                    peer_id.clone(),
                    self.nickname_of(peer_id),
                    if self
                        .peers
                        .iter()
                        .any(|peer| peer.peer_id == *peer_id && peer.has_session)
                    {
                        format!("{} — session chiffree 🔒", peer_id)
                    } else {
                        format!("{peer_id} — pas de session")
                    },
                ),
            };
            self.show_messages(ui, &key, &title, &subtitle);
        });
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

    // Node engine on its own thread; UI talks to it over plain std channels.
    let (event_tx, event_rx) = std_mpsc::channel::<LinkEvent>();
    let (cmd_tx, cmd_rx) = std_mpsc::channel::<UiCommand>();
    let node_nickname = nickname.clone();
    thread::spawn(move || {
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
        our_id,
        nickname: nickname.clone(),
        nick_draft: nickname,
        editing_nick: false,
        status: "scan BLE en cours…".to_string(),
        peers: Vec::new(),
        logs: HashMap::new(),
        selected: Selection::Broadcast,
        draft: String::new(),
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1024.0, 680.0]),
        ..Default::default()
    };
    eframe::run_native(
        "bitchatd",
        options,
        Box::new(|cc| {
            cc.egui_ctx.set_visuals(bitchat_visuals());
            Ok(Box::new(app))
        }),
    )
}
