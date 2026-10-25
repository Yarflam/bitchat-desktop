//! Desktop UI for the BitChat mesh node (egui).
//!
//! Same engine as `node_chat` (BLE central + GATT server, Noise XX, gossip
//! sync) with a chat window: peer list on the left, the mesh-wide channel plus
//! one conversation per peer, and an input box at the bottom.
//!
//! Usage:
//!   cargo run -p bitchat-node --example node_gui

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc as std_mpsc;
use std::thread;
use std::time::Duration;

use bitchat_node::{LinkEvent, MeshLink, NodeIdentity};
use eframe::egui;

/// Key used for the mesh-wide channel in the log map.
const CHANNEL_KEY: &str = "";

enum UiCommand {
    SendPrivate { peer_id: String, content: String },
    SendBroadcast { content: String },
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
    status: String,
    peers: Vec<PeerInfo>,
    logs: HashMap<String, Vec<ChatLine>>,
    selected: Selection,
    draft: String,
}

impl BitchatApp {
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
            LinkEvent::Delivered { peer_id, message_id } => {
                self.status = format!("delivre: {message_id} -> {peer_id}");
            }
            LinkEvent::PeerDisconnected { peer_id } => {
                self.set_session(&peer_id, false);
                self.status = format!("{peer_id} deconnecte");
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
            .map_or_else(|| peer_id[..peer_id.len().min(8)].to_string(), |p| p.nickname.clone())
    }

    fn push_line(&mut self, key: &str, who: &str, text: String) {
        self.logs.entry(key.to_string()).or_default().push(ChatLine {
            who: who.to_string(),
            text,
        });
    }

    fn selected_key(&self) -> String {
        match &self.selected {
            Selection::Broadcast => CHANNEL_KEY.to_string(),
            Selection::Peer(peer_id) => peer_id.clone(),
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
                self.push_line(CHANNEL_KEY, "moi", text);
            }
            Selection::Peer(peer_id) => {
                let _ = self.cmd_out.send(UiCommand::SendPrivate {
                    peer_id: peer_id.clone(),
                    content: text.clone(),
                });
                self.push_line(&peer_id, "moi", text);
            }
        }
        self.draft.clear();
    }
}

impl eframe::App for BitchatApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        while let Ok(event) = self.events_in.try_recv() {
            self.apply(event);
        }
        // Keep polling link events while the window idles.
        ctx.request_repaint_after(Duration::from_millis(300));

        let short_id: String = self.our_id.chars().take(8).collect();
        egui::TopBottomPanel::top("status_bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("bitchatd");
                ui.label(egui::RichText::new(format!("peer {short_id}")).weak());
                ui.separator();
                ui.label(&self.status);
            });
        });

        egui::SidePanel::left("peers")
            .resizable(true)
            .default_width(240.0)
            .show(ctx, |ui| {
                ui.add_space(4.0);
                let channel_selected = self.selected == Selection::Broadcast;
                if ui
                    .selectable_label(channel_selected, "🌐 Canal commun")
                    .clicked()
                {
                    self.selected = Selection::Broadcast;
                }
                ui.separator();
                ui.label(egui::RichText::new("Peers").strong());
                ui.add_space(2.0);
                let mut pick: Option<Selection> = None;
                for peer in &self.peers {
                    let label = format!(
                        "{} {}",
                        if peer.has_session { "🔒" } else { "" },
                        peer.nickname
                    );
                    let selected = self.selected == Selection::Peer(peer.peer_id.clone());
                    let response = ui.selectable_label(selected, label);
                    if response.clicked() {
                        pick = Some(Selection::Peer(peer.peer_id.clone()));
                    }
                    response.on_hover_text(format!(
                        "{}\nverified={} session={}",
                        peer.peer_id, peer.verified, peer.has_session
                    ));
                    if !peer.verified {
                        ui.label(
                            egui::RichText::new("  signature non verifiee")
                                .small()
                                .weak(),
                        );
                    }
                }
                if let Some(selection) = pick {
                    self.selected = selection;
                }
            });

        let key = self.selected_key();
        let title = match &self.selected {
            Selection::Broadcast => "Canal commun (mesh-wide, non chiffre)".to_string(),
            Selection::Peer(peer_id) => format!("Conversation avec {}", self.nickname_of(peer_id)),
        };

        egui::TopBottomPanel::bottom("input").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let response = ui.add(
                    egui::TextEdit::singleline(&mut self.draft)
                        .hint_text("Message… (Entree pour envoyer)")
                        .desired_width(f32::INFINITY),
                );
                let enter = response.lost_focus()
                    && ui.input(|input| input.key_pressed(egui::Key::Enter));
                if (ui.button("Envoyer").clicked() || enter) && !self.draft.trim().is_empty() {
                    self.send_draft();
                }
            });
            ui.add_space(4.0);
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.label(egui::RichText::new(title).heading());
            ui.separator();
            let lines = self.logs.get(&key).cloned().unwrap_or_default();
            egui::ScrollArea::vertical()
                .stick_to_bottom(true)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for line in &lines {
                        let (who, color) = if line.who == "moi" {
                            ("moi", ui.visuals().weak_text_color())
                        } else {
                            (line.who.as_str(), egui::Color32::from_rgb(0x6f, 0xc2, 0x76))
                        };
                        ui.horizontal_wrapped(|ui| {
                            ui.label(egui::RichText::new(format!("{who}:")).strong().color(color));
                            ui.label(&line.text);
                        });
                    }
                    if lines.is_empty() {
                        ui.label(
                            egui::RichText::new("Aucun message pour le moment.")
                                .weak(),
                        );
                    }
                });
        });
    }
}

fn main() -> eframe::Result<()> {
    let identity_path = PathBuf::from(
        std::env::var("BITCHATD_IDENTITY").unwrap_or_else(|_| "bitchatd.identity".to_string()),
    );
    let nickname = std::env::var("BITCHATD_NICKNAME").unwrap_or_else(|_| "bitchatd".to_string());
    let identity = NodeIdentity::load_or_generate(&identity_path)
        .expect("load or generate node identity");
    let our_id = identity.peer_id.clone();

    // Node engine on its own thread; UI talks to it over plain std channels.
    let (event_tx, event_rx) = std_mpsc::channel::<LinkEvent>();
    let (cmd_tx, cmd_rx) = std_mpsc::channel::<UiCommand>();
    thread::spawn(move || {
        let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
        runtime.block_on(async move {
            let mut link = match MeshLink::start(identity, &nickname, 256).await {
                Ok(link) => link,
                Err(error) => {
                    eprintln!("bitchatd: failed to start mesh link: {error}");
                    return;
                }
            };
            // Bridge std commands into the async link without blocking it.
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
                            // The UI drains promptly; a full queue means it is gone.
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
        status: "scan BLE en cours…".to_string(),
        peers: Vec::new(),
        logs: HashMap::new(),
        selected: Selection::Broadcast,
        draft: String::new(),
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([980.0, 640.0]),
        ..Default::default()
    };
    eframe::run_native("bitchatd", options, Box::new(|_cc| Ok(Box::new(app))))
}
