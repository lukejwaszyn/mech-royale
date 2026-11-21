//! Debug tool: render the walkability grid (and optionally a path) to a PNG or PPM.
//!
//!   navdump [--arena DIR] [--out nav.png] [--scale 2] [--cell 2.0] [--margin 0.6]
//!           [--path x0,y0,x1,y1] [--crop x0,y0,x1,y1] [--stats]
//!
//! Colours: grey = main ground component (brighter = higher), yellow = ground not
//! connected to the main component, blue/cyan = roof and deck nodes (brighter =
//! higher), dark red = solid/unwalkable inside a prism footprint, black =
//! unwalkable otherwise, green dots = roof access points, magenta = path.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use world::{Nav, NavParams, World, LAYER_TERRAIN};

fn crc32(data: &[u8], mut c: u32) -> u32 {
    for &b in data {
        c ^= b as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xEDB88320 ^ (c >> 1) } else { c >> 1 };
        }
    }
    c
}

fn png(w: usize, h: usize, rgb: &[u8]) -> Vec<u8> {
    let mut raw = Vec::with_capacity((w * 3 + 1) * h);
    for y in 0..h {
        raw.push(0);
        raw.extend_from_slice(&rgb[y * w * 3..(y + 1) * w * 3]);
    }
    // zlib, stored blocks
    let mut z = vec![0x78, 0x01];
    let mut chunks = raw.chunks(65535).peekable();
    while let Some(c) = chunks.next() {
        z.push(if chunks.peek().is_none() { 1 } else { 0 });
        z.extend_from_slice(&(c.len() as u16).to_le_bytes());
        z.extend_from_slice(&(!(c.len() as u16)).to_le_bytes());
        z.extend_from_slice(c);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &v in &raw {
        a = (a + v as u32) % 65521;
        b = (b + a) % 65521;
    }
    z.extend_from_slice(&((b << 16) | a).to_be_bytes());
    let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let mut chunk = |tag: &[u8; 4], data: &[u8]| {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        out.extend_from_slice(tag);
        out.extend_from_slice(data);
        let c = crc32(data, crc32(tag, 0xFFFF_FFFF)) ^ 0xFFFF_FFFF;
        out.extend_from_slice(&c.to_be_bytes());
    };
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&(w as u32).to_be_bytes());
    ihdr.extend_from_slice(&(h as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(b"IHDR", &ihdr);
    chunk(b"IDAT", &z);
    chunk(b"IEND", &[]);
    out
}

fn parse4(s: &str) -> [f32; 4] {
    let v: Vec<f32> = s.split(',').map(|t| t.trim().parse().expect("number")).collect();
    assert!(v.len() == 4, "expected x0,y0,x1,y1");
    [v[0], v[1], v[2], v[3]]
}

fn main() {
    let mut arena = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../arena"));
    let mut out = PathBuf::from("nav.png");
    let mut scale = 2usize;
    let mut params = NavParams::default();
    let mut path: Option<[f32; 4]> = None;
    let mut crop: Option<[f32; 4]> = None;
    let mut stats = false;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut val = || args.next().expect("missing value");
        match a.as_str() {
            "--arena" => arena = PathBuf::from(val()),
            "--out" => out = PathBuf::from(val()),
            "--scale" => scale = val().parse().expect("scale"),
            "--cell" => params.cell = val().parse().expect("cell"),
            "--margin" => params.margin = val().parse().expect("margin"),
            "--radius" => params.radius = val().parse().expect("radius"),
            "--height" => params.height = val().parse().expect("height"),
            "--path" => path = Some(parse4(&val())),
            "--crop" => crop = Some(parse4(&val())),
            "--stats" => stats = true,
            other => panic!("unknown argument {other}"),
        }
    }
    let t0 = Instant::now();
    let world = Arc::new(World::load(&arena).expect("load world"));
    let t1 = Instant::now();
    let nav = Nav::build(world.clone(), params);
    let t2 = Instant::now();
    eprintln!(
        "load {:.1} ms, nav build {:.1} ms: grid {}x{} cell {} m, {} nodes, main component {} nodes, {} regions",
        (t1 - t0).as_secs_f64() * 1e3,
        (t2 - t1).as_secs_f64() * 1e3,
        nav.nx,
        nav.ny,
        nav.cell,
        nav.node_count(),
        nav.comp_size(nav.main_comp()),
        nav.regions().len()
    );
    if stats {
        let mut rs: Vec<_> = nav.regions().iter().collect();
        rs.sort_by(|a, b| b.nodes.cmp(&a.nodes));
        for r in rs.iter().take(40) {
            eprintln!(
                "region comp {:5} {:5} nodes roof={} z {:.1}..{:.1} centroid ({:.0},{:.0}) min_climb {:.1} access {}",
                r.comp, r.nodes, r.is_roof, r.z_min, r.z_max, r.centroid[0], r.centroid[1], r.min_climb, r.access.len()
            );
        }
    }

    let b = world.bounds;
    let c = crop.unwrap_or([b.min_x, b.min_y, b.max_x, b.max_y]);
    let (i0, j0) = nav.cell_of(c[0], c[1]);
    let (i1, j1) = nav.cell_of(c[2], c[3]);
    let (i0, j0, i1, j1) = (i0.max(0), j0.max(0), i1.min(nav.nx - 1), j1.min(nav.ny - 1));
    let (gw, gh) = ((i1 - i0 + 1) as usize, (j1 - j0 + 1) as usize);
    let (w, h) = (gw * scale, gh * scale);
    let mut img = vec![0u8; w * h * 3];
    let mut put = |px: i64, py: i64, col: [u8; 3]| {
        if px >= 0 && py >= 0 && (px as usize) < w && (py as usize) < h {
            let k = ((h - 1 - py as usize) * w + px as usize) * 3;
            img[k..k + 3].copy_from_slice(&col);
        }
    };
    for j in j0..=j1 {
        for i in i0..=i1 {
            let x = nav.x0 + (i as f32 + 0.5) * nav.cell;
            let y = nav.y0 + (j as f32 + 0.5) * nav.cell;
            let mut col = [0u8, 0, 0];
            let mut in_prism = false;
            world.prisms_at(x, y, |_, _| in_prism = true);
            if in_prism {
                col = [70, 10, 10];
            }
            // topmost node wins
            if let Some(u) = nav.cell_nodes(i, j).last() {
                let z = nav.node_pos(u)[2];
                if nav.node_layer(u) != LAYER_TERRAIN {
                    let v = (60.0 + (z + 10.0) * 3.5).clamp(60.0, 255.0) as u8;
                    col = if nav.is_main(u) { [v / 3, v, v] } else { [20, v / 2, v] };
                } else if nav.is_main(u) {
                    let v = (90.0 + (z + 10.0) * 5.0).clamp(60.0, 240.0) as u8;
                    col = [v, v, v];
                } else {
                    col = [220, 200, 40];
                }
            }
            for sy in 0..scale {
                for sx in 0..scale {
                    put(((i - i0) as usize * scale + sx) as i64, ((j - j0) as usize * scale + sy) as i64, col);
                }
            }
        }
    }
    let to_px = |x: f32, y: f32| -> (i64, i64) {
        (
            (((x - nav.x0) / nav.cell - i0 as f32) * scale as f32) as i64,
            (((y - nav.y0) / nav.cell - j0 as f32) * scale as f32) as i64,
        )
    };
    for r in nav.regions() {
        for a in &r.access {
            let (px, py) = to_px(a.ground[0], a.ground[1]);
            put(px, py, [40, 230, 60]);
        }
    }
    if let Some(p) = path {
        // endpoints are snapped to the main ground component
        let from = nav.nearest_walkable(p[0], p[1]).expect("no walkable ground");
        let to = nav.nearest_walkable(p[2], p[3]).expect("no walkable ground");
        let t = Instant::now();
        let res = nav.find_path(from, to);
        let dt = t.elapsed();
        match res {
            Some(pa) => {
                eprintln!("path: {} waypoints, {:.1} m, goal error {:.2} m, {:.3} ms", pa.points.len(), pa.length, pa.goal_error, dt.as_secs_f64() * 1e3);
                let mut prev = from;
                for q in &pa.points {
                    let len = (q[0] - prev[0]).hypot(q[1] - prev[1]);
                    let n = (len / (nav.cell / scale as f32 * 0.5)).ceil().max(1.0) as usize;
                    for s in 0..=n {
                        let t = s as f32 / n as f32;
                        let (px, py) = to_px(prev[0] + (q[0] - prev[0]) * t, prev[1] + (q[1] - prev[1]) * t);
                        put(px, py, [255, 0, 255]);
                    }
                    let (px, py) = to_px(q[0], q[1]);
                    for dy in -1..=1 {
                        for dx in -1..=1 {
                            put(px + dx, py + dy, [255, 60, 60]);
                        }
                    }
                    prev = *q;
                }
            }
            None => eprintln!("path: none"),
        }
    }
    let bytes = if out.extension().map_or(false, |e| e == "ppm") {
        let mut v = format!("P6\n{} {}\n255\n", w, h).into_bytes();
        v.extend_from_slice(&img);
        v
    } else {
        png(w, h, &img)
    };
    std::fs::write(&out, bytes).expect("write image");
    eprintln!("wrote {} ({}x{})", out.display(), w, h);
}
