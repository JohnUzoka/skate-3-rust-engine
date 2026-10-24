use super::Mods;
use bevy::prelude::*;
#[derive(Resource, Default)]
pub(crate) struct ModMenu {
    pub open: bool,
    just_opened: bool,
    id: Option<String>,
    custom: Option<String>,
    return_to_pause: bool,
    path: Vec<String>,
    selected: usize,
    editing: Option<(String, String)>,
    status: String,
}
impl ModMenu {
    pub(crate) fn open_registered(&mut self, owner:String, key:String) {
        self.begin(); self.id=Some(owner); self.custom=Some(key); self.return_to_pause=true;
    }
    pub fn begin(&mut self) {
        self.open = true;
        self.return_to_pause = false;
        self.just_opened = true;
        self.id = None;
        self.custom = None;
        self.path.clear();
        self.selected = 0;
        self.editing = None;
        self.status.clear();
    }
}
#[derive(Component)]
struct Root;
#[derive(Component)]
struct Row(usize);
#[derive(Component)]
struct Label(usize);
#[derive(Component)]
struct Badge(usize);
#[derive(Component)]
struct Detail;
#[derive(Component)] struct Title;
#[derive(Component)] struct Page;
#[derive(Clone)]
enum Action {
    Select(String),
    CustomOpen(String,String,Vec<String>),
    CustomInvoke(String,String,String,bool),
    Enable(String),
    Reload(String),
    Reset(String),
    Setting(String, String),
    Scan,
    OpenFolder,
    Back,
}
fn back(menu:&mut ModMenu) {
    if !menu.path.is_empty() { menu.path.pop(); }
    else if menu.custom.take().is_some() {menu.id=None;if menu.return_to_pause {menu.open=false;}}
    else if menu.id.take().is_none() {menu.open=false;}
}
fn rows(menu: &ModMenu, mods: &Mods) -> Vec<(String, Action)> {
    let manager = &mods.manager;
    if let (Some(owner),Some(key))=(&menu.id,&menu.custom) {
        if let Some(definition)=mods.custom_menus.get(&(owner.clone(),key.clone())) {
            let mut items=&definition.items;
            let mut inherited=true;
            for id in &menu.path {
                if let Some(item)=items.iter().find(|i|&i.id==id) {inherited &= item.enabled;items=&item.children;}
                else {return vec![("Back".into(),Action::Back)];}
            }
            let mut result=Vec::new();
            for item in items {
                let action=if !item.children.is_empty() && item.enabled && inherited {
                    let mut path=menu.path.clone();path.push(item.id.clone());
                    Action::CustomOpen(owner.clone(),key.clone(),path)
                } else {Action::CustomInvoke(owner.clone(),key.clone(),item.id.clone(),item.enabled && inherited)};
                result.push((format!("{}{}",item.label,if item.enabled && inherited {""} else {" (unavailable)"}),action));
            }
            result.push(("Back".into(),Action::Back));return result;
        }
    }
    if let Some(id) = &menu.id {
        if let Some(p) = manager.packages.get(id) {
            let mut rows = vec![
                (
                    if p.running() {
                        "Turn mod off".into()
                    } else {
                        "Turn mod on".into()
                    },
                    Action::Enable(id.clone()),
                ),
                ("Reload from disk".into(), Action::Reload(id.clone())),
                (
                    "Reset settings to defaults".into(),
                    Action::Reset(id.clone()),
                ),
            ];
            for (key, s) in &p.manifest.settings {
                rows.push((
                    format!("{}: {}", s.label, p.settings[key]),
                    Action::Setting(id.clone(), key.clone()),
                ));
            }
            rows.push(("Back".into(), Action::Back));
            return rows;
        }
    }
    let mut rows: Vec<_> = manager
        .packages
        .iter()
        .map(|(id, p)| (p.manifest.name.clone(), Action::Select(id.clone())))
        .collect();
    for ((owner,key),definition) in &mods.custom_menus {
        if definition.section.is_none() && manager.packages.get(owner).is_some_and(|p|p.running()) {
            rows.insert(0,(definition.title.clone(),Action::CustomOpen(owner.clone(),key.clone(),Vec::new())));
        }
    }
    rows.push(("Open mods folder".into(), Action::OpenFolder));
    rows.push(("Refresh installed mods".into(), Action::Scan));
    rows.push(("Back to pause menu".into(), Action::Back));
    rows
}
pub(super) fn install(app: &mut App) {
    app.add_systems(PostStartup, setup)
        .add_systems(
            PreUpdate,
            input
                .after(crate::graphics_menu::MenuInput)
                .before(crate::map_transition::MapTransitionSet),
        )
        .add_systems(Update, draw);
}
fn setup(mut commands: Commands) {
    commands.spawn((Root,crate::graphics_menu::MenuLayoutRoot,GlobalZIndex(20),Node{display:Display::None,width:percent(100),height:percent(100),position_type:PositionType::Absolute,align_items:AlignItems::Center,justify_content:JustifyContent::Center,..default()},BackgroundColor(Color::srgba(0.015,0.02,0.025,0.96)))).with_children(|root| {
        root.spawn((Node{width:px(1180),height:px(700),flex_shrink:0.,padding:UiRect::all(px(24)),column_gap:px(28),..default()},BackgroundColor(Color::srgb(0.035,0.045,0.05)))).with_children(|panel| {
            panel.spawn(Node{width:px(210),flex_shrink:0.,flex_direction:FlexDirection::Column,row_gap:px(18),..default()}).with_children(|rail| {
                rail.spawn((Text::new("SKATE / 3"),TextFont{font_size:30.,..default()},TextColor(Color::srgb(0.78,0.96,0.3))));
                rail.spawn((Text::new("MAKE IT YOURS"),TextFont{font_size:12.,..default()},TextColor(Color::srgb(0.55,0.62,0.62))));
                rail.spawn((Node{padding:UiRect::all(px(12)),margin:UiRect::top(px(18)),..default()},BackgroundColor(Color::srgb(0.24,0.33,0.12))))
                    .with_child((Text::new("MOD LIBRARY"),TextFont{font_size:16.,..default()},TextColor(Color::WHITE)));
                rail.spawn((Text::new("Select a mod to turn it on, change its settings, or see more details."),TextFont{font_size:16.,..default()},TextColor(Color::srgb(0.65,0.72,0.72))));
                rail.spawn((Text::new("ESC / B   BACK"),TextFont{font_size:12.,..default()},TextColor(Color::srgb(0.55,0.62,0.62))));
            });
            panel.spawn(Node{flex_grow:1.,min_width:px(0),flex_direction:FlexDirection::Column,row_gap:px(10),..default()}).with_children(|body| {
                body.spawn((Title,Text::new("MODS"),TextFont{font_size:32.,..default()},TextColor(Color::WHITE)));
                body.spawn((Node{width:px(64),height:px(3),..default()},BackgroundColor(Color::srgb(0.78,0.96,0.3))));
                for i in 0..8 { body.spawn((Button,Row(i),Node{min_height:px(42),flex_shrink:0.,padding:UiRect::axes(px(14),px(9)),align_items:AlignItems::Center,justify_content:JustifyContent::SpaceBetween,column_gap:px(12),..default()},BackgroundColor(Color::srgb(0.075,0.09,0.095)))).with_children(|row| {
                    row.spawn((Label(i),Text::new(""),TextFont{font_size:17.,..default()},TextColor(Color::WHITE),Node{flex_grow:1.,min_width:px(0),..default()}));
                    row.spawn((Badge(i),Text::new(""),TextFont{font_size:14.,..default()},TextColor(Color::WHITE),Node{flex_shrink:0.,..default()}));
                }); }
                body.spawn(Node{column_gap:px(12),align_items:AlignItems::Center,..default()}).with_children(|pages| {
                    for (i,label) in [(8,"< Previous"),(9,"Next >")] {
                        pages.spawn((Button,Row(i),Node{padding:UiRect::all(px(8)),..default()},BackgroundColor(Color::srgb(0.075,0.09,0.095))))
                            .with_child((Text::new(label),TextFont{font_size:14.,..default()},TextColor(Color::WHITE)));
                    }
                    pages.spawn((Page,Text::new(""),TextFont{font_size:13.,..default()},TextColor(Color::srgb(0.65,0.72,0.72))));
                });
                body.spawn((Detail,Text::new(""),TextFont{font_size:14.,..default()},TextColor(Color::srgb(0.65,0.72,0.72)),Node{flex_grow:1.,min_height:px(0),overflow:Overflow::clip(),..default()}));
                body.spawn((Text::new("Up/Down Select   Enter / A Open   Left/Right Adjust   Esc / B Back"),TextFont{font_size:12.,..default()},TextColor(Color::srgb(0.55,0.62,0.62))));
            });
        });
    });
}
fn input(
    mut menu: ResMut<ModMenu>,
    mut mods: ResMut<Mods>,
    keys: Res<ButtonInput<KeyCode>>,
    nav: Res<crate::customiser::Navigation>,
    buttons: Query<(&Interaction, &Row), Changed<Interaction>>,
    mut typing: MessageReader<bevy::input::keyboard::KeyboardInput>,
    mut wheel: MessageReader<bevy::input::mouse::MouseWheel>,
) {
    let wheel_y: f32 = wheel.read().map(|e| e.y).sum();
    if !menu.open {
        typing.clear();
        return;
    }
    if menu.just_opened {
        menu.just_opened = false;
        typing.clear();
        return;
    }
    if let Some((key, mut value)) = menu.editing.take() {
        if keys.just_pressed(KeyCode::Escape) || nav.pressed & 0x2000 != 0 {
            typing.clear();
            return;
        }
        if keys.just_pressed(KeyCode::Enter) || nav.pressed & 0x1000 != 0 {
            if let Some(id) = &menu.id {
                menu.status = mods
                    .manager
                    .setting(id, &key, value.into())
                    .err()
                    .map(|error| { warn!("Mod setting: {error}"); friendly_error(&error) })
                    .unwrap_or_else(|| "Saved".into());
            }
            typing.clear();
            return;
        }
        if keys.just_pressed(KeyCode::Backspace) {
            value.pop();
        }
        for e in typing.read() {
            if e.state == bevy::input::ButtonState::Pressed {
                if let Some(t) = &e.text {
                    for c in t.chars().filter(|c| !c.is_control()) {
                        if value.chars().count() < 128 {
                            value.push(c);
                        }
                    }
                }
            }
        }
        menu.editing = Some((key, value));
        return;
    }
    typing.clear();
    if keys.just_pressed(KeyCode::Escape) || nav.pressed & (0x2000 | 0x10) != 0 {
        back(&mut menu);
        menu.selected = 0;
        return;
    }
    let entries = rows(&menu, &mods);
    let count = entries.len();
    menu.selected = menu.selected.min(count - 1);
    if keys.just_pressed(KeyCode::ArrowUp) || nav.pressed & 1 != 0 {
        menu.selected = (menu.selected + count - 1) % count;
    }
    if keys.just_pressed(KeyCode::ArrowDown) || nav.pressed & 2 != 0 {
        menu.selected = (menu.selected + 1) % count;
    }
    if wheel_y != 0. { menu.selected = if wheel_y > 0. { menu.selected.saturating_sub(1) } else { (menu.selected + 1).min(count-1) }; }
    let mut direction = 0;
    if keys.just_pressed(KeyCode::ArrowLeft) || nav.pressed & 4 != 0 {
        direction = -1;
    }
    if keys.just_pressed(KeyCode::ArrowRight)
        || keys.just_pressed(KeyCode::Enter)
        || nav.pressed & (8 | 0x1000) != 0
    {
        direction = 1;
    }
    let offset = menu.selected / 8 * 8;
    for (interaction, row) in &buttons {
        if *interaction == Interaction::Pressed && row.0 >= 8 {
            menu.selected = if row.0 == 8 { offset.saturating_sub(8) } else { (offset + 8).min(count - 1) };
            return;
        }
        if *interaction == Interaction::Pressed && row.0 + offset < count {
            menu.selected = row.0 + offset;
            direction = 1;
        }
    }
    if direction == 0 {
        return;
    }
    let action = entries[menu.selected].1.clone();
    let activate = keys.just_pressed(KeyCode::Enter) || nav.pressed & 0x1000 != 0 || buttons.iter().any(|(i,_)| *i == Interaction::Pressed);
    if !activate && !matches!(action, Action::Setting(_, _)) { return; }
    let result = match action {
        Action::CustomOpen(owner,key,path) => {
            menu.id=Some(owner);menu.custom=Some(key);menu.path=path;menu.selected=0;Ok(())
        }
        Action::CustomInvoke(owner,key,item,enabled) => {
            if !enabled { menu.status.clear(); return; }
            if enabled && mods.manager.packages.get(&owner).is_some_and(|p|p.running()) {
                mods.manager.call(&owner,"on_event",serde_json::json!({"name":"menu_action","menu":key,"item":item}));
                Ok(())
            } else {Err("This action is currently unavailable".into())}
        }
        Action::Select(id) => {
            menu.id = Some(id);
            menu.selected = 0;
            Ok(())
        }
        Action::Back => {
            back(&mut menu);
            menu.selected = 0;
            Ok(())
        }
        Action::OpenFolder => {
            #[cfg(target_os = "windows")]
            let program = "explorer.exe";
            #[cfg(target_os = "macos")]
            let program = "open";
            #[cfg(not(any(target_os = "windows", target_os = "macos")))]
            let program = "xdg-open";
            std::process::Command::new(program)
                .arg(mods.manager.root())
                .spawn()
                .map(|_| ())
                .map_err(|e| format!("Could not open mods folder: {e}"))
        }
        Action::Scan => {
            mods.manager.scan(true);
            Ok(())
        }
        Action::Enable(id) => {
            let enabled = !mods.manager.packages[&id].running();
            mods.manager.enable(&id, enabled)
        }
        Action::Reload(id) => {
            mods.manager.scan(true);
            mods.manager.reload(&id);
            Ok(())
        }
        Action::Reset(id) => mods.manager.reset(&id),
        Action::Setting(id, key) => {
            let p = &mods.manager.packages[&id];
            let s = &p.manifest.settings[&key];
            let value = &p.settings[&key];
            let next = match s.kind.as_str() {
                "boolean" => Some(serde_json::json!(!value.as_bool().unwrap())),
                "number" => Some(serde_json::json!(
                    (value.as_f64().unwrap() + direction as f64 * s.step.unwrap())
                        .clamp(s.min.unwrap(), s.max.unwrap())
                )),
                "choice" => {
                    let i = s
                        .choices
                        .iter()
                        .position(|v| Some(v.as_str()) == value.as_str())
                        .unwrap_or(0);
                    Some(serde_json::json!(
                        s.choices
                            [(i as i32 + direction).rem_euclid(s.choices.len() as i32) as usize]
                    ))
                }
                "string" => {
                    menu.editing = Some((key.clone(), value.as_str().unwrap().into()));
                    None
                }
                _ => None,
            };
            if let Some(value) = next {
                mods.manager.setting(&id, &key, value)
            } else {
                Ok(())
            }
        }
    };
    menu.status = result.err().map(|error| { warn!("Mod menu: {error}"); friendly_error(&error) }).unwrap_or_default();
}
fn friendly_error(error: &str) -> String {
    if error.contains("os error 2") || error.contains("not found") {
        "A mod file is missing. Open the mods folder, restore it, then refresh.".into()
    } else { "This mod could not complete the action. See the session log for details.".into() }
}
fn display_text(text: &str) -> String { text.replace(['—','–','•'], " / ") }
fn draw(
    menu: Res<ModMenu>, mods: Res<Mods>,
    mut root: Single<&mut Node, With<Root>>,
    mut labels: Query<(&Label, &mut Text), (Without<Badge>,Without<Title>,Without<Page>,Without<Detail>)>,
    mut badges: Query<(&Badge, &mut Text, &mut TextColor), (Without<Label>,Without<Title>,Without<Page>,Without<Detail>)>,
    mut buttons: Query<(&Row, &Interaction, &mut Node, &mut BackgroundColor), Without<Root>>,
    mut detail: Single<&mut Text, (With<Detail>,Without<Label>,Without<Badge>,Without<Title>,Without<Page>)>,
    mut title: Single<&mut Text, (With<Title>,Without<Label>,Without<Badge>,Without<Detail>,Without<Page>)>,
    mut page: Single<&mut Text, (With<Page>,Without<Label>,Without<Badge>,Without<Title>,Without<Detail>)>,
    mut logged: Local<String>,
    audio: Option<Res<crate::game_audio::AudioContent>>,
) {
    // Only on change: Bevy counts every write as one, and lays the UI out again (findings 63 §6.2).
    let display = if menu.open { Display::Flex } else { Display::None };
    if root.display != display { root.display = display; }
    if !menu.open { return; }
    let entries = rows(&menu, &mods);
    let selected = menu.selected.min(entries.len()-1);
    let offset = selected / 8 * 8;
    for (label, mut text) in &mut labels { **text = entries.get(offset+label.0).map(|e| display_text(&e.0)).unwrap_or_default(); }
    for (row, interaction, mut node, mut color) in &mut buttons {
        let visible = match row.0 { 8 => offset > 0, 9 => offset+8 < entries.len(), _ => offset+row.0 < entries.len() };
        node.display = if visible { Display::Flex } else { Display::None };
        color.0 = if (row.0 < 8 && offset+row.0 == selected) || *interaction == Interaction::Hovered { Color::srgb(0.24,0.33,0.12) } else { Color::srgb(0.075,0.09,0.095) };
    }
    for (badge, mut text, mut color) in &mut badges {
        let package = entries.get(offset+badge.0).and_then(|(_,a)| match a {
            Action::Select(id) | Action::Enable(id) => mods.manager.packages.get(id), _ => None,
        });
        **text = package.map(|p| if p.running() { "ON" } else { "OFF" }).unwrap_or("").into();
        color.0 = if package.is_some_and(|p| p.running()) { Color::srgb(0.78,0.96,0.3) } else { Color::srgb(0.55,0.62,0.62) };
    }
    let active = menu.id.as_ref().and_then(|id| mods.manager.packages.get(id));
    ***title = display_text(&active.map(|p| p.manifest.name.clone()).unwrap_or_else(|| "MODS".into()));
    if let (Some(owner),Some(key))=(&menu.id,&menu.custom) {
        if let Some(definition)=mods.custom_menus.get(&(owner.clone(),key.clone())) { ***title = display_text(&definition.title); }
    }
    ***page = format!("Page {} of {}",offset/8+1,entries.len().div_ceil(8));
    let selected_package = active.or_else(|| match &entries[selected].1 { Action::Select(id) => mods.manager.packages.get(id), _ => None });
    let mut description = menu.status.clone();
    if let Some(package) = selected_package {
        description += &format!("\n{}  /  {}\n{}",package.manifest.name,package.manifest.author,package.manifest.description);
        if let Some(error) = &package.error { description += &format!("\n{}",friendly_error(error)); }
        // Audio content: the overlay's load error, conflicts with other mods (the first by mod id
        // wins), entries the install lacks. Kept until the overlays change.
        for line in audio.as_deref().map(|a| a.messages_for(&package.manifest.id)).unwrap_or_default().into_iter().take(2) {
            description += &format!("\n{line}");
        }
        if let Action::Setting(_,key) = &entries[selected].1 { description += &format!("\n{}",package.manifest.settings[key].description); }
    } else if !mods.manager.diagnostics.is_empty() {
        description += "\nSome mod files could not load. Restore missing files, then refresh. Details are in the session log.";
    } else if let Some(line) = audio.as_deref().and_then(|a| a.summary()) {
        description += &format!("\n{line}");
    } else if mods.manager.packages.is_empty() {
        description += "\nNo mods installed yet. Open the mods folder to add a mod, then refresh.";
    }
    if let (Some(owner),Some(key))=(&menu.id,&menu.custom) {
        if let Some(d)=mods.custom_menus.get(&(owner.clone(),key.clone())) {
            let mut heading=d.title.clone();let mut items=&d.items;
            for id in &menu.path {if let Some(item)=items.iter().find(|i|&i.id==id) {heading.push_str(" / ");heading.push_str(&item.label);items=&item.children;}}
            ***title=display_text(&heading);
            description=format!("{}\n{}",items.get(selected).map_or("",|i|i.description.as_str()),menu.status);
        }
    }
    let diagnostics = format!("{}\n{}",mods.manager.diagnostics.join("\n"),selected_package.and_then(|p| p.error.as_deref()).unwrap_or(""));
    if *logged != diagnostics { if !diagnostics.trim().is_empty() { warn!("Mod library: {diagnostics}"); } *logged = diagnostics; }
    if let Some((_,value)) = &menu.editing { description = format!("Editing: {value}_\nEnter saves. Esc cancels."); }
    ***detail = display_text(&description.trim().chars().take(320).collect::<String>());
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn section_menu_back_returns_directly_to_pause() {
        let mut menu=ModMenu::default();
        menu.open_registered("test.mod".into(),"activity".into());
        menu.path.push("nested".into());
        back(&mut menu);assert!(menu.open);
        back(&mut menu);assert!(!menu.open);assert!(menu.custom.is_none());
    }
    #[test]
    fn nested_menu_back_preserves_parent_then_returns_to_mod_list() {
        let mut menu=ModMenu::default();menu.begin();menu.id=Some("tests.menu".into());menu.custom=Some("challenges".into());menu.path=vec!["races".into(),"sprint".into()];
        back(&mut menu);assert_eq!(menu.path,vec!["races"]);assert!(menu.open);
        back(&mut menu);assert!(menu.path.is_empty());assert!(menu.custom.is_some());
        back(&mut menu);assert!(menu.id.is_none());assert!(menu.custom.is_none());assert!(menu.open);
        back(&mut menu);assert!(!menu.open);
    }
}
