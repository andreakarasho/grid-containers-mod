//! Pure grid logic — a straight port of the host's GridContainerGumpPlugin /
//! GridMultiMovePlugin helpers, kept free of host calls so `cargo test` covers it.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

pub const INSET: i32 = 8;
pub const TITLE_H: i32 = 14;
pub const ICON_SIZE: i32 = 28;
pub const ICON_TOP: i32 = 6;
pub const HEADER_H: i32 = 44;
pub const FOOTER_H: i32 = 16;
pub const HEADER_GAP: i32 = 4;
pub const CELL: i32 = 50;
pub const TILE: i32 = CELL - 4;
pub const GRIP: i32 = 14;
pub const SORT_W: i32 = 48;
pub const MIN_WIN: i32 = 160;
pub const MAX_WIN: i32 = 900;
pub const PIN_MENU_W: i32 = 74;
pub const PIN_MENU_ROW_H: i32 = 20;
pub const PIN_MENU_PAD: i32 = 3;
pub const PINNED_HUE: u16 = 0x038E;
pub const PINNED_ALPHA: f32 = 0.5;
pub const HOVER_HUE: u16 = 0x0035;
pub const CORPSE_GUMP: u16 = 0x0009;
/// ScrollbarGeometry.FlatWidth: the flat scrollbar's lane, reserved as right padding.
pub const FLAT_SCROLLBAR_W: i32 = 8;
/// UiZCounter.OverlayFloor — the lasso box sits in the pinned-overlay band.
pub const OVERLAY_FLOOR: i32 = 10000;
pub const CORPSE_MAX_DIST: i32 = 3;

pub fn cols(w: i32) -> i32 {
    ((w - INSET * 2 - FLAT_SCROLLBAR_W) / CELL).max(1)
}
pub fn content_h(h: i32) -> i32 {
    h - HEADER_H - FOOTER_H - INSET
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Sort {
    #[default]
    Free,
    Name,
    Layer,
    Graphic,
}

impl Sort {
    pub fn next(self) -> Sort {
        match self {
            Sort::Free => Sort::Name,
            Sort::Name => Sort::Layer,
            Sort::Layer => Sort::Graphic,
            Sort::Graphic => Sort::Free,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Sort::Name => "Name",
            Sort::Layer => "Layer",
            Sort::Graphic => "Type",
            Sort::Free => "Free",
        }
    }
    pub fn index(self) -> u64 {
        self as u64
    }
}

#[derive(Clone, Debug, Default)]
pub struct GridItem {
    pub serial: u32,
    pub graphic: u16,
    pub hue: u16,
    pub amount: u16,
    pub layer: u8,
    /// Stackable with amount > 1: drawn twice, offset, like legacy ItemGump.
    pub stacked: bool,
    /// None = the graphic is past the tiledata table and there is no OPL name.
    pub name: Option<String>,
}

/// .NET `StringComparison.OrdinalIgnoreCase`: per-char upper-case, then ordinal.
pub fn cmp_ignore_case(a: &str, b: &str) -> Ordering {
    let mut x = a.chars().map(upper);
    let mut y = b.chars().map(upper);
    loop {
        match (x.next(), y.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(p), Some(q)) => match p.cmp(&q) {
                Ordering::Equal => continue,
                o => return o,
            },
        }
    }
}

fn upper(c: char) -> char {
    let mut u = c.to_uppercase();
    match (u.next(), u.next()) {
        (Some(one), None) => one,
        _ => c,
    }
}

fn upper_str(s: &str) -> String {
    s.chars().map(upper).collect()
}

/// Search/sort name for a container item: the OPL (tooltip) name if loaded, else the
/// tiledata name; None only when the graphic is past the tiledata table.
pub fn resolve_item_search_name(opl_name: Option<&str>, tile_name: Option<&str>) -> Option<String> {
    match opl_name {
        Some(n) if !n.is_empty() => Some(n.to_string()),
        _ => tile_name.map(str::to_string),
    }
}

/// An empty query shows everything; otherwise a case-insensitive substring match.
pub fn matches_search(name: Option<&str>, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    match name {
        Some(n) if !n.is_empty() => upper_str(n).contains(&upper_str(query)),
        _ => false,
    }
}

fn round_up_rows(slots: i32, cols: i32) -> i32 {
    (slots + cols - 1) / cols * cols
}

/// Sorted modes: items in order, then empty tiles to fill the visible grid.
/// Free: every item with a remembered slot takes it (first come wins a collision), the
/// rest fill the first free tiles AND get remembered there — a tile is sticky from the
/// first time it is shown, so lifting an item out of the middle leaves a hole instead
/// of re-packing its neighbours. The grid grows to the last remembered slot, rounded
/// up to a full row. `layout[slot]` = index into `list`, -1 = empty tile.
pub fn lay_out(
    list: &mut Vec<GridItem>,
    sort: Sort,
    remembered: Option<&mut HashMap<u32, i32>>,
    cols: i32,
    visible_rows: i32,
    layout: &mut Vec<i32>,
) {
    layout.clear();
    let min_slots = cols * visible_rows;

    let remembered = match (sort, remembered) {
        (Sort::Free, Some(r)) => r,
        _ => {
            let name = |g: &GridItem| g.name.clone().unwrap_or_default();
            match sort {
                Sort::Name => list.sort_by(|a, b| cmp_ignore_case(&name(a), &name(b))),
                Sort::Layer => list.sort_by(|a, b| {
                    // Wearables by layer, everything else (layer 0) after them.
                    let la = if a.layer == 0 { 255 } else { a.layer as i32 };
                    let lb = if b.layer == 0 { 255 } else { b.layer as i32 };
                    la.cmp(&lb).then_with(|| cmp_ignore_case(&name(a), &name(b)))
                }),
                Sort::Graphic => list.sort_by(|a, b| a.graphic.cmp(&b.graphic)),
                Sort::Free => list.sort_by(|a, b| a.serial.cmp(&b.serial)),
            }
            let total = round_up_rows((list.len() as i32).max(min_slots), cols);
            for i in 0..total {
                layout.push(if i < list.len() as i32 { i } else { -1 });
            }
            return;
        }
    };

    list.sort_by(|a, b| a.serial.cmp(&b.serial));
    let mut max_slot = -1;
    for it in list.iter() {
        if let Some(&s) = remembered.get(&it.serial) {
            max_slot = max_slot.max(s);
        }
    }
    let size = round_up_rows((max_slot + 1).max(list.len() as i32).max(min_slots), cols);
    layout.resize(size as usize, -1);

    for (i, it) in list.iter().enumerate() {
        if let Some(&s) = remembered.get(&it.serial) {
            if layout[s as usize] < 0 {
                layout[s as usize] = i as i32;
            }
        }
    }
    let mut next = 0usize;
    for (i, it) in list.iter().enumerate() {
        if let Some(&s) = remembered.get(&it.serial) {
            if layout[s as usize] == i as i32 {
                continue;
            }
        }
        while next < layout.len() && layout[next] >= 0 {
            next += 1;
        }
        if next >= layout.len() {
            for _ in 0..cols {
                layout.push(-1);
            }
        }
        layout[next] = i as i32;
        remembered.insert(it.serial, next as i32);
    }
}

/// Rebuild signature: cols / sort / slot count / query / serials / names / layout.
/// Hue, amount and graphic changes rebuild through the slot-event invalidation.
pub fn sig(list: &[GridItem], layout: &[i32], cols: i32, query: &str, sort: Sort) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    cols.hash(&mut h);
    sort.index().hash(&mut h);
    layout.len().hash(&mut h);
    query.hash(&mut h);
    for it in list {
        it.serial.hash(&mut h);
        it.name.hash(&mut h);
    }
    layout.hash(&mut h);
    h.finish()
}

/// "… 23/400 stones" -> "23/400 stones". Scans the '\n'-joined OPL body.
pub fn parse_stones(opl_data: Option<&str>) -> String {
    let Some(data) = opl_data else { return String::new() };
    if data.is_empty() {
        return String::new();
    }
    let chars: Vec<char> = data.chars().collect();
    let up: Vec<char> = chars.iter().map(|&c| upper(c)).collect();
    let needle: Vec<char> = "STONES".chars().collect();
    let Some(at) = up.windows(needle.len()).position(|w| w == needle.as_slice()) else {
        return String::new();
    };
    let mut end = at;
    while end > 0 && chars[end - 1] == ' ' {
        end -= 1;
    }
    let mut start = end;
    while start > 0 && (chars[start - 1].is_numeric() || chars[start - 1] == '/') {
        start -= 1;
    }
    let span: String = chars[start..end].iter().collect();
    match span.find('/') {
        Some(i) if i > 0 => format!("{span} stones"),
        _ => String::new(),
    }
}

/// ShaderHueTranslator.GetHueVector(hue, partial: false, alpha) for an un-gumped,
/// non-effect draw.
pub fn hue_vector(hue: u16, alpha: f32) -> (f32, f32, f32) {
    let mut hue = hue as i32;
    let mut partial = false;
    if hue & 0x8000 != 0 {
        partial = true;
        hue &= 0x7FFF;
    }
    if hue == 0 {
        partial = false;
    }
    let ty = if hue & 0x4000 != 0 {
        7 // SHADER_SPECTRAL
    } else if hue != 0 {
        hue -= 1;
        if partial {
            2
        } else {
            1
        }
    } else {
        0
    };
    (hue as f32, ty as f32, alpha)
}

/// ItemGraphics.IsCoin.
pub fn is_coin(graphic: u16) -> bool {
    graphic == 0x0EEA || graphic == 0x0EED || graphic == 0x0EF0
}

/// ItemGraphics.Displayed: coins show the small / large pile art.
pub fn displayed(graphic: u16, amount: i32) -> u16 {
    if is_coin(graphic) {
        if amount > 5 {
            return graphic + 2;
        }
        if amount > 1 {
            return graphic + 1;
        }
    }
    graphic
}

/// ItemGraphics.DrawStacked.
pub fn draw_stacked(graphic: u16, amount: i32, stackable: bool) -> bool {
    stackable && amount > 1 && !is_coin(graphic)
}

pub type Rgba = (u8, u8, u8, u8);

pub const BG: Rgba = (9, 12, 17, 250);
pub const PANEL: Rgba = (16, 21, 29, 255);
pub const PINNED_SLOT: Rgba = (20, 64, 60, 255);
pub const FIELD: Rgba = (34, 43, 56, 255);
pub const CONTROL: Rgba = (28, 36, 47, 255);
pub const FRAME: Rgba = (39, 45, 54, 255);
pub const ACCENT: Rgba = (47, 191, 174, 255);
pub const TEXT: Rgba = (232, 237, 242, 255);
pub const TEXT_MUTED: Rgba = (152, 162, 173, 255);
pub const SELECTED: Rgba = (47, 191, 174, 110);
pub const BOX_FILL: Rgba = (47, 191, 174, 40);

/// The panel background for a container hue: the hue's brightest ramp cell
/// (`tint` = GetPolygoneColor(31, hue & 0x3FFF), packed ABGR) scaled onto the dark
/// panel so it reads as a tint.
pub fn panel_color(hue: u16, tint: u32) -> Rgba {
    if hue & 0x3FFF == 0 || tint == 0 {
        return BG;
    }
    let shade = |c: u32| (14.0f32 + c as f32 * 0.22f32) as u8;
    (shade(tint & 0xFF), shade((tint >> 8) & 0xFF), shade((tint >> 16) & 0xFF), BG.3)
}

/// Constants.BAD_CONTAINER_LAYERS: true = a corpse shows items on that layer.
const BAD_CONTAINER_LAYERS: [bool; 30] = [
    false, true, true, true, true, true, true, true, true, true, true, false, true, true, true, false,
    false, true, true, true, true, false, true, true, true, false, false, false, false, false,
];
const TILE_WEARABLE: u64 = 0x0040_0000;
const LAYER_HAIR: u8 = 0x0B;
const LAYER_FACE: u8 = 0x0F;
const LAYER_BEARD: u8 = 0x10;

/// ContainerGumpPlugin.ShouldSkipSlot: un-lootable corpse layers, and hair / beard /
/// face anywhere, never get a slot.
pub fn should_skip_slot(in_table: bool, layer: u8, flags: u64, is_corpse: bool) -> bool {
    if !in_table {
        return false;
    }
    if is_corpse && layer > 0 && (layer as usize) < BAD_CONTAINER_LAYERS.len() && !BAD_CONTAINER_LAYERS[layer as usize] {
        return true;
    }
    flags & TILE_WEARABLE != 0 && matches!(layer, LAYER_FACE | LAYER_BEARD | LAYER_HAIR)
}

pub const KEY_ESCAPE: i32 = 27;
pub const KEY_LSHIFT: i32 = 160;
pub const KEY_RSHIFT: i32 = 161;
pub const KEY_LCTRL: i32 = 162;
pub const KEY_RCTRL: i32 = 163;
pub const KEY_LALT: i32 = 164;
pub const KEY_RALT: i32 = 165;

/// One frame of keyboard: held keys + this frame's press edges.
#[derive(Default)]
pub struct Keys {
    pub held: Vec<i32>,
    pub once: Vec<i32>,
}

impl Keys {
    pub fn held(&self, k: i32) -> bool {
        self.held.contains(&k)
    }
    pub fn once(&self, k: i32) -> bool {
        self.once.contains(&k)
    }
    fn either(&self, a: i32, b: i32) -> bool {
        self.held(a) || self.held(b) || self.once(a) || self.once(b)
    }
    pub fn ctrl(&self) -> bool {
        self.either(KEY_LCTRL, KEY_RCTRL)
    }
    pub fn shift(&self) -> bool {
        self.either(KEY_LSHIFT, KEY_RSHIFT)
    }
    pub fn alt(&self) -> bool {
        self.either(KEY_LALT, KEY_RALT)
    }

    /// HotkeyPlugin.KeyHeld for one key binding: a modifier key counts either side.
    pub fn binding_held(&self, key: i32, ctrl: bool, shift: bool, alt: bool) -> bool {
        if key == 0 {
            return false;
        }
        let main = match key {
            KEY_LCTRL | KEY_RCTRL => self.ctrl(),
            KEY_LSHIFT | KEY_RSHIFT => self.shift(),
            KEY_LALT | KEY_RALT => self.alt(),
            k => self.held(k) || self.once(k),
        };
        main && (!ctrl || self.ctrl()) && (!shift || self.shift()) && (!alt || self.alt())
    }
}

/// What the mod keeps per character (character-scoped mod storage).
#[derive(Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct CharData {
    pub pins: Vec<u32>,
    /// container serial -> item serial -> remembered slot.
    pub slots: HashMap<u32, HashMap<u32, i32>>,
}

/// The rest of a multi-selection follows the item the player moved.
#[derive(Default, Debug)]
pub struct MoveQueue {
    pub items: std::collections::VecDeque<u32>,
    pub from_container: u32,
    pub x: u16,
    pub y: u16,
    pub z: i8,
    pub to: u32,
    /// Waiting for the server's answer to the last move until this time (ms); 0 = idle.
    pub wait_until: f32,
}

#[derive(Default, Debug)]
pub struct Selection {
    pub window: u64,
    pub container: u32,
    pub serials: HashSet<u32>,
    pub dragging: bool,
    pub start: (f32, f32),
    pub boxed: bool,
}

impl Selection {
    pub fn clear(&mut self) {
        self.serials.clear();
        self.window = 0;
        self.container = 0;
    }
}

#[allow(clippy::too_many_arguments)]
pub fn enqueue(q: &mut MoveQueue, s: &mut Selection, moved: u32, x: u16, y: u16, z: i8, to: u32, now: f32, delay_ms: i32) {
    q.items.clear();
    let mut rest: Vec<u32> = s.serials.iter().copied().filter(|&v| v != moved).collect();
    rest.sort_unstable();
    q.items.extend(rest);
    q.from_container = s.container;
    q.x = x;
    q.y = y;
    q.z = z;
    q.to = to;
    // The player's own move is in flight: wait for its answer (or the timeout).
    q.wait_until = now + delay_ms.max(0) as f32;
    s.clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items(v: &[(u32, u8, &str)]) -> Vec<GridItem> {
        v.iter()
            .map(|&(serial, layer, name)| GridItem {
                serial,
                layer,
                name: Some(name.into()),
                graphic: serial as u16,
                amount: 1,
                ..Default::default()
            })
            .collect()
    }

    #[test]
    fn free_pins_items_to_their_slots_and_fills_the_rest_in_order() {
        let mut list = items(&[(1, 0, "a"), (2, 0, "b"), (3, 0, "c")]);
        let mut pinned = HashMap::from([(3, 0), (1, 5)]);
        let mut layout = vec![];
        lay_out(&mut list, Sort::Free, Some(&mut pinned), 3, 1, &mut layout);
        assert_eq!(layout.len(), 6);
        assert_eq!(list[layout[0] as usize].serial, 3);
        assert_eq!(list[layout[1] as usize].serial, 2);
        assert_eq!(list[layout[5] as usize].serial, 1);
        assert_eq!(layout[2], -1);
    }

    #[test]
    fn free_collision_first_serial_keeps_the_slot_other_falls_through() {
        let mut list = items(&[(7, 0, "a"), (8, 0, "b")]);
        let mut pinned = HashMap::from([(7, 1), (8, 1)]);
        let mut layout = vec![];
        lay_out(&mut list, Sort::Free, Some(&mut pinned), 2, 1, &mut layout);
        assert_eq!(list[layout[1] as usize].serial, 7);
        assert_eq!(list[layout[0] as usize].serial, 8);
    }

    #[test]
    fn free_pins_unpinned_items_where_they_land_so_lifting_one_leaves_a_hole() {
        let mut list = items(&[(1, 0, "a"), (2, 0, "b"), (3, 0, "c")]);
        let mut pinned = HashMap::new();
        let mut layout = vec![];
        lay_out(&mut list, Sort::Free, Some(&mut pinned), 3, 1, &mut layout);
        assert_eq!(pinned, HashMap::from([(1, 0), (2, 1), (3, 2)]));
        list.remove(1);
        lay_out(&mut list, Sort::Free, Some(&mut pinned), 3, 1, &mut layout);
        assert_eq!(list[layout[0] as usize].serial, 1);
        assert_eq!(layout[1], -1);
        assert_eq!(list[layout[2] as usize].serial, 3);
    }

    #[test]
    fn free_grows_to_the_last_pinned_slot_in_whole_rows() {
        let mut list = items(&[(1, 0, "a")]);
        let mut pinned = HashMap::from([(1, 7)]);
        let mut layout = vec![];
        lay_out(&mut list, Sort::Free, Some(&mut pinned), 3, 1, &mut layout);
        assert_eq!(layout.len(), 9);
        assert_eq!(layout[7], 0);
    }

    #[test]
    fn layer_sorts_wearables_by_layer_then_name_and_puts_layer_zero_last() {
        let mut list = items(&[(1, 0, "gold"), (2, 5, "shirt"), (3, 1, "sword"), (4, 5, "cloak")]);
        let mut layout = vec![];
        lay_out(&mut list, Sort::Layer, None, 4, 1, &mut layout);
        let names: Vec<_> = layout.iter().map(|&i| list[i as usize].name.clone().unwrap()).collect();
        assert_eq!(names, ["sword", "cloak", "shirt", "gold"]);
    }

    #[test]
    fn sorted_modes_pad_to_the_visible_grid() {
        let mut list = items(&[(1, 0, "a")]);
        let mut layout = vec![];
        lay_out(&mut list, Sort::Name, None, 3, 2, &mut layout);
        assert_eq!(layout.len(), 6);
        assert_eq!(layout[0], 0);
        assert!(layout[1..].iter().all(|&i| i == -1));
    }

    #[test]
    fn parse_stones_reads_the_contents_line() {
        assert_eq!(parse_stones(Some("Contents: 3/125 Items, 23/400 Stones")), "23/400 stones");
        assert_eq!(parse_stones(Some("a bag\nContents: 0/125 items, 0/400 stones")), "0/400 stones");
        assert_eq!(parse_stones(Some("a bag\nInsured")), "");
        assert_eq!(parse_stones(None), "");
    }

    #[test]
    fn search_name_prefers_opl_and_falls_back_to_tiledata() {
        assert_eq!(resolve_item_search_name(None, Some("katana")).as_deref(), Some("katana"));
        assert_eq!(
            resolve_item_search_name(Some("a vanquishing katana"), Some("katana")).as_deref(),
            Some("a vanquishing katana")
        );
        assert_eq!(resolve_item_search_name(Some(""), Some("katana")).as_deref(), Some("katana"));
        assert_eq!(resolve_item_search_name(None, None), None);
    }

    #[test]
    fn matches_search_is_a_case_insensitive_substring() {
        assert!(matches_search(Some("katana"), ""));
        assert!(matches_search(None, ""));
        assert!(matches_search(Some("a vanquishing katana"), "KATANA"));
        assert!(matches_search(Some("katana"), "kat"));
        assert!(!matches_search(Some("katana"), "shield"));
        assert!(!matches_search(None, "kat"));
    }

    #[test]
    fn layout_formulas_at_the_default_size() {
        assert_eq!(cols(320), 5);
        assert_eq!(content_h(300), 232);
        assert_eq!(cols(10), 1);
    }

    #[test]
    fn hue_vector_matches_the_shader_translator() {
        assert_eq!(hue_vector(0, 1.0), (0.0, 0.0, 1.0));
        assert_eq!(hue_vector(0x0035, 1.0), (52.0, 1.0, 1.0));
        assert_eq!(hue_vector(0x8035, 1.0), (52.0, 2.0, 1.0));
        assert_eq!(hue_vector(0x4000, 0.5), (16384.0, 7.0, 0.5));
        assert_eq!(hue_vector(PINNED_HUE, PINNED_ALPHA), (0x038D as f32, 1.0, 0.5));
    }

    #[test]
    fn coins_use_pile_art_and_never_double_draw() {
        assert_eq!(displayed(0x0EED, 1), 0x0EED);
        assert_eq!(displayed(0x0EED, 3), 0x0EEE);
        assert_eq!(displayed(0x0EED, 6), 0x0EEF);
        assert_eq!(displayed(0x1F4C, 6), 0x1F4C);
        assert!(!draw_stacked(0x0EED, 6, true));
        assert!(draw_stacked(0x0F7A, 2, true));
        assert!(!draw_stacked(0x0F7A, 1, true));
    }

    #[test]
    fn panel_color_shades_the_ramp_or_keeps_the_plain_panel() {
        assert_eq!(panel_color(0, 0xFF_FF_FF_FF), BG);
        assert_eq!(panel_color(0x21, 0), BG);
        // R=100, G=200, B=0 packed ABGR.
        assert_eq!(panel_color(0x21, 0x00_00_C8_64), (36, 58, 14, 250));
    }

    #[test]
    fn moving_one_selected_item_queues_the_others_for_the_same_destination() {
        let mut s = Selection { container: 0x4000_0010, ..Default::default() };
        s.serials.extend([1, 2, 3]);
        let mut q = MoveQueue::default();
        enqueue(&mut q, &mut s, 2, 0xFFFF, 0xFFFF, 0, 0x4000_0020, 1000.0, 600);
        assert_eq!(q.items.iter().copied().collect::<Vec<_>>(), vec![1, 3]);
        assert_eq!((q.from_container, q.to), (0x4000_0010, 0x4000_0020));
        assert_eq!(q.wait_until, 1600.0);
        assert!(s.serials.is_empty());
    }

    #[test]
    fn skip_slot_filters_corpse_layers_and_hair() {
        let wear = TILE_WEARABLE;
        assert!(!should_skip_slot(true, 0x03, wear, true)); // shoes on a corpse
        assert!(should_skip_slot(true, 0x15, wear, true)); // the corpse's own backpack
        assert!(!should_skip_slot(true, 0x15, wear, false)); // a backpack in a bag
        assert!(should_skip_slot(true, 0x0B, wear, true)); // hair
        assert!(should_skip_slot(true, 0x0B, wear, false));
        assert!(!should_skip_slot(true, 0x00, 0, true)); // loose loot
        assert!(!should_skip_slot(false, 0x0B, wear, true)); // past the table
    }

    #[test]
    fn binding_held_matches_either_side_modifiers() {
        let k = Keys { held: vec![KEY_RCTRL], once: vec![] };
        assert!(k.binding_held(KEY_LCTRL, false, false, false));
        assert!(!k.binding_held(KEY_LCTRL, false, true, false));
        let k = Keys { held: vec![], once: vec![65, KEY_LSHIFT] };
        assert!(k.binding_held(65, false, true, false));
        assert!(!k.binding_held(0, false, false, false));
    }

    #[test]
    fn a_negative_delay_is_treated_as_zero() {
        let mut s = Selection::default();
        s.serials.extend([1, 2]);
        let mut q = MoveQueue::default();
        enqueue(&mut q, &mut s, 1, 10, 20, 5, 0x4000_0020, 50.0, -5);
        assert_eq!(q.wait_until, 50.0);
        assert_eq!((q.x, q.y, q.z), (10, 20, 5));
    }
}
