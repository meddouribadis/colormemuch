//! The egui RGB control screen — the mockup made real.
//!
//! Layout mirrors `docs/mockups/colormemuch-ui.html`: a left device list, a
//! central row of per-zone control panels, and a shared-clock strip under them.
//! Every zone commands its own [`ZoneSource`]; zones running the same effect are
//! phase-locked to one global clock, shown Link or Spread per group.
//!
//! The OpenRGB connect + enumerate is done on a worker thread so the window
//! never blocks on a missing server. Once connected, each frame samples
//! [`crate::effects::render_plan`] at the global clock and pushes it.

#![cfg(windows)]

use std::collections::HashSet;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, RichText};

use crate::effects::{render_plan, scale, shared_clock_groups, Effect, Params, ZoneSource};
use crate::openrgb::{Controller, OpenRgb};
use crate::rgb::Rgb;

/// How often we push frames to the hardware while animating.
const PUSH_HZ: f32 = 30.0;

#[derive(Clone, Copy, PartialEq)]
enum Source {
    Solid,
    Function,
}

/// Editable state for one zone (one LED).
#[derive(Clone)]
struct ZoneUi {
    source: Source,
    color: [u8; 3],
    effect: Effect,
    speed: u32,      // 1..=9
    brightness: u32, // 0..=100
}

impl Default for ZoneUi {
    fn default() -> Self {
        Self {
            source: Source::Solid,
            color: [0x00, 0xE5, 0xFF],
            effect: Effect::Rainbow,
            speed: 5,
            brightness: 100,
        }
    }
}

impl ZoneUi {
    fn to_source(&self) -> ZoneSource {
        let color = Rgb(self.color[0], self.color[1], self.color[2]);
        match self.source {
            Source::Solid => ZoneSource::Solid(color),
            Source::Function => ZoneSource::Function(
                self.effect,
                Params {
                    color,
                    color_b: Rgb(0xFF, 0x00, 0x88),
                    speed: self.speed as f32 / 5.0, // 5 == nominal 1.0x
                    brightness: self.brightness as f32 / 100.0,
                },
            ),
        }
    }
}

enum Conn {
    Connecting,
    Ready {
        client: OpenRgb,
        controllers: Vec<Controller>,
        /// Parallel to `controllers`; one Vec of zones per controller.
        zones: Vec<Vec<ZoneUi>>,
    },
    Failed(String),
}

type ConnResult = Result<(OpenRgb, Vec<Controller>), String>;

pub struct RgbControl {
    rx: Option<mpsc::Receiver<ConnResult>>,
    conn: Conn,
    selected: usize,
    /// Effects (across the selected device) rendered as Spread rather than Link.
    spread: HashSet<Effect>,
    master: u32, // 0..=100
    clock: Instant,
    last_push: Instant,
    /// Set when a control changes, so an all-solid device still gets one push.
    dirty: bool,
}

impl RgbControl {
    pub fn new() -> Self {
        let rx = spawn_connect();
        Self {
            rx: Some(rx),
            conn: Conn::Connecting,
            selected: 0,
            spread: HashSet::new(),
            master: 100,
            clock: Instant::now(),
            last_push: Instant::now(),
            dirty: true,
        }
    }

    pub fn show(&mut self, ctx: &egui::Context) {
        self.poll_connection();
        let t = self.clock.elapsed().as_secs_f32();

        egui::SidePanel::left("devices")
            .exact_width(190.0)
            .show(ctx, |ui| self.device_panel(ui));

        egui::CentralPanel::default().show(ctx, |ui| self.editor(ui, t));

        // Drive the hardware, and keep animating if any zone is a function.
        let animating = self.push_if_due(t);
        if animating {
            ctx.request_repaint_after(Duration::from_secs_f32(1.0 / PUSH_HZ));
        }
    }

    fn poll_connection(&mut self) {
        if let Some(rx) = &self.rx {
            if let Ok(res) = rx.try_recv() {
                self.rx = None;
                self.conn = match res {
                    Ok((client, controllers)) => {
                        let zones = controllers
                            .iter()
                            .map(|c| vec![ZoneUi::default(); c.led_count as usize])
                            .collect();
                        Conn::Ready {
                            client,
                            controllers,
                            zones,
                        }
                    }
                    Err(e) => Conn::Failed(e),
                };
                self.dirty = true;
            }
        }
    }

    fn device_panel(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        ui.label(RichText::new("DEVICES").weak().small());
        ui.add_space(4.0);

        match &self.conn {
            Conn::Connecting => {
                ui.spinner();
                ui.label("connecting…");
            }
            Conn::Failed(e) => {
                ui.colored_label(Color32::from_rgb(0xff, 0x6b, 0x6b), "no OpenRGB server");
                ui.label(RichText::new(e.as_str()).weak().small());
                if ui.button("Retry").clicked() {
                    self.rx = Some(spawn_connect());
                    self.conn = Conn::Connecting;
                }
            }
            Conn::Ready {
                controllers, zones, ..
            } => {
                for (i, c) in controllers.iter().enumerate() {
                    let swatch = zones[i]
                        .first()
                        .map(|z| z.color)
                        .unwrap_or([80, 80, 80]);
                    ui.horizontal(|ui| {
                        let (rect, _) =
                            ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                        ui.painter().rect_filled(
                            rect,
                            2.0,
                            Color32::from_rgb(swatch[0], swatch[1], swatch[2]),
                        );
                        if ui
                            .selectable_label(self.selected == i, short_name(&c.name))
                            .clicked()
                        {
                            self.selected = i;
                        }
                    });
                }
            }
        }

        // Master brightness pinned to the bottom.
        egui::TopBottomPanel::bottom("master")
            .frame(egui::Frame::none())
            .show_inside(ui, |ui| {
                ui.separator();
                ui.label(RichText::new("Master brightness").weak().small());
                if ui
                    .add(egui::Slider::new(&mut self.master, 0..=100).suffix("%"))
                    .changed()
                {
                    self.dirty = true;
                }
            });
    }

    fn editor(&mut self, ui: &mut egui::Ui, t: f32) {
        let sel = self.selected;
        let (name, sources, frame) = match &self.conn {
            Conn::Ready {
                controllers, zones, ..
            } if sel < controllers.len() => {
                let srcs: Vec<ZoneSource> = zones[sel].iter().map(|z| z.to_source()).collect();
                let mut f = render_plan(&srcs, &self.spread, t);
                let m = self.master as f32 / 100.0;
                for c in &mut f {
                    *c = scale(*c, m);
                }
                (controllers[sel].name.clone(), srcs, f)
            }
            _ => {
                ui.centered_and_justified(|ui| {
                    ui.label("Connect to an OpenRGB server to edit lighting.");
                });
                return;
            }
        };

        ui.horizontal(|ui| {
            ui.heading(short_name(&name));
            ui.label(RichText::new(format!("· clock {t:6.2}s")).weak().monospace());
        });
        ui.separator();

        let groups = shared_clock_groups(&sources);

        // Per-zone control panels, side by side.
        let n = sources.len();
        egui::ScrollArea::horizontal().show(ui, |ui| {
            ui.horizontal_top(|ui| {
                for zi in 0..n {
                    self.zone_panel(ui, zi, frame.get(zi).copied().unwrap_or(Rgb(0, 0, 0)), &groups);
                }
            });
        });

        // Shared-clock strip: one row per ganged group with a Link/Spread toggle.
        if !groups.is_empty() {
            ui.add_space(10.0);
            ui.separator();
            ui.label(RichText::new("SHARED CLOCKS").weak().small());
            for (effect, idxs) in &groups {
                ui.horizontal(|ui| {
                    let (rect, _) =
                        ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                    ui.painter().circle_filled(rect.center(), 5.0, effect_hue(*effect));
                    ui.label(format!(
                        "{} · zones {}",
                        effect.label(),
                        idxs.iter().map(|i| (i + 1).to_string()).collect::<Vec<_>>().join(",")
                    ));
                    let mut is_spread = self.spread.contains(effect);
                    ui.selectable_value(&mut is_spread, false, "Link");
                    ui.selectable_value(&mut is_spread, true, "Spread");
                    if is_spread {
                        if self.spread.insert(*effect) {
                            self.dirty = true;
                        }
                    } else if self.spread.remove(effect) {
                        self.dirty = true;
                    }
                });
            }
        }
    }

    fn zone_panel(
        &mut self,
        ui: &mut egui::Ui,
        zi: usize,
        preview: Rgb,
        groups: &[(Effect, Vec<usize>)],
    ) {
        // Which shared-clock group (if any) this zone belongs to.
        let group_effect = groups
            .iter()
            .find(|(_, idxs)| idxs.contains(&zi))
            .map(|(e, _)| *e);

        egui::Frame::group(ui.style())
            .fill(ui.visuals().faint_bg_color)
            .show(ui, |ui| {
                ui.set_width(150.0);

                // Preview header: the zone's live color, plus a link badge.
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(150.0, 34.0), egui::Sense::hover());
                ui.painter().rect_filled(
                    rect,
                    4.0,
                    Color32::from_rgb(preview.0, preview.1, preview.2),
                );
                ui.painter().text(
                    rect.left_top() + egui::vec2(6.0, 4.0),
                    egui::Align2::LEFT_TOP,
                    format!("Zone {}", zi + 1),
                    egui::FontId::proportional(12.0),
                    Color32::from_black_alpha(180),
                );
                if let Some(e) = group_effect {
                    // Link badge: a small dot in the effect's hue, top-right.
                    ui.painter()
                        .circle_filled(rect.right_top() + egui::vec2(-9.0, 9.0), 5.0, effect_hue(e));
                }

                let Conn::Ready { zones, .. } = &mut self.conn else {
                    return;
                };
                let z = &mut zones[self.selected][zi];

                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if ui
                        .selectable_label(z.source == Source::Solid, "Solid")
                        .clicked()
                    {
                        z.source = Source::Solid;
                        self.dirty = true;
                    }
                    if ui
                        .selectable_label(z.source == Source::Function, "Function")
                        .clicked()
                    {
                        z.source = Source::Function;
                        self.dirty = true;
                    }
                });

                ui.horizontal(|ui| {
                    ui.label("Color");
                    if ui.color_edit_button_srgb(&mut z.color).changed() {
                        self.dirty = true;
                    }
                });

                if z.source == Source::Function {
                    egui::ComboBox::from_id_source(("effect", self.selected, zi))
                        .selected_text(z.effect.label())
                        .show_ui(ui, |ui| {
                            for e in Effect::ALL {
                                if ui
                                    .selectable_value(&mut z.effect, e, e.label())
                                    .clicked()
                                {
                                    self.dirty = true;
                                }
                            }
                        });
                    if ui
                        .add(egui::Slider::new(&mut z.speed, 1..=9).text("spd"))
                        .changed()
                    {
                        self.dirty = true;
                    }
                }

                if ui
                    .add(egui::Slider::new(&mut z.brightness, 0..=100).suffix("%"))
                    .changed()
                {
                    self.dirty = true;
                }
            });
    }

    /// Returns whether anything is animating (needs continuous repaint).
    fn push_if_due(&mut self, t: f32) -> bool {
        let Conn::Ready {
            client,
            controllers,
            zones,
        } = &mut self.conn
        else {
            return false;
        };

        let animating = zones
            .iter()
            .flatten()
            .any(|z| z.source == Source::Function);

        let due = self.last_push.elapsed().as_secs_f32() >= 1.0 / PUSH_HZ;
        if !(animating && due) && !self.dirty {
            return animating;
        }

        let m = self.master as f32 / 100.0;
        for (ci, ctrl) in controllers.iter().enumerate() {
            let srcs: Vec<ZoneSource> = zones[ci].iter().map(|z| z.to_source()).collect();
            let mut frame = render_plan(&srcs, &self.spread, t);
            for c in &mut frame {
                *c = scale(*c, m);
            }
            let _ = client.set_leds(ctrl, &frame);
        }
        self.last_push = Instant::now();
        self.dirty = false;
        animating
    }
}

/// Connect + enumerate on a worker thread; the window shows "connecting…" until
/// it replies.
fn spawn_connect() -> mpsc::Receiver<ConnResult> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let res = (|| {
            let mut c = OpenRgb::connect().map_err(|e| e.to_string())?;
            let ctrls = c.controllers().map_err(|e| e.to_string())?;
            Ok((c, ctrls))
        })();
        let _ = tx.send(res);
    });
    rx
}

/// Trim OpenRGB's verbose "AcerHIDKeyboard Device" to something readable.
fn short_name(name: &str) -> String {
    name.trim_end_matches(" Device")
        .replace("AcerHID", "")
        .replace("CoverLogoLED", "Cover Logo")
        .replace("ModeKeyLED", "Mode Key")
        .trim()
        .to_string()
}

/// Per-effect hue for the shared-clock badges (matches the mockup tokens).
fn effect_hue(e: Effect) -> Color32 {
    let (r, g, b) = match e {
        Effect::Comet => (0x1f, 0xb7, 0xa6),
        Effect::Fire => (0xff, 0x95, 0x00),
        Effect::Rainbow => (0xd8, 0x4b, 0xff),
        Effect::Breathe => (0x5e, 0x5c, 0xe6),
        Effect::Wave => (0x34, 0xc0, 0xff),
        Effect::Police => (0xff, 0x3b, 0x4e),
        Effect::Gradient => (0x4b, 0xbf, 0x73),
    };
    Color32::from_rgb(r, g, b)
}
