//! Grid containers — a port of the client's former host GridContainerGumpPlugin +
//! GridMultiMovePlugin to an out-of-process mod, behaviour for behaviour, on the
//! host's GENERIC mod surface only (nothing in the host knows this mod exists).
//!
//! The window carries the host's classic container markers (ContainerWindow /
//! ContainerGumpTag / ContainerItemUI / NoPickup / UiMovable), so the host's own
//! pickup, drop, double-click, hover hue, tooltip, targeting, right-click close,
//! z-stack, distance close and gumps.xml save drive it like a native container. The
//! mod owns layout + rebuild, the header, sort, search, pins, free placement, quick
//! move and multi-move. Settings live on the Options window's "Mods" page; pins and
//! the free-slot layout in character-scoped mod storage.
//!
//! The client pushes each system the data its parameters declare, so every stage
//! starts with small "snapshot" systems copying what the stage's logic reads into
//! the mod state; the logic systems after them read the state.
//!
//! Stage map:
//!   Startup     the options schema.
//!   First       snapshots (input, game, layout); main: options + claims, storage,
//!               quick-move target, pin menu, quick move, multi-select. Observers:
//!               container opened / slot, clicks, host hotkey (QuickMoveItem on a
//!               mouse binding).
//!   Update      item feed (ChildOf + changed graphic / hue / amount of contained
//!               items, OPL), world + layout snapshots; main: sort / pin clicks,
//!               highlight, pin prune, corpse close, drop-slot record, rebuild.
//!               Observer: container closed.
//!   PostUpdate  selection tint, the ack-paced multi-move queue. Observers: drop
//!               sent, move result.

mod logic;

use std::collections::{BTreeMap, HashMap, HashSet};

use cuo_mod_sdk::actions::{self, Destination, Point};
use cuo_mod_sdk::assets::{self, StaticInfo};
use cuo_mod_sdk::host::Scope;
use cuo_mod_sdk::prelude::*;
use cuo_mod_sdk::types::Item as IsItem;
use logic::*;

const CLAIM_CONTAINERS: &str = "cuo:gump/container";
const CLAIM_CORPSES: &str = "cuo:gump/corpse";
const MOUSE_LEFT: u8 = 1;
const MOUSE_RIGHT: u8 = 3;
const LAYER_BACKPACK: usize = 0x15;
const ART: u8 = 3; // UOCustomKind.Art
const TILE_STACKABLE: u64 = 0x0000_0800;
const TILE_CONTAINER: u64 = 0x0020_0000;
const CORPSE_GRAPHIC: u16 = 0x2006;
const WORLD_SWEEP_MIN: usize = 512;
const GAME_SCREEN: u8 = 6;
const NAME_REFRESH_MS: f32 = 1000.0;

// Option keys (cuo:options/schema).
const OPT_CONTAINERS: &str = "containers";
const OPT_QUICK_MOVE: &str = "quick_move";
const OPT_MULTI_MOVE: &str = "multi_move";
const OPT_MULTI_WAIT: &str = "multi_move_wait";

// ── state ───────────────────────────────────────────────────────────────────────

/// One item cell of a view's current slot tree.
struct GridCell {
    slot: usize,
    serial: u32,
    pinned: bool,
    entity: Entity,
    bg: Entity,
}

/// One open grid window (the host's former GridContainerState.View).
struct View {
    serial: u32,
    is_corpse: bool,
    sort: Sort,
    last_sig: Option<u64>,
    window: Entity,
    content: Entity,
    search: Entity,
    title: Entity,
    sort_btn: Entity,
    icon: Entity,
    weight: Entity,
    /// Rows of the current slot tree (despawned with their cells on rebuild).
    rows: Vec<Entity>,
    cells: Vec<GridCell>,
    title_text: String,
    weight_text: String,
    sort_text: String,
    icon_asset: u32,
    panel: Rgba,
    border: Rgba,
    loot: Option<(u64, u64)>,
}

/// What the item feed knows about one contained item.
#[derive(Clone)]
struct Item {
    graphic: u16,
    hue: u16,
    amount: i32,
}

impl Default for Item {
    fn default() -> Item {
        Item { graphic: 0, hue: 0, amount: 1 }
    }
}

#[derive(Default, Clone, Copy)]
struct Opts {
    containers: bool,
    quick_move: bool,
    multi_move: bool,
    multi_wait: i32,
    // Host options (Options > Containers), from cuo:game/settings.
    grid_loot: i32,
    skip_empty_corpse: bool,
}

/// The host data the logic reads, copied in by the snapshot systems. Most snapshots
/// are run-on-change and fed by Changed filters, so these fields are maintained
/// incrementally and persist across the frames a snapshot is skipped.
#[derive(Default)]
struct Frame {
    // First: input (resources).
    mouse: MouseInputDto,
    keys: Keys,
    pick: UiPickDto,
    held_serial: u32,
    targeting: bool,
    host_hotkeys: Vec<HostHotkeyDto>,
    // First: game.
    game_screen: bool,
    player: (u32, String),
    backpack: u32,
    popups: Vec<Entity>,
    // First + Update: layout, our views only. Window root bits -> (Node, z); content
    // column bits -> (rect, scroll).
    windows: HashMap<u64, (Node, i32)>,
    contents: HashMap<u64, (ComputedNode, f32)>,
    // Update: world. Container-like items by entity bits -> (serial, graphic, hue) /
    // (serial, position); looked up through the serial's live entity.
    looks: HashMap<u64, (u32, u16, u16)>,
    positions: HashMap<u64, (u32, WorldPosition)>,
    /// Size above which `looks` + `positions` are swept of despawned entities.
    world_cap: usize,
    player_weight: Option<(u16, u16)>,
    auto_corpses: HashSet<u64>,
    manual_corpses: HashSet<u64>,
    /// Our search fields' text, by field entity bits.
    texts: HashMap<u64, String>,
    // Update, every frame: the clock (`update`), the player's position (snap_live).
    now: f32,
    player_pos: Option<WorldPosition>,
}

struct State {
    views: BTreeMap<u32, View>,
    last_size: (i32, i32),
    data: CharData,
    pins: HashSet<u32>,
    pins_seen: HashSet<u32>,
    char_key: Option<(u32, String)>,
    char_loaded: bool,
    target: u32,
    sel: Selection,
    /// (container serial, slot) of the tiles painted as selected.
    tinted: HashSet<(u32, usize)>,
    queue: MoveQueue,
    /// The open Pin / Unpin menu: (item, container, menu, entry).
    pin_menu: Option<(u32, u32, Entity, Entity)>,
    select_box: Option<Entity>,
    opts: Opts,
    claims: (Option<Entity>, Option<Entity>),
    in_game: bool,
    // Item feed (Update): container entity -> its items' serials, item entity -> its
    // container entity, and the item cache keyed by serial.
    members: HashMap<u64, Vec<u32>>,
    parent_of: HashMap<u64, u64>,
    // The membership the two maps above are derived from, maintained incrementally:
    // item entity -> its serial, container entity -> its item entities in arrival order,
    // and how many contained entities carry each serial.
    member_serial: HashMap<u64, u32>,
    children: HashMap<u64, Vec<u64>>,
    live: HashMap<u32, u32>,
    items: HashMap<u32, Item>,
    props: HashMap<u32, EntityPropertiesDto>,
    props_asked: HashMap<u32, f32>,
    tiles: HashMap<u16, Option<StaticInfo>>,
    ramps: HashMap<u16, u32>,
    sort_clicks: Vec<u32>,
    pin_clicked: bool,
    f: Frame,
}

static mut STATE: Option<State> = None;

fn st() -> &'static mut State {
    // SAFETY: single-threaded guest; the host never re-enters an export, and no
    // caller holds the reference across another st().
    unsafe {
        (*std::ptr::addr_of_mut!(STATE)).get_or_insert_with(|| State {
            views: BTreeMap::new(),
            last_size: (320, 300),
            data: CharData::default(),
            pins: HashSet::new(),
            pins_seen: HashSet::new(),
            char_key: None,
            char_loaded: false,
            target: 0,
            sel: Selection::default(),
            tinted: HashSet::new(),
            queue: MoveQueue::default(),
            pin_menu: None,
            select_box: None,
            opts: Opts::default(),
            claims: (None, None),
            in_game: false,
            members: HashMap::new(),
            parent_of: HashMap::new(),
            member_serial: HashMap::new(),
            children: HashMap::new(),
            live: HashMap::new(),
            items: HashMap::new(),
            props: HashMap::new(),
            props_asked: HashMap::new(),
            tiles: HashMap::new(),
            ramps: HashMap::new(),
            sort_clicks: vec![],
            pin_clicked: false,
            f: Frame::default(),
        })
    }
}

// ── tiny builders ───────────────────────────────────────────────────────────────

fn col(c: Rgba) -> Color {
    Color::rgba(c.0, c.1, c.2, c.3)
}

fn grow() -> Val {
    Val { type_: val_type::GROW, value: 0.0, is_auto: false }
}

fn px(v: i32) -> Val {
    Val::px(v as f32)
}

fn flex(w: Val, h: Val) -> Node {
    Node { width: w, height: h, ..Node::base() }
}

fn sized(w: i32, h: i32) -> Node {
    flex(px(w), px(h))
}

fn column(mut n: Node) -> Node {
    n.flex_direction = flex_direction::COLUMN;
    n
}

fn centred(mut n: Node) -> Node {
    n.justify_content = justify_content::CENTER;
    n.align_items = align_items::CENTER;
    n
}

fn pad(l: i32, r: i32, top: i32, b: i32) -> UiRect {
    UiRect { left: px(l), right: px(r), top: px(top), bottom: px(b) }
}

fn hue_vec(h: (f32, f32, f32)) -> Vector3 {
    Vector3 { x: h.0, y: h.1, z: h.2 }
}

fn art(asset: u32, hue: (f32, f32, f32), stacked: bool) -> UiCustomDto {
    UiCustomDto { kind: ART, asset_id: asset, hue_x: hue.0, hue_y: hue.1, hue_z: hue.2, stacked }
}

fn border(c: Rgba) -> BorderColorDto {
    BorderColorDto { r: c.0 as f32, g: c.1 as f32, b: c.2 as f32, a: c.3 as f32 }
}

fn bg(c: Rgba) -> BackgroundColor {
    BackgroundColor { value: col(c) }
}

fn px_or(v: &Val, fallback: i32) -> i32 {
    if v.type_ == val_type::PX {
        v.value as i32
    } else {
        fallback
    }
}

fn set_text(cmds: &mut Commands, entity: Entity, cache: &mut String, value: Option<&str>) {
    let Some(value) = value else { return };
    if cache == value {
        return;
    }
    *cache = value.to_string();
    cmds.entity(entity).insert(Text { value: value.to_string() });
}

fn label(node: Node, color: Rgba, size: u16) -> (Node, Text, TextFont, TextColor, TextWrap, Interaction) {
    (
        node,
        Text::default(),
        TextFont { font_id: 1, size },
        TextColor { value: col(color) },
        TextWrap { kind: text_wrap_kind::NONE },
        Interaction::NONE,
    )
}

fn consume_mouse(cmds: &mut Commands, button: u8) {
    cmds.send(InputConsume::mouse(button));
}

fn resolve(serial: u32) -> u64 {
    if serial == 0 {
        return 0;
    }
    host::resolve_serial(serial).unwrap_or(0)
}

// ── lookups over the state ──────────────────────────────────────────────────────

/// The backpack itself or anything nested in it (same 16-deep cap as the host).
fn in_backpack(s: &State, mut ent: u64, backpack: u64) -> bool {
    if backpack == 0 {
        return false;
    }
    for _ in 0..16 {
        if ent == 0 {
            return false;
        }
        if ent == backpack {
            return true;
        }
        ent = s.parent_of.get(&ent).copied().unwrap_or(0);
    }
    false
}

fn view_by_window(views: &BTreeMap<u32, View>, window: u64) -> Option<&View> {
    if window == 0 {
        return None;
    }
    views.values().find(|v| v.window.to_bits() == window)
}

/// The grid cell an entity is: (item serial, owning window root bits, pinned).
fn grid_cell(s: &State, entity: u64) -> Option<(u32, u64, bool)> {
    if entity == 0 {
        return None;
    }
    s.views.values().find_map(|v| {
        v.cells
            .iter()
            .find(|c| c.entity.to_bits() == entity)
            .map(|c| (c.serial, v.window.to_bits(), c.pinned))
    })
}

fn window_z(s: &State, window: Entity) -> i32 {
    s.f.windows.get(&window.to_bits()).map_or(i32::MIN, |w| w.1)
}

fn content_rect(s: &State, content: Entity) -> Option<(ComputedNode, f32)> {
    s.f.contents.get(&content.to_bits()).cloned()
}

fn contains(c: &ComputedNode, x: f32, y: f32) -> bool {
    x >= c.position.x && y >= c.position.y && x < c.position.x + c.size.x && y < c.position.y + c.size.y
}

fn tile(tiles: &mut HashMap<u16, Option<StaticInfo>>, graphic: u16) -> Option<StaticInfo> {
    tiles.entry(graphic).or_insert_with(|| assets::static_tile(graphic)).clone()
}

/// GetPolygoneColor(31, hue), the cell the dyed-bag panel shades from.
fn ramp31(ramps: &mut HashMap<u16, u32>, hue: u16) -> u32 {
    *ramps.entry(hue).or_insert_with(|| assets::hue_ramp(hue).and_then(|r| r.get(31).copied()).unwrap_or(0))
}

/// Asks the server for an OPL at most once per NAME_REFRESH_MS.
fn ask_props(s: &mut State, serial: u32) {
    let now = s.f.now;
    let at = s.props_asked.entry(serial).or_insert(f32::MIN);
    if now - *at >= NAME_REFRESH_MS {
        *at = now;
        actions::request_properties(serial);
    }
}

fn save_char(s: &mut State) {
    s.data.pins = s.pins.iter().copied().collect();
    s.data.pins.sort_unstable();
    storage::save(Scope::Character, &s.data);
    // What this character has now is what was just written.
    s.char_loaded = true;
}

fn invalidate_all(s: &mut State) {
    for v in s.views.values_mut() {
        v.last_sig = None;
    }
}

/// Forget a view's slot tree (`cmds` = also despawn its rows, the cells go with them).
fn forget_tree(view: &mut View, tinted: &mut HashSet<(u32, usize)>, cmds: Option<&mut Commands>) {
    if let Some(cmds) = cmds {
        for row in &view.rows {
            cmds.entity(*row).despawn();
        }
    }
    view.rows.clear();
    view.cells.clear();
    tinted.retain(|t| t.0 != view.serial);
}

fn drop_view(s: &mut State, serial: u32) {
    if let Some(mut view) = s.views.remove(&serial) {
        forget_tree(&mut view, &mut s.tinted, None);
        s.f.windows.remove(&view.window.to_bits());
        s.f.contents.remove(&view.content.to_bits());
        s.f.texts.remove(&view.search.to_bits());
    }
}

fn despawn_pin_menu(s: &mut State, cmds: &mut Commands) {
    // One menu at a time: every open popup goes (the host's own included).
    for p in std::mem::take(&mut s.f.popups) {
        cmds.entity(p).despawn();
    }
    if let Some((_, _, menu, _)) = s.pin_menu.take() {
        cmds.entity(menu).despawn();
    }
}

// ── setup ───────────────────────────────────────────────────────────────────────

fn setup(app: &mut App) {
    app.add_systems(Schedule::Startup, declare_options);
    // The snapshots skip idle frames: run-on-change, fed by resources and Changed
    // queries. What can't be change-filtered stays plain (a plain system runs while a
    // row matches, once on the frame its queries empty, then the host's idle-skip
    // parks it): the popups (a tag), and snap_live — the player's position, written
    // in place without a change tick, so it runs every in-game frame.
    app.add_systems(
        Schedule::First,
        (
            snap_input.run_on_change(),
            snap_game.after(snap_input).run_on_change(),
            snap_popups.after(snap_input),
            snap_layout.after(snap_game).run_on_change(),
            first.after(snap_layout).after(snap_popups),
        ),
    );
    app.add_systems(
        Schedule::Update,
        (
            item_feed.run_on_change(),
            snap_world.after(item_feed).run_on_change(),
            snap_live.after(item_feed),
            snap_layout.after(snap_world).run_on_change(),
            update.after(snap_world).after(snap_live),
        ),
    );
    app.add_systems(Schedule::PostUpdate, post);

    app.add_observer(on_container_opened);
    app.add_observer(on_container_closed);
    app.add_observer(on_container_slot);
    app.add_observer(on_click);
    app.add_observer(on_hotkey);
    app.add_observer(on_drop_sent);
    app.add_observer(on_move_result);
    app.add_observer(on_member_gone);
}

export_mod!(setup);

#[system]
fn declare_options(mut schema: ResMut<ModOptionsSchemaDto>) {
    let opt = |key: &str, label: &str, kind: u8, default: i32, keywords: &str| ModOptionDto {
        key: key.into(),
        label: label.into(),
        group: "Grid containers".into(),
        keywords: keywords.into(),
        kind,
        default,
        ..Default::default()
    };
    let mut wait = opt(OPT_MULTI_WAIT, "Multi-move wait (ms)", 1, 600, "grid move multiple throttle timeout");
    (wait.min, wait.max, wait.step) = (0, 2000, 50);
    schema.options = vec![
        opt(OPT_CONTAINERS, "Grid containers", 0, 0, "grid view search sort"),
        opt(OPT_QUICK_MOVE, "Quick move to last container", 0, 1, "grid ctrl click target border"),
        opt(OPT_MULTI_MOVE, "Multi-select items (Shift+drag)", 0, 0, "grid drag select move multiple items lasso"),
        wait,
    ];
}

// ── snapshots ───────────────────────────────────────────────────────────────────

#[system]
fn snap_input(
    mouse: Option<Res<MouseInputDto>>,
    kb: Option<Res<KeyboardInputDto>>,
    pick: Option<Res<UiPickDto>>,
    grabbed: Option<Res<GrabbedItem>>,
    target: Option<Res<TargetingState>>,
    hotkeys: Option<Res<HostHotkeysDto>>,
) {
    // No clock here (it changes every frame and would keep this awake): `update`
    // reads it. A skipped run means every resource equals what this one copied.
    let f = &mut st().f;
    f.mouse = mouse.map(|m| m.clone()).unwrap_or_default();
    f.keys = kb.map(|k| Keys { held: k.pressed.clone(), once: k.pressed_once.clone() }).unwrap_or_default();
    f.pick = pick.map(|p| p.clone()).unwrap_or_default();
    f.held_serial = grabbed.map_or(0, |g| g.serial);
    f.targeting = target.is_some_and(|t| t.is_targeting);
    f.host_hotkeys = hotkeys.map(|h| h.hotkeys.clone()).unwrap_or_default();
}

/// The open popups (UiPopup is a tag: no change ticks, so a plain query).
#[system]
fn snap_popups(popups: Query<&UiPopup>) {
    st().f.popups = popups.iter().map(|(e, _)| e).collect();
}

#[system]
fn snap_game(
    state: Option<Res<GameStateDto>>,
    ctx: Option<Res<GameContextDto>>,
    values: Option<Res<ModOptionsValuesDto>>,
    settings: Option<Res<GameSettingsDto>>,
    equipment: Query<&EquipmentSlotsDto, (With<Player>, Changed<EquipmentSlotsDto>)>,
) {
    let s = st();
    s.f.game_screen = state.is_some_and(|g| g.current == GAME_SCREEN);
    s.f.player = ctx.map(|c| (c.player_serial, c.player_name.clone())).unwrap_or_default();
    // The player entity (and its equipment) respawns on every login, so its first
    // sight arrives as a change; leave_game forgets the old one.
    if let Some((_, e)) = equipment.single() {
        s.f.backpack = e.serials.get(LAYER_BACKPACK).copied().unwrap_or(0);
    }

    let get = |k: &str, d: i32| values.as_ref().and_then(|v| v.values.get(k).copied()).unwrap_or(d);
    s.opts.containers = get(OPT_CONTAINERS, 0) != 0;
    s.opts.quick_move = get(OPT_QUICK_MOVE, 1) != 0;
    s.opts.multi_move = get(OPT_MULTI_MOVE, 0) != 0;
    s.opts.multi_wait = get(OPT_MULTI_WAIT, 600);
    if let Some(g) = settings {
        s.opts.grid_loot = g.grid_loot_type;
        s.opts.skip_empty_corpse = g.skip_empty_corpse;
    }
}

/// The grid windows' roots (size, display, z) and their content columns' rect +
/// scroll offset, as they change. Entries leave with their view (drop_view).
#[allow(clippy::type_complexity)]
#[system]
fn snap_layout(
    moved: Query<(&Node, &GlobalZIndex), (With<GridContainerWindow>, Changed<Node>)>,
    raised: Query<(&Node, &GlobalZIndex), (With<GridContainerWindow>, Changed<GlobalZIndex>)>,
    laid_out: Query<(&ComputedNode, &ScrollPosition), (With<UiScrollbar>, Changed<ComputedNode>)>,
    scrolled: Query<(&ComputedNode, &ScrollPosition), (With<UiScrollbar>, Changed<ScrollPosition>)>,
) {
    let s = st();
    for (e, (n, z)) in moved.iter().chain(raised.iter()) {
        if view_by_window(&s.views, e.to_bits()).is_some() {
            s.f.windows.insert(e.to_bits(), (n.clone(), z.value));
        }
    }
    for (e, (c, p)) in laid_out.iter().chain(scrolled.iter()) {
        if s.views.values().any(|v| v.content == e) {
            s.f.contents.insert(e.to_bits(), (c.clone(), p.offset_y));
        }
    }
}

/// The open containers' items: membership as it changes (a contained item's parent link
/// changing or first appearing; departures arrive through `on_member_gone`), graphic /
/// hue / amount when they change (a contained item's first sight counts as a change),
/// and the OPLs when they change. The OPL cache keeps every OPL seen, keyed by serial
/// (lookups only ever ask for contained items and containers): an item moving into a
/// container keeps the OPL it already had, which a contained-only query would only see
/// again on its next change.
#[system]
fn item_feed(
    members: Query<(&ChildOf, &Serial), (With<ContainedInto>, Changed<ChildOf>)>,
    graphics: Query<(&Graphic, &Serial), (With<ContainedInto>, Changed<Graphic>)>,
    hues: Query<(&Hue, &Serial), (With<ContainedInto>, Changed<Hue>)>,
    amounts: Query<(&Amount, &Serial), (With<ContainedInto>, Changed<Amount>)>,
    props: Query<&EntityPropertiesDto, Changed<EntityPropertiesDto>>,
) {
    let s = st();
    for (e, (parent, serial)) in &members {
        join_member(s, e.to_bits(), parent.parent.to_bits(), serial.value);
    }
    for (_, (g, serial)) in &graphics {
        s.items.entry(serial.value).or_default().graphic = g.value;
    }
    for (_, (h, serial)) in &hues {
        s.items.entry(serial.value).or_default().hue = h.value;
    }
    for (_, (a, serial)) in &amounts {
        s.items.entry(serial.value).or_default().amount = a.value;
    }
    // Items that left every container drop out; coming back re-sends their data.
    prune_items(s);
    for (_, p) in &props {
        s.props.insert(p.serial, p.clone());
    }
}

/// A contained item left its container: moved out (equipped — the host drops its slot
/// position with its contained state) or left the client. Every contained item carries
/// a slot position and nothing else does, so this fires only for real departures — a
/// ChildOf removal would also fire for every UI node the host despawns.
#[system]
fn on_member_gone(on: On<Remove, ContainerSlotPosition>) {
    let s = st();
    leave_member(s, on.entity().to_bits());
    prune_items(s);
}

fn prune_items(s: &mut State) {
    let live = &s.live;
    s.items.retain(|serial, _| live.contains_key(serial));
}

/// `members[parent]` = the serials of its item entities, in arrival order.
fn rebuild_members(s: &mut State, parent: u64) {
    match s.children.get(&parent) {
        Some(kids) if !kids.is_empty() => {
            let serials = kids.iter().filter_map(|e| s.member_serial.get(e).copied()).collect();
            s.members.insert(parent, serials);
        }
        _ => {
            s.children.remove(&parent);
            s.members.remove(&parent);
        }
    }
}

fn join_member(s: &mut State, e: u64, parent: u64, serial: u32) {
    let old_parent = s.parent_of.get(&e).copied();
    if old_parent != Some(parent) {
        leave_member(s, e);
        s.children.entry(parent).or_default().push(e);
        s.parent_of.insert(e, parent);
    } else if let Some(old) = s.member_serial.get(&e).copied() {
        if old == serial {
            return;
        }
        unlive(s, old);
    }
    s.member_serial.insert(e, serial);
    *s.live.entry(serial).or_default() += 1;
    rebuild_members(s, parent);
}

fn leave_member(s: &mut State, e: u64) {
    let Some(parent) = s.parent_of.remove(&e) else { return };
    if let Some(kids) = s.children.get_mut(&parent) {
        kids.retain(|k| *k != e);
    }
    if let Some(serial) = s.member_serial.remove(&e) {
        unlive(s, serial);
    }
    rebuild_members(s, parent);
}

fn unlive(s: &mut State, serial: u32) {
    if let Some(n) = s.live.get_mut(&serial) {
        *n -= 1;
        if *n == 0 {
            s.live.remove(&serial);
        }
    }
}

/// Container looks + positions, the player's weight and our search fields' text, as
/// they change.
///
/// Containers: the host's IsContainer is a tag (no change ticks) that lands only when
/// a container's contents first arrive — long after its graphic did — so a
/// change-filtered IsContainer query would miss it. Instead every item's graphic /
/// hue / position change is seen, and the container-like ones (tiledata container
/// flag, corpses) kept, by entity. Lookups go through the serial's live entity, so a
/// despawned container's entry is never read; those are swept by size (and on logout).
#[allow(clippy::type_complexity)]
#[system]
fn snap_world(
    graphics: Query<(&Graphic, &Hue, &Serial), (With<IsItem>, Changed<Graphic>)>,
    hues: Query<(&Graphic, &Hue, &Serial), (With<IsItem>, Changed<Hue>)>,
    moved: Query<(&WorldPosition, &Graphic, &Serial), (With<IsItem>, Changed<WorldPosition>)>,
    player_data: Query<&PlayerData, (With<Player>, Changed<PlayerData>)>,
    texts: Query<&Text, (With<EditableText>, Changed<Text>)>,
) {
    let s = st();
    for (e, (g, h, serial)) in graphics.iter().chain(hues.iter()) {
        let key = e.to_bits();
        if container_like(&mut s.tiles, g.value) {
            s.f.looks.insert(key, (serial.value, g.value, h.value));
        } else {
            s.f.looks.remove(&key);
            s.f.positions.remove(&key);
        }
    }
    for (e, (p, g, serial)) in &moved {
        if container_like(&mut s.tiles, g.value) {
            s.f.positions.insert(e.to_bits(), (serial.value, p.clone()));
        }
    }
    sweep_world(&mut s.f);
    if let Some((_, d)) = player_data.single() {
        s.f.player_weight = Some((d.weight, d.weight_max));
    }
    for (e, t) in &texts {
        if s.views.values().any(|v| v.search == e) {
            s.f.texts.insert(e.to_bits(), t.value.clone());
        }
    }
}

/// Anything the client may open as a container window: tiledata container flag, or a
/// corpse.
fn container_like(tiles: &mut HashMap<u16, Option<StaticInfo>>, graphic: u16) -> bool {
    graphic == CORPSE_GRAPHIC || tile(tiles, graphic).is_some_and(|t| t.flags & TILE_CONTAINER != 0)
}

/// Drop the entries of despawned containers once the maps outgrow twice their live
/// size (amortised: one serial lookup per entry added since the last sweep).
fn sweep_world(f: &mut Frame) {
    if f.looks.len() + f.positions.len() <= f.world_cap.max(WORLD_SWEEP_MIN) {
        return;
    }
    f.looks.retain(|e, l| resolve(l.0) == *e);
    f.positions.retain(|e, p| resolve(p.0) == *e);
    f.world_cap = (f.looks.len() + f.positions.len()) * 2;
}

/// What has no usable change ticks, read every frame: the player's position (the
/// movement systems write it in place, no tick) and who opened which corpse (tags).
/// Plain queries; see setup.
#[system]
fn snap_live(
    player: Query<&WorldPosition, With<Player>>,
    auto: Query<&AutoOpenedCorpse>,
    manual: Query<&ManualOpenedCorpse>,
) {
    let f = &mut st().f;
    f.player_pos = player.single().map(|(_, p)| p.clone());
    f.auto_corpses = auto.iter().map(|(e, _)| e.to_bits()).collect();
    f.manual_corpses = manual.iter().map(|(e, _)| e.to_bits()).collect();
}

// ── First ───────────────────────────────────────────────────────────────────────

#[system]
fn first(mut cmds: Commands) {
    let s = st();
    if !s.f.game_screen {
        if s.in_game {
            leave_game(s, &mut cmds);
        }
        return;
    }
    s.in_game = true;

    claims(s, &mut cmds);
    load_char(s);
    track_quick_move_target(s);

    let mut left_pressed = s.f.mouse.left_pressed;
    if s.f.held_serial == 0 {
        pin_context_menu(s, &mut cmds);
        let pick = s.f.pick.clone();
        if !s.f.targeting && quick_move(s, &mut cmds, &pick, false) {
            left_pressed = false;
        }
    }
    if s.opts.multi_move {
        select(s, &mut cmds, left_pressed);
    }
}

/// Keep the classic-window claims in step with the options: grid loot 1 = grid only
/// (claim corpses); 2 = both windows (no claim).
fn claims(s: &mut State, cmds: &mut Commands) {
    let want = (s.opts.containers, s.opts.grid_loot == 1);
    for (on, slot, feature) in [(want.0, &mut s.claims.0, CLAIM_CONTAINERS), (want.1, &mut s.claims.1, CLAIM_CORPSES)] {
        match (on, *slot) {
            (true, None) => *slot = Some(cmds.spawn(ModSupersedes { feature: feature.into() }).id()),
            (false, Some(e)) => {
                cmds.entity(e).despawn();
                *slot = None;
            }
            _ => {}
        }
    }
}

/// Pins + free placement come back from character storage once per character.
fn load_char(s: &mut State) {
    if s.char_key.as_ref() != Some(&s.f.player) {
        s.char_key = Some(s.f.player.clone());
        s.char_loaded = false;
        s.data = CharData::default();
        s.pins.clear();
        s.pins_seen.clear();
    }
    if s.char_loaded {
        return;
    }
    // Nothing until the character's profile is loaded (or nothing was ever saved):
    // keep asking until a blob arrives or this character writes one.
    let Some(data) = storage::load::<CharData>(Scope::Character) else { return };
    s.char_loaded = true;
    s.data = data;
    s.pins = s.data.pins.iter().copied().collect();
    invalidate_all(s);
}

fn leave_game(s: &mut State, cmds: &mut Commands) {
    s.in_game = false;
    // Logout despawns the windows (host DisposeOnLogout) without a close event.
    let serials: Vec<u32> = s.views.keys().copied().collect();
    for serial in serials {
        drop_view(s, serial);
    }
    // Logout despawns every item; a stale Seen would read as deleted.
    s.pins_seen.clear();
    // ...and the player and every container with it: their next sight is a change.
    s.f.backpack = 0;
    s.f.player_weight = None;
    s.f.looks.clear();
    s.f.positions.clear();
    s.f.world_cap = 0;
    s.members.clear();
    s.parent_of.clear();
    s.member_serial.clear();
    s.children.clear();
    s.live.clear();
    s.items.clear();
    s.props.clear();
    if let Some(b) = s.select_box.take() {
        cmds.entity(b).despawn();
    }
    s.pin_menu = None;
    s.sel.dragging = false;
    s.sel.boxed = false;
    s.sel.clear();
    s.tinted.clear();
    s.queue.items.clear();
    s.queue.wait_until = 0.0;
}

/// Any plain click on a grid window makes it the quick-move destination. A ctrl+click
/// is the move gesture itself and never re-targets; a Shift press is the multi-select
/// box, not a pick of the destination.
fn track_quick_move_target(s: &mut State) {
    let f = &s.f;
    if !s.opts.quick_move || !f.mouse.left_pressed || f.keys.held(KEY_LCTRL) || f.keys.held(KEY_RCTRL) {
        return;
    }
    if s.opts.multi_move && f.keys.shift() {
        return;
    }
    if f.pick.entity == 0 {
        return;
    }
    if let Some(view) = view_by_window(&s.views, f.pick.root) {
        s.target = view.serial;
    }
}

/// Right-click a cell in the backpack (or a bag nested in it): one-entry Pin / Unpin
/// menu at the cursor. The press is consumed so the window under it doesn't
/// right-click-close and the host doesn't dismiss the menu just opened.
fn pin_context_menu(s: &mut State, cmds: &mut Commands) {
    if !s.f.mouse.right_pressed {
        return;
    }
    let Some((item, window, _)) = grid_cell(s, s.f.pick.entity) else { return };
    let container = view_by_window(&s.views, window).map_or(0, |v| v.serial);
    let container_ent = resolve(container);
    if container_ent == 0 || !in_backpack(s, container_ent, resolve(s.f.backpack)) {
        return;
    }
    consume_mouse(cmds, MOUSE_RIGHT);
    despawn_pin_menu(s, cmds);

    let mut root = centred(Node::abs(
        s.f.mouse.x as i32 as f32,
        s.f.mouse.y as i32 as f32,
        PIN_MENU_W as f32,
        (PIN_MENU_ROW_H + PIN_MENU_PAD * 2) as f32,
    ));
    root.display = display::FLEX;
    let menu = cmds
        .spawn((
            root,
            bg(BG),
            BorderRadius::all(4.0),
            Interaction::NONE,
            UiPopup {},
            UiNoWindowDrag {},
            GlobalZIndex { value: -1 },
        ))
        .id();
    let caption = if s.pins.contains(&item) { "Unpin" } else { "Pin" };
    let entry = cmds
        .spawn((
            centred(sized(PIN_MENU_W - PIN_MENU_PAD * 2, PIN_MENU_ROW_H)),
            bg(CONTROL),
            BorderRadius::all(4.0),
            Text { value: caption.into() },
            TextFont { font_id: 1, size: 11 },
            TextColor { value: col(TEXT) },
            Interaction::NONE,
            UiNoWindowDrag {},
            UiName { value: "grid:pin".into() },
            ChildOf::new(menu),
        ))
        .id();
    s.pin_menu = Some((item, container, menu, entry));
}

/// Ctrl + left press on a cell (or the QuickMoveItem mouse/wheel binding over one):
/// lift + drop straight into the highlighted window, else the topmost other grid
/// window, else the backpack. Returns true when it consumed the press.
fn quick_move(s: &mut State, cmds: &mut Commands, pick: &UiPickDto, by_hotkey: bool) -> bool {
    if !s.opts.quick_move {
        return false;
    }
    if !by_hotkey && (!s.f.mouse.left_pressed || !quick_move_key_held(s)) {
        return false;
    }
    let Some((item, window, pinned)) = grid_cell(s, pick.entity) else { return false };
    if pinned {
        return false;
    }
    let source = view_by_window(&s.views, window).map_or(0, |v| v.serial);

    let mut target = 0;
    if s.target != 0 && s.target != source && s.views.contains_key(&s.target) {
        target = s.target;
    }
    if target == 0 {
        let mut top_z = i32::MIN;
        for view in s.views.values() {
            if view.serial == source {
                continue;
            }
            let z = window_z(s, view.window);
            if z <= top_z {
                continue;
            }
            top_z = z;
            target = view.serial;
        }
    }
    if target == 0 && s.f.backpack != source {
        target = s.f.backpack;
    }
    if target == 0 {
        return false;
    }

    let amount = s.items.get(&item).map_or(1, |it| it.amount.clamp(1, 0xFFFF));
    actions::move_item(item, amount as u16, Destination::Container(target));

    if by_hotkey {
        return false;
    }
    // Neither a pickup nor a world click: consume the press (the host closes the
    // pickup latch for this hold along with it).
    consume_mouse(cmds, MOUSE_LEFT);
    true
}

/// HotkeyPlugin.KeyHeld(QuickMoveItem): the first key binding of the action.
fn quick_move_key_held(s: &State) -> bool {
    s.f.host_hotkeys
        .iter()
        .find(|h| h.action == "QuickMoveItem")
        .is_some_and(|h| s.f.keys.binding_held(h.key, h.ctrl, h.shift, h.alt))
}

/// Shift + drag on empty grid space draws a box; on release every visible, unpinned
/// cell of that window whose centre it covers is selected.
fn select(s: &mut State, cmds: &mut Commands, left_pressed: bool) {
    let (x, y) = (s.f.mouse.x, s.f.mouse.y);
    if s.f.keys.once(KEY_ESCAPE) && !s.sel.dragging {
        s.sel.clear();
    }

    if s.sel.dragging {
        let (min, max) = boxed(s.sel.start, (x, y));
        if let Some(b) = s.select_box {
            let mut n = Node::abs(min.0, min.1, max.0 - min.0, max.1 - min.1);
            n.border = UiRect::splat(Val::px(1.0));
            cmds.entity(b).insert(n);
        }
        if s.f.mouse.left {
            consume_mouse(cmds, MOUSE_LEFT);
            return;
        }
        s.sel.dragging = false;
        if let Some(b) = s.select_box.take() {
            cmds.entity(b).despawn();
        }
        s.sel.boxed = false;
        s.sel.serials.clear();
        let Some(view) = view_by_window(&s.views, s.sel.window) else { return };
        // The cells sit on a CELL grid inside the content column, scrolled by its
        // offset; the column is the cells' only clip (UiPick.HiddenOrClippedOut).
        let Some((clip, offset_y)) = content_rect(s, view.content) else { return };
        let cols = cols(s.f.windows.get(&view.window.to_bits()).map_or(s.last_size.0, |w| px_or(&w.0.width, s.last_size.0)));
        let mut picked = vec![];
        for cell in view.cells.iter().filter(|c| !c.pinned) {
            let (column, row) = (cell.slot as i32 % cols, cell.slot as i32 / cols);
            let cx = clip.position.x + (column * CELL) as f32 + CELL as f32 * 0.5;
            let cy = clip.position.y + (row * CELL) as f32 + CELL as f32 * 0.5 - offset_y;
            if cx < min.0 || cy < min.1 || cx > max.0 || cy > max.1 || !contains(&clip, cx, cy) {
                continue;
            }
            picked.push(cell.serial);
        }
        s.sel.serials.extend(picked);
        return;
    }

    if !left_pressed {
        return;
    }

    if let Some((item, _, _)) = grid_cell(s, s.f.pick.entity) {
        // A press on an item: a selected one starts the move (normal pickup); an
        // unselected one without Shift drops the selection.
        if !s.sel.serials.contains(&item) && !s.f.keys.shift() {
            s.sel.clear();
        }
        return;
    }

    let shift = s.f.keys.shift();
    let root = if s.f.pick.entity != 0 { s.f.pick.root } else { 0 };
    let Some(view) = (if shift { view_by_window(&s.views, root) } else { None }) else {
        if !shift {
            s.sel.clear();
        }
        return;
    };

    // Only inside the item area — the header keeps its own gestures.
    if !content_rect(s, view.content).is_some_and(|(c, _)| contains(&c, x, y)) {
        return;
    }
    let container = view.serial;

    if s.sel.window != root {
        s.sel.clear();
    }
    s.sel.window = root;
    s.sel.container = container;
    s.sel.dragging = true;
    s.sel.start = (x, y);
    let mut n = Node::abs(x, y, 0.0, 0.0);
    n.border = UiRect::splat(Val::px(1.0));
    s.select_box =
        Some(cmds.spawn((n, bg(BOX_FILL), border(ACCENT), GlobalZIndex { value: OVERLAY_FLOOR })).id());
    s.sel.boxed = true;
    consume_mouse(cmds, MOUSE_LEFT);
}

fn boxed(a: (f32, f32), b: (f32, f32)) -> ((f32, f32), (f32, f32)) {
    ((a.0.min(b.0), a.1.min(b.1)), (a.0.max(b.0), a.1.max(b.1)))
}

// ── observers ───────────────────────────────────────────────────────────────────

#[system]
fn on_container_opened(ev: On<Event, ContainerOpenedEvent>, positions: Option<ResMut<ContainerPositionsDto>>, mut cmds: Commands) {
    open_window(&mut cmds, positions, ev.serial, ev.graphic);
}

#[system]
fn on_container_closed(ev: On<Event, ContainerClosedEvent>, mut cmds: Commands) {
    let s = st();
    if let Some(view) = s.views.get(&ev.serial) {
        cmds.entity(view.window).despawn();
    }
    drop_view(s, ev.serial);
}

/// Any slot event for a container forces its grid to rebuild: the host's classic
/// container plugin despawns every ContainerItemUI carrying the updated serial —
/// grid cells included — so an unchanged-looking item set still needs new cells.
/// An Add also carries the item's new graphic / hue / amount.
#[system]
fn on_container_slot(ev: On<Event, ContainerSlotEvent>) {
    let s = st();
    if ev.action == 0 {
        let it = s.items.entry(ev.item_serial).or_default();
        it.graphic = ev.graphic;
        it.hue = ev.hue;
        it.amount = ev.amount as i32;
    }
    if ev.container_serial != 0 {
        if let Some(view) = s.views.get_mut(&ev.container_serial) {
            view.last_sig = None;
        }
    }
}

/// Sort buttons and the pin menu's entry; handled in Update.
#[system]
fn on_click(click: On<Event, UiClick>) {
    let s = st();
    let e = click.entity();
    if s.pin_menu.is_some_and(|(_, _, _, entry)| entry == e) {
        s.pin_clicked = true;
    } else if let Some(view) = s.views.values().find(|v| v.sort_btn == e) {
        s.sort_clicks.push(view.serial);
    }
}

/// QuickMoveItem bound to a mouse button / the wheel: move the hovered item.
#[system]
fn on_hotkey(ev: On<Event, ModHotkeyFired>, pick: Option<Res<UiPickDto>>, mut cmds: Commands) {
    if ev.from_mod || ev.name != "QuickMoveItem" || ev.key != 0 {
        return;
    }
    let s = st();
    if !s.in_game || s.f.held_serial != 0 || s.f.targeting {
        return;
    }
    let pick = pick.map(|p| p.clone()).unwrap_or_default();
    quick_move(s, &mut cmds, &pick, true);
}

/// A selected item was just dropped (a drag, a quick move): queue the rest of the
/// selection for the same destination. Every drop request arrives here, one each.
#[system]
fn on_drop_sent(ev: On<Event, ItemDropSent>, time: Option<Res<Time>>) {
    let s = st();
    if !s.sel.serials.contains(&ev.serial) {
        return;
    }
    let now = time.map_or(s.f.now, |t| t.total);
    enqueue(&mut s.queue, &mut s.sel, ev.serial, ev.x, ev.y, ev.z, ev.container, now, s.opts.multi_wait);
}

/// The server answered the move in flight: accepted → the next one may go now;
/// denied / dragging ended → stop (the rest would fail the same way).
#[system]
fn on_move_result(ev: On<Event, ItemMoveResult>) {
    let q = &mut st().queue;
    if q.wait_until == 0.0 {
        return;
    }
    q.wait_until = 0.0;
    if ev.result != 0 {
        q.items.clear();
    }
}

// ── open / close ────────────────────────────────────────────────────────────────

fn open_window(cmds: &mut Commands, positions: Option<ResMut<ContainerPositionsDto>>, serial: u32, graphic: u16) {
    let s = st();
    if !s.in_game || graphic == 0xFFFF || graphic == 0x0030 {
        return;
    }
    // Corpse grids ride the host's GridLootType (0 = the classic corpse window only),
    // everything else this mod's own option.
    let is_corpse = graphic == CORPSE_GUMP;
    if if is_corpse { s.opts.grid_loot == 0 } else { !s.opts.containers } {
        return;
    }

    // Already open: the server re-sends the container on every double click, so raise
    // the window. A negative z asks the host for the counter's top (UiZCounter.Bump).
    if let Some(view) = s.views.get(&serial) {
        cmds.entity(view.window).insert(GlobalZIndex { value: -1 });
        return;
    }

    let (w, h) = s.last_size;
    // A backend switch (or the gumps.xml restore) stashes the window's last spot;
    // consume it once, else default.
    let (mut left, mut top) = (120, 120);
    if let Some(mut mem) = positions {
        if let Some(i) = mem.saved.iter().position(|e| e.serial == serial) {
            let e = mem.saved.remove(i);
            (left, top) = (e.x, e.y);
        }
    }

    // Root: the floating window, laid out as a column — header, content, footer.
    let mut root_node = column(Node::abs(left as f32, top as f32, w as f32, h as f32));
    root_node.border = UiRect::splat(Val::px(2.0));
    let container_ent = resolve(serial);
    let window = cmds
        .spawn((
            root_node,
            bg(BG),
            border(FRAME),
            BorderRadius::all(8.0),
            Interaction::NONE,
            UiMovable {},
            GlobalZIndex { value: -1 },
            ContainerWindow { serial },
            // ContainerGumpTag makes the host's DropItem recognise a drop onto this
            // window (Case 1). Bounds = the content rect so drop coords clamp inside
            // it; the grid re-lays out by slot anyway. Never updated on resize.
            ContainerGumpTagDto {
                graphic,
                original_graphic: graphic,
                scale: 1.0,
                bounds_x: INSET,
                bounds_y: HEADER_H,
                bounds_w: w - INSET * 2,
                bounds_h: content_h(h),
            },
            GridContainerWindow { serial },
            // The bottom-right grip is the host's resize gesture, grabbed over the grip
            // square drawn in the footer (14 px, 2 px in from the corner), whole pixels.
            UiResizable {
                min_w: MIN_WIN as f32,
                min_h: MIN_WIN as f32,
                max_w: MAX_WIN as f32,
                max_h: MAX_WIN as f32,
                grip: GRIP as f32,
                grip_inset: 2.0,
                whole_pixels: true,
            },
        ))
        .id();
    if is_corpse {
        cmds.entity(window)
            .insert(GridLootWindow { corpse_serial: serial, corpse_entity: container_ent, content_sig: -1 });
    }

    // Header: [icon] [title / (search · sort)], 4 below the frame, 4 above the grid.
    let mut header = flex(grow(), px(HEADER_H));
    header.padding = pad(INSET, INSET, 4, 4);
    header.gap = px(HEADER_GAP);
    let header = cmds.spawn((header, Interaction::NONE, ChildOf::new(window))).id();

    // The icon sits 2 below the title row (y 6 vs 4).
    let mut icon_col = column(flex(px(ICON_SIZE), Val::auto()));
    icon_col.padding = pad(0, 0, ICON_TOP - 4, 0);
    let icon_col = cmds.spawn((icon_col, Interaction::NONE, ChildOf::new(header))).id();
    let icon = cmds
        .spawn((sized(ICON_SIZE, ICON_SIZE), art(0, (0.0, 0.0, 0.0), false), Interaction::NONE, ChildOf::new(icon_col)))
        .id();

    let mut head_col = column(flex(grow(), Val::auto()));
    head_col.gap = px(HEADER_GAP);
    let head_col = cmds.spawn((head_col, Interaction::NONE, ChildOf::new(header))).id();

    let mut title = centred(flex(grow(), px(TITLE_H)));
    title.overflow = overflow::CLIP;
    let title = cmds.spawn((label(title, TEXT, 12), ChildOf::new(head_col))).id();

    let mut search_row = flex(grow(), px(18));
    search_row.gap = px(HEADER_GAP);
    let search_row = cmds.spawn((search_row, Interaction::NONE, ChildOf::new(head_col))).id();

    // Search field (filters by item name): a self-editable field — the host focuses it
    // on press and its shared editor owns the keystrokes and the caret.
    let mut search = flex(grow(), px(18));
    search.align_items = align_items::CENTER;
    search.padding = pad(5, 0, 0, 0);
    search.overflow = overflow::CLIP;
    let search = cmds
        .spawn((
            search,
            bg(FIELD),
            BorderRadius::all(4.0),
            Text::default(),
            TextFont { font_id: 1, size: 12 },
            TextColor { value: col(TEXT) },
            Interaction::NONE,
            UiContainsByBounds {},
            UiNoWindowDrag {},
            TextInput {},
            EditableText { multiline: false },
            ChildOf::new(search_row),
        ))
        .id();

    // Sort button (cycles Free/Name/Layer/Type; caption tracks the mode).
    let sort_btn = cmds
        .spawn((
            centred(sized(SORT_W, 18)),
            bg(CONTROL),
            BorderRadius::all(4.0),
            Text { value: Sort::Free.label().into() },
            TextFont { font_id: 1, size: 11 },
            TextColor { value: col(TEXT) },
            Interaction::NONE,
            UiNoWindowDrag {},
            ChildOf::new(search_row),
        ))
        .id();

    // The scrollable item column; the flat scrollbar's lane is reserved as right
    // padding, which cols() leaves room for.
    let mut wrap = flex(grow(), grow());
    wrap.padding = pad(INSET, INSET, 0, 0);
    let wrap = cmds.spawn((wrap, Interaction::NONE, ChildOf::new(window))).id();
    let mut content = column(flex(grow(), grow()));
    content.overflow = overflow::SCROLL;
    let content = cmds
        .spawn((
            content,
            Interaction::NONE,
            ScrollPosition::default(),
            UiScrollbar { width: 0.0, style: scrollbar_style::FLAT },
            ChildOf::new(wrap),
        ))
        .id();

    // Footer: the weight readout centred under the grid, the grip in the corner.
    let mut footer = flex(grow(), px(FOOTER_H + INSET));
    footer.padding = pad(INSET * 2, 2, 0, 2);
    let footer = cmds.spawn((footer, Interaction::NONE, ChildOf::new(window))).id();
    let mut weight = centred(flex(grow(), px(FOOTER_H)));
    weight.overflow = overflow::CLIP;
    let weight = cmds.spawn((label(weight, TEXT_MUTED, 12), ChildOf::new(footer))).id();
    let mut grip_col = column(flex(px(GRIP), grow()));
    grip_col.justify_content = justify_content::END;
    let grip_col = cmds.spawn((grip_col, Interaction::NONE, ChildOf::new(footer))).id();
    cmds.spawn((
        sized(GRIP, GRIP),
        bg(FRAME),
        BorderRadius::all(4.0),
        Interaction::NONE,
        UiNoWindowDrag {},
        ChildOf::new(grip_col),
    ));

    s.views.insert(
        serial,
        View {
            serial,
            is_corpse,
            sort: Sort::Free,
            last_sig: None,
            window,
            content,
            search,
            title,
            sort_btn,
            icon,
            weight,
            rows: vec![],
            cells: vec![],
            title_text: String::new(),
            weight_text: String::new(),
            sort_text: Sort::Free.label().into(),
            icon_asset: 0,
            panel: BG,
            border: FRAME,
            loot: None,
        },
    );
}

// ── Update ──────────────────────────────────────────────────────────────────────

#[system]
fn update(time: Option<Res<Time>>, mut cmds: Commands) {
    let s = st();
    s.f.now = time.map_or(0.0, |t| t.total);
    let sort_clicks = std::mem::take(&mut s.sort_clicks);
    let pin_clicked = std::mem::take(&mut s.pin_clicked);
    if !s.in_game {
        return;
    }

    for serial in sort_clicks {
        if let Some(view) = s.views.get_mut(&serial) {
            view.sort = view.sort.next();
            view.last_sig = None;
        }
    }
    if pin_clicked {
        if let Some((item, container, _, _)) = s.pin_menu {
            if !s.pins.remove(&item) {
                s.pins.insert(item);
            }
            save_char(s);
            // The cell's hue + the pin marker are baked at rebuild time.
            if let Some(view) = s.views.get_mut(&container) {
                view.last_sig = None;
            }
            despawn_pin_menu(s, &mut cmds);
        }
    }

    highlight_quick_move_target(s, &mut cmds);
    if s.f.held_serial == 0 {
        prune_pins(s);
    }
    close_gone_corpses(s);
    if s.f.held_serial != 0 && s.f.mouse.left_released {
        record_drop_slot(s);
    }
    rebuild_grids(s, &mut cmds);
}

/// Accent ring on the quick-move destination, the theme frame on every other window
/// (same width, so flipping it never moves the contents).
fn highlight_quick_move_target(s: &mut State, cmds: &mut Commands) {
    let target = s.target;
    for view in s.views.values_mut() {
        let want = if s.opts.quick_move && view.serial == target { ACCENT } else { FRAME };
        if view.border == want {
            continue;
        }
        view.border = want;
        cmds.entity(view.window).insert(border(want));
    }
}

/// A pinned item seen inside the backpack that since left it (or got deleted) loses
/// its pin. Unseen ones may simply not be loaded yet.
fn prune_pins(s: &mut State) {
    if s.pins.is_empty() {
        return;
    }
    let backpack = resolve(s.f.backpack);
    if backpack == 0 {
        return;
    }
    let mut gone = vec![];
    let mut seen = vec![];
    for &serial in &s.pins {
        let ent = resolve(serial);
        if ent != 0 && in_backpack(s, ent, backpack) {
            seen.push(serial);
        } else if s.pins_seen.contains(&serial) {
            gone.push(serial);
        }
    }
    s.pins_seen.extend(seen);
    if gone.is_empty() {
        return;
    }
    for serial in gone {
        s.pins.remove(&serial);
        s.pins_seen.remove(&serial);
    }
    save_char(s);
    // A bag carried out keeps its contents: its open window must drop the look.
    invalidate_all(s);
}

/// A corpse grid closes itself when the corpse is gone or the player walks out of
/// reach (legacy GridLootGump) — re-sent every frame until the window goes.
fn close_gone_corpses(s: &mut State) {
    for view in s.views.values().filter(|v| v.is_corpse) {
        let ent = resolve(view.serial);
        let mut gone = ent == 0;
        if !gone {
            if let (Some((_, pos)), Some(p)) = (s.f.positions.get(&ent), s.f.player_pos.as_ref()) {
                let dist = (pos.x as i32 - p.x as i32).abs().max((pos.y as i32 - p.y as i32).abs());
                gone = dist > CORPSE_MAX_DIST;
            }
        }
        if gone {
            actions::close_container(view.serial);
        }
    }
}

/// The release frame of a drag over a grid window: remember the held item at the tile
/// under the cursor (topmost grid window wins). Free mode lays it out there once the
/// server confirms the move.
fn record_drop_slot(s: &mut State) {
    let (x, y) = (s.f.mouse.x, s.f.mouse.y);
    let mut best = 0;
    let mut best_z = i32::MIN;
    let mut best_slot = 0;
    for view in s.views.values() {
        let Some((bb, offset_y)) = content_rect(s, view.content) else { continue };
        if !contains(&bb, x, y) {
            continue;
        }
        let z = window_z(s, view.window);
        if z <= best_z {
            continue;
        }
        let w = s.f.windows.get(&view.window.to_bits()).map_or(s.last_size.0, |w| px_or(&w.0.width, s.last_size.0));
        let cols = cols(w);
        let column = (((x - bb.position.x) / CELL as f32) as i32).min(cols - 1);
        let row = ((y - bb.position.y + offset_y) / CELL as f32) as i32;
        best_z = z;
        best = view.serial;
        best_slot = row.max(0) * cols + column;
    }
    if best == 0 {
        return;
    }
    let held = s.f.held_serial;
    s.data.slots.entry(best).or_default().insert(held, best_slot);
    save_char(s);
}

/// Rebuild a window's slot tree when its signature changes; the header refreshes every
/// frame before the gate (late OPL data lands the frame it arrives). Never rebuilds
/// while an item is on the cursor, so a drag isn't interrupted.
fn rebuild_grids(s: &mut State, cmds: &mut Commands) {
    let backpack_ent = resolve(s.f.backpack);
    let hover = hue_vector(HOVER_HUE, 1.0);

    let serials: Vec<u32> = s.views.keys().copied().collect();
    for serial in serials {
        let container = resolve(serial);
        if container == 0 {
            continue;
        }
        let pinnable = in_backpack(s, container, backpack_ent);

        let props = s.props.get(&serial).cloned();
        if props.is_none() {
            ask_props(s, serial);
        }

        // Title = container name from its OPL; the item graphic's tiledata name meanwhile.
        let mut title = props.as_ref().filter(|p| !p.name.is_empty()).map(|p| p.name.clone());
        let look = s.f.looks.get(&container).map(|l| (l.1, l.2)).or_else(|| s.items.get(&serial).map(|i| (i.graphic, i.hue)));
        if let Some((graphic, hue)) = look {
            let tl = tile(&mut s.tiles, graphic);
            if title.is_none() {
                title = tl.as_ref().map(|t| t.name.clone());
            }
            let ramp = if hue & 0x3FFF == 0 { 0 } else { ramp31(&mut s.ramps, hue & 0x3FFF) };
            let tinted = panel_color(hue, ramp);
            let view = s.views.get_mut(&serial).unwrap();
            if view.icon_asset != graphic as u32 {
                view.icon_asset = graphic as u32;
                cmds.entity(view.icon).insert(art(graphic as u32, hue_vector(hue, 1.0), false));
            }
            // A dyed bag tints its own window (hue 0 = the plain panel).
            if view.panel != tinted {
                view.panel = tinted;
                cmds.entity(view.window).insert(bg(tinted));
            }
        }
        let weight = if serial == s.f.backpack {
            s.f.player_weight.map(|(w, max)| format!("{w}/{max} stones"))
        } else {
            None
        }
        .unwrap_or_else(|| {
            let data = props.as_ref().map(|p| p.lines.iter().skip(1).map(|l| l.text.as_str()).collect::<Vec<_>>().join("\n"));
            parse_stones(data.as_deref())
        });
        {
            let view = s.views.get_mut(&serial).unwrap();
            let (title_ent, weight_ent, sort_ent) = (view.title, view.weight, view.sort_btn);
            set_text(cmds, title_ent, &mut view.title_text, title.as_deref());
            set_text(cmds, weight_ent, &mut view.weight_text, Some(&weight));
            set_text(cmds, sort_ent, &mut view.sort_text, Some(view.sort.label()));
        }

        if s.f.held_serial != 0 {
            continue;
        }

        let view = &s.views[&serial];
        let Some((root, win_z)) = s.f.windows.get(&view.window.to_bits()).cloned() else { continue };
        let w = px_or(&root.width, s.last_size.0);
        let hh = px_or(&root.height, s.last_size.1);
        // The host's resize writes only the root's size; the rest follows through flex.
        s.last_size = (w, hh);
        let cols = cols(w);
        let visible_rows = (content_h(hh) / CELL).max(1);
        let query = s.f.texts.get(&view.search.to_bits()).map(|t| t.trim().to_string()).unwrap_or_default();
        let (is_corpse, sort) = (view.is_corpse, view.sort);
        let need_names = !query.is_empty() || matches!(sort, Sort::Name | Sort::Layer);

        // The container's items, deduped by serial: a same-container drop leaves a
        // stale source entity with the moved item's serial for a frame.
        let members = s.members.get(&container).cloned().unwrap_or_default();
        let mut seen = HashSet::new();
        let mut list = vec![];
        for item_serial in members {
            if !seen.insert(item_serial) {
                continue;
            }
            let it = s.items.get(&item_serial).cloned().unwrap_or_default();
            let tl = tile(&mut s.tiles, it.graphic);
            let (in_table, layer, flags) = tl.as_ref().map_or((false, 0, 0), |t| (true, t.layer, t.flags));
            // Same filter the classic window applies.
            if should_skip_slot(in_table, layer, flags, is_corpse) {
                continue;
            }
            // The OPL (tooltip) name while searching / name-sorting; asked for when
            // missing so the full name upgrades the tiledata fallback.
            let mut opl_name = None;
            if need_names {
                opl_name = s.props.get(&item_serial).map(|p| p.name.clone()).filter(|n| !n.is_empty());
                if opl_name.is_none() {
                    ask_props(s, item_serial);
                }
            }
            let name = resolve_item_search_name(opl_name.as_deref(), tl.as_ref().map(|t| t.name.as_str()));
            if !matches_search(name.as_deref(), &query) {
                continue;
            }
            list.push(GridItem {
                serial: item_serial,
                graphic: it.graphic,
                hue: it.hue,
                amount: it.amount.clamp(1, 0xFFFF) as u16,
                layer,
                stacked: draw_stacked(it.graphic, it.amount, flags & TILE_STACKABLE != 0),
                name,
            });
        }

        let mut layout = vec![];
        let remembered = if sort == Sort::Free { Some(s.data.slots.entry(serial).or_default()) } else { None };
        lay_out(&mut list, sort, remembered, cols, visible_rows, &mut layout);
        let sig = sig(&list, &layout, cols, &query, sort);

        let view = s.views.get_mut(&serial).unwrap();
        // SkipEmptyCorpse: an auto-opened corpse that turns out empty stays hidden
        // (legacy GridLootGump._hideIfEmpty). Manual opens always show.
        if is_corpse {
            let lsig = (sig, container);
            if view.loot != Some(lsig) {
                view.loot = Some(lsig);
                cmds.entity(view.window).insert(GridLootWindow {
                    corpse_serial: serial,
                    corpse_entity: container,
                    content_sig: sig as i32,
                });
            }
            let hide = list.is_empty()
                && s.opts.skip_empty_corpse
                && s.f.auto_corpses.contains(&container)
                && !s.f.manual_corpses.contains(&container);
            let want = if hide { display::NONE } else { display::FLEX };
            if root.display != want {
                let mut n = root.clone();
                n.display = want;
                cmds.entity(view.window).insert(n);
            }
        }

        if view.last_sig == Some(sig) {
            continue;
        }
        view.last_sig = Some(sig);

        // Despawn the old rows (cells go with them).
        forget_tree(view, &mut s.tinted, Some(cmds));

        let mut row = None;
        for (slot, &index) in layout.iter().enumerate() {
            if slot as i32 % cols == 0 {
                let r = cmds.spawn((flex(Val::auto(), px(CELL)), Interaction::NONE, ChildOf::new(view.content))).id();
                view.rows.push(r);
                row = Some(r);
            }
            let row = row.unwrap();

            // The tile: background only, so a drop on an EMPTY tile resolves to the
            // window root (DropItem Case 1) and the drop-slot record keeps the slot.
            let tile_e = cmds.spawn((centred(sized(CELL, CELL)), Interaction::NONE, ChildOf::new(row))).id();
            let item = (index >= 0).then(|| &list[index as usize]);
            let pinned = pinnable && item.is_some_and(|it| s.pins.contains(&it.serial));
            let tile_bg = cmds
                .spawn((
                    centred(sized(TILE, TILE)),
                    bg(if pinned { PINNED_SLOT } else { PANEL }),
                    BorderRadius::all(4.0),
                    Interaction::NONE,
                    ChildOf::new(tile_e),
                ))
                .id();

            let Some(it) = item else { continue };
            // A pinned item paints in the legacy hidden-object look.
            let hue = if pinned { hue_vector(PINNED_HUE, PINNED_ALPHA) } else { hue_vector(it.hue, 1.0) };
            let cell = cmds
                .spawn((
                    sized(TILE, TILE),
                    art(displayed(it.graphic, it.amount as i32) as u32, hue, it.stacked),
                    Interaction::NONE,
                    // Whole-slot hit: press / hover / tooltip resolve anywhere in the cell.
                    UiContainsByBounds {},
                    ContainerItemUI {
                        container: view.window,
                        serial: it.serial,
                        original_hue: hue_vec(hue),
                        hover_hue: hue_vec(hover),
                    },
                    // The host's drop / selection pass matches cells by GlobalZIndex; it
                    // keeps tracking the window root on a z bump.
                    GlobalZIndex { value: win_z },
                    ChildOf::new(tile_bg),
                ))
                .id();
            if pinned {
                cmds.entity(cell).insert(NoPickup {});
            }
            view.cells.push(GridCell { slot, serial: it.serial, pinned, entity: cell, bg: tile_bg });
        }
    }
}

// ── PostUpdate (multi-move) ─────────────────────────────────────────────────────

#[system]
fn post(grabbed: Option<Res<GrabbedItem>>, time: Option<Res<Time>>, mut cmds: Commands) {
    let s = st();
    if !s.in_game {
        return;
    }
    tint(s, &mut cmds);
    let held = grabbed.map_or(0, |g| g.serial);
    let now = time.map_or(s.f.now, |t| t.total);
    drain(s, held, now);
}

/// Selected cells paint their slot tile gold; re-applied after every rebuild.
fn tint(s: &mut State, cmds: &mut Commands) {
    if s.sel.container != 0 && !s.views.contains_key(&s.sel.container) {
        s.sel.clear();
    }
    let mut want = HashMap::new();
    if let Some(view) = view_by_window(&s.views, s.sel.window) {
        for cell in &view.cells {
            if s.sel.serials.contains(&cell.serial) {
                want.insert((view.serial, cell.slot), cell.bg);
            }
        }
    }
    for (key, &e) in &want {
        if s.tinted.insert(*key) {
            cmds.entity(e).insert(bg(SELECTED));
        }
    }
    let stale: Vec<(u32, usize)> = s.tinted.iter().filter(|k| !want.contains_key(*k)).copied().collect();
    for key in stale {
        s.tinted.remove(&key);
        let e = s.views.get(&key.0).and_then(|v| v.cells.iter().find(|c| c.slot == key.1)).map(|c| c.bg);
        if let Some(e) = e {
            cmds.entity(e).insert(bg(PANEL));
        }
    }
}

/// One queued item at a time, as a pick-up + drop pair: the next goes when the server
/// answered the last (or it timed out). An item that already left the source
/// container is skipped.
fn drain(s: &mut State, held: u32, now: f32) {
    if s.queue.items.is_empty() {
        return;
    }
    if s.queue.wait_until != 0.0 && now < s.queue.wait_until {
        return;
    }
    // The player is holding something: their pickup owns the one-item slot.
    if held != 0 {
        return;
    }
    let from = resolve(s.queue.from_container);
    while let Some(serial) = s.queue.items.pop_front() {
        let ent = resolve(serial);
        // Gone, no longer contained, or not in the source container any more.
        if ent == 0 || from == 0 || s.parent_of.get(&ent) != Some(&from) {
            continue;
        }
        let amount = s.items.get(&serial).map_or(1, |it| it.amount.clamp(1, 0xFFFF));
        let q = &s.queue;
        let to = if q.to == 0 || q.to == 0xFFFF_FFFF {
            Destination::Ground(Point { x: q.x, y: q.y, z: q.z })
        } else if q.x == 0xFFFF && q.y == 0xFFFF {
            Destination::Container(q.to)
        } else {
            Destination::ContainerAt((q.to, q.x, q.y))
        };
        actions::move_item(serial, amount as u16, to);
        s.queue.wait_until = now + s.opts.multi_wait.max(1) as f32;
        return;
    }
    s.queue.wait_until = 0.0;
}
