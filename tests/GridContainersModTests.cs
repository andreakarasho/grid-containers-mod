using System;
using System.IO;
using ClassicUO.Configuration;
using ClassicUO.Ecs.Modding;
using TinyEcs;
using TinyEcs.Bevy;
using TinyEcs.Bevy.Modding;
using TinyEcs.Bevy.UI;
using Xunit;

namespace ClassicUO.Ecs.Tests;

// The grid containers mod (ClassicUO/grid-containers-mod, built into
// ecs-mods/grid-containers by its `make build`) driven through the real modding
// plugin: a container open builds the grid window out of the host's container
// markers, the rebuild lays out the slot rows, and a close tears it down.
[Collection("clay-layout")]
public class GridContainersModTests
{
    private const string Mod = "grid-containers";
    private const uint Bag = 0x4000_0001;
    private const uint Dagger = 0x4000_0002;

    private static App BuildApp()
    {
        var srcWasm = Path.Combine(RepoRoot(), "ecs-mods", Mod, "mod.wasm");
        Assert.True(File.Exists(srcWasm),
            $"mod not built: {srcWasm} — run `make build` in the grid-containers-mod repo");
        var root = Path.Combine(Path.GetTempPath(), "cuo-ecsmod-grid");
        var modDir = Path.Combine(root, "iso-mods", Mod);
        if (Directory.Exists(modDir))
            Directory.Delete(modDir, recursive: true);
        Directory.CreateDirectory(modDir);
        File.Copy(srcWasm, Path.Combine(modDir, "mod.wasm"), overwrite: true);
        File.WriteAllText(Path.Combine(modDir, "mod.json"),
            $"{{\"name\":\"{Mod}\",\"version\":\"0.0.0\",\"wasm\":\"mod.wasm\",\"ruleset\":{{}}}}");
        Directory.SetCurrentDirectory(root);

        var app = new App(ThreadingMode.Single);
        app.AddState(GameState.GameScreen);
        // The mod's own option (Options > Mods > Grid containers) switched on.
        var profile = new Profile();
        profile.ModOptions[Mod] = new() { ["containers"] = 1 };
        app.AddResource(profile);
        app.AddResource(new NetworkEntitiesMap());
        app.AddPlugin(new UiZOrderPlugin());
        app.AddResource(new ModdingConfig { ModFolder = "iso-mods" });
        app.AddPlugin<Modding.ModdingPlugin>();
        return app;
    }

    private static string RepoRoot()
    {
        var dir = new DirectoryInfo(AppContext.BaseDirectory);
        while (dir != null && !Directory.Exists(Path.Combine(dir.FullName, ".git"))
                           && !File.Exists(Path.Combine(dir.FullName, ".git")))
            dir = dir.Parent;
        Assert.True(dir != null, "repo root (.git) not found");
        return dir!.FullName;
    }

    private static ulong GridWindow(World world, uint serial)
    {
        foreach (var (e, g) in world.Query<Data<GridContainerWindow>>())
            if (g.Ref.Serial == serial)
                return e.Ref;
        return 0;
    }

    [Fact]
    public void Open_builds_the_grid_window_rebuild_lays_out_slots_and_close_tears_it_down()
    {
        var app = BuildApp();
        var send = 0; // 1 = open, 2 = close
        app.AddSystem((Commands c, ResMut<NetworkEntitiesMap> map, Local<bool> made) =>
        {
            if (made.Value) return;
            made.Value = true;
            var bag = map.Value.GetOrCreate(c, Bag);
            // One item inside the bag, as the container packets leave it.
            var dagger = map.Value.GetOrCreate(c, Dagger)
                .Insert(new Graphic { Value = 0x0F52 })
                .Insert(new Hue { Value = 0 })
                .Insert<ContainedInto>();
            bag.AddChild(dagger);
        }).InStage(Stage.Update).Build();
        app.AddSystem((EventWriter<ContainerOpenedEvent> open, EventWriter<ContainerClosedEvent> close) =>
        {
            if (send == 1) open.Send(new ContainerOpenedEvent(Bag, 0x003C));
            if (send == 2) close.Send(new ContainerClosedEvent(Bag));
            send = 0;
        }).InStage(Stage.Update).Build();

        app.RunStartup();
        app.Update();
        send = 1;
        for (var i = 0; i < 4; i++)
            app.Update();

        var world = app.GetWorld();
        var root = GridWindow(world, Bag);
        Assert.True(root != 0, "no grid window after the open event");

        // The host container markers the shared pickup / drop / close paths key on.
        Assert.Equal(Bag, world.Get<ContainerWindow>(root).Serial);
        var tag = world.Get<ContainerGumpTag>(root);
        Assert.Equal((ushort)0x003C, tag.OriginalGraphic);
        Assert.Equal(new ClassicUO.Primitives.Rectangle(8, 44, 304, 232), tag.Bounds);
        Assert.True(world.Has<UiMovable>(root));
        Assert.True(world.Get<GlobalZIndex>(root).Value > 0, "negative z request not resolved to the counter's top");
        var rs = world.Get<UiResizable>(root);
        Assert.Equal((160f, 900f, 14f, 2f, true), (rs.MinW, rs.MaxW, rs.Grip, rs.GripInset, rs.WholePixels));
        var node = world.Get<Node>(root);
        Assert.Equal((120f, 120f, 320f, 300f), (node.Left.Value, node.Top.Value, node.Width.Value, node.Height.Value));

        // The claim the classic window stands down for — ordinary containers only (the
        // host's GridLootType is 0, so corpses keep the classic window).
        var claims = new System.Collections.Generic.List<string>();
        foreach (var (_, c) in world.Query<Data<ModSupersedes>>())
            claims.Add(c.Ref.Feature);
        Assert.Equal(new[] { GridContainers.ContainerFeature }, claims);

        // The options the mod declared, for the Options window's Mods page.
        var schema = app.GetResource<ModOptionsRegistry>().Schemas[Mod];
        Assert.Equal(new[] { "containers", "quick_move", "multi_move", "multi_move_wait" },
            System.Array.ConvertAll(schema, o => o.Key));

        // The content column (Overflow.Scroll + flat scrollbar), somewhere under the
        // window's flex tree: an empty bag at 320x300 is 5 columns x 4 visible rows.
        bool Under(ulong e, ulong ancestor)
        {
            for (var i = 0; i < 8 && world.Has<TinyEcs.Parent>(e); i++)
                if ((e = (ulong)world.Get<TinyEcs.Parent>(e).Id) == ancestor)
                    return true;
            return false;
        }
        ulong content = 0;
        foreach (var (e, _) in world.Query<Data<UiScrollbar>>())
            if (Under(e.Ref, root))
                content = e.Ref;
        Assert.True(content != 0, "no content column under the window");
        Assert.True(world.Has<Children>(content), "rebuild laid out no rows");
        var rows = 0;
        foreach (var _ in world.Get<Children>(content))
            rows++;
        Assert.Equal(4, rows);

        // The item reached the grid through the scoped item feed: one cell carries it,
        // linked to this window for the host's pickup / drop / tooltip paths.
        var cells = new System.Collections.Generic.List<ContainerItemUI>();
        foreach (var (_, link) in world.Query<Data<ContainerItemUI>>())
            cells.Add(link.Ref);
        Assert.Single(cells);
        Assert.Equal((Dagger, root), (cells[0].Serial, cells[0].Container));

        send = 2;
        for (var i = 0; i < 3; i++)
            app.Update();
        Assert.Equal(0ul, GridWindow(world, Bag));
    }

    // A server delete (0x1D despawns the item's entity) while the grid is open drops
    // that item's cell; the rest stay.
    [Fact]
    public void Deleted_item_leaves_the_open_grid()
    {
        var app = BuildApp();
        var send = false;
        var items = new ulong[3];
        app.AddSystem((Commands c, ResMut<NetworkEntitiesMap> map, Local<bool> made) =>
        {
            if (made.Value) return;
            made.Value = true;
            var bag = map.Value.GetOrCreate(c, Bag);
            for (var i = 0; i < items.Length; i++)
            {
                var item = map.Value.GetOrCreate(c, Dagger + (uint)i)
                    .Insert(new Graphic { Value = 0x0F52 })
                    .Insert(new Hue { Value = 0 })
                    .Insert(new ContainerSlotPosition { X = (ushort)(20 * i), Y = 20 })
                    .Insert<ContainedInto>();
                bag.AddChild(item);
                items[i] = item.Id;
            }
        }).InStage(Stage.Update).Build();
        app.AddSystem((EventWriter<ContainerOpenedEvent> open) =>
        {
            if (send) open.Send(new ContainerOpenedEvent(Bag, 0x003C));
            send = false;
        }).InStage(Stage.Update).Build();

        app.RunStartup();
        app.Update();
        send = true;
        for (var i = 0; i < 4; i++)
            app.Update();

        var world = app.GetWorld();
        uint[] Cells()
        {
            var serials = new System.Collections.Generic.List<uint>();
            foreach (var (_, link) in world.Query<Data<ContainerItemUI>>())
                serials.Add(link.Ref.Serial);
            serials.Sort();
            return serials.ToArray();
        }
        Assert.Equal(new[] { Dagger, Dagger + 1, Dagger + 2 }, Cells());

        var deleted = items[1];
        var despawn = true;
        app.AddSystem((Commands c) =>
        {
            if (!despawn) return;
            despawn = false;
            c.Entity(deleted).Despawn();
        }).InStage(Stage.Update).Build();
        for (var i = 0; i < 3; i++)
            app.Update();

        Assert.Equal(new[] { Dagger, Dagger + 2 }, Cells());
    }
}
