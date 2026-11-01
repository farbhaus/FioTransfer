//! The app's line icons, traced from the SVGs the webview drew them with.
//!
//! Each icon is the path data of `I.<name>` in `frontend/dist/app.js` or the
//! inline `<svg>` in `index.html` (at `60b6c61`), copied verbatim: a 20-unit
//! view box, strokes only, in the current colour. [`paint`] reads the path and
//! strokes it, so a folder or a chevron is the same shape on every screen. A
//! `<rect>` or `<circle>` is written here as the equivalent path.

use egui::{pos2, Color32, Painter, Pos2, Shape, Stroke, Vec2};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Up,
    Down,
    /// The two-arrow mark: the brand mark, Transfers' nav item, a mirror watch.
    Sync,
    Folder,
    Project,
    Team,
    Close,
    Retry,
    ChevronDown,
    ChevronUp,
    /// `ChevronDown` turned a quarter: a row that opens.
    ChevronRight,
    Open,
    Copy,
    Pencil,
    Pause,
    Play,
    Trash,
    Search,
    Link,
    Eye,
    Clock,
    Sliders,
    /// The Space mark.
    Donut,
}

/// One `<path>`: its data, `stroke-width`, and whether its caps and joins are
/// round (`stroke-linecap` / `stroke-linejoin="round"`).
struct Part {
    d: &'static str,
    width: f32,
    caps: bool,
    joins: bool,
}

const fn part(d: &'static str, width: f32, caps: bool, joins: bool) -> Part {
    Part { d, width, caps, joins }
}

/// An icon's parts as a `&'static` slice (a `const`, so `part` may build them).
macro_rules! icon {
    ($($p:expr),+ $(,)?) => {{
        const P: &[Part] = &[$($p),+];
        P
    }};
}

fn parts(icon: Icon) -> &'static [Part] {
    match icon {
        Icon::Up => icon![part("M10 15.5V4.5m0 0L5.5 9M10 4.5L14.5 9", 1.5, true, true)],
        Icon::Down => icon![part("M10 4.5v11m0 0L5.5 11m4.5 4.5l4.5-4.5", 1.5, true, true)],
        Icon::Sync => icon![part(
            "M6.5 15V5m0 0L3.8 7.7M6.5 5l2.7 2.7M13.5 5v10m0 0l-2.7-2.7m2.7 2.7l2.7-2.7",
            1.5,
            true,
            true,
        )],
        Icon::Folder => icon![part(
            "M3 6.2c0-.7.5-1.2 1.2-1.2h3.4l1.6 1.7h6.6c.7 0 1.2.5 1.2 1.2v6.9c0 .7-.5 1.2-1.2 1.2H4.2c-.7 0-1.2-.5-1.2-1.2z",
            1.4,
            false,
            false,
        )],
        // <rect x="3" y="4" width="14" height="12" rx="2"/> and a rule under the top.
        Icon::Project => icon![
            part("M5 4h10a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2z", 1.4, false, false),
            part("M3 8h14", 1.4, false, false),
        ],
        // <circle cx="8" cy="7.5" r="2.6"/>, then the shoulders and the second head.
        Icon::Team => icon![
            part("M5.4 7.5a2.6 2.6 0 1 0 5.2 0a2.6 2.6 0 1 0-5.2 0z", 1.4, false, false),
            part(
                "M3.5 15.5c.6-2.4 2.3-3.6 4.5-3.6s3.9 1.2 4.5 3.6M13 5.2a2.4 2.4 0 0 1 0 4.6M14.5 11.9c1.3.5 2.1 1.6 2.4 3.6",
                1.4,
                true,
                false,
            ),
        ],
        Icon::Close => icon![part("M6 6l8 8M14 6l-8 8", 1.5, true, false)],
        Icon::Retry => icon![part("M15.5 10a5.5 5.5 0 1 1-1.6-3.9M15.5 4v3.4h-3.4", 1.5, true, true)],
        Icon::ChevronDown => icon![part("M7 8l3 3 3-3", 1.5, true, true)],
        Icon::ChevronUp => icon![part("M7 12l3-3 3 3", 1.5, true, true)],
        Icon::ChevronRight => icon![part("M8.5 7l3 3-3 3", 1.5, true, true)],
        Icon::Open => icon![part(
            "M8 5H5.5A1.5 1.5 0 0 0 4 6.5v8A1.5 1.5 0 0 0 5.5 16h8a1.5 1.5 0 0 0 1.5-1.5V12M11 4h5v5M16 4l-7 7",
            1.5,
            true,
            true,
        )],
        // <rect x="7" y="7" width="9" height="9" rx="1.5"/> and the sheet behind it.
        Icon::Copy => icon![
            part("M8.5 7h6a1.5 1.5 0 0 1 1.5 1.5v6a1.5 1.5 0 0 1-1.5 1.5h-6A1.5 1.5 0 0 1 7 14.5v-6A1.5 1.5 0 0 1 8.5 7z", 1.4, false, false),
            part("M13 7V5.5A1.5 1.5 0 0 0 11.5 4h-6A1.5 1.5 0 0 0 4 5.5v6A1.5 1.5 0 0 0 5.5 13H7", 1.4, false, false),
        ],
        Icon::Pencil => icon![part(
            "M12.8 4.7l2.5 2.5M4 16l.7-3.2 8.6-8.6a1.2 1.2 0 0 1 1.7 0l.8.8a1.2 1.2 0 0 1 0 1.7l-8.6 8.6z",
            1.4,
            false,
            true,
        )],
        Icon::Pause => icon![part("M7.5 5v10M12.5 5v10", 1.6, true, false)],
        Icon::Play => icon![part("M7 5.5v9l7-4.5z", 1.5, false, true)],
        Icon::Trash => icon![part("M4.5 6h11M8 6V4.5h4V6M6 6l.7 9.5h6.6L14 6", 1.4, true, true)],
        // <circle cx="9" cy="9" r="5"/> and the handle.
        Icon::Search => icon![
            part("M4 9a5 5 0 1 0 10 0a5 5 0 1 0-10 0z", 1.5, false, false),
            part("M13 13l3.5 3.5", 1.5, true, false),
        ],
        Icon::Link => icon![part(
            "M8.5 11.5l3-3M7.2 9.3l-1.6 1.6a2.6 2.6 0 0 0 3.7 3.7l1.6-1.6M12.8 10.7l1.6-1.6a2.6 2.6 0 0 0-3.7-3.7L9.1 7",
            1.5,
            true,
            false,
        )],
        // The lids, and <circle cx="10" cy="10" r="2.2"/>.
        Icon::Eye => icon![
            part("M2.5 10s2.7-5 7.5-5 7.5 5 7.5 5-2.7 5-7.5 5-7.5-5-7.5-5z", 1.5, false, true),
            part("M7.8 10a2.2 2.2 0 1 0 4.4 0a2.2 2.2 0 1 0-4.4 0z", 1.5, false, false),
        ],
        Icon::Clock => icon![part("M3.5 10a6.5 6.5 0 1 0 1.9-4.6M3.5 4v3.4h3.4M10 6.5V10l2.5 1.6", 1.5, true, true)],
        // Two rails, and the knobs <circle cx="13" cy="6" r="2"/>, <circle cx="7" cy="14" r="2"/>.
        Icon::Sliders => icon![
            part("M4 6h7m4 0h1M4 14h1m4 0h7", 1.5, true, false),
            part("M11 6a2 2 0 1 0 4 0a2 2 0 1 0-4 0z", 1.5, false, false),
            part("M5 14a2 2 0 1 0 4 0a2 2 0 1 0-4 0z", 1.5, false, false),
        ],
        // <circle r="6.6"/>, <circle r="2.4"/> and two ticks: a donut seen end on.
        Icon::Donut => icon![
            part("M3.4 10a6.6 6.6 0 1 0 13.2 0a6.6 6.6 0 1 0-13.2 0z", 1.5, false, false),
            part("M7.6 10a2.4 2.4 0 1 0 4.8 0a2.4 2.4 0 1 0-4.8 0z", 1.5, false, false),
            part("M10 3.4v4.2M16.6 10h-4.2", 1.5, true, false),
        ],
    }
}

/// Paints `icon` in the square `size` px wide centred on `centre`, with the
/// stroke widths its SVG gives, scaled the way the browser scaled them.
pub fn paint(painter: &Painter, centre: Pos2, size: f32, icon: Icon, colour: Color32) {
    paint_parts(painter, centre, size, icon, colour, None);
}

/// [`paint`] with one stroke width (in view-box units) for every part: the
/// stylesheet draws the same path heavier in a few places (the brand mark at 1.7).
pub fn paint_width(painter: &Painter, centre: Pos2, size: f32, icon: Icon, colour: Color32, width: f32) {
    paint_parts(painter, centre, size, icon, colour, Some(width));
}

fn paint_parts(painter: &Painter, centre: Pos2, size: f32, icon: Icon, colour: Color32, width: Option<f32>) {
    let scale = size / 20.0;
    if scale <= 0.0 {
        return;
    }
    let origin = centre - Vec2::splat(size / 2.0);
    let at = |p: Pos2| origin + p.to_vec2() * scale;
    for p in parts(icon) {
        // Never thinner than a pixel: a 1.4 stroke at 14 px would be 0.98.
        let w = (width.unwrap_or(p.width) * scale).max(1.0);
        let stroke = Stroke::new(w, colour);
        for sub in trace(p.d) {
            let pts: Vec<Pos2> = sub.points.iter().map(|&q| at(q)).collect();
            stroke_sub(painter, &pts, &sub.corners, sub.closed, stroke, p.caps, p.joins);
        }
    }
}

/// egui strokes a polyline with mitred joins and flat ends. A round join is the
/// polyline cut at each corner with a dot of half the stroke width there; a
/// round cap is the same dot on each open end.
fn stroke_sub(painter: &Painter, pts: &[Pos2], corners: &[bool], closed: bool, stroke: Stroke, caps: bool, joins: bool) {
    if pts.len() < 2 {
        return;
    }
    let dot = |p: Pos2| painter.circle_filled(p, stroke.width / 2.0, stroke.color);
    if !joins {
        if closed {
            painter.add(Shape::closed_line(pts.to_vec(), stroke));
        } else {
            painter.add(Shape::line(pts.to_vec(), stroke));
        }
    } else {
        let mut ring = pts.to_vec();
        let mut corner = corners.to_vec();
        if closed {
            ring.push(pts[0]);
            corner.push(true);
        }
        let mut run = vec![ring[0]];
        for i in 1..ring.len() {
            run.push(ring[i]);
            if corner[i] || i == ring.len() - 1 {
                painter.add(Shape::line(std::mem::take(&mut run), stroke));
                run.push(ring[i]);
            }
        }
        for (i, p) in ring.iter().enumerate() {
            let inner = closed || (i > 0 && i < ring.len() - 1);
            if corner[i] && inner {
                dot(*p);
            }
        }
    }
    if caps && !closed {
        dot(pts[0]);
        dot(pts[pts.len() - 1]);
    }
}

/// One subpath in view-box units. `corners[i]` marks the points a command ends
/// on, as opposed to the points a curve is flattened through.
#[derive(Debug, Default)]
struct Sub {
    points: Vec<Pos2>,
    corners: Vec<bool>,
    closed: bool,
}

impl Sub {
    fn push(&mut self, p: Pos2, corner: bool) {
        self.points.push(p);
        self.corners.push(corner);
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Tok {
    Cmd(char),
    Num(f32),
}

/// Splits path data into commands and numbers. Numbers may run together the
/// way minified SVG writes them: `.7.5-1.2` is 0.7, 0.5, -1.2.
fn tokens(d: &str) -> Vec<Tok> {
    let b = d.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i] as char;
        if c.is_ascii_alphabetic() && c != 'e' && c != 'E' {
            out.push(Tok::Cmd(c));
            i += 1;
        } else if c == '-' || c == '+' || c == '.' || c.is_ascii_digit() {
            let start = i;
            i += 1;
            let mut dot = c == '.';
            while i < b.len() {
                let n = b[i] as char;
                if n.is_ascii_digit() {
                    i += 1;
                } else if n == '.' && !dot {
                    dot = true;
                    i += 1;
                } else if (n == 'e' || n == 'E') && i + 1 < b.len() {
                    i += 1;
                    if b[i] == b'-' || b[i] == b'+' {
                        i += 1;
                    }
                } else {
                    break;
                }
            }
            out.push(Tok::Num(d[start..i].parse().unwrap_or(0.0)));
        } else {
            i += 1; // whitespace, commas
        }
    }
    out
}

/// Path data to polylines: M L H V C S A Z, absolute and relative, with the
/// implicit repeats SVG allows. The icons use nothing else.
fn trace(d: &str) -> Vec<Sub> {
    let toks = tokens(d);
    let mut subs: Vec<Sub> = Vec::new();
    let mut cur = Sub::default();
    let (mut p, mut start) = (Pos2::ZERO, Pos2::ZERO);
    let mut last_ctrl: Option<Pos2> = None;
    let mut cmd = 'M';
    let mut i = 0;
    let finish = |cur: &mut Sub, subs: &mut Vec<Sub>| {
        if cur.points.len() > 1 {
            subs.push(std::mem::take(cur));
        } else {
            *cur = Sub::default();
        }
    };
    while i < toks.len() {
        if let Tok::Cmd(c) = toks[i] {
            cmd = c;
            i += 1;
            if c == 'Z' || c == 'z' {
                cur.closed = true;
                if cur.points.len() > 1 && cur.points.last() == cur.points.first() {
                    cur.points.pop();
                    cur.corners.pop();
                }
                finish(&mut cur, &mut subs);
                p = start;
                last_ctrl = None;
                continue;
            }
        }
        let rel = cmd.is_ascii_lowercase();
        let n = match cmd.to_ascii_uppercase() {
            'M' | 'L' => 2,
            'H' | 'V' => 1,
            'C' => 6,
            'S' => 4,
            'A' => 7,
            _ => {
                i += 1;
                continue;
            }
        };
        let mut a = [0.0_f32; 7];
        for slot in a.iter_mut().take(n) {
            match toks.get(i) {
                Some(Tok::Num(v)) => {
                    *slot = *v;
                    i += 1;
                }
                _ => return subs_with(subs, cur),
            }
        }
        let base = if rel { p.to_vec2() } else { Vec2::ZERO };
        let pt = |x: f32, y: f32| pos2(x, y) + base;
        match cmd.to_ascii_uppercase() {
            'M' => {
                finish(&mut cur, &mut subs);
                p = pt(a[0], a[1]);
                start = p;
                cur.push(p, true);
                last_ctrl = None;
                // Pairs after a moveto are linetos.
                cmd = if rel { 'l' } else { 'L' };
            }
            'L' => {
                p = pt(a[0], a[1]);
                cur.push(p, true);
                last_ctrl = None;
            }
            'H' => {
                p = pos2(if rel { p.x + a[0] } else { a[0] }, p.y);
                cur.push(p, true);
                last_ctrl = None;
            }
            'V' => {
                p = pos2(p.x, if rel { p.y + a[0] } else { a[0] });
                cur.push(p, true);
                last_ctrl = None;
            }
            'C' | 'S' => {
                let (c1, c2, end) = if cmd.to_ascii_uppercase() == 'C' {
                    (pt(a[0], a[1]), pt(a[2], a[3]), pt(a[4], a[5]))
                } else {
                    let c1 = last_ctrl.map_or(p, |c| p + (p - c));
                    (c1, pt(a[0], a[1]), pt(a[2], a[3]))
                };
                cubic(&mut cur, p, c1, c2, end);
                p = end;
                last_ctrl = Some(c2);
            }
            'A' => {
                let end = pt(a[5], a[6]);
                arc(&mut cur, p, a[0], a[1], a[2], a[3] != 0.0, a[4] != 0.0, end);
                p = end;
                last_ctrl = None;
            }
            _ => {}
        }
    }
    subs_with(subs, cur)
}

fn subs_with(mut subs: Vec<Sub>, cur: Sub) -> Vec<Sub> {
    if cur.points.len() > 1 {
        subs.push(cur);
    }
    subs
}

fn cubic(sub: &mut Sub, p0: Pos2, c1: Pos2, c2: Pos2, p1: Pos2) {
    const STEPS: usize = 8;
    for k in 1..=STEPS {
        let t = k as f32 / STEPS as f32;
        let u = 1.0 - t;
        let q = p0.to_vec2() * (u * u * u)
            + c1.to_vec2() * (3.0 * u * u * t)
            + c2.to_vec2() * (3.0 * u * t * t)
            + p1.to_vec2() * (t * t * t);
        sub.push(q.to_pos2(), k == STEPS);
    }
}

/// An elliptical arc from `p0` to `p1`, by the SVG spec's endpoint-to-centre
/// conversion (Implementation Notes, F.6.5).
#[expect(clippy::too_many_arguments, reason = "the arc command's own seven numbers")]
fn arc(sub: &mut Sub, p0: Pos2, rx: f32, ry: f32, rot_deg: f32, large: bool, sweep: bool, p1: Pos2) {
    if p0 == p1 {
        return;
    }
    let (mut rx, mut ry) = (rx.abs(), ry.abs());
    if rx == 0.0 || ry == 0.0 {
        sub.push(p1, true);
        return;
    }
    let (sin, cos) = rot_deg.to_radians().sin_cos();
    let (dx, dy) = ((p0.x - p1.x) / 2.0, (p0.y - p1.y) / 2.0);
    let x1 = cos * dx + sin * dy;
    let y1 = -sin * dx + cos * dy;
    let lambda = (x1 * x1) / (rx * rx) + (y1 * y1) / (ry * ry);
    if lambda > 1.0 {
        rx *= lambda.sqrt();
        ry *= lambda.sqrt();
    }
    let num = rx * rx * ry * ry - rx * rx * y1 * y1 - ry * ry * x1 * x1;
    let den = rx * rx * y1 * y1 + ry * ry * x1 * x1;
    let mut k = (num / den).max(0.0).sqrt();
    if large == sweep {
        k = -k;
    }
    let (cx1, cy1) = (k * rx * y1 / ry, -k * ry * x1 / rx);
    let cx = cos * cx1 - sin * cy1 + (p0.x + p1.x) / 2.0;
    let cy = sin * cx1 + cos * cy1 + (p0.y + p1.y) / 2.0;
    let angle = |ux: f32, uy: f32, vx: f32, vy: f32| (ux * vy - uy * vx).atan2(ux * vx + uy * vy);
    let (ux, uy) = ((x1 - cx1) / rx, (y1 - cy1) / ry);
    let (vx, vy) = ((-x1 - cx1) / rx, (-y1 - cy1) / ry);
    let theta = angle(1.0, 0.0, ux, uy);
    let mut delta = angle(ux, uy, vx, vy);
    if !sweep && delta > 0.0 {
        delta -= std::f32::consts::TAU;
    } else if sweep && delta < 0.0 {
        delta += std::f32::consts::TAU;
    }
    let steps = ((delta.abs() / (std::f32::consts::PI / 16.0)).ceil() as usize).max(2);
    for s in 1..=steps {
        if s == steps {
            sub.push(p1, true);
            break;
        }
        let t = theta + delta * s as f32 / steps as f32;
        let (st, ct) = t.sin_cos();
        sub.push(pos2(cos * rx * ct - sin * ry * st + cx, sin * rx * ct + cos * ry * st + cy), false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Icon; 23] = [
        Icon::Up,
        Icon::Down,
        Icon::Sync,
        Icon::Folder,
        Icon::Project,
        Icon::Team,
        Icon::Close,
        Icon::Retry,
        Icon::ChevronDown,
        Icon::ChevronUp,
        Icon::ChevronRight,
        Icon::Open,
        Icon::Copy,
        Icon::Pencil,
        Icon::Pause,
        Icon::Play,
        Icon::Trash,
        Icon::Search,
        Icon::Link,
        Icon::Eye,
        Icon::Clock,
        Icon::Sliders,
        Icon::Donut,
    ];

    fn close(a: Pos2, b: Pos2) -> bool {
        (a - b).length() < 1e-3
    }

    #[test]
    fn numbers_run_together_the_way_minified_svg_writes_them() {
        let nums: Vec<f32> = tokens("c0-.7.5-1.2 1.2-1.2")
            .into_iter()
            .filter_map(|t| match t {
                Tok::Num(v) => Some(v),
                Tok::Cmd(_) => None,
            })
            .collect();
        assert_eq!(nums, vec![0.0, -0.7, 0.5, -1.2, 1.2, -1.2]);
    }

    /// The chevron the transfers row and the destination bar draw: `M7 8l3 3 3-3`,
    /// 6 x 3 units, not the width of its box.
    #[test]
    fn the_chevron_is_the_webviews_small_v() {
        let subs = trace("M7 8l3 3 3-3");
        assert_eq!(subs.len(), 1);
        let p = &subs[0].points;
        assert_eq!(p.len(), 3);
        assert!(close(p[0], pos2(7.0, 8.0)) && close(p[1], pos2(10.0, 11.0)) && close(p[2], pos2(13.0, 8.0)), "{p:?}");
    }

    /// The folder is one closed outline with the tab cut into its top edge.
    #[test]
    fn the_folder_is_one_closed_outline_with_its_tab() {
        let subs = trace(parts(Icon::Folder)[0].d);
        assert_eq!(subs.len(), 1);
        let f = &subs[0];
        assert!(f.closed);
        let (min_x, max_x) = f.points.iter().fold((f32::MAX, f32::MIN), |(a, b), p| (a.min(p.x), b.max(p.x)));
        let (min_y, max_y) = f.points.iter().fold((f32::MAX, f32::MIN), |(a, b), p| (a.min(p.y), b.max(p.y)));
        assert!((min_x - 3.0).abs() < 1e-3 && (max_x - 17.0).abs() < 1e-3, "{min_x}..{max_x}");
        assert!((min_y - 5.0).abs() < 1e-3 && (max_y - 16.0).abs() < 1e-3, "{min_y}..{max_y}");
        // The tab: the top edge steps down from y 5 to y 6.7 between x 7.6 and 9.2.
        assert!(f.points.iter().any(|p| close(*p, pos2(7.6, 5.0))));
        assert!(f.points.iter().any(|p| close(*p, pos2(9.2, 6.7))));
    }

    /// Retry's `a5.5 5.5 0 1 1-1.6-3.9` is the large arc round a centre near
    /// (10, 10), ending where the arrow head starts.
    #[test]
    fn an_arc_follows_its_circle() {
        let subs = trace(parts(Icon::Retry)[0].d);
        let ring = &subs[0];
        assert!(close(*ring.points.last().unwrap(), pos2(13.9, 6.1)));
        for p in &ring.points {
            let r = (*p - pos2(10.0, 10.0)).length();
            assert!((r - 5.5).abs() < 0.1, "{p:?} is {r} from the centre");
        }
        // The large arc: it passes the far side of the circle.
        assert!(ring.points.iter().any(|p| p.x < 5.0));
    }

    #[test]
    fn every_icon_traces_inside_its_view_box() {
        for icon in ALL {
            let mut n = 0;
            for p in parts(icon) {
                for sub in trace(p.d) {
                    n += 1;
                    for q in &sub.points {
                        assert!((1.0..=19.0).contains(&q.x) && (1.0..=19.0).contains(&q.y), "{icon:?}: {q:?}");
                    }
                }
            }
            assert!(n > 0, "{icon:?} drew nothing");
        }
    }
}
