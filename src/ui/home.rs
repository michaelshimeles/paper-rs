//! The projects home screen: a recents grid with thumbnails, plus the trash.

use crate::project::{Entry, TrashEntry};
use crate::theme::{self, Icon};
use egui::{Color32, RichText, Ui};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Debug, Clone, PartialEq)]
pub enum HomeAction {
    New,
    Import,
    Open(PathBuf),
    Rename(PathBuf, String),
    Duplicate(PathBuf),
    Trash(PathBuf),
    Restore(PathBuf),
    DeleteForever(PathBuf),
    EmptyTrash,
}

#[derive(Default)]
pub struct HomeView {
    pub entries: Vec<Entry>,
    pub trash: Vec<TrashEntry>,
    pub show_trash: bool,
    pub renaming: Option<(PathBuf, String)>,
    /// Requests for the app, drained after each frame.
    pub actions: Vec<HomeAction>,
    /// "Now" for relative timestamps (unix ms); fixed in screenshots.
    pub now_ms: u64,
    pub root: PathBuf,
    thumbs: HashMap<PathBuf, (Option<SystemTime>, egui::TextureHandle)>,
}

const CARD_W: f32 = 232.0;
const THUMB_H: f32 = 148.0;

impl HomeView {
    /// Re-read the project list and trash from the store (keeps cached thumbnails).
    pub fn refresh(&mut self, store: &crate::project::Store, now_ms: u64) {
        self.root = store.root.clone();
        self.entries = store.list();
        self.trash = store.list_trash();
        self.now_ms = now_ms;
    }

    pub fn load(store: &crate::project::Store, now_ms: u64) -> HomeView {
        let mut hv = HomeView::default();
        hv.refresh(store, now_ms);
        hv
    }

    fn thumb(&mut self, ctx: &egui::Context, path: &Path) -> Option<egui::TextureHandle> {
        let mtime = std::fs::metadata(path).and_then(|m| m.modified()).ok();
        if let Some((t, h)) = self.thumbs.get(path)
            && *t == mtime
        {
            return Some(h.clone());
        }
        let img = image::open(path).ok()?.to_rgba8();
        let size = [img.width() as usize, img.height() as usize];
        let ci = egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw());
        let h = ctx.load_texture(path.to_string_lossy(), ci, egui::TextureOptions::LINEAR);
        self.thumbs.insert(path.to_path_buf(), (mtime, h.clone()));
        Some(h)
    }
}

/// "Edited 5 minutes ago" style relative time.
pub fn relative_time(then_ms: u64, now_ms: u64) -> String {
    let s = now_ms.saturating_sub(then_ms) / 1000;
    match s {
        0..=59 => "just now".into(),
        60..=3599 => plural(s / 60, "minute"),
        3600..=86_399 => plural(s / 3600, "hour"),
        86_400..=2_591_999 => plural(s / 86_400, "day"),
        _ => plural(s / 2_592_000, "month"),
    }
}

fn plural(n: u64, unit: &str) -> String {
    if n == 1 {
        format!("1 {unit} ago")
    } else {
        format!("{n} {unit}s ago")
    }
}

pub fn draw(
    ui: &mut Ui,
    hv: &mut HomeView,
    native_chrome: bool,
    translucent: bool,
    options: &mut crate::options::Options,
    options_dirty: &mut bool,
) {
    let p = theme::pal(ui);
    egui::Panel::left("home-sidebar")
        .resizable(false)
        .exact_size(200.0)
        .frame(
            egui::Frame::NONE
                .fill(if translucent {
                    Color32::TRANSPARENT
                } else {
                    p.sidebar
                })
                .inner_margin(egui::Margin::symmetric(10, 8)),
        )
        .show(ui, |ui| {
            if native_chrome {
                ui.add_space(theme::TOOLBAR_H - 8.0);
            }
            ui.add_space(4.0);
            ui.label(
                RichText::new("Sketch")
                    .font(theme::semibold(theme::SECONDARY))
                    .color(p.secondary),
            );
            ui.add_space(4.0);
            if sidebar_item(ui, Icon::Grid, "Recents", None, !hv.show_trash) {
                hv.show_trash = false;
            }
            let n = hv.trash.len();
            if sidebar_item(
                ui,
                Icon::Trash,
                "Trash",
                (n > 0).then(|| n.to_string()),
                hv.show_trash,
            ) {
                hv.show_trash = true;
            }
            ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                ui.add_space(4.0);
                ui.label(
                    RichText::new(hv.root.display().to_string())
                        .size(theme::SECONDARY - 1.0)
                        .color(p.secondary),
                )
                .on_hover_text("Projects folder");
            });
        });
    egui::Panel::top("home-toolbar")
        .exact_size(theme::TOOLBAR_H + 8.0)
        .frame(
            egui::Frame::NONE
                .fill(p.window)
                .inner_margin(egui::Margin::symmetric(20, 0)),
        )
        .show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                ui.label(
                    RichText::new(if hv.show_trash { "Trash" } else { "Recents" })
                        .font(theme::semibold(17.0)),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    super::theme_button(ui, options, options_dirty);
                    ui.add_space(6.0);
                    if hv.show_trash {
                        if ui
                            .add_enabled(!hv.trash.is_empty(), egui::Button::new("Empty Trash"))
                            .clicked()
                        {
                            hv.actions.push(HomeAction::EmptyTrash);
                        }
                    } else {
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new("New Project").color(Color32::WHITE),
                                )
                                .fill(theme::accent()),
                            )
                            .clicked()
                        {
                            hv.actions.push(HomeAction::New);
                        }
                        if ui.button("Import…").clicked() {
                            hv.actions.push(HomeAction::Import);
                        }
                    }
                });
            });
            let r = ui.max_rect();
            ui.painter().hline(
                r.x_range(),
                r.bottom() - 0.5,
                egui::Stroke::new(1.0, p.separator),
            );
        });
    egui::CentralPanel::default()
        .frame(egui::Frame::NONE.fill(p.window).inner_margin(20))
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink(false)
                .show(ui, |ui| {
                    if hv.show_trash {
                        trash_grid(ui, hv);
                    } else {
                        recents_grid(ui, hv);
                    }
                });
        });
}

fn sidebar_item(
    ui: &mut Ui,
    icon: Icon,
    label: &str,
    badge: Option<String>,
    selected: bool,
) -> bool {
    let p = theme::pal(ui);
    let (row, resp) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 26.0), egui::Sense::click());
    if selected {
        ui.painter().rect_filled(row, 6.0, p.pressed);
    } else if resp.hovered() {
        ui.painter().rect_filled(row, 6.0, p.hover);
    }
    let ir = egui::Rect::from_center_size(
        egui::pos2(row.left() + 16.0, row.center().y),
        egui::vec2(15.0, 15.0),
    );
    theme::paint_icon(ui.painter(), ir, icon, theme::accent());
    ui.painter().text(
        egui::pos2(row.left() + 32.0, row.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(theme::BODY),
        p.text,
    );
    if let Some(b) = badge {
        ui.painter().text(
            egui::pos2(row.right() - 8.0, row.center().y),
            egui::Align2::RIGHT_CENTER,
            b,
            egui::FontId::proportional(theme::SECONDARY),
            p.secondary,
        );
    }
    resp.clicked()
}

fn columns(ui: &Ui) -> usize {
    ((ui.available_width() + 20.0) / (CARD_W + 20.0))
        .floor()
        .max(1.0) as usize
}

fn recents_grid(ui: &mut Ui, hv: &mut HomeView) {
    let p = theme::pal(ui);
    if hv.entries.is_empty() {
        ui.add_space(80.0);
        ui.vertical_centered(|ui| {
            ui.label(RichText::new("No projects yet").font(theme::semibold(17.0)));
            ui.add_space(4.0);
            ui.label(
                RichText::new("Projects are folders in your Documents, saved as you work.")
                    .color(p.secondary),
            );
            ui.add_space(12.0);
            if ui
                .add(
                    egui::Button::new(RichText::new("New Project").color(Color32::WHITE))
                        .fill(theme::accent()),
                )
                .clicked()
            {
                hv.actions.push(HomeAction::New);
            }
        });
        return;
    }
    let cols = columns(ui);
    let entries = hv.entries.clone();
    for row in entries.chunks(cols) {
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 20.0;
            for e in row {
                project_card(ui, hv, e);
            }
        });
        ui.add_space(16.0);
    }
}

fn card_frame(
    ui: &mut Ui,
    hv: &mut HomeView,
    thumb: Option<&Path>,
    id: egui::Id,
) -> egui::Response {
    let p = theme::pal(ui);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(CARD_W, THUMB_H), egui::Sense::click());
    let hovered = resp.hovered();
    let painter = ui.painter();
    painter.rect_filled(
        rect,
        8.0,
        if ui.visuals().dark_mode {
            Color32::from_rgb(0x1f, 0x1f, 0x21)
        } else {
            Color32::from_rgb(0xee, 0xee, 0xf0)
        },
    );
    if let Some(t) = thumb.and_then(|t| hv.thumb(ui.ctx(), t)) {
        // Fit the thumbnail inside the card, centered.
        let [tw, th] = t.size();
        let s = ((rect.width() - 16.0) / tw as f32)
            .min((rect.height() - 16.0) / th as f32)
            .min(1.0);
        let r =
            egui::Rect::from_center_size(rect.center(), egui::vec2(tw as f32 * s, th as f32 * s));
        ui.painter().image(
            t.id(),
            r,
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            Color32::WHITE,
        );
    } else {
        theme::paint_icon(
            ui.painter(),
            egui::Rect::from_center_size(rect.center(), egui::vec2(28.0, 28.0)),
            Icon::Page,
            p.tertiary,
        );
    }
    ui.painter().rect_stroke(
        rect,
        8.0,
        egui::Stroke::new(
            if hovered { 2.0 } else { 1.0 },
            if hovered {
                theme::accent()
            } else {
                p.separator
            },
        ),
        egui::StrokeKind::Inside,
    );
    let _ = id;
    resp
}

fn project_card(ui: &mut Ui, hv: &mut HomeView, e: &Entry) {
    let p = theme::pal(ui);
    ui.vertical(|ui| {
        ui.set_width(CARD_W);
        ui.spacing_mut().item_spacing.y = 2.0;
        let id = egui::Id::new(("card", &e.dir));
        let resp = card_frame(ui, hv, e.thumbnail.as_deref(), id);
        if resp.double_clicked() {
            hv.actions.push(HomeAction::Open(e.dir.clone()));
        }
        resp.context_menu(|ui| card_menu(ui, hv, e));
        ui.add_space(6.0);
        let renaming = hv.renaming.as_ref().is_some_and(|(d, _)| *d == e.dir);
        if renaming {
            let (_, name) = hv.renaming.as_mut().unwrap();
            let r = ui.add(egui::TextEdit::singleline(name).desired_width(CARD_W));
            r.request_focus();
            if r.lost_focus() {
                let (dir, name) = hv.renaming.take().unwrap();
                if !ui.input(|i| i.key_pressed(egui::Key::Escape)) && name.trim() != e.manifest.name
                {
                    hv.actions.push(HomeAction::Rename(dir, name));
                }
            }
        } else {
            ui.horizontal(|ui| {
                let name = ui.add(
                    egui::Label::new(
                        RichText::new(&e.manifest.name).font(theme::semibold(theme::BODY)),
                    )
                    .truncate()
                    .sense(egui::Sense::click()),
                );
                if name.double_clicked() {
                    hv.renaming = Some((e.dir.clone(), e.manifest.name.clone()));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let more =
                        theme::icon_button(ui, Icon::More, 20.0, false).on_hover_text("More");
                    egui::Popup::menu(&more).show(|ui| card_menu(ui, hv, e));
                });
            });
        }
        let pages = e.manifest.pages.len();
        ui.label(
            RichText::new(format!(
                "Edited {} · {} page{}",
                relative_time(e.manifest.modified, hv.now_ms),
                pages,
                if pages == 1 { "" } else { "s" }
            ))
            .size(theme::SECONDARY)
            .color(p.secondary),
        );
    });
}

fn card_menu(ui: &mut Ui, hv: &mut HomeView, e: &Entry) {
    if ui.button("Open").clicked() {
        hv.actions.push(HomeAction::Open(e.dir.clone()));
        ui.close();
    }
    if ui.button("Rename").clicked() {
        hv.renaming = Some((e.dir.clone(), e.manifest.name.clone()));
        ui.close();
    }
    if ui.button("Duplicate").clicked() {
        hv.actions.push(HomeAction::Duplicate(e.dir.clone()));
        ui.close();
    }
    ui.separator();
    if ui.button("Move to Trash").clicked() {
        hv.actions.push(HomeAction::Trash(e.dir.clone()));
        ui.close();
    }
}

fn trash_grid(ui: &mut Ui, hv: &mut HomeView) {
    let p = theme::pal(ui);
    if hv.trash.is_empty() {
        ui.add_space(80.0);
        ui.vertical_centered(|ui| {
            ui.label(RichText::new("Trash is empty").font(theme::semibold(17.0)));
            ui.label(
                RichText::new("Deleted projects stay here until you empty the trash.")
                    .color(p.secondary),
            );
        });
        return;
    }
    let cols = columns(ui);
    let entries = hv.trash.clone();
    for row in entries.chunks(cols) {
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 20.0;
            for e in row {
                ui.vertical(|ui| {
                    ui.set_width(CARD_W);
                    let thumb = e.dir.join(crate::project::THUMBNAIL);
                    let thumb = thumb.is_file().then_some(thumb);
                    let resp = card_frame(ui, hv, thumb.as_deref(), egui::Id::new(&e.dir));
                    resp.context_menu(|ui| {
                        if ui.button("Restore").clicked() {
                            hv.actions.push(HomeAction::Restore(e.dir.clone()));
                            ui.close();
                        }
                        if ui.button("Delete Permanently").clicked() {
                            hv.actions.push(HomeAction::DeleteForever(e.dir.clone()));
                            ui.close();
                        }
                    });
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::Label::new(
                                RichText::new(&e.manifest.name).font(theme::semibold(theme::BODY)),
                            )
                            .truncate(),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if theme::icon_button(ui, Icon::Restore, 20.0, false)
                                .on_hover_text("Restore")
                                .clicked()
                            {
                                hv.actions.push(HomeAction::Restore(e.dir.clone()));
                            }
                        });
                    });
                    ui.label(
                        RichText::new(format!("Deleted {}", relative_time(e.deleted, hv.now_ms)))
                            .size(theme::SECONDARY)
                            .color(p.secondary),
                    );
                });
            }
        });
        ui.add_space(16.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_times() {
        let now = 10_000_000_000;
        assert_eq!(relative_time(now - 5_000, now), "just now");
        assert_eq!(relative_time(now - 60_000, now), "1 minute ago");
        assert_eq!(relative_time(now - 3 * 3_600_000, now), "3 hours ago");
        assert_eq!(relative_time(now - 2 * 86_400_000, now), "2 days ago");
        assert_eq!(relative_time(now + 1, now), "just now");
    }
}
