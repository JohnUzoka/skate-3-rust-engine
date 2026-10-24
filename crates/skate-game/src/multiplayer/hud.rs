//! Compact gameplay roster. Transport diagnostics remain in logs and menus.
use super::Multiplayer;
use bevy::prelude::*;

pub(super) const PING_KEY: &str = "mp:ping";
#[derive(Component)]
pub(super) struct RosterRoot;
#[derive(Component)]
pub(super) struct RosterRow(usize);
#[derive(Component)]
pub(super) struct RosterText { row: usize, ping: bool }
#[derive(Component)]
pub(super) struct RosterTitle;

pub(super) fn setup(mut commands: Commands) {
    commands.spawn((RosterRoot, GlobalZIndex(5), bevy::ui::FocusPolicy::Pass,
        Node { position_type: PositionType::Absolute, right: px(24), bottom: px(24),
            width: px(310), max_width: percent(48), flex_direction: FlexDirection::Column,
            padding: UiRect::all(px(8)), row_gap: px(3), display: Display::None, ..default() },
        BackgroundColor(Color::srgba(0.035, 0.065, 0.10, 0.88)),
    )).with_children(|panel| {
        panel.spawn((RosterTitle, Text::new("FREESKATE"),
            TextFont { font_size: 14., ..default() }, TextColor(Color::srgb(0.55, 0.81, 1.)),
            Node { margin: UiRect::bottom(px(5)), ..default() }));
        for row in 0..10 {
            panel.spawn((RosterRow(row), Node { width: percent(100), min_height: px(30),
                align_items: AlignItems::Center, padding: UiRect::axes(px(8), px(4)),
                column_gap: px(8), display: Display::None, ..default() },
                BackgroundColor(Color::srgba(0.16, 0.24, 0.34, 0.72)),
            )).with_children(|line| {
                line.spawn((RosterText { row, ping: false }, Text::new(""),
                    TextFont { font_size: 18., ..default() }, TextColor(Color::WHITE),
                    Node { flex_grow: 1., min_width: px(0), ..default() }));
                line.spawn((RosterText { row, ping: true }, Text::new(""),
                    TextFont { font_size: 16., ..default() }, TextColor(Color::srgb(0.69, 0.79, 0.88)),
                    TextLayout::new_with_justify(Justify::Right),
                    Node { width: px(70), flex_shrink: 0., ..default() }));
            });
        }
    });
}
fn remote_ping(bytes: &[u8]) -> Option<u64> {
    let value = u64::from_le_bytes(bytes.try_into().ok()?);
    (value <= 60_000).then_some(value)
}
pub(super) fn draw(
    net: Res<Multiplayer>, windows: Query<&Window>,
    mut nodes: Query<(&mut Node, Option<&RosterRoot>, Option<&RosterRow>, Option<&RosterText>), Or<(With<RosterRoot>, With<RosterRow>, With<RosterText>)>>,
    mut labels: Query<(&mut Text, &mut TextFont, Option<&RosterText>, Option<&RosterTitle>), Or<(With<RosterText>, With<RosterTitle>)>>,
    mut drawn: Local<Option<(Vec<(String, String)>, u32)>>,
) {
    let scale = windows.iter().next().map_or(1., |w| (w.height() / 1080.).clamp(0.65, 2.));
    let mut roster = Vec::new();
    if let Some(lobby) = &net.lobby {
        let mut ids = net.player_ids();
        ids.sort_by_key(|id| (*id != lobby.local, *id));
        for id in ids.into_iter().take(10) {
            let name = if id == lobby.local { net.published_name() }
                else { net.names.get(&id).cloned().unwrap_or_else(|| "Player".into()) };
            // Ping is each player's RTT to the host. A host has no network hop.
            let ping = if id == lobby.local {
                    if lobby.is_host() { Some(0) } else { (lobby.stats.rtt_ms > 0).then_some(lobby.stats.rtt_ms) }
                }
                else { lobby.actors.get(&id).and_then(|a| a.application.get(PING_KEY))
                    .and_then(|record| remote_ping(&record.value)) };
            roster.push((name, ping.map_or_else(|| "--".into(), |ms| format!("{ms} ms"))));
        }
    }
    // Write the roster's nodes and texts only when the roster or the window's scale changed:
    // Bevy counts every write as a change and lays out and shapes the text again (findings 63 §6.2).
    let key = (roster.clone(), scale.to_bits());
    if drawn.as_ref() == Some(&key) {
        return;
    }
    *drawn = Some(key);
    for (mut node, root, row, field) in &mut nodes {
        if root.is_some() {
            node.display = if roster.is_empty() { Display::None } else { Display::Flex };
            node.right = px(24. * scale); node.bottom = px(24. * scale);
            node.width = px(310. * scale); node.padding = UiRect::all(px(8. * scale));
            node.row_gap = px(3. * scale);
        } else if let Some(row) = row {
            node.display = if row.0 < roster.len() { Display::Flex } else { Display::None };
            node.min_height = px(30. * scale);
            node.padding = UiRect::axes(px(8. * scale), px(4. * scale));
            node.column_gap = px(8. * scale);
        } else if field.is_some_and(|f| f.ping) {
            node.width = px(70. * scale);
        }
    }
    for (mut text, mut font, field, title) in &mut labels {
        if let Some(field) = field {
            font.font_size = if field.ping { 16. } else { 18. } * scale;
            text.0 = roster.get(field.row).map(|(name, ping)| if field.ping { ping.clone() } else { name.clone() }).unwrap_or_default();
        } else if title.is_some() { font.font_size = 14. * scale; }
    }
}
#[cfg(test)]
mod tests {
    use super::remote_ping;
    #[test]
    fn roster_ping_rejects_missing_or_malformed_peer_metadata() {
        assert_eq!(remote_ping(&55u64.to_le_bytes()), Some(55));
        assert_eq!(remote_ping(&0u64.to_le_bytes()), Some(0)); // host
        for bytes in [vec![], vec![1; 7], u64::MAX.to_le_bytes().to_vec()] {
            assert_eq!(remote_ping(&bytes), None);
        }
    }
}
