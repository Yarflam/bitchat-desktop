import pathlib

p = pathlib.Path('crates/bitchat-node/examples/node_gui.rs')
s = p.read_text(encoding='utf-8')

marker = '\nfn main() {'
idx = s.index(marker)
body = s[:idx]

body = body.replace('use bitchat_node::link::MAX_FILE_CONTENT_BYTES;',
                    'use crate::link::MAX_FILE_CONTENT_BYTES;')
body = body.replace('use bitchat_node::{ChatTheme, LinkEvent, MeshLink, NodeConfig, NodeIdentity};',
                    'use crate::config::{ChatTheme, NodeConfig};\nuse crate::identity::NodeIdentity;\nuse crate::link::{LinkEvent, MeshLink};')

header = '''//! Desktop UI for the BitChat mesh node (egui), styled after the Android
//! client's retro-terminal mesh theme: monospace type, black background,
//! flat green (peers) / amber (self) containers.
//!
//! Built as a library module so egui_kittest can drive it headlessly in
//! tests (widget queries and rect assertions catch layout regressions
//! without a human in the loop).

'''
lib = header + body + '''
/// Run the GUI against a live mesh link (used by the `node_gui` example).
pub fn run(
    link: MeshLink,
    our_id: String,
    nickname: String,
    identity_path: PathBuf,
) -> eframe::Result<()> {
    let (event_tx, event_rx) = std_mpsc::channel::<LinkEvent>();
    let (cmd_tx, cmd_rx) = std_mpsc::channel::<UiCommand>();
    let (async_cmd_tx, mut async_cmd_rx) = tokio::sync::mpsc::unbounded_channel();

    thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build();
        let Ok(runtime) = runtime else {
            eprintln!("bitchatd: tokio runtime failed");
            return;
        };
        runtime.block_on(async move {
            while let Ok(command) = cmd_rx.recv() {
                let _ = async_cmd_tx.send(command);
            }
        });
    });

    thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build();
        let Ok(runtime) = runtime else {
            eprintln!("bitchatd: tokio runtime failed");
            return;
        };
        runtime.block_on(async move {
            let mut link = link;
            loop {
                tokio::select! {
                    event = link.events.recv() => match event {
                        Some(event) => {
                            eprintln!("[event] {event:?}");
                            if event_tx.send(event).is_err() {
                                break;
                            }
                        }
                        None => break,
                    },
                    command = async_cmd_rx.recv() => match command {
                        Some(UiCommand::SendPrivate { peer_id, content }) => {
                            if let Err(error) = link.send_private_message(&peer_id, &content).await {
                                eprintln!("bitchatd: send private to {peer_id} failed: {error}");
                            }
                        }
                        Some(UiCommand::SendBroadcast { content }) => {
                            if let Err(error) = link.send_broadcast_message(&content).await {
                                eprintln!("bitchatd: send broadcast failed: {error}");
                            }
                        }
                        Some(UiCommand::SendPrivateFile { peer_id, file }) => {
                            if let Err(error) = link.send_private_file(&peer_id, file).await {
                                eprintln!("bitchatd: send private file failed: {error}");
                            }
                        }
                        Some(UiCommand::SendBroadcastFile { file }) => {
                            if let Err(error) = link.send_broadcast_file(file).await {
                                eprintln!("bitchatd: send broadcast file failed: {error}");
                            }
                        }
                        Some(UiCommand::SetNickname { nickname }) => {
                            if let Err(error) = link.set_nickname(&nickname).await {
                                eprintln!("bitchatd: set nickname failed: {error}");
                            }
                        }
                        Some(UiCommand::Connect { address }) => match parse_mac(&address) {
                            Some(mac) => {
                                if let Err(error) = link.connect_by_address(mac).await {
                                    eprintln!("bitchatd: connect failed: {error}");
                                }
                            }
                            None => eprintln!("bitchatd: invalid MAC address '{address}'"),
                        },
                        None => break,
                    },
                }
            }
        });
    });

    let app = BitchatApp {
        events_in: event_rx,
        cmd_out: cmd_tx,
        start_signal: None,
        our_id,
        nickname: nickname.clone(),
        nick_draft: nickname,
        editing_nick: false,
        status: String::new(),
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
        theme: BitchatApp::load_theme(&identity_path),
        image_cache: HashMap::new(),
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
'''
pathlib.Path('crates/bitchat-node/src/gui.rs').write_text(lib, encoding='utf-8')

example = '''//! Desktop GUI node: wires the mesh link into the egui app.
//!
//! Usage:
//!   cargo run -p bitchat-node --example node_gui

use bitchat_node::identity::NodeIdentity;
use bitchat_node::link::MeshLink;

fn main() -> eframe::Result<()> {
    let identity_path = std::path::PathBuf::from(
        std::env::var("BITCHATD_IDENTITY").unwrap_or_else(|_| "bitchatd.identity".to_string()),
    );
    let identity = NodeIdentity::load_or_create(&identity_path);
    let nickname = std::env::var("BITCHATD_NICKNAME")
        .ok()
        .filter(|nick| !nick.trim().is_empty())
        .unwrap_or_else(|| bitchat_node::NodeConfig::load(&identity_path).nickname_or_default());

    let link = mesh_start(identity, &nickname);
    let our_id = link.our_peer_id().to_string();
    eprintln!("bitchatd: peer_id={our_id} nickname={nickname}");
    eprintln!("bitchatd: identity {}", NodeIdentity::describe());

    bitchat_node::gui::run(link, our_id, nickname, identity_path)
}

fn mesh_start(identity: NodeIdentity, nickname: &str) -> MeshLink {
    // MeshLink::start is async; run it on a dedicated thread with its own
    // reactor so the GUI example stays runtime-agnostic.
    let (tx, rx) = std::sync::mpsc::channel();
    let nick = nickname.to_string();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        let link = runtime.block_on(MeshLink::start(identity, &nick, 256));
        tx.send(link).ok();
    });
    rx.recv().expect("mesh link start")
}
'''
pathlib.Path('crates/bitchat-node/examples/node_gui.rs').write_text(example, encoding='utf-8')
print('refactor done')
