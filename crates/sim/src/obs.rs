//! Building the protocol `obs` message (and replay frames) from the match state.
use crate::config::WKind;
use crate::sim::{Sim, Weapon};
use crate::util::*;
use serde_json::{json, Value};

fn j2(a: V2) -> Value {
    json!([r3(a[0]), r3(a[1])])
}

impl Sim {
    fn weapon_json(&self, i: usize, w: &Weapon) -> Value {
        let m = &self.mechs[i];
        let stats = Value::Object(m.wst.maps[w.kind as usize].clone());
        let mut v = json!({"kind": w.kind.name(), "cooldown": r2(w.cooldown.max(0.0)), "stats": stats});
        match w.kind {
            WKind::Gun => {
                v["ammo"] = json!(w.ammo);
                v["reload"] = json!(r2(w.reload.max(0.0)));
            }
            WKind::Laser => {
                v["charge"] = json!(r2(if w.charging { w.charge.min(m.wst.laser.charge).max(0.01) } else { 0.0 }));
            }
            _ => {}
        }
        v
    }

    pub fn zone_json(&self) -> Value {
        let z = &self.zone;
        json!({"center":[r2(z.center[0]), r2(z.center[1])], "radius": r2(z.radius),
               "next_center":[r2(z.next_center[0]), r2(z.next_center[1])], "next_radius": r2(z.next_radius),
               "shrink_start": r2(z.shrink_start), "shrink_end": r2(z.shrink_end), "dps": r2(z.dps)})
    }

    /// The observation for mech `i` at this decision. Drains the mech's event queue.
    /// Call `update_vision()` first and `end_decision()` after all observations.
    pub fn observe(&mut self, i: usize) -> Value {
        let cfg = self.cfg.clone();
        let t = self.t;
        let me_pos = self.mechs[i].kin.pos;
        let show_hp = cfg.vision.show_enemy_hp;

        let mut mechs: Vec<Value> = Vec::new();
        for j in 0..self.mechs.len() {
            if j == i || !self.sees(i, j) {
                continue;
            }
            let m = &self.mechs[j];
            let weapons: Vec<&str> = m.weapons.iter().flatten().map(|w| w.kind.name()).collect();
            let mut v = json!({"id": j, "kind": "mech", "pos": j3(m.kin.pos), "vel": j3(m.kin.vel), "aim": j2(m.aim),
                               "cubes": m.cubes, "weapons": weapons, "charging": m.charging()});
            if show_hp {
                v["hp"] = json!(r2(m.hp));
                v["max_hp"] = json!(r2(m.max_hp));
            }
            mechs.push(v);
        }
        for k in 0..self.npcs.len() {
            if !self.sees_npc(i, k) {
                continue;
            }
            let n = &self.npcs[k];
            mechs.push(json!({"id": n.id, "kind": "npc", "pos": j3(n.kin.pos), "vel": j3(n.kin.vel), "aim": j2(n.aim), "hp": r2(n.hp), "max_hp": r2(cfg.npc.hp)}));
        }

        let mut projectiles: Vec<Value> = Vec::new();
        for p in &self.projs {
            if p.owner != i as i32 && self.can_see(i, p.pos, 0.0) {
                projectiles.push(json!({"id": p.id, "kind": p.kind.name(), "pos": j3(p.pos), "vel": j3(p.vel)}));
            }
        }
        let mut crates: Vec<Value> = Vec::new();
        for c in &self.crates {
            if c.alive && self.can_see(i, c.pos, cfg.crates.height) {
                crates.push(json!({"id": c.id, "pos": j3(c.pos), "hp": r2(c.hp)}));
            }
        }
        let mut cubes: Vec<Value> = Vec::new();
        for c in &self.cubes {
            if self.can_see(i, c.pos, 1.0) {
                cubes.push(json!({"id": c.id, "pos": j3(c.pos)}));
            }
        }
        let mut pickups: Vec<Value> = Vec::new();
        for p in &self.pickups {
            if self.can_see(i, p.pos, 1.0) {
                pickups.push(json!({"id": p.id, "kind": p.w.kind.name(), "pos": j3(p.pos)}));
            }
        }

        let sigma = cfg.sound.bearing_noise_deg.to_radians() * self.mechs[i].st.hearing;
        let mut sounds: Vec<Value> = Vec::new();
        for s in 0..self.sounds.len() {
            let (kind, pos, st, src) = {
                let s = &self.sounds[s];
                (s.kind, s.pos, s.t, s.src)
            };
            if src == Some(i) {
                continue;
            }
            let d = dist(me_pos, pos);
            if d > cfg.sound_range(kind) {
                continue;
            }
            let bearing = wrap_pi(yaw_to(me_pos, pos) + self.rng.gauss() * sigma);
            let class = if d < cfg.sound.near { "near" } else if d < cfg.sound.mid { "mid" } else { "far" };
            sounds.push(json!({"kind": kind, "bearing": r3(bearing), "dist": class, "age": r2(t - st)}));
        }

        let mut beams: Vec<Value> = Vec::new();
        for b in &self.beams {
            if b.by == i as i32 {
                continue;
            }
            let mut seen = false;
            for w in 0..b.path.len() {
                if self.can_see(i, b.path[w], 0.0) {
                    seen = true;
                    break;
                }
                if w + 1 < b.path.len() {
                    let mid = scale(add(b.path[w], b.path[w + 1]), 0.5);
                    if self.can_see(i, mid, 0.0) {
                        seen = true;
                        break;
                    }
                }
            }
            if seen {
                let path: Vec<Value> = b.path.iter().map(|p| j3(*p)).collect();
                beams.push(json!({"by": b.by, "kind": b.kind, "path": path, "age": r2(t - b.t)}));
            }
        }

        let drops: Vec<Value> = self
            .drops
            .iter()
            .map(|d| {
                json!({"id": d.id, "pos": j3(d.pos), "radius": cfg.drops.radius, "lands_in": r2((d.lands_at - t).max(0.0)),
                       "capture_s": cfg.drops.capture_s, "progress": r2(d.progress), "holder": d.holder})
            })
            .collect();

        let weapons: Vec<Value> = (0..2)
            .map(|s| match &self.mechs[i].weapons[s] {
                Some(w) => self.weapon_json(i, w),
                None => Value::Null,
            })
            .collect();
        let zone = self.zone_json();
        let alive = self.alive_count;
        let tick = self.tick;

        let m = &mut self.mechs[i];
        let mut events = std::mem::take(&mut m.events);
        if m.zone_acc > 0.0 {
            events.push(json!({"type":"damaged","amount": r2(m.zone_acc),"weapon":"zone","bearing": null,"by": null}));
            m.zone_acc = 0.0;
        }
        let offer = match &m.offer {
            Some(o) => {
                let ch: Vec<Value> = o.choices.iter().map(|&u| json!({"id": cfg.upgrades[u].id, "name": cfg.upgrades[u].name, "desc": cfg.upgrades[u].desc})).collect();
                json!({"id": o.id, "expires": r2(o.expires), "choices": ch})
            }
            None => Value::Null,
        };
        let stats = serde_json::to_value(m.st).unwrap_or(Value::Null);
        json!({
            "type": "obs", "tick": tick, "t": r2(t), "alive": alive,
            "self": {
                "id": i, "pos": j3(m.kin.pos), "vel": j3(m.kin.vel), "aim": j2(m.aim),
                "hp": r2(m.hp), "max_hp": r2(m.max_hp), "energy": r2(m.energy), "max_energy": r2(m.st.energy),
                "grounded": m.kin.grounded, "cubes": m.cubes, "upgrades": m.upgrades,
                "weapons": weapons, "in_zone": m.in_zone, "stats": stats
            },
            "mechs": mechs, "projectiles": projectiles, "crates": crates, "cubes": cubes, "pickups": pickups,
            "sounds": sounds, "beams": beams, "events": events, "zone": zone, "offer": offer, "drops": drops
        })
    }

    /// A compact frame for the JSONL replay.
    pub fn frame(&self) -> Value {
        let mechs: Vec<Value> = self
            .mechs
            .iter()
            .filter(|m| m.alive)
            .map(|m| {
                let w: Vec<&str> = m.weapons.iter().flatten().map(|w| w.kind.name()).collect();
                json!({"id": m.id, "p": j3(m.kin.pos), "a": j2(m.aim), "hp": r2(m.hp), "e": r2(m.energy), "c": m.cubes, "w": w, "g": m.kin.grounded, "wall": m.wall})
            })
            .collect();
        let npcs: Vec<Value> = self.npcs.iter().filter(|n| n.alive).map(|n| json!({"id": n.id, "p": j3(n.kin.pos), "hp": r2(n.hp)})).collect();
        let projs: Vec<Value> = self.projs.iter().map(|p| json!({"k": p.kind.name(), "p": j3(p.pos), "o": p.owner})).collect();
        let beams: Vec<Value> = self.beams.iter().map(|b| json!({"by": b.by, "k": b.kind, "path": b.path.iter().map(|p| j3(*p)).collect::<Vec<_>>()})).collect();
        let z = &self.zone;
        json!({"type":"frame","tick": self.tick, "t": r2(self.t), "alive": self.alive_count,
               "zone": [r2(z.center[0]), r2(z.center[1]), r2(z.radius)],
               "mechs": mechs, "npcs": npcs, "proj": projs, "beams": beams,
               "cubes": self.cubes.len(), "pickups": self.pickups.len(), "crates": self.crates.len(),
               "drops": self.drops.iter().map(|d| json!({"id": d.id, "p": j3(d.pos), "landed": d.landed, "holder": d.holder})).collect::<Vec<_>>()})
    }
}
