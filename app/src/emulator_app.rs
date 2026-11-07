//! The eframe application: drives the emulator in real time, draws its
//! screen, and hosts the settings menu.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align2, Color32, ColorImage, Event, Pos2, Rect, TextureHandle, TextureId, TextureOptions,
    ViewportCommand,
};
use gb_core::{Button, CPU_HZ, CYCLES_PER_FRAME, GameBoy, SCREEN_HEIGHT, SCREEN_WIDTH};

use crate::audio::Audio;
use crate::saves::{SaveFile, StateFile};
use crate::settings::{
    self, KeyBindings, LOAD_STATE_KEY, MAX_SCALE, MENU_KEY, MIN_SCALE, RESERVED_KEYS,
    SAVE_STATE_KEY, Settings,
};

/// Shades 0 (lightest) to 3 (darkest) as RGB.
const PALETTE: [[u8; 3]; 4] = [
    [0xE8, 0xE8, 0xE8],
    [0xA0, 0xA0, 0xA0],
    [0x58, 0x58, 0x58],
    [0x10, 0x10, 0x10],
];

/// Never run more than this many frames in one update, so a stall
/// (window drag, suspend) does not fast-forward the game.
const MAX_CATCH_UP_FRAMES: u32 = 3;

/// How long status messages ("State saved") stay on screen.
const STATUS_DURATION: Duration = Duration::from_millis(2500);

/// Window inner size for an integer scale.
pub fn window_size(scale: u8) -> egui::Vec2 {
    egui::vec2(
        SCREEN_WIDTH as f32 * scale as f32,
        SCREEN_HEIGHT as f32 * scale as f32,
    )
}

pub struct EmulatorApp {
    gb: GameBoy,
    save: Option<SaveFile>,
    state: StateFile,
    /// Short message shown over the game, with the time it appeared.
    status: Option<(String, Instant)>,
    /// None when no audio device could be opened; the game runs silently.
    audio: Option<Audio>,
    audio_buffer: Vec<f32>,
    settings: Settings,
    /// None if the OS has no config directory; settings then last one session.
    settings_path: Option<PathBuf>,
    settings_dirty: bool,
    menu_open: bool,
    /// Button waiting for its new key in the menu.
    rebinding: Option<Button>,
    texture: Option<TextureHandle>,
    frame_duration: Duration,
    last_update: Instant,
    /// Real time not yet consumed by emulated frames.
    pending: Duration,
}

impl EmulatorApp {
    pub fn new(
        mut gb: GameBoy,
        save: Option<SaveFile>,
        state: StateFile,
        settings: Settings,
        settings_path: Option<PathBuf>,
    ) -> Self {
        let audio = match Audio::start(settings.volume, settings.muted) {
            Ok(audio) => Some(audio),
            Err(err) => {
                eprintln!("warning: no sound ({err}); continuing silently");
                None
            }
        };
        if let Some(audio) = &audio {
            gb.set_audio_sample_rate(audio.push(&[]));
        }
        Self {
            gb,
            save,
            state,
            status: None,
            audio,
            audio_buffer: Vec::new(),
            settings,
            settings_path,
            settings_dirty: false,
            menu_open: false,
            rebinding: None,
            texture: None,
            frame_duration: Duration::from_secs_f64(CYCLES_PER_FRAME as f64 / CPU_HZ as f64),
            last_update: Instant::now(),
            pending: Duration::ZERO,
        }
    }

    fn screen_image(&self) -> ColorImage {
        let rgb: Vec<u8> = self
            .gb
            .framebuffer()
            .iter()
            .flat_map(|&shade| PALETTE[shade as usize & 3])
            .collect();
        ColorImage::from_rgb([SCREEN_WIDTH, SCREEN_HEIGHT], &rgb)
    }

    fn show_status(&mut self, text: String) {
        self.status = Some((text, Instant::now()));
    }

    fn save_state(&mut self) {
        let text = match self.state.save(&self.gb) {
            Ok(()) => "State saved".to_string(),
            Err(err) => format!("Save state failed: {err}"),
        };
        self.show_status(text);
    }

    fn load_state(&mut self) {
        let text = match self.state.load(&mut self.gb) {
            Ok(()) => {
                self.last_update = Instant::now();
                self.pending = Duration::ZERO;
                "State loaded".to_string()
            }
            Err(err) => format!("Load state failed: {err}"),
        };
        self.show_status(text);
    }

    fn save_settings(&mut self) {
        if !self.settings_dirty {
            return;
        }
        if let Some(path) = &self.settings_path {
            match self.settings.save(path) {
                Ok(()) => self.settings_dirty = false,
                Err(err) => eprintln!("saving settings to {} failed: {err}", path.display()),
            }
        }
    }

    fn set_menu_open(&mut self, open: bool) {
        self.menu_open = open;
        self.rebinding = None;
        if open {
            for button in Button::ALL {
                self.gb.set_button(button, false);
            }
        } else {
            self.save_settings();
            // Resume from now instead of catching up on the paused time.
            self.last_update = Instant::now();
            self.pending = Duration::ZERO;
        }
    }

    fn handle_input(&mut self, ctx: &egui::Context) {
        let (menu_toggled, save_state, load_state, captured) = ctx.input(|input| {
            if self.rebinding.is_some() {
                let key = input.events.iter().find_map(|event| match event {
                    Event::Key {
                        key,
                        pressed: true,
                        repeat: false,
                        ..
                    } => Some(*key),
                    _ => None,
                });
                (false, false, false, key)
            } else {
                (
                    input.key_pressed(MENU_KEY),
                    input.key_pressed(SAVE_STATE_KEY),
                    input.key_pressed(LOAD_STATE_KEY),
                    None,
                )
            }
        });

        if let (Some(button), Some(key)) = (self.rebinding, captured) {
            // Reserved keys (Esc, F5, F9) cancel instead of binding.
            if !RESERVED_KEYS.contains(&key) {
                self.settings.keys.set(button, key);
                self.settings_dirty = true;
            }
            self.rebinding = None;
        }
        if menu_toggled {
            self.set_menu_open(!self.menu_open);
        }
        if save_state {
            self.save_state();
        }
        if load_state {
            self.load_state();
        }
        if !self.menu_open {
            ctx.input(|input| {
                for button in Button::ALL {
                    let down = input.key_down(self.settings.keys.key(button));
                    self.gb.set_button(button, down);
                }
            });
        }
    }

    fn run_emulation(&mut self) {
        let now = Instant::now();
        self.pending += now - self.last_update;
        self.last_update = now;

        let mut frames = 0;
        while self.pending >= self.frame_duration && frames < MAX_CATCH_UP_FRAMES {
            self.gb.run_frame();
            self.pending -= self.frame_duration;
            frames += 1;
        }
        if frames == MAX_CATCH_UP_FRAMES {
            self.pending = Duration::ZERO;
        }
        if let Some(audio) = &self.audio {
            self.audio_buffer.clear();
            self.gb.drain_audio(&mut self.audio_buffer);
            let rate = audio.push(&self.audio_buffer);
            self.gb.set_audio_sample_rate(rate);
        }
    }

    fn draw_screen(&mut self, ui: &mut egui::Ui) -> TextureId {
        let image = self.screen_image();
        let texture = match &mut self.texture {
            Some(texture) => {
                texture.set(image, TextureOptions::NEAREST);
                texture
            }
            None => self.texture.insert(ui.ctx().load_texture(
                "screen",
                image,
                TextureOptions::NEAREST,
            )),
        };

        let area = ui.max_rect();
        ui.painter().rect_filled(area, 0.0, Color32::BLACK);
        // Largest integer scale that fits, centred.
        let scale = (area.width() / SCREEN_WIDTH as f32)
            .min(area.height() / SCREEN_HEIGHT as f32)
            .floor()
            .max(1.0);
        let size = egui::vec2(SCREEN_WIDTH as f32 * scale, SCREEN_HEIGHT as f32 * scale);
        let rect = Rect::from_center_size(area.center(), size);
        let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
        ui.painter().image(texture.id(), rect, uv, Color32::WHITE);
        texture.id()
    }

    fn draw_menu(&mut self, ui: &mut egui::Ui) {
        let area = ui.max_rect();
        ui.painter()
            .rect_filled(area, 0.0, Color32::from_black_alpha(170));

        let ctx = ui.ctx().clone();
        let mut close = false;
        egui::Window::new("Settings")
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .collapsible(false)
            .resizable(false)
            .show(&ctx, |ui| {
                egui::ScrollArea::vertical()
                    .max_height(area.height() - 60.0)
                    .show(ui, |ui| {
                        self.sound_section(ui);
                        ui.separator();
                        self.display_section(ui);
                        ui.separator();
                        self.controls_section(ui);
                        ui.separator();
                        ui.strong("Save state");
                        ui.horizontal(|ui| {
                            if ui.button("Save (F5)").clicked() {
                                self.save_state();
                            }
                            if ui.button("Load (F9)").clicked() {
                                self.load_state();
                            }
                        });
                        ui.separator();
                        if ui.button("Resume (Esc)").clicked() {
                            close = true;
                        }
                    });
            });
        if close {
            self.set_menu_open(false);
        }
    }

    fn draw_status(&mut self, ui: &mut egui::Ui) {
        let Some((text, shown_at)) = &self.status else {
            return;
        };
        if shown_at.elapsed() > STATUS_DURATION {
            self.status = None;
            return;
        }
        let painter = ui.painter();
        let font = egui::FontId::proportional(14.0);
        let galley = painter.layout_no_wrap(text.clone(), font, Color32::WHITE);
        let pos = ui.max_rect().left_bottom() + egui::vec2(8.0, -8.0 - galley.size().y);
        let background = Rect::from_min_size(pos, galley.size()).expand(4.0);
        painter.rect_filled(background, 3.0, Color32::from_black_alpha(190));
        painter.galley(pos, galley, Color32::WHITE);
    }

    fn sound_section(&mut self, ui: &mut egui::Ui) {
        ui.strong("Sound");
        let Some(audio) = &self.audio else {
            ui.label("No audio device: sound unavailable.");
            return;
        };
        let volume = ui.add(
            egui::Slider::new(&mut self.settings.volume, 0..=100)
                .text("Volume")
                .suffix("%"),
        );
        if volume.changed() {
            audio.set_volume(self.settings.volume);
            self.settings_dirty = true;
        }
        if ui.checkbox(&mut self.settings.muted, "Mute").changed() {
            audio.set_muted(self.settings.muted);
            self.settings_dirty = true;
        }
    }

    fn display_section(&mut self, ui: &mut egui::Ui) {
        ui.strong("Display");
        let scale = ui.add(
            egui::Slider::new(&mut self.settings.scale, MIN_SCALE..=MAX_SCALE)
                .text("Scale")
                .suffix("x"),
        );
        if scale.changed() {
            ui.ctx()
                .send_viewport_cmd(ViewportCommand::InnerSize(window_size(self.settings.scale)));
            self.settings_dirty = true;
        }
    }

    fn controls_section(&mut self, ui: &mut egui::Ui) {
        ui.strong("Controls");
        ui.label("Click a button, then press the new key. Esc cancels.");
        ui.label("Esc, F5 and F9 are reserved.");
        egui::Grid::new("keybinds").num_columns(2).show(ui, |ui| {
            for button in Button::ALL {
                ui.label(settings::button_label(button));
                let text = if self.rebinding == Some(button) {
                    "Press a key...".to_string()
                } else {
                    self.settings.keys.key(button).name().to_string()
                };
                if ui.button(text).clicked() {
                    self.rebinding = Some(button);
                }
                ui.end_row();
            }
        });
        if ui.button("Reset controls to defaults").clicked() {
            self.settings.keys = KeyBindings::default();
            self.rebinding = None;
            self.settings_dirty = true;
        }
    }
}

impl eframe::App for EmulatorApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_input(ctx);
        if !self.menu_open {
            self.run_emulation();
        }
        if let Some(save) = &mut self.save
            && let Err(err) = save.autosave(&mut self.gb)
        {
            eprintln!("autosave to {} failed: {err}", save.path().display());
        }
        ctx.request_repaint();
    }

    fn on_exit(&mut self) {
        self.save_settings();
        if let Some(save) = &mut self.save
            && let Err(err) = save.write(&self.gb)
        {
            eprintln!("saving {} failed: {err}", save.path().display());
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.draw_screen(ui);
        if self.menu_open {
            self.draw_menu(ui);
        }
        self.draw_status(ui);
    }
}
