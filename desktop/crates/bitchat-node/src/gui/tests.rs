//! Headless UI tests for the chat transcript layouts (egui_kittest).
//!
//! These catch the layout regression classes we hit in review: bubbles
//! stretching instead of hugging their content, timestamps floating
//! vertically centered instead of bottom-aligned, nick and body glued on
//! one line, matrix timestamps flung to the window edge, and overlapping
//! settings controls. Everything is asserted on widget rects — no pixels.

use std::sync::mpsc as std_mpsc;

use super::*;
use egui_kittest::{Harness, kittest::Queryable};

// Match the real app window so snapshots reflect production geometry.
const TEST_WIDTH: f32 = 1160.0;
const TEST_HEIGHT: f32 = 760.0;
/// Central panel starts after the (default 250 px) peer list; bubbles have
/// a small inner margin, so left-hugging content begins well before 290.
const CENTRAL_X: f32 = 235.0;

fn fixture_app(theme: ChatTheme, lines: Vec<ChatLine>) -> BitchatApp {
    let (_event_tx, event_rx) = std_mpsc::channel::<LinkEvent>();
    let (cmd_tx, _cmd_rx) = std_mpsc::channel::<UiCommand>();
    let identity_path = std::env::temp_dir().join("bitchatd-gui-ui-test");
    let _ = std::fs::create_dir_all(&identity_path);
    let mut app = BitchatApp::new(
        event_rx,
        cmd_tx,
        None,
        "0123456789abcdef".to_string(),
        "moi".to_string(),
        &identity_path,
    );
    app.theme = theme;
    app.chat.insert(CHANNEL_KEY.to_string(), lines);
    app
}

fn text_line(who: &str, author_id: &str, mine: bool, text: &str, ts_millis: i64) -> ChatLine {
    ChatLine {
        id: uuid_line_id(),
        who: who.to_string(),
        author_id: author_id.to_string(),
        mine,
        content: ChatContent::Text(text.to_string()),
        ts_millis,
        confirmed: false,
    }
}

fn harness_for(app: BitchatApp) -> Harness<'static, BitchatApp> {
    let mut harness = Harness::builder()
        .with_size(egui::vec2(TEST_WIDTH, TEST_HEIGHT))
        .build_eframe(move |cc| {
            terminal_style(&cc.egui_ctx);
            app
        });
    for _ in 0..3 {
        harness.step();
    }
    harness
}

#[derive(Debug)]
struct Bounds {
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
}

fn bounds(node: &egui_kittest::kittest::Node<'_>) -> Bounds {
    let rect = node.raw_bounds().expect("node has bounds");
    Bounds {
        left: rect.x0 as f32,
        top: rect.y0 as f32,
        right: rect.x1 as f32,
        bottom: rect.y1 as f32,
    }
}

fn overlaps(a: &Bounds, b: &Bounds) -> bool {
    a.left < b.right && b.left < a.right && a.top < b.bottom && b.top < a.bottom
}

/// The central-panel copy of a label (the peer list also shows `@nick`).
fn central_label(harness: &mut Harness<'_, BitchatApp>, text: &str) -> Bounds {
    harness
        .get_all_by_label(text)
        .map(|node| bounds(&node))
        .find(|rect| rect.left >= CENTRAL_X)
        .unwrap_or_else(|| panic!("no central label '{text}'"))
}

#[test]
fn bubbles_own_short_message_hugs_content_and_time_is_bottom_right() {
    let ts = 1_700_000_000_000;
    let app = fixture_app(
        ChatTheme::Bubbles,
        vec![text_line("moi", "me", true, "hey ui test", ts)],
    );
    let mut harness = harness_for(app);

    let body = central_label(&mut harness, "hey ui test");
    let time = central_label(&mut harness, &format_chat_time(ts));

    // Hugging: the bubble must NOT stretch to the 80% row cap for a short
    // message (regression: it filled 0.8 * row).
    assert!(
        body.right - body.left < TEST_WIDTH * 0.5,
        "own bubble should hug its content, got width {}",
        body.right - body.left
    );
    // Right side of the screen.
    assert!(
        time.right > TEST_WIDTH * 0.7,
        "own message should sit on the right side (time.right={})",
        time.right
    );
    // Timestamp bottom-aligned with the body line, not vertically centered.
    assert!(
        (time.bottom - body.bottom).abs() <= 2.5,
        "timestamp bottom-aligned: time.bottom={} body.bottom={}",
        time.bottom,
        body.bottom
    );
    // Own bubbles never show a name.
    assert!(
        harness.query_all_by_label("@moi").next().is_none(),
        "own bubbles must not show a nickname"
    );
}

#[test]
fn bubbles_other_nick_heads_bubble_and_time_bottom_right() {
    let ts = 1_700_000_100_000;
    let app = fixture_app(
        ChatTheme::Bubbles,
        vec![text_line(
            "autrui",
            "2a6a0d41451fbe94",
            false,
            "bonjour depuis le test",
            ts,
        )],
    );
    let mut harness = harness_for(app);

    let nick = central_label(&mut harness, "@autrui");
    let body = central_label(&mut harness, "bonjour depuis le test");
    let time = central_label(&mut harness, &format_chat_time(ts));

    // Nick on its own line above the body (regression: glued on one line).
    assert!(
        nick.bottom <= body.top + 1.0,
        "nick must head the bubble on its own line: nick.bottom={} body.top={}",
        nick.bottom,
        body.top
    );
    // Bubble on the left half.
    assert!(
        body.left < TEST_WIDTH * 0.5,
        "other bubble should sit on the left (body.left={})",
        body.left
    );
    // Timestamp at the bottom-right of the bubble content.
    assert!(
        (time.bottom - body.bottom).abs() <= 2.5,
        "timestamp bottom-aligned: time.bottom={} body.bottom={}",
        time.bottom,
        body.bottom
    );
    assert!(
        time.right >= body.right - 1.0 && time.right <= body.right + 175.0,
        "timestamp should end at the bubble's right edge zone: time.right={} body.right={}",
        time.right,
        body.right
    );
}

#[test]
fn bubbles_long_message_wraps_within_eighty_percent() {
    let long_text = "lorem ipsum dolor sit amet ".repeat(24);
    let ts = 1_700_000_200_000;
    let app = fixture_app(
        ChatTheme::Bubbles,
        vec![text_line("autrui", "peer42", false, &long_text, ts)],
    );
    let mut harness = harness_for(app);

    let body = central_label(&mut harness, &long_text);
    assert!(
        body.bottom - body.top > 26.0,
        "long message must wrap to multiple lines, height {}",
        body.bottom - body.top
    );
    assert!(
        body.right <= CENTRAL_X + (TEST_WIDTH - CENTRAL_X) * 0.8 + 24.0,
        "long message must stay within the 80% bubble cap, right={}",
        body.right
    );
}

#[test]
fn matrix_time_sits_immediately_right_of_text_bottom() {
    let ts = 1_700_000_300_000;
    let app = fixture_app(
        ChatTheme::Matrix,
        vec![text_line(
            "autrui",
            "peer77",
            false,
            "ligne matrix du test",
            ts,
        )],
    );
    let mut harness = harness_for(app);

    let body = central_label(&mut harness, "ligne matrix du test");
    let time = central_label(&mut harness, &format_chat_time(ts));

    // Immediately right of the text block, not at the window edge.
    assert!(
        time.left >= body.right - 2.0 && time.left < body.right + 30.0,
        "matrix timestamp must sit right after the text: time.left={} body.right={}",
        time.left,
        body.right
    );
    assert!(
        time.right < TEST_WIDTH * 0.75,
        "matrix timestamp must NOT be flung to the window edge: time.right={}",
        time.right
    );
    // Bottom-aligned with the text block.
    assert!(
        (time.bottom - body.bottom).abs() <= 3.0,
        "matrix timestamp bottom-aligned: time.bottom={} body.bottom={}",
        time.bottom,
        body.bottom
    );
}

#[test]
fn settings_chat_layout_toggle_does_not_overlap_its_row() {
    let app = fixture_app(ChatTheme::Bubbles, Vec::new());
    let mut app = app;
    app.show_settings = true;
    app.settings_tab = SettingsTab::Settings;
    let harness = harness_for(app);

    // Compare against the actual painted text extents (the smallest matching
    // node is the TextRun; the Label widget rect fills its whole column).
    let subtitle = harness
        .query_all_by_label_contains("terminal transcript")
        .map(|node| bounds(&node))
        .min_by(|a, b| (a.right - a.left).total_cmp(&(b.right - b.left)))
        .expect("chat layout subtitle text");
    let toggle = bounds(&harness.get_by_label("Bubbles"));

    assert!(
        !overlaps(&subtitle, &toggle),
        "toggle must not overlap the subtitle text: subtitle={:?} toggle={:?}",
        subtitle,
        toggle
    );
}

/// Renders the two screens under review to PNG so a human (or a reviewer
/// agent) can look at them: `tests/snapshots/visual_*.new.png`. Never fails
/// on image diff — the rect assertions above are the enforced contract.
#[test]
fn visual_snapshots_for_review() {
    let ts = 1_700_000_500_000;
    let app = fixture_app(
        ChatTheme::Bubbles,
        vec![
            text_line("moi", "me", true, "message perso court", ts - 60_000),
            text_line(
                "pseudo-tres-long-pour-le-test",
                "peer55",
                false,
                "reçu avec un pseudo long",
                ts,
            ),
        ],
    );
    let mut harness = harness_for(app);
    let _ = harness.try_snapshot("visual_bubbles");

    let mut app = fixture_app(
        ChatTheme::Matrix,
        vec![text_line("autrui", "peer66", false, "vue matrix", ts)],
    );
    app.show_settings = true;
    app.settings_tab = SettingsTab::Settings;
    let mut harness = harness_for(app);
    let _ = harness.try_snapshot("visual_settings");
}

#[test]
fn theme_toggle_switches_transcript_layout() {
    let ts = 1_700_000_400_000;
    let app = fixture_app(
        ChatTheme::Bubbles,
        vec![text_line("autrui", "peer99", false, "message a basculer", ts)],
    );
    let mut harness = harness_for(app);

    // Bubbles: the nick heads the bubble above the body.
    let nick = central_label(&mut harness, "@autrui");
    let body = central_label(&mut harness, "message a basculer");
    assert!(nick.bottom <= body.top + 1.0);

    // Switch to matrix via the settings toggle.
    harness.state_mut().show_settings = true;
    harness.state_mut().settings_tab = SettingsTab::Settings;
    harness.step();
    harness.get_by_label("Matrix").click();
    harness.step();

    assert_eq!(harness.state().theme, ChatTheme::Matrix);

    // Matrix: nick is still above, but the timestamp now sits right of the
    // text (bubble geometry is gone — same left alignment).
    let time = central_label(&mut harness, &format_chat_time(ts));
    let body = central_label(&mut harness, "message a basculer");
    assert!(
        time.left >= body.right - 2.0,
        "after switching to matrix the timestamp must sit right of the text"
    );
}

