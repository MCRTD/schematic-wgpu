//! Block light: what torches, lanterns, glowstone and other light sources
//! give off, spread through the build the way Minecraft does it.

use std::collections::{HashMap, VecDeque};

use litematic::{BlockState, Region};

/// Light level of each cell that has any, by region-local position. Cells
/// outside the region count as air, so light reaches faces on its edge.
pub(crate) type LightMap = HashMap<[i32; 3], u8>;

/// The block light level (0..=15) a block in this state gives off, as in
/// vanilla Minecraft.
pub(crate) fn emission(state: &BlockState) -> u8 {
    let name = state.short_name();
    let prop = |k: &str| state.properties.get(k).map(String::as_str);
    let lit = prop("lit") != Some("false");
    let count = |k: &str| prop(k).and_then(|v| v.parse::<u8>().ok()).unwrap_or(1);
    match name.strip_prefix("waxed_").unwrap_or(name) {
        "glowstone" | "sea_lantern" | "shroomlight" | "lantern" | "jack_o_lantern" | "beacon" | "conduit" | "lava"
        | "fire" | "end_gateway" | "end_portal" | "ochre_froglight" | "verdant_froglight" | "pearlescent_froglight"
        | "lava_cauldron" => 15,
        "campfire" | "redstone_lamp" | "copper_bulb" if lit => 15,
        "exposed_copper_bulb" if lit => 12,
        "weathered_copper_bulb" if lit => 8,
        "oxidized_copper_bulb" if lit => 4,
        "respawn_anchor" => [0, 3, 7, 11, 15][count("charges").min(4) as usize],
        "light" => prop("level").and_then(|v| v.parse().ok()).unwrap_or(15),
        "torch" | "wall_torch" | "end_rod" => 14,
        "cave_vines" | "cave_vines_plant" if prop("berries") == Some("true") => 14,
        "furnace" | "blast_furnace" | "smoker" if lit && prop("lit").is_some() => 13,
        "nether_portal" => 11,
        "soul_torch" | "soul_wall_torch" | "soul_lantern" | "soul_fire" | "crying_obsidian" => 10,
        "soul_campfire" if lit => 10,
        "redstone_ore" | "deepslate_redstone_ore" if lit && prop("lit").is_some() => 9,
        "enchanting_table" | "ender_chest" | "glow_lichen" => 7,
        "redstone_torch" | "redstone_wall_torch" if lit => 7,
        "sculk_catalyst" => 6,
        "amethyst_cluster" => 5,
        "large_amethyst_bud" => 4,
        "magma_block" => 3,
        "sea_pickle" if prop("waterlogged") == Some("true") => 3 * (count("pickles").min(4) + 1),
        "candle_cake" if prop("lit") == Some("true") => 3,
        n if n.ends_with("candle_cake") && prop("lit") == Some("true") => 3,
        n if n.ends_with("candle") && prop("lit") == Some("true") => 3 * count("candles").min(4),
        "medium_amethyst_bud" | "firefly_bush" => 2,
        "small_amethyst_bud" | "brewing_stand" | "brown_mushroom" | "dragon_egg" | "end_portal_frame"
        | "sculk_sensor" | "calibrated_sculk_sensor" => 1,
        _ => 0,
    }
}

/// Spread light from every block that emits some (`emission`, by block
/// index) through cells that don't block it (`blocks_light`, by local
/// position), losing a level per step. Full opaque blocks stop light but
/// still give off their own.
pub(crate) fn spread(region: &Region, emission: impl Fn(usize) -> u8, blocks_light: impl Fn([i32; 3]) -> bool) -> LightMap {
    let mut levels = LightMap::new();
    let mut queue = VecDeque::new();
    for i in 0..region.blocks.len() {
        let level = emission(i);
        if level > 0 {
            let p = region.position(i);
            levels.insert([p.x, p.y, p.z], level);
            queue.push_back(([p.x, p.y, p.z], level));
        }
    }
    while let Some((cell, level)) = queue.pop_front() {
        // Superseded by a brighter path.
        if level <= 1 || levels.get(&cell) != Some(&level) {
            continue;
        }
        for d in [[1, 0, 0], [-1, 0, 0], [0, 1, 0], [0, -1, 0], [0, 0, 1], [0, 0, -1]] {
            let next = [cell[0] + d[0], cell[1] + d[1], cell[2] + d[2]];
            if blocks_light(next) || levels.get(&next).is_some_and(|&l| l >= level - 1) {
                continue;
            }
            levels.insert(next, level - 1);
            queue.push_back((next, level - 1));
        }
    }
    levels
}

#[cfg(test)]
mod tests {
    use super::*;
    use litematic::Vec3i;
    use std::collections::BTreeMap;

    fn state(name: &str, props: &[(&str, &str)]) -> BlockState {
        BlockState {
            name: format!("minecraft:{name}"),
            properties: props.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect::<BTreeMap<_, _>>(),
        }
    }

    #[test]
    fn emission_follows_vanilla_levels_and_states() {
        assert_eq!(emission(&state("glowstone", &[])), 15);
        assert_eq!(emission(&state("wall_torch", &[("facing", "north")])), 14);
        assert_eq!(emission(&state("furnace", &[("lit", "true")])), 13);
        assert_eq!(emission(&state("furnace", &[("lit", "false")])), 0);
        assert_eq!(emission(&state("redstone_lamp", &[("lit", "false")])), 0);
        assert_eq!(emission(&state("blue_candle", &[("candles", "3"), ("lit", "true")])), 9);
        assert_eq!(emission(&state("blue_candle", &[("candles", "3"), ("lit", "false")])), 0);
        assert_eq!(emission(&state("waxed_weathered_copper_bulb", &[("lit", "true")])), 8);
        assert_eq!(emission(&state("stone", &[])), 0);
    }

    #[test]
    fn light_fades_by_one_per_block_and_stops_at_walls() {
        // A row: torch, air, air, wall, air.
        let size = Vec3i { x: 5, y: 1, z: 1 };
        let region = Region {
            name: "r".into(),
            origin: Vec3i { x: 0, y: 0, z: 0 },
            size,
            palette: vec![],
            blocks: vec![0; 5],
            block_entities: vec![],
        };
        let levels = spread(&region, |i| if i == 0 { 14 } else { 0 }, |c| c == [3, 0, 0]);
        let at = |x| levels.get(&[x, 0, 0]).copied().unwrap_or(0);
        assert_eq!([at(0), at(1), at(2), at(3)], [14, 13, 12, 0]);
        // Behind the wall, light only arrives the long way round, outside the region.
        assert_eq!(at(4), 12 - 4);
        // It also spills outside the region.
        assert_eq!(levels.get(&[0, 1, 0]), Some(&13));
    }
}
