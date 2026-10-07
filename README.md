# grid-containers

Grid container windows for the ClassicUO ECS client, as a Rust core-wasm mod. A 1:1
port of the client's former host `GridContainerGumpPlugin` + `GridMultiMovePlugin`:

- resizable dark panel per container, slots in a scrollable grid, container icon,
  name and weight header
- search (by OPL / tiledata name) and sort (Free / Name / Layer / Type)
- free placement remembered per container
- pin items in the backpack (right-click a cell)
- quick move: Ctrl+click (or the QuickMoveItem mouse binding) sends an item to the
  last clicked grid window
- multi-move: Shift+drag on empty space selects, moving one moves them all
- corpse grid (`GridLootType`), with SkipEmptyCorpse and the out-of-range close

Settings are on the client's Options window, **Mods > Grid containers** (declared via
`cuo:options/schema`, stored per character in the profile). The corpse grid follows the
client's own *Containers > Grid loot* option. Pins and the free-slot layout live in
character-scoped mod storage (`<character profile>/mods/grid-containers.json`).

While grid containers are on, the mod claims `cuo:gump/container` (and `cuo:gump/corpse`
for "grid only" loot) and the client's classic window stands down; without the mod the
client opens classic container windows. The mod uses only the client's generic mod
surface — no host code knows about it.

## Build

```
make build            # -> ../cuo-agents/ecs-mods/grid-containers/{mod.wasm,mod.json}
make test             # logic unit tests (native)
```

`CUO_REPO=<path>` points at another client checkout. The crate builds against that
checkout's `external/classicuo-mods-sdk`, so its generated paths / types match the
host registry the mod will load into.
