use std::cell::Cell;

use egui::{Color32, ColorImage, Rect, Sense, TextureHandle, TextureOptions, pos2};
use treemap::{MapItem, Mappable, TreemapLayout};

use super::ContextAction;
use crate::format_size;
use crate::model::color::{ColorMap, PALETTE_BRIGHTNESS};
use crate::model::tree::{FileNode, FileTree, NodeRect, TreePath};

/// Cushion surface coefficients: [a_x, a_y, c_x, c_y]
/// z(x,y) = a_x*x^2 + a_y*y^2 + c_x*x + c_y*y
type Surface = [f64; 4];

/// Leaf data collected during layout: (treemap rect, surface coefficients, color)
struct CushionLeaf {
    rect: treemap::Rect,
    surface: Surface,
    color: Color32,
}

const CUSHION_HEIGHT: f64 = 0.38;
const CUSHION_SCALE: f64 = 0.91;

// Lighting parameters (WinDirStat defaults)
const AMBIENT: f64 = 0.13;
const DIFFUSE: f64 = 1.0 - AMBIENT;
const BRIGHTNESS_FACTOR: f64 = 0.88 / PALETTE_BRIGHTNESS;

#[allow(clippy::too_many_arguments)]
pub fn show(
    ui: &mut egui::Ui,
    tree: &mut FileTree,
    view_root: &[usize],
    selected: &mut Option<TreePath>,
    color_map: &ColorMap,
    cached_layout_rect: &mut Option<Rect>,
    cached_view_root: &mut Vec<usize>,
    treemap_texture: &mut Option<TextureHandle>,
) -> Option<(TreePath, ContextAction)> {
    let available = ui.available_size();
    let (response, painter) = ui.allocate_painter(available, Sense::click_and_drag());
    let canvas = response.rect;

    if canvas.width() < 2.0 || canvas.height() < 2.0 {
        return None;
    }

    // Resolve the subtree to display; fall back to the whole tree if the path is
    // stale (e.g. a delete reindexed siblings).
    let vr: Vec<usize> = if tree.root.resolve_path(view_root).is_some() {
        view_root.to_vec()
    } else {
        Vec::new()
    };

    let w = canvas.width();
    let h = canvas.height();

    // Re-layout/re-render when the canvas changed OR we're now showing a
    // different subtree (the cushion texture is keyed on both).
    let rect_changed = match *cached_layout_rect {
        Some(cached) => {
            (canvas.left() - cached.left()).abs() > 1.0
                || (canvas.top() - cached.top()).abs() > 1.0
                || (w - cached.width()).abs() > 1.0
                || (h - cached.height()).abs() > 1.0
        }
        None => true,
    };
    let needs_update = rect_changed || *cached_view_root != vr;

    if needs_update {
        let bounds = treemap::Rect::from_points(
            canvas.left() as f64,
            canvas.top() as f64,
            w as f64,
            h as f64,
        );

        // Navigate to the (validated) view-root node and lay out its subtree.
        let mut node = &mut tree.root;
        for &i in &vr {
            node = node.children.get_mut(i).expect("view_root validated above");
        }
        layout_node(node, bounds);

        let mut leaves = Vec::new();
        let surface = [0.0f64; 4];
        collect_cushion_leaves(node, surface, CUSHION_HEIGHT, true, color_map, &mut leaves);

        let pw = w as usize;
        let ph = h as usize;
        if pw > 0 && ph > 0 {
            let image = render_cushion_image(pw, ph, canvas, &leaves);
            let texture = ui
                .ctx()
                .load_texture("treemap_cushion", image, TextureOptions::NEAREST);
            *treemap_texture = Some(texture);
        }

        *cached_layout_rect = Some(canvas);
        *cached_view_root = vr.clone();
    }

    // Paint the cached texture
    if let Some(tex) = treemap_texture {
        let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
        painter.image(tex.id(), canvas, uv, Color32::WHITE);
    }

    // Immutable handle to the currently displayed subtree.
    let base = tree.root.resolve_path(&vr).unwrap_or(&tree.root);

    // Click a node to select it; click the current view's background to go up.
    if response.clicked()
        && let Some(pos) = response.interact_pointer_pos()
    {
        let mut rel = Vec::new();
        if find_node_at(base, pos, &mut rel) {
            if rel.is_empty() {
                if !vr.is_empty() {
                    let parent = &vr[..vr.len() - 1];
                    *selected = if parent.is_empty() {
                        None
                    } else {
                        Some(parent.to_vec())
                    };
                }
            } else {
                let mut abs = vr.clone();
                abs.extend(rel);
                *selected = Some(abs);
            }
        }
    }

    // Hover tooltip
    if let Some(pos) = response.hover_pos() {
        let mut rel = Vec::new();
        if find_node_at(base, pos, &mut rel)
            && !rel.is_empty()
            && let Some(node) = base.resolve_path(&rel)
        {
            let mut abs = vr.clone();
            abs.extend(rel);
            let full_path = build_path(&tree.root, &abs);
            let tip = format!("{}\n{}", full_path, format_size(node.size));
            egui::show_tooltip_at_pointer(ui.ctx(), ui.layer_id(), response.id.with("tip"), |ui| {
                ui.label(tip);
            });
        }
    }

    // Draw selection highlight (only when the selection is inside this view).
    if let Some(sel_path) = selected.as_ref()
        && sel_path.len() >= vr.len()
        && sel_path[..vr.len()] == vr[..]
        && let Some(node) = base.resolve_path(&sel_path[vr.len()..])
    {
        let r = to_egui_rect(&node.rect);
        if r.width() > 0.0 && r.height() > 0.0 {
            painter.rect_stroke(
                r,
                0.0,
                egui::Stroke::new(2.0, Color32::WHITE),
                egui::StrokeKind::Outside,
            );
        }
    }

    // Context menu on right-click: persist the right-clicked node path across
    // frames via egui memory (the popup stays open across multiple frames).
    let ctx_node_id = response.id.with("ctx_node");
    if response.secondary_clicked()
        && let Some(pos) = response.interact_pointer_pos()
    {
        let mut rel = Vec::new();
        let abs = if find_node_at(base, pos, &mut rel) && !rel.is_empty() {
            let mut a = vr.clone();
            a.extend(rel);
            a
        } else {
            Vec::new()
        };
        ui.ctx()
            .data_mut(|d| d.insert_temp::<Vec<usize>>(ctx_node_id, abs));
    }
    let ctx_node: Option<Vec<usize>> = ui.ctx().data(|d| {
        d.get_temp::<Vec<usize>>(ctx_node_id)
            .filter(|p| !p.is_empty())
    });

    let action_cell: Cell<Option<ContextAction>> = Cell::new(None);
    response.context_menu(|ui| {
        if ctx_node.is_some() {
            if ui.button("Open in Finder").clicked() {
                action_cell.set(Some(ContextAction::OpenInFinder));
                ui.close_menu();
            }
            if ui.button("Reveal in Finder").clicked() {
                action_cell.set(Some(ContextAction::RevealInFinder));
                ui.close_menu();
            }
            if ui.button("Copy Path").clicked() {
                action_cell.set(Some(ContextAction::CopyPath));
                ui.close_menu();
            }
            ui.separator();
            if ui.button("Delete\u{2026}").clicked() {
                action_cell.set(Some(ContextAction::Delete));
                ui.close_menu();
            }
        }
    });

    if let Some(action) = action_cell.into_inner() {
        if let Some(path) = ctx_node {
            return Some((path, action));
        }
    }
    None
}

fn layout_node(node: &mut FileNode, bounds: treemap::Rect) {
    node.rect = NodeRect {
        x: bounds.x as f32,
        y: bounds.y as f32,
        w: bounds.w as f32,
        h: bounds.h as f32,
    };

    if node.children.is_empty() || node.size == 0 {
        return;
    }

    let mut items: Vec<MapItem> = node
        .children
        .iter()
        .map(|c| MapItem::with_size(c.size as f64))
        .collect();

    let layout = TreemapLayout::new();
    layout.layout_items(&mut items, bounds);

    for (child, item) in node.children.iter_mut().zip(items.iter()) {
        let b: treemap::Rect = *item.bounds();
        layout_node(child, b);
    }
}

fn add_ridge(surface: &mut Surface, rect: &treemap::Rect, h: f64) {
    if rect.w < 0.001 || rect.h < 0.001 {
        return;
    }
    let h4 = 4.0 * h;
    let wf = h4 / rect.w;
    surface[0] -= wf; // a_x
    surface[2] += wf * (2.0 * rect.x + rect.w); // c_x
    let hf = h4 / rect.h;
    surface[1] -= hf; // a_y
    surface[3] += hf * (2.0 * rect.y + rect.h); // c_y
}

fn collect_cushion_leaves(
    node: &FileNode,
    mut surface: Surface,
    h: f64,
    is_root: bool,
    color_map: &ColorMap,
    leaves: &mut Vec<CushionLeaf>,
) {
    // Sub-pixel pruning: a non-root node smaller than one pixel paints nothing,
    // and the treemap subdivides children as sub-rectangles of their parent, so
    // every descendant is also sub-pixel. Stop here — render-equivalent, and on
    // multi-million-file trees it bounds `leaves` by visible pixels, not files.
    if !is_root && (node.rect.w < 0.5 || node.rect.h < 0.5) {
        return;
    }

    let rect = treemap::Rect::from_points(
        node.rect.x as f64,
        node.rect.y as f64,
        node.rect.w as f64,
        node.rect.h as f64,
    );

    // Add ridge for this node (skip root per WinDirStat)
    if !is_root {
        add_ridge(&mut surface, &rect, h);
    }

    if node.children.is_empty() {
        // Leaf node
        let color = color_map.get_treemap(node.extension());
        leaves.push(CushionLeaf {
            rect,
            surface,
            color,
        });
    } else {
        let child_h = h * CUSHION_SCALE;
        for child in node.children.iter() {
            collect_cushion_leaves(child, surface, child_h, false, color_map, leaves);
        }
    }
}

fn render_cushion_image(pw: usize, ph: usize, canvas: Rect, leaves: &[CushionLeaf]) -> ColorImage {
    let mut image = ColorImage::new([pw, ph], Color32::BLACK);

    // Precompute normalized light direction
    let (lx, ly, lz) = {
        let len = (1.0f64 + 1.0 + 100.0).sqrt();
        (-1.0 / len, -1.0 / len, 10.0 / len)
    };

    let canvas_left = canvas.left() as f64;
    let canvas_top = canvas.top() as f64;

    for leaf in leaves {
        let r = &leaf.rect;
        if r.w < 0.5 || r.h < 0.5 {
            continue;
        }

        // Convert treemap coords to pixel coords (they're in canvas space).
        // Clamp to 0 before casting to usize to prevent negative-to-unsigned wrapping.
        let left = (r.x - canvas_left).max(0.0) as usize;
        let top = (r.y - canvas_top).max(0.0) as usize;
        let right = ((r.x + r.w - canvas_left).max(0.0) as usize + 1).min(pw);
        let bottom = ((r.y + r.h - canvas_top).max(0.0) as usize + 1).min(ph);

        if left >= right || top >= bottom {
            continue;
        }

        let s = &leaf.surface;
        let col_r = leaf.color.r() as f64;
        let col_g = leaf.color.g() as f64;
        let col_b = leaf.color.b() as f64;

        for iy in top..bottom {
            // The surface coords are in canvas pixel space
            let sy = canvas_top + iy as f64 + 0.5;
            let ny = -(2.0 * s[1] * sy + s[3]);
            let row_offset = iy * pw;

            for ix in left..right {
                let sx = canvas_left + ix as f64 + 0.5;
                let nx = -(2.0 * s[0] * sx + s[2]);

                let cosa = (nx * lx + ny * ly + lz) / (nx * nx + ny * ny + 1.0).sqrt();
                let cosa = cosa.clamp(0.0, 1.0);

                let pixel = (DIFFUSE * cosa + AMBIENT) * BRIGHTNESS_FACTOR;

                let pr = (col_r * pixel).min(255.0) as u8;
                let pg = (col_g * pixel).min(255.0) as u8;
                let pb = (col_b * pixel).min(255.0) as u8;

                if let Some(dest) = image.pixels.get_mut(row_offset + ix) {
                    *dest = Color32::from_rgb(pr, pg, pb);
                }
            }
        }
    }

    image
}

fn find_node_at(node: &FileNode, pos: egui::Pos2, path: &mut Vec<usize>) -> bool {
    let r = to_egui_rect(&node.rect);
    if !r.contains(pos) {
        return false;
    }

    for (i, child) in node.children.iter().enumerate() {
        path.push(i);
        if find_node_at(child, pos, path) {
            return true;
        }
        path.pop();
    }

    true
}

fn build_path(root: &FileNode, path: &[usize]) -> String {
    let mut parts = vec![&*root.name];
    let mut node = root;
    for &idx in path {
        if let Some(child) = node.children.get(idx) {
            parts.push(&child.name);
            node = child;
        }
    }
    parts.join("/")
}

fn to_egui_rect(r: &NodeRect) -> Rect {
    Rect::from_min_max(pos2(r.x, r.y), pos2(r.x + r.w, r.y + r.h))
}
