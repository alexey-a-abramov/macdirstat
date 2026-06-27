//! Sunburst ("round diagram") view: concentric rings where the center is the
//! current folder and each ring out is one level deeper. A segment's angular
//! span is proportional to its size within its parent. Hover shows a tooltip;
//! clicking a segment navigates into it, clicking the center navigates up.

use std::f32::consts::TAU;

use egui::{Align2, Color32, FontId, Mesh, Pos2, Sense, Shape, pos2};

use crate::format_size;
use crate::model::color::ColorMap;
use crate::model::tree::{FileNode, FileTree, TreePath};

const MAX_DEPTH: usize = 6;
/// Segments thinner than this (radians) are dropped — like the treemap's
/// sub-pixel pruning, this bounds segment count on huge trees.
const MIN_ANGLE: f32 = 0.012;

struct Segment {
    path: TreePath,
    depth: usize,
    a0: f32,
    a1: f32,
    color: Color32,
}

/// Render the sunburst rooted at `view_root`. Mutates `selected` on click
/// (the app derives the displayed root from the selection).
pub fn show(
    ui: &mut egui::Ui,
    tree: &FileTree,
    view_root: &[usize],
    selected: &mut Option<TreePath>,
    activated: &mut Option<TreePath>,
    color_map: &ColorMap,
) {
    let available = ui.available_size();
    let (response, painter) = ui.allocate_painter(available, Sense::click());
    let canvas = response.rect;
    if canvas.width() < 16.0 || canvas.height() < 16.0 {
        return;
    }

    let center = canvas.center();
    let max_radius = (canvas.width().min(canvas.height()) * 0.5) - 12.0;
    if max_radius < 20.0 {
        return;
    }
    let r0 = (max_radius * 0.20).max(28.0); // center disc
    let ring_w = (max_radius - r0) / MAX_DEPTH as f32;

    let base = tree.root.resolve_path(view_root).unwrap_or(&tree.root);

    // Lay out segments for the visible rings.
    let mut segs: Vec<Segment> = Vec::new();
    let mut abs: Vec<usize> = view_root.to_vec();
    layout(base, &mut abs, 0.0, TAU, 0, color_map, &mut segs);

    // Hit-test the pointer against the rings.
    let hovered = response
        .hover_pos()
        .and_then(|p| hit_test(p, center, r0, ring_w, &segs));

    // Draw segments (deepest last so they sit on top visually is irrelevant —
    // rings don't overlap, but draw shallow→deep for stable ordering).
    let mut order: Vec<usize> = (0..segs.len()).collect();
    order.sort_by_key(|&i| segs[i].depth);
    for &i in &order {
        let s = &segs[i];
        let r_in = r0 + (s.depth as f32 - 1.0) * ring_w;
        let r_out = r_in + ring_w;
        let mut col = s.color;
        if Some(i) == hovered {
            col = lighten(col, 0.35);
        } else if selected.as_deref() == Some(s.path.as_slice()) {
            col = lighten(col, 0.2);
        }
        fill_sector(&painter, center, r_in, r_out, s.a0, s.a1, col);
        // Thin separator stroke between segments at the same level.
        painter.add(Shape::line(
            vec![polar(center, r_in, s.a0), polar(center, r_out, s.a0)],
            egui::Stroke::new(0.5, Color32::from_black_alpha(60)),
        ));
    }

    // Center disc.
    painter.circle_filled(center, r0, Color32::from_rgb(48, 52, 60));
    let base_name = if view_root.is_empty() {
        tree.root.name.rsplit('/').next().unwrap_or(&tree.root.name)
    } else {
        &base.name
    };
    painter.text(
        pos2(center.x, center.y - 6.0),
        Align2::CENTER_CENTER,
        truncate(base_name, 18),
        FontId::proportional(13.0),
        Color32::from_gray(230),
    );
    painter.text(
        pos2(center.x, center.y + 11.0),
        Align2::CENTER_CENTER,
        format_size(base.size),
        FontId::proportional(11.0),
        Color32::from_gray(170),
    );

    // Hover tooltip.
    if let Some(i) = hovered {
        let s = &segs[i];
        if let Some(node) = tree.root.resolve_path(&s.path) {
            let items = node.children.len();
            egui::show_tooltip_at_pointer(
                ui.ctx(),
                ui.layer_id(),
                response.id.with("sun_tip"),
                |ui| {
                    ui.strong(&*node.name);
                    ui.label(format!(
                        "{} \u{2022} {} items",
                        format_size(node.size),
                        items
                    ));
                },
            );
        }
    }

    // Single click selects a segment; double click navigates (a segment → into
    // it; the center → up one level).
    if let Some(pos) = response.interact_pointer_pos() {
        let on_center = center.distance(pos) <= r0;
        let seg = if on_center {
            None
        } else {
            hit_test(pos, center, r0, ring_w, &segs)
        };
        if response.clicked()
            && let Some(i) = seg
        {
            *selected = Some(segs[i].path.clone());
        }
        if response.double_clicked() {
            if let Some(i) = seg {
                *activated = Some(segs[i].path.clone());
            } else if on_center && !view_root.is_empty() {
                *activated = Some(view_root[..view_root.len() - 1].to_vec());
            }
        }
    }
}

/// Recursively assign angular spans to descendants of `node`.
/// `abs` holds the absolute index path to `node`; depth 0 is the center.
fn layout(
    node: &FileNode,
    abs: &mut Vec<usize>,
    a0: f32,
    a1: f32,
    depth: usize,
    color_map: &ColorMap,
    out: &mut Vec<Segment>,
) {
    if depth > 0 {
        out.push(Segment {
            path: abs.clone(),
            depth,
            a0,
            a1,
            color: seg_color(node, depth, color_map),
        });
    }
    if depth >= MAX_DEPTH || node.children.is_empty() || node.size == 0 {
        return;
    }
    let span = a1 - a0;
    let total = node.size.max(1) as f32;
    let mut acc = a0;
    for (i, child) in node.children.iter().enumerate() {
        let frac = (child.size as f32 / total).min(1.0);
        let ca1 = (acc + span * frac).min(a1);
        if ca1 - acc >= MIN_ANGLE {
            abs.push(i);
            layout(child, abs, acc, ca1, depth + 1, color_map, out);
            abs.pop();
        }
        acc = ca1;
    }
}

fn seg_color(node: &FileNode, depth: usize, color_map: &ColorMap) -> Color32 {
    if node.is_dir {
        // Directories: a depth-shaded green-gray, lighter the deeper we go.
        let d = depth.min(MAX_DEPTH) as u8;
        Color32::from_rgb(70 + d * 6, 92 + d * 9, 78 + d * 6)
    } else {
        color_map.get_treemap(node.extension())
    }
}

fn hit_test(p: Pos2, center: Pos2, r0: f32, ring_w: f32, segs: &[Segment]) -> Option<usize> {
    let dx = p.x - center.x;
    let dy = p.y - center.y;
    let r = (dx * dx + dy * dy).sqrt();
    if r < r0 {
        return None;
    }
    let depth = (((r - r0) / ring_w).floor() as usize) + 1;
    let mut theta = dy.atan2(dx);
    if theta < 0.0 {
        theta += TAU;
    }
    segs.iter()
        .position(|s| s.depth == depth && theta >= s.a0 && theta < s.a1)
}

fn polar(center: Pos2, r: f32, a: f32) -> Pos2 {
    let (sin, cos) = a.sin_cos();
    pos2(center.x + r * cos, center.y + r * sin)
}

fn fill_sector(
    painter: &egui::Painter,
    center: Pos2,
    r_in: f32,
    r_out: f32,
    a0: f32,
    a1: f32,
    color: Color32,
) {
    let steps = (((a1 - a0).abs() / 0.10).ceil() as usize).max(1);
    let mut mesh = Mesh::default();
    for i in 0..=steps {
        let a = a0 + (a1 - a0) * (i as f32 / steps as f32);
        mesh.colored_vertex(polar(center, r_in, a), color);
        mesh.colored_vertex(polar(center, r_out, a), color);
    }
    for i in 0..steps {
        let k = (i * 2) as u32;
        mesh.add_triangle(k, k + 1, k + 2);
        mesh.add_triangle(k + 1, k + 3, k + 2);
    }
    painter.add(Shape::mesh(mesh));
}

fn lighten(c: Color32, amt: f32) -> Color32 {
    let f = |v: u8| (v as f32 + (255.0 - v as f32) * amt) as u8;
    Color32::from_rgb(f(c.r()), f(c.g()), f(c.b()))
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let t: String = s.chars().take(max - 1).collect();
        format!("{t}\u{2026}")
    }
}
