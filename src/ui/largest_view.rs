//! "Largest folders" view: a flat, ranked list of every directory in the scan,
//! biggest first, as horizontal size bars. Clicking a row selects/navigates to
//! that folder.

use egui::{Align2, Color32, FontId, Rect, Sense, pos2, vec2};

use crate::format_size;
use crate::model::tree::{DirSummary, FileTree, TreePath};

const ROW_HEIGHT: f32 = 40.0;

/// Render the ranked directory list. `dirs` is pre-sorted, largest first.
pub fn show(
    ui: &mut egui::Ui,
    tree: &FileTree,
    dirs: &[DirSummary],
    selected: &mut Option<TreePath>,
) {
    ui.add_space(2.0);
    ui.label(
        egui::RichText::new(format!("Largest folders \u{2014} top {}", dirs.len()))
            .strong()
            .size(15.0),
    );
    ui.add_space(6.0);

    if dirs.is_empty() {
        ui.weak("No subdirectories to rank.");
        return;
    }

    let max = dirs.iter().map(|d| d.size).max().unwrap_or(1).max(1);
    let bar_base = Color32::from_rgb(56, 132, 244);

    egui::ScrollArea::vertical()
        .auto_shrink([false; 2])
        .show(ui, |ui| {
            for (rank, d) in dirs.iter().enumerate() {
                let is_sel = selected.as_deref() == Some(d.path.as_slice());
                let width = ui.available_width();
                let (rect, resp) = ui.allocate_exact_size(vec2(width, ROW_HEIGHT), Sense::click());
                let p = ui.painter();

                // Row background (selection or zebra).
                let bg = if is_sel {
                    ui.visuals().selection.bg_fill
                } else if rank % 2 == 1 {
                    Color32::from_rgba_unmultiplied(255, 255, 255, 6)
                } else {
                    Color32::TRANSPARENT
                };
                if bg != Color32::TRANSPARENT {
                    p.rect_filled(rect, 4.0, bg);
                }

                // Proportional size bar along the bottom of the row.
                let frac = (d.size as f32 / max as f32).clamp(0.0, 1.0);
                let bar_h = 6.0;
                let bar_rect = Rect::from_min_size(
                    pos2(rect.left() + 8.0, rect.bottom() - bar_h - 5.0),
                    vec2((rect.width() - 16.0) * frac, bar_h),
                );
                let bar_col = if is_sel { Color32::WHITE } else { bar_base };
                p.rect_filled(bar_rect, 2.0, bar_col);

                let text_col = if is_sel {
                    Color32::WHITE
                } else {
                    ui.visuals().text_color()
                };

                // Rank + folder name (top-left).
                p.text(
                    pos2(rect.left() + 8.0, rect.top() + 9.0),
                    Align2::LEFT_CENTER,
                    format!("{}.  {}", rank + 1, d.name),
                    FontId::proportional(14.0),
                    text_col,
                );

                // Relative location (small, gray).
                if let Some(fs) = tree.build_fs_path(&d.path) {
                    let rel = fs
                        .strip_prefix(&tree.root_path)
                        .unwrap_or(&fs)
                        .to_string_lossy()
                        .into_owned();
                    p.text(
                        pos2(rect.left() + 8.0, rect.top() + 25.0),
                        Align2::LEFT_CENTER,
                        rel,
                        FontId::proportional(10.0),
                        Color32::from_gray(140),
                    );
                }

                // Size + counts (top-right).
                p.text(
                    pos2(rect.right() - 8.0, rect.top() + 9.0),
                    Align2::RIGHT_CENTER,
                    format_size(d.size),
                    FontId::proportional(13.0),
                    text_col,
                );
                p.text(
                    pos2(rect.right() - 8.0, rect.top() + 25.0),
                    Align2::RIGHT_CENTER,
                    format!("{} files \u{2022} {} dirs", d.file_count, d.dir_count),
                    FontId::proportional(10.0),
                    Color32::from_gray(140),
                );

                if resp.clicked() {
                    *selected = Some(d.path.clone());
                }
            }
        });
}
