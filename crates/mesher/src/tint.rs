//! Colors for faces with a `tintindex`. Minecraft computes these from the
//! biome or block state at runtime; we use plains biome defaults.

use mc_assets::Properties;

const GRASS: [u8; 3] = [0x91, 0xbd, 0x59];
const FOLIAGE: [u8; 3] = [0x77, 0xab, 0x2f];
const WATER: [u8; 3] = [0x3f, 0x76, 0xe4];

pub fn color(block: &str, props: &Properties) -> [u8; 3] {
    match block {
        "birch_leaves" => [0x80, 0xa7, 0x55],
        "spruce_leaves" => [0x61, 0x99, 0x61],
        "lily_pad" => [0x20, 0x80, 0x30],
        "water" | "water_cauldron" | "bubble_column" => WATER,
        "redstone_wire" => redstone(props.get("power").and_then(|p| p.parse().ok()).unwrap_or(0)),
        "attached_melon_stem" | "attached_pumpkin_stem" => [0xe0, 0xc7, 0x1c],
        "melon_stem" | "pumpkin_stem" => {
            let age: u32 = props.get("age").and_then(|a| a.parse().ok()).unwrap_or(0);
            [(age * 32) as u8, (255 - age * 8) as u8, (age * 4) as u8]
        }
        b if b.ends_with("leaves") || b == "vine" => FOLIAGE,
        _ => GRASS,
    }
}

/// `RedStoneWireBlock.getColorForPower`.
fn redstone(power: u32) -> [u8; 3] {
    let f = power.min(15) as f32 / 15.0;
    let r = f * 0.6 + if f > 0.0 { 0.4 } else { 0.3 };
    let g = (f * f * 0.7 - 0.5).clamp(0.0, 1.0);
    let b = (f * f * 0.6 - 0.7).clamp(0.0, 1.0);
    [(r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redstone_power_brightens_red() {
        assert_eq!(redstone(0), [76, 0, 0]);
        let full = redstone(15);
        // 0.7f - 0.5f is just under 0.2, so Minecraft itself lands on 50 or 51.
        assert!(full[0] == 255 && (50..=51).contains(&full[1]) && full[2] == 0);
        let props = Properties::from([("power".to_string(), "15".to_string())]);
        assert_eq!(color("redstone_wire", &props), full);
        assert_eq!(color("oak_leaves", &Properties::new()), FOLIAGE);
        assert_eq!(color("grass_block", &Properties::new()), GRASS);
    }
}
