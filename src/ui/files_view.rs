//! "All Files" view: a flat, searchable list of every individual file in the
//! scan (not per-folder rollups), sorted largest-first. Since this can be
//! millions of rows, the list is rendered with `ScrollArea::show_rows` so only
//! the currently visible rows are ever laid out/painted, regardless of total
//! count — a plain per-row loop like `largest_view` uses would be far too slow
//! here.

use egui::{Align2, Color32, FontId, Sense, pos2, vec2};

use crate::app::format_file_count;
use crate::format_size;
use crate::model::tree::{FileSummary, FileTree, TreePath};

const ROW_HEIGHT: f32 = 34.0;

/// Render the flat file list. `files` is the full scan's files (pre-sorted,
/// largest first, from `FileTree::all_files`). `search` and `filter_cache` are
/// owned by the caller (`LoadedState`) so the filtered index list persists
/// across frames instead of being recomputed on every repaint.
pub fn show(
    ui: &mut egui::Ui,
    tree: &FileTree,
    files: &[FileSummary],
    search: &mut String,
    filter_cache: &mut Option<(String, Vec<usize>)>,
    selected: &mut Option<TreePath>,
    activated: &mut Option<TreePath>,
) {
    ui.add_space(2.0);
    ui.label(
        egui::RichText::new(format!("All Files \u{2014} {}", format_file_count(files.len() as u64)))
            .strong()
            .size(15.0),
    );
    ui.add_space(4.0);
    ui.add(
        egui::TextEdit::singleline(search)
            .hint_text("Search by name")
            .desired_width(f32::INFINITY),
    );
    ui.add_space(6.0);

    if files.is_empty() {
        ui.weak("No files found.");
        return;
    }

    // Recompute the filtered index list only when the search string changes.
    let q = search.trim().to_lowercase();
    let cache_hit = filter_cache.as_ref().is_some_and(|(cq, _)| *cq == q);
    if !cache_hit {
        let indices: Vec<usize> = if q.is_empty() {
            (0..files.len()).collect()
        } else {
            files
                .iter()
                .enumerate()
                .filter(|(_, f)| f.name.to_lowercase().contains(&q))
                .map(|(i, _)| i)
                .collect()
        };
        *filter_cache = Some((q, indices));
    }
    let indices = &filter_cache.as_ref().unwrap().1;

    if indices.is_empty() {
        ui.weak("No files match your search.");
        return;
    }
    if !search.trim().is_empty() {
        ui.label(
            egui::RichText::new(format!("{} matching", format_file_count(indices.len() as u64)))
                .small()
                .color(Color32::GRAY),
        );
        ui.add_space(4.0);
    }

    egui::ScrollArea::vertical()
        .auto_shrink([false; 2])
        .show_rows(ui, ROW_HEIGHT, indices.len(), |ui, row_range| {
            for row in row_range {
                let f = &files[indices[row]];
                let is_sel = selected.as_deref() == Some(f.path.as_slice());
                let width = ui.available_width();
                let (rect, resp) = ui.allocate_exact_size(vec2(width, ROW_HEIGHT), Sense::click());
                let p = ui.painter();

                let bg = if is_sel {
                    ui.visuals().selection.bg_fill
                } else if row % 2 == 1 {
                    Color32::from_rgba_unmultiplied(255, 255, 255, 6)
                } else {
                    Color32::TRANSPARENT
                };
                if bg != Color32::TRANSPARENT {
                    p.rect_filled(rect, 4.0, bg);
                }

                let text_col = if is_sel {
                    Color32::WHITE
                } else {
                    ui.visuals().text_color()
                };

                p.text(
                    pos2(rect.left() + 8.0, rect.top() + 9.0),
                    Align2::LEFT_CENTER,
                    &*f.name,
                    FontId::proportional(13.0),
                    text_col,
                );
                if let Some(fs) = tree.build_fs_path(&f.path) {
                    let rel = fs
                        .parent()
                        .unwrap_or(&fs)
                        .strip_prefix(&tree.root_path)
                        .unwrap_or(&fs)
                        .to_string_lossy()
                        .into_owned();
                    p.text(
                        pos2(rect.left() + 8.0, rect.top() + 23.0),
                        Align2::LEFT_CENTER,
                        rel,
                        FontId::proportional(10.0),
                        Color32::from_gray(140),
                    );
                }

                p.text(
                    pos2(rect.right() - 8.0, rect.center().y),
                    Align2::RIGHT_CENTER,
                    format_size(f.size),
                    FontId::proportional(13.0),
                    text_col,
                );

                if resp.clicked() {
                    *selected = Some(f.path.clone());
                }
                if resp.double_clicked() {
                    *activated = Some(f.path.clone());
                }
            }
        });
}
