//! Parts of modded blocks that the mod draws with code, from "partial
//! models" shipped in its jar, such as Create's belts. The block models of
//! these blocks are empty or only show a frame; the transforms here follow
//! the mod's block entity renderers, frozen at rest.

use glam::{Mat4, Quat, Vec3};
use litematic::BlockState;

/// A model drawn as part of a block.
pub(crate) struct Part {
    pub model: String,
    /// Blockstate-style rotation around X then Y in degrees.
    pub x: i32,
    pub y: i32,
    /// Applied after the rotation, in block units (0..1).
    pub transform: Mat4,
}

impl Part {
    fn new(model: impl Into<String>, transform: Mat4) -> Self {
        Part { model: model.into(), x: 0, y: 0, transform }
    }
}

/// Parts to draw on top of the block's own model.
pub(crate) fn parts(state: &BlockState) -> Vec<Part> {
    let prop = |k: &str| state.properties.get(k).map(String::as_str).unwrap_or_default();
    let facing = prop("facing");
    let axis = || prop("axis").chars().next().unwrap_or('y');
    match state.name.strip_prefix("create:").unwrap_or_default() {
        "belt" => belt(prop("slope"), prop("part"), facing),
        "blaze_burner" => blaze_burner(prop("blaze")),
        "water_wheel" => {
            // The wheel turns to face like the block's hub.
            let (x, y) = facing_rotation(facing);
            vec![Part { model: "create:block/water_wheel/wheel".into(), x, y, transform: Mat4::IDENTITY }]
        }
        // A whole shaft through the block (`ShaftRenderer`).
        "andesite_encased_shaft" | "brass_encased_shaft" | "metal_girder_encased_shaft" | "encased_chain_drive"
        | "adjustable_chain_gearshift" | "rotation_speed_controller" | "flywheel" => vec![shaft(axis())],
        "mechanical_piston" | "sticky_mechanical_piston" => {
            vec![shaft(directional_axis(facing, prop("axis_along_first") == "true"))]
        }
        "speedometer" | "stressometer" => gauge(state.name.ends_with("speedometer"), facing, prop("axis_along_first") == "true"),
        // Half shafts out of every side but the box's own axis.
        "gearbox" => DIRECTIONS.into_iter().filter(|d| axis_of(d) != axis()).map(half_shaft).collect(),
        // Two halves, which turn separately.
        "clutch" | "gearshift" => split_shaft(axis()),
        "sequenced_gearshift" => split_shaft(if prop("vertical") == "true" { 'y' } else { axis() }),
        "creative_motor" => vec![half_shaft(facing)],
        "mechanical_pump" => vec![face("create:block/mechanical_pump/cog", facing)],
        "encased_fan" => {
            let back = opposite(facing);
            vec![half_shaft(back), face("create:block/encased_fan/propeller", back)]
        }
        "millstone" => vec![Part::new("create:block/millstone/inner", Mat4::IDENTITY)],
        "mechanical_drill" => vec![face("create:block/mechanical_drill/head", facing)],
        "mechanical_press" => {
            vec![shaft(axis_of(facing)), face("create:block/mechanical_press/head", facing)]
        }
        "mechanical_mixer" => vec![
            Part::new("create:block/cogwheel_shaftless", Mat4::IDENTITY),
            Part::new("create:block/mechanical_mixer/pole", Mat4::IDENTITY),
            Part::new("create:block/mechanical_mixer/head", Mat4::IDENTITY),
        ],
        "andesite_encased_cogwheel" | "brass_encased_cogwheel" => encased_cog(false, axis(), prop("top_shaft"), prop("bottom_shaft")),
        "andesite_encased_large_cogwheel" | "brass_encased_large_cogwheel" => {
            encased_cog(true, axis(), prop("top_shaft"), prop("bottom_shaft"))
        }
        "mechanical_bearing" | "clockwork_bearing" => bearing("create:block/bearing/top", facing),
        "windmill_bearing" => bearing("create:block/bearing/top_wooden", facing),
        "hand_crank" => vec![face("create:block/hand_crank/handle", opposite(facing))],
        "mechanical_arm" => arm(prop("ceiling") == "true"),
        "steam_engine" => steam_engine(prop("face"), facing),
        "andesite_funnel" | "brass_funnel" if axis_of(facing) != 'y' => flaps("create:block/funnel/flap", facing, 1.0 / 16.0),
        "andesite_belt_funnel" | "brass_belt_funnel" => {
            let offset = match prop("shape") {
                "extended" => 8.0 / 16.0,
                "pulling" | "pushing" => -2.0 / 16.0,
                _ => 0.0,
            };
            flaps("create:block/belt_funnel/flap", facing, -offset)
        }
        "andesite_tunnel" | "brass_tunnel" => tunnel(axis(), prop("shape")),
        "mechanical_saw" => saw(facing, prop("axis_along_first") == "true"),
        "deployer" => deployer(facing, prop("axis_along_first") == "true"),
        "cuckoo_clock" | "mysterious_cuckoo_clock" => {
            let turn = about_center(rot_y(horizontal_angle(counter_clockwise(facing))));
            let mut parts = vec![half_shaft(opposite(facing))];
            for model in ["hour_hand", "minute_hand", "left_door", "right_door"] {
                parts.push(Part::new(format!("create:block/cuckoo_clock/{model}"), turn));
            }
            parts
        }
        "analog_lever" => {
            let x = match prop("face") {
                "floor" => 0.0,
                "wall" => 90.0,
                _ => 180.0,
            };
            vec![Part::new("create:block/analog_lever/handle", about_center(rot_y(horizontal_angle(facing)) * rot_x(x)))]
        }
        "weighted_ejector" => {
            vec![Part::new("create:block/weighted_ejector/top", about_center(rot_y(180.0 + horizontal_angle(facing))))]
        }
        "mechanical_harvester" => {
            vec![Part::new("create:block/mechanical_harvester/blade", about_center(rot_y(horizontal_angle(facing))))]
        }
        "mechanical_roller" => {
            let n = normal(facing) * (17.0 / 16.0);
            let wheel = translate(0.0, -0.25, 0.0)
                * translate(n.x, n.y, n.z)
                * about_center(rot_y(horizontal_angle(facing)))
                * translate(0.0, -0.5, 0.5)
                * Mat4::from_quat(rot_y(90.0));
            vec![
                Part::new("create:block/mechanical_roller/wheel", wheel),
                Part::new("create:block/mechanical_roller/frame", about_center(rot_y(horizontal_angle(facing) + 180.0))),
            ]
        }
        "sticker" => {
            let out = if prop("extended") == "true" { 4.0 / 16.0 } else { 0.0 };
            let turn = about_center(rot_y(horizontal_angle(facing)) * rot_x(vertical_angle(facing) + 90.0));
            vec![Part::new("create:block/sticker/head", turn * translate(0.0, out, 0.0))]
        }
        "spout" => ["top", "middle", "bottom"]
            .into_iter()
            .map(|bit| Part::new(format!("create:block/spout/{bit}"), Mat4::IDENTITY))
            .collect(),
        "fluid_valve" => fluid_valve(facing, prop("axis_along_first") == "true"),
        "steam_whistle" => {
            let size = match prop("size") {
                "large" => "large",
                "medium" => "medium",
                _ => "small",
            };
            vec![Part::new(format!("create:block/steam_whistle/{size}_mouth"), about_center(rot_y(horizontal_angle(facing))))]
        }
        "rope_pulley" => pulley("rope_pulley/rope_coil", "rope_pulley/rope_half_magnet", axis()),
        "hose_pulley" => pulley("hose_pulley/hose_coil", "hose_pulley/rope_half_magnet", axis_of(clockwise(facing))),
        "elevator_pulley" => vec![
            shaft(axis_of(facing)),
            face("create:block/elevator_pulley/rope_coil", facing),
            Part::new("create:block/elevator_pulley/pulley_magnet", about_center(rot_y(180.0 + horizontal_angle(facing)))),
        ],
        "portable_storage_interface" | "portable_fluid_interface" => {
            let kind = state.name.strip_prefix("create:").unwrap_or_default();
            let turn = about_center(rot_y(horizontal_angle(facing)) * rot_x(pipe_tilt(facing)));
            vec![
                Part::new(format!("create:block/{kind}/block_middle"), turn * translate(0.0, 0.375, 0.0)),
                Part::new(format!("create:block/{kind}/block_top"), turn),
            ]
        }
        "schematicannon" => {
            // Aimed at nothing: its saved default yaw is not in the block state.
            let (yaw, pitch) = (0f32, 40f32);
            let pivot = Vec3::new(0.5, 15.0 / 16.0, 0.5);
            vec![
                Part::new(
                    "create:block/schematicannon/connector",
                    translate(0.5, 0.0, 0.5) * Mat4::from_quat(rot_y(yaw + 90.0)) * translate(-0.5, 0.0, -0.5),
                ),
                Part::new(
                    "create:block/schematicannon/pipe",
                    Mat4::from_translation(pivot)
                        * Mat4::from_quat(rot_y(yaw + 90.0) * Quat::from_rotation_z(pitch.to_radians()))
                        * Mat4::from_translation(-pivot),
                ),
            ]
        }
        "gantry_carriage" => {
            let along = prop("axis_along_first") == "true";
            let roll = if along ^ (axis_of(facing) == 'x') { 0.0 } else { 90.0 };
            let turn = rot_y(horizontal_angle(facing)) * rot_x(pipe_tilt(facing)) * rot_y(roll);
            vec![Part::new("create:block/gantry_carriage/wheels", about_center(turn))]
        }
        "packager" | "repackager" => {
            let out = opposite(facing);
            let n = normal(out) * 0.49999;
            let hatch = translate(n.x, n.y, n.z) * about_center(rot_y(horizontal_angle(out)) * rot_x(vertical_angle(out)));
            let tray = if state.name.ends_with(":packager") { "create:block/packager/tray" } else { "create:block/repackager/tray" };
            vec![
                Part::new("create:block/packager/hatch_closed", hatch),
                Part::new(tray, about_center(rot_y(y_rot(out)))),
            ]
        }
        "chain_conveyor" => vec![Part::new("create:block/chain_conveyor/wheel", Mat4::IDENTITY)],
        "copper_backtank" | "netherite_backtank" => {
            let kind = state.name.strip_prefix("create:").unwrap_or_default();
            vec![
                Part::new(format!("create:block/{kind}/block_shaft_input"), Mat4::IDENTITY),
                Part::new(format!("create:block/{kind}/block_cogs"), about_center(rot_y(180.0 + horizontal_angle(facing)))),
            ]
        }
        "contraption_controls" => {
            let front = opposite(facing);
            vec![
                face("create:block/contraption_controls/button", front),
                face("create:block/contraption_controls/indicator_0", front),
            ]
        }
        "mechanical_crafter" => {
            let x = match prop("pointing") {
                "right" => 90.0,
                "down" => 180.0,
                "left" => 270.0,
                _ => 0.0,
            };
            let cog_turn = if axis_of(facing) == 'x' { 90.0 } else { 0.0 };
            let turn = about_center(rot_y(horizontal_angle(facing) + 90.0) * rot_x(x));
            // Crafters usually pass items on to a neighbour, shown with a belt.
            vec![
                Part::new("create:block/cogwheel_shaftless", about_center(rot_y(cog_turn) * rot_x(90.0))),
                Part::new("create:block/mechanical_crafter/belt_animated", turn),
                Part::new("create:block/mechanical_crafter/belt", turn),
            ]
        }
        name => {
            if let Some(color) = name.strip_suffix("_toolbox") {
                let turn = about_center(rot_y(-y_rot(opposite(facing))));
                vec![
                    Part::new(format!("create:block/toolbox/lid/{color}"), turn),
                    Part::new("create:block/toolbox/drawer", turn),
                    Part::new("create:block/toolbox/drawer", turn * translate(0.0, 1.0 / 8.0, 0.0)),
                ]
            } else if name.ends_with("_postbox") {
                vec![Part::new("create:block/package_postbox/flag", about_center(rot_y(180.0 - y_rot(facing))))]
            } else {
                Vec::new()
            }
        }
    }
}

fn normal(dir: &str) -> Vec3 {
    match dir {
        "down" => -Vec3::Y,
        "up" => Vec3::Y,
        "north" => -Vec3::Z,
        "south" => Vec3::Z,
        "west" => -Vec3::X,
        _ => Vec3::X,
    }
}

fn clockwise(dir: &str) -> &'static str {
    match dir {
        "north" => "east",
        "east" => "south",
        "south" => "west",
        _ => "north",
    }
}

fn counter_clockwise(dir: &str) -> &'static str {
    match dir {
        "north" => "west",
        "west" => "south",
        "south" => "east",
        _ => "north",
    }
}

/// `Direction.toYRot`: south 0, west 90, north 180, east 270.
fn y_rot(dir: &str) -> f32 {
    match dir {
        "west" => 90.0,
        "north" => 180.0,
        "east" => 270.0,
        _ => 0.0,
    }
}

/// Tilt that stands an upright model on the `dir` face: up 0, down 180, sides 90.
fn pipe_tilt(dir: &str) -> f32 {
    match dir {
        "up" => 0.0,
        "down" => 180.0,
        _ => 90.0,
    }
}

/// `FlapStuffs.renderFlaps` at rest: four flaps across a funnel or tunnel side.
fn flaps(model: &'static str, side: &str, z: f32) -> Vec<Part> {
    let base = about_center(rot_y(horizontal_angle(opposite(side)))) * translate(0.075 / 16.0, 0.0, z);
    (0..4).map(|i| Part::new(model, base * translate(i as f32 * -3.05 / 16.0, 0.0, 0.0))).collect()
}

/// `BeltTunnelBlockEntity.updateTunnelConnections`: flaps at both belt ends
/// and on the open sides of T and cross tunnels. Flaps between two
/// neighbouring tunnels are hidden in game; that needs the neighbours.
fn tunnel(axis: char, shape: &str) -> Vec<Part> {
    let mut parts = Vec::new();
    for dir in ["north", "south", "west", "east"] {
        if axis_of(dir) != axis {
            let positive = matches!(dir, "east" | "south") ^ (axis_of(dir) == 'z');
            if matches!(shape, "straight" | "window" | "closed")
                || (positive && shape == "t_left")
                || (!positive && shape == "t_right")
            {
                continue;
            }
        }
        parts.extend(flaps("create:block/belt_tunnel/flap", dir, 0.0));
    }
    parts
}

/// `SawRenderer`: the blade at rest and the shaft behind it.
fn saw(facing: &str, along_first: bool) -> Vec<Part> {
    if axis_of(facing) != 'y' {
        return vec![
            face("create:block/mechanical_saw/blade_horizontal_inactive", facing),
            half_shaft(opposite(facing)),
        ];
    }
    let mut blade = face("create:block/mechanical_saw/blade_vertical_inactive", facing);
    if along_first {
        blade.transform *= about_center(rot_y(90.0));
    }
    vec![blade, shaft(directional_axis(facing, along_first))]
}

/// `DeployerRenderer` with the hand pulled in.
fn deployer(facing: &str, along_first: bool) -> Vec<Part> {
    let x = match facing {
        "up" => 270.0,
        "down" => 90.0,
        _ => 0.0,
    };
    let roll = if along_first ^ (axis_of(facing) == 'z') { 90f32 } else { 0.0 };
    let turn = |roll: f32| about_center(rot_y(horizontal_angle(facing)) * rot_x(x) * Quat::from_rotation_z(roll.to_radians()));
    vec![
        shaft(directional_axis(facing, along_first)),
        Part::new("create:block/deployer/pole", turn(roll)),
        Part::new("create:block/deployer/hand_pointing", turn(0.0)),
    ]
}

/// `FluidValveRenderer`: the pointer, closed.
fn fluid_valve(facing: &str, along_first: bool) -> Vec<Part> {
    // `FluidValveBlock.getPipeAxis`: one of the two axes across the facing.
    let across: Vec<char> = ['x', 'y', 'z'].into_iter().filter(|&a| a != axis_of(facing)).collect();
    let pipe = if along_first { across[1] } else { across[0] };
    let shaft_axis = directional_axis(facing, along_first);
    let offset = if (pipe != 'y' && shaft_axis == 'x') || pipe == 'y' { 90.0 } else { 0.0 };
    let turn = rot_y(horizontal_angle(facing)) * rot_x(pipe_tilt(facing)) * rot_y(offset);
    vec![shaft(shaft_axis), Part::new("create:block/fluid_valve/pointer", about_center(turn))]
}

/// `AbstractPulleyRenderer` with nothing lowered: shaft, coil and the
/// magnet tucked under the block.
fn pulley(coil: &str, magnet: &str, axis: char) -> Vec<Part> {
    vec![
        shaft(axis),
        face(format!("create:block/{coil}"), positive(axis)),
        Part::new(format!("create:block/{magnet}"), Mat4::IDENTITY),
    ]
}

const DIRECTIONS: [&str; 6] = ["down", "up", "north", "south", "west", "east"];

fn axis_of(dir: &str) -> char {
    match dir {
        "east" | "west" => 'x',
        "north" | "south" => 'z',
        _ => 'y',
    }
}

fn opposite(dir: &str) -> &'static str {
    match dir {
        "down" => "up",
        "up" => "down",
        "north" => "south",
        "south" => "north",
        "west" => "east",
        _ => "west",
    }
}

fn positive(axis: char) -> &'static str {
    match axis {
        'x' => "east",
        'z' => "south",
        _ => "up",
    }
}

fn rot_x(deg: f32) -> Quat {
    Quat::from_rotation_x(deg.to_radians())
}

fn rot_y(deg: f32) -> Quat {
    Quat::from_rotation_y(deg.to_radians())
}

fn translate(x: f32, y: f32, z: f32) -> Mat4 {
    Mat4::from_translation(Vec3::new(x, y, z))
}

/// `AngleHelper.verticalAngle`.
fn vertical_angle(dir: &str) -> f32 {
    match dir {
        "up" => -90.0,
        "down" => 90.0,
        _ => 0.0,
    }
}

/// `CachedBuffers.partialFacing`: a model made facing south, turned to `dir`.
fn face(model: impl Into<String>, dir: &str) -> Part {
    Part::new(model, about_center(rot_y(horizontal_angle(dir)) * rot_x(vertical_angle(dir))))
}

/// `CachedBuffers.partialFacingVertical`: a model made facing up, turned to `dir`.
fn face_vertical(model: impl Into<String>, dir: &str) -> Part {
    Part::new(model, about_center(rot_y(horizontal_angle(dir)) * rot_x(vertical_angle(dir) + 90.0)))
}

fn shaft(axis: char) -> Part {
    face_vertical("create:block/shaft", positive(axis))
}

fn half_shaft(dir: &str) -> Part {
    face("create:block/shaft_half", dir)
}

fn split_shaft(axis: char) -> Vec<Part> {
    vec![half_shaft(positive(axis)), half_shaft(opposite(positive(axis)))]
}

/// `DirectionalAxisKineticBlock.getRotationAxis`.
fn directional_axis(facing: &str, along_first: bool) -> char {
    match axis_of(facing) {
        'x' => if along_first { 'y' } else { 'z' },
        'y' => if along_first { 'x' } else { 'z' },
        _ => if along_first { 'x' } else { 'y' },
    }
}

/// `GaugeRenderer`: a shaft, and a dial on each side that shows one.
fn gauge(speed: bool, facing: &str, along_first: bool) -> Vec<Part> {
    let axis = directional_axis(facing, along_first);
    let head = if speed { "create:block/gauge/speedometer/head" } else { "create:block/gauge/stressometer/head" };
    let mut parts = vec![shaft(axis)];
    for side in ["north", "south", "west", "east"] {
        if side == opposite(facing) || axis_of(side) == axis || (axis == 'y' && side != facing) {
            continue;
        }
        // `Direction.toYRot`: south 0, west 90, north 180, east 270.
        let y_rot = match side {
            "west" => 90.0,
            "north" => 180.0,
            "east" => 270.0,
            _ => 0.0,
        };
        let turn = about_center(rot_y(-y_rot - 90.0));
        parts.push(Part::new(head, turn));
        parts.push(Part::new("create:block/gauge/dial", turn));
    }
    parts
}

/// `EncasedCogRenderer`: the cog, and half shafts where the block has them.
fn encased_cog(large: bool, axis: char, top: &str, bottom: &str) -> Vec<Part> {
    let cog = if large { "create:block/large_cogwheel_shaftless" } else { "create:block/cogwheel_shaftless" };
    let mut parts = vec![face_vertical(cog, positive(axis))];
    if top == "true" {
        parts.push(half_shaft(positive(axis)));
    }
    if bottom == "true" {
        parts.push(half_shaft(opposite(positive(axis))));
    }
    parts
}

/// `BearingRenderer`: the turning top and a half shaft out of the back.
fn bearing(top: impl Into<String>, facing: &str) -> Vec<Part> {
    let mut rot = Quat::IDENTITY;
    if axis_of(facing) != 'y' {
        rot *= rot_y(horizontal_angle(opposite(facing)));
    }
    rot *= rot_x(-90.0 - vertical_angle(facing));
    vec![half_shaft(opposite(facing)), Part::new(top, about_center(rot))]
}

/// `ArmRenderer` at rest (`ArmAngleTarget.NO_TARGET`): the arm folded up
/// over its base, pointing north. Its models are made around the origin.
fn arm(ceiling: bool) -> Vec<Part> {
    let (base_angle, lower_angle, upper_angle, head_angle) = (0.0, 135.0, 45.0, 0.0);
    let mut m = translate(0.5, 0.5, 0.5);
    if ceiling {
        m *= Mat4::from_quat(rot_x(180.0));
    }
    let base = m * translate(0.0, 4.0 / 16.0, 0.0) * Mat4::from_quat(rot_y(base_angle));
    let lower = base * translate(0.0, 2.0 / 16.0, 0.0) * Mat4::from_quat(rot_x(lower_angle));
    let upper = lower * translate(0.0, 0.0, -14.0 / 16.0) * Mat4::from_quat(rot_x(upper_angle - 180.0));
    let head = upper * translate(0.0, 0.0, -15.0 / 16.0) * Mat4::from_quat(rot_x(head_angle - 45.0));
    vec![
        Part::new("create:block/mechanical_arm/cog", Mat4::IDENTITY),
        Part::new("create:block/mechanical_arm/base", base),
        Part::new("create:block/mechanical_arm/lower_body", lower),
        Part::new("create:block/mechanical_arm/upper_body", upper),
        Part::new("create:block/mechanical_arm/claw_base", head),
        Part::new("create:block/mechanical_arm/lower_claw_grip", head * translate(0.0, -1.0 / 16.0, -6.0 / 16.0)),
        Part::new("create:block/mechanical_arm/upper_claw_grip", head * translate(0.0, 1.0 / 16.0, -6.0 / 16.0)),
    ]
}

/// `SteamEngineRenderer` at crank angle 0: the piston, its linkage and the
/// crank on the powered shaft two blocks away. The shaft's own block only
/// has its end caps. Assumes the shaft runs horizontally, as it does when
/// the engine sits on a boiler's side.
fn steam_engine(face: &str, facing: &str) -> Vec<Part> {
    // `FaceAttachedHorizontalDirectionalBlock.getConnectedDirection`.
    let toward = match face {
        "floor" => "up",
        "ceiling" => "down",
        _ => facing,
    };
    let base = about_center(rot_y(horizontal_angle(toward)) * rot_x(vertical_angle(toward) + 90.0));
    let angle = 0f32;
    let (s, c) = angle.sin_cos();
    let piston = 6.0 / 16.0 * s - ((14.0f32 / 16.0).powi(2) - (6.0f32 / 16.0).powi(2) * c * c).sqrt();
    let distance = (piston - 6.0 / 16.0 * s).abs();
    let swing = (distance / (14.0 / 16.0)).acos().to_degrees() * if c >= 0.0 { 1.0 } else { -1.0 };
    let lift = translate(0.0, piston + 20.0 / 16.0, 0.0);
    vec![
        Part::new("create:block/steam_engine/piston", base * lift),
        Part::new(
            "create:block/steam_engine/linkage",
            base * translate(0.0, 1.0, 0.0)
                * lift
                * translate(0.0, 4.0 / 16.0, 8.0 / 16.0)
                * Mat4::from_quat(rot_x(swing))
                * translate(0.0, -4.0 / 16.0, -8.0 / 16.0),
        ),
        Part::new(
            "create:block/steam_engine/shaft_connector",
            base * translate(0.0, 2.0, 0.0) * about_center(rot_x(-(angle.to_degrees() + 90.0))),
        ),
    ]
}

/// `rot` around the block's center, like Create's `center().rotate…().uncenter()`.
fn about_center(rot: Quat) -> Mat4 {
    let c = Vec3::splat(0.5);
    Mat4::from_translation(c) * Mat4::from_quat(rot) * Mat4::from_translation(-c)
}

/// Create's `AngleHelper.horizontalAngle`: models face south at 0°.
fn horizontal_angle(facing: &str) -> f32 {
    match facing {
        "east" => 90.0,
        "north" => 180.0,
        "west" => 270.0,
        _ => 0.0,
    }
}

/// Blockstate rotation that turns a model's top towards `facing`.
fn facing_rotation(facing: &str) -> (i32, i32) {
    match facing {
        "down" => (180, 0),
        "north" => (90, 0),
        "east" => (90, 90),
        "south" => (90, 180),
        "west" => (90, 270),
        _ => (0, 0),
    }
}

/// `BeltRenderer`: the belt surface, top and bottom, and a pulley at both ends.
fn belt(slope: &str, part: &str, facing: &str) -> Vec<Part> {
    let downward = slope == "downward";
    let upward = slope == "upward";
    let sideways = slope == "sideways";
    let diagonal = downward || upward;
    let (mut start, mut end) = (part == "start", part == "end");

    let angle = horizontal_angle(facing) + if upward { 180.0 } else { 0.0 } + if sideways { 270.0 } else { 0.0 };
    let rot = Quat::from_rotation_y(angle.to_radians())
        * Quat::from_rotation_z(if sideways { 90f32.to_radians() } else { 0.0 })
        * Quat::from_rotation_x(if !diagonal && slope != "horizontal" { 90f32.to_radians() } else { 0.0 });
    let positive = matches!(facing, "east" | "south");
    if downward || (slope == "vertical" && positive) {
        std::mem::swap(&mut start, &mut end);
    }

    let transform = about_center(rot);
    let mut parts = Vec::new();
    if diagonal {
        let model = match (start, end) {
            (true, _) => "create:block/belt/diagonal_start",
            (_, true) => "create:block/belt/diagonal_end",
            _ => "create:block/belt/diagonal_middle",
        };
        parts.push(Part::new(model, transform));
    } else {
        let (top, bottom) = match (start, end) {
            (true, _) => ("create:block/belt/start", "create:block/belt/start_bottom"),
            (_, true) => ("create:block/belt/end", "create:block/belt/end_bottom"),
            _ => ("create:block/belt/middle", "create:block/belt/middle_bottom"),
        };
        parts.push(Part::new(top, transform));
        parts.push(Part::new(bottom, transform));
    }

    if part != "middle" {
        // The pulley's axis: the facing turned clockwise, or up when sideways.
        let axis = if sideways {
            'y'
        } else if matches!(facing, "north" | "south") {
            'x'
        } else {
            'z'
        };
        let mut rot = Quat::IDENTITY;
        if axis == 'x' {
            rot *= Quat::from_rotation_y(90f32.to_radians());
        }
        if axis == 'y' {
            rot *= Quat::from_rotation_x(90f32.to_radians());
        }
        rot *= Quat::from_rotation_x(90f32.to_radians());
        parts.push(Part::new("create:block/belt_pulley", about_center(rot)));
    }
    parts
}

/// `BlazeBurnerRenderer`: the blaze inside the cage, and its rods when lit.
fn blaze_burner(heat: &str) -> Vec<Part> {
    let head = match heat {
        "none" => return Vec::new(),
        "seething" => "create:block/blaze_burner/blaze/super",
        "fading" | "kindled" => "create:block/blaze_burner/blaze/idle",
        _ => "create:block/blaze_burner/blaze/inert",
    };
    let mut parts = vec![Part::new(head, Mat4::IDENTITY)];
    if matches!(heat, "fading" | "kindled" | "seething") {
        let (small, large) = if heat == "seething" {
            ("create:block/blaze_burner/superheated_rods_small", "create:block/blaze_burner/superheated_rods_large")
        } else {
            ("create:block/blaze_burner/rods_small", "create:block/blaze_burner/rods_large")
        };
        parts.push(Part::new(small, Mat4::from_translation(Vec3::Y * 0.125)));
        parts.push(Part::new(large, Mat4::from_translation(Vec3::Y * (-3.0 / 16.0))));
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(name: &str, props: &[(&str, &str)]) -> BlockState {
        BlockState {
            name: name.into(),
            properties: props.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        }
    }

    #[test]
    fn belt_ends_get_a_pulley_across_the_belt() {
        let parts = parts(&state("create:belt", &[("slope", "horizontal"), ("part", "start"), ("facing", "east")]));
        let models: Vec<_> = parts.iter().map(|p| p.model.as_str()).collect();
        assert_eq!(models, ["create:block/belt/start", "create:block/belt/start_bottom", "create:block/belt_pulley"]);
        // The belt model runs north-south; facing east turns it to run east-west.
        let along = parts[0].transform.transform_vector3(Vec3::Z);
        assert!(along.abs_diff_eq(Vec3::X, 1e-5), "{along}");
        // The pulley model's axis is vertical; across an east belt it runs north-south.
        let axis = parts[2].transform.transform_vector3(Vec3::Y);
        assert!(axis.abs().abs_diff_eq(Vec3::Z, 1e-5), "{axis}");
    }

    #[test]
    fn downward_belts_swap_their_ends_and_middles_have_no_pulley() {
        let parts = parts(&state("create:belt", &[("slope", "downward"), ("part", "start"), ("facing", "north")]));
        assert_eq!(parts.iter().map(|p| p.model.as_str()).collect::<Vec<_>>(), ["create:block/belt/diagonal_end", "create:block/belt_pulley"]);
        let middle = super::parts(&state("create:belt", &[("slope", "horizontal"), ("part", "middle"), ("facing", "north")]));
        assert_eq!(middle.len(), 2);
    }

    fn models(state: &BlockState) -> Vec<String> {
        parts(state).into_iter().map(|p| p.model).collect()
    }

    #[test]
    fn gearbox_has_four_half_shafts_around_its_axis() {
        let parts = parts(&state("create:gearbox", &[("axis", "y")]));
        assert_eq!(parts.len(), 4);
        // Each half shaft points out of a different horizontal side.
        let mut ends: Vec<Vec3> = parts.iter().map(|p| p.transform.transform_point3(Vec3::new(0.5, 0.5, 1.0))).collect();
        ends.sort_by(|a, b| (a.x + 2.0 * a.z).total_cmp(&(b.x + 2.0 * b.z)));
        let expect = [Vec3::new(0.5, 0.5, 0.0), Vec3::new(0.0, 0.5, 0.5), Vec3::new(1.0, 0.5, 0.5), Vec3::new(0.5, 0.5, 1.0)];
        for (end, want) in ends.iter().zip(expect) {
            assert!(end.abs_diff_eq(want, 1e-5), "{end} != {want}");
        }
    }

    #[test]
    fn facing_parts_point_where_the_block_faces() {
        // Models made facing south (+Z) turn towards the block's facing.
        for (dir, normal) in [("north", -Vec3::Z), ("east", Vec3::X), ("up", Vec3::Y), ("down", -Vec3::Y)] {
            let part = face("x", dir);
            let got = part.transform.transform_vector3(Vec3::Z);
            assert!(got.abs_diff_eq(normal, 1e-5), "{dir}: {got}");
        }
        let shaft = shaft('x');
        assert!(shaft.transform.transform_vector3(Vec3::Y).abs_diff_eq(Vec3::X, 1e-5));
    }

    #[test]
    fn kinetic_blocks_get_their_moving_parts() {
        assert_eq!(models(&state("create:mechanical_pump", &[("facing", "east")])), ["create:block/mechanical_pump/cog"]);
        assert_eq!(models(&state("create:mechanical_arm", &[("ceiling", "false")])).len(), 7);
        assert_eq!(models(&state("create:steam_engine", &[("face", "wall"), ("facing", "north")])).len(), 3);
        assert_eq!(models(&state("create:clutch", &[("axis", "x")])), ["create:block/shaft_half"; 2]);
        // A vertical speedometer shows one dial, on its front.
        let gauge = models(&state("create:speedometer", &[("facing", "north"), ("axis_along_first", "false")]));
        assert_eq!(gauge, ["create:block/shaft", "create:block/gauge/speedometer/head", "create:block/gauge/dial"]);
        assert_eq!(models(&state("create:andesite_encased_cogwheel", &[("axis", "y"), ("top_shaft", "true"), ("bottom_shaft", "false")])).len(), 2);
    }

    #[test]
    fn blaze_burner_head_follows_heat() {
        assert!(parts(&state("create:blaze_burner", &[("blaze", "none")])).is_empty());
        assert_eq!(parts(&state("create:blaze_burner", &[("blaze", "smouldering")])).len(), 1);
        let lit = parts(&state("create:blaze_burner", &[("blaze", "seething")]));
        assert_eq!(lit[0].model, "create:block/blaze_burner/blaze/super");
        assert_eq!(lit.len(), 3);
    }
}
