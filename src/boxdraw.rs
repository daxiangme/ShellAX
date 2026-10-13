//! Self-drawn box-drawing (U+2500–257F), block elements (U+2580–259F) and Powerline separators
//! (U+E0B0–E0B3, E0B8–E0BF) so they tile seamlessly regardless of the font. Original code.
//! Coordinates are in pixels relative to the cell's top-left; the caller paints them.

#[derive(Clone, Debug, PartialEq)]
pub enum Prim {
    /// x, y, w, h, alpha (shade blocks use alpha < 1)
    Rect(f32, f32, f32, f32, f32),
    /// filled triangle
    Tri([(f32, f32); 3]),
    /// open polyline with stroke width
    Stroke(Vec<(f32, f32)>, f32),
}

pub fn is_custom(c: char) -> bool {
    matches!(c as u32, 0x2500..=0x259F | 0xE0B0..=0xE0B3 | 0xE0B8..=0xE0BF)
}

/// Line weights (left, right, up, down): 0 none, 1 light, 2 heavy, 3 double.
fn weights(c: char) -> Option<[u8; 4]> {
    let w = match c as u32 {
        0x2500 | 0x2504 | 0x2508 | 0x254C => [1, 1, 0, 0],
        0x2501 | 0x2505 | 0x2509 | 0x254D => [2, 2, 0, 0],
        0x2502 | 0x2506 | 0x250A | 0x254E => [0, 0, 1, 1],
        0x2503 | 0x2507 | 0x250B | 0x254F => [0, 0, 2, 2],
        0x250C => [0, 1, 0, 1], 0x250D => [0, 2, 0, 1], 0x250E => [0, 1, 0, 2], 0x250F => [0, 2, 0, 2],
        0x2510 => [1, 0, 0, 1], 0x2511 => [2, 0, 0, 1], 0x2512 => [1, 0, 0, 2], 0x2513 => [2, 0, 0, 2],
        0x2514 => [0, 1, 1, 0], 0x2515 => [0, 2, 1, 0], 0x2516 => [0, 1, 2, 0], 0x2517 => [0, 2, 2, 0],
        0x2518 => [1, 0, 1, 0], 0x2519 => [2, 0, 1, 0], 0x251A => [1, 0, 2, 0], 0x251B => [2, 0, 2, 0],
        0x251C => [0, 1, 1, 1], 0x251D => [0, 2, 1, 1], 0x251E => [0, 1, 2, 1], 0x251F => [0, 1, 1, 2],
        0x2520 => [0, 1, 2, 2], 0x2521 => [0, 2, 2, 1], 0x2522 => [0, 2, 1, 2], 0x2523 => [0, 2, 2, 2],
        0x2524 => [1, 0, 1, 1], 0x2525 => [2, 0, 1, 1], 0x2526 => [1, 0, 2, 1], 0x2527 => [1, 0, 1, 2],
        0x2528 => [1, 0, 2, 2], 0x2529 => [2, 0, 2, 1], 0x252A => [2, 0, 1, 2], 0x252B => [2, 0, 2, 2],
        0x252C => [1, 1, 0, 1], 0x252D => [2, 1, 0, 1], 0x252E => [1, 2, 0, 1], 0x252F => [2, 2, 0, 1],
        0x2530 => [1, 1, 0, 2], 0x2531 => [2, 1, 0, 2], 0x2532 => [1, 2, 0, 2], 0x2533 => [2, 2, 0, 2],
        0x2534 => [1, 1, 1, 0], 0x2535 => [2, 1, 1, 0], 0x2536 => [1, 2, 1, 0], 0x2537 => [2, 2, 1, 0],
        0x2538 => [1, 1, 2, 0], 0x2539 => [2, 1, 2, 0], 0x253A => [1, 2, 2, 0], 0x253B => [2, 2, 2, 0],
        0x253C => [1, 1, 1, 1], 0x253D => [2, 1, 1, 1], 0x253E => [1, 2, 1, 1], 0x253F => [2, 2, 1, 1],
        0x2540 => [1, 1, 2, 1], 0x2541 => [1, 1, 1, 2], 0x2542 => [1, 1, 2, 2], 0x2543 => [2, 1, 2, 1],
        0x2544 => [1, 2, 2, 1], 0x2545 => [2, 1, 1, 2], 0x2546 => [1, 2, 1, 2], 0x2547 => [2, 2, 2, 1],
        0x2548 => [2, 2, 1, 2], 0x2549 => [2, 1, 2, 2], 0x254A => [1, 2, 2, 2], 0x254B => [2, 2, 2, 2],
        0x2550 => [3, 3, 0, 0], 0x2551 => [0, 0, 3, 3],
        0x2552 => [0, 3, 0, 1], 0x2553 => [0, 1, 0, 3], 0x2554 => [0, 3, 0, 3],
        0x2555 => [3, 0, 0, 1], 0x2556 => [1, 0, 0, 3], 0x2557 => [3, 0, 0, 3],
        0x2558 => [0, 3, 1, 0], 0x2559 => [0, 1, 3, 0], 0x255A => [0, 3, 3, 0],
        0x255B => [3, 0, 1, 0], 0x255C => [1, 0, 3, 0], 0x255D => [3, 0, 3, 0],
        0x255E => [0, 3, 1, 1], 0x255F => [0, 1, 3, 3], 0x2560 => [0, 3, 3, 3],
        0x2561 => [3, 0, 1, 1], 0x2562 => [1, 0, 3, 3], 0x2563 => [3, 0, 3, 3],
        0x2564 => [3, 3, 0, 1], 0x2565 => [1, 1, 0, 3], 0x2566 => [3, 3, 0, 3],
        0x2567 => [3, 3, 1, 0], 0x2568 => [1, 1, 3, 0], 0x2569 => [3, 3, 3, 0],
        0x256A => [3, 3, 1, 1], 0x256B => [1, 1, 3, 3], 0x256C => [3, 3, 3, 3],
        // rounded corners: drawn as square corners (arc drawing TBD)
        0x256D => [0, 1, 0, 1], 0x256E => [1, 0, 0, 1], 0x256F => [1, 0, 1, 0], 0x2570 => [0, 1, 1, 0],
        0x2574 => [1, 0, 0, 0], 0x2575 => [0, 0, 1, 0], 0x2576 => [0, 1, 0, 0], 0x2577 => [0, 0, 0, 1],
        0x2578 => [2, 0, 0, 0], 0x2579 => [0, 0, 2, 0], 0x257A => [0, 2, 0, 0], 0x257B => [0, 0, 0, 2],
        0x257C => [1, 2, 0, 0], 0x257D => [0, 0, 1, 2], 0x257E => [2, 1, 0, 0], 0x257F => [0, 0, 2, 1],
        _ => return None,
    };
    Some(w)
}

/// Primitives for `c` in a `w`×`h` cell; None if not a custom glyph (e.g. diagonals U+2571–2573).
pub fn prims(c: char, w: f32, h: f32) -> Option<Vec<Prim>> {
    let mut v = Vec::new();
    let t1 = (w / 8.0).round().max(1.0);
    let t2 = (t1 * 2.0).max(2.0);
    let cx = (w / 2.0).floor();
    let cy = (h / 2.0).floor();
    if let Some([l, r, u, d]) = weights(c) {
        let th = |k: u8| match k { 0 => 0.0, 2 => t2, 3 => 3.0 * t1, _ => t1 };
        // pixel-exact joins: a segment reaching the center covers the perpendicular stroke's span
        let tv = th(u).max(th(d));
        let thz = th(l).max(th(r));
        let hseg = |x0: f32, x1: f32, k: u8, v: &mut Vec<Prim>| match k {
            0 => {}
            3 => { v.push(Prim::Rect(x0, cy - 1.5 * t1, x1 - x0, t1, 1.0)); v.push(Prim::Rect(x0, cy + 0.5 * t1, x1 - x0, t1, 1.0)); }
            k => v.push(Prim::Rect(x0, cy - (th(k) / 2.0).floor(), x1 - x0, th(k), 1.0)),
        };
        let vseg = |y0: f32, y1: f32, k: u8, v: &mut Vec<Prim>| match k {
            0 => {}
            3 => { v.push(Prim::Rect(cx - 1.5 * t1, y0, t1, y1 - y0, 1.0)); v.push(Prim::Rect(cx + 0.5 * t1, y0, t1, y1 - y0, 1.0)); }
            k => v.push(Prim::Rect(cx - (th(k) / 2.0).floor(), y0, th(k), y1 - y0, 1.0)),
        };
        let lo = |t: f32| (t / 2.0).floor();
        let hi = |t: f32| t - (t / 2.0).floor();
        hseg(0.0, cx + hi(tv.max(th(l))), l, &mut v);
        hseg(cx - lo(tv.max(th(r))), w, r, &mut v);
        vseg(0.0, cy + hi(thz.max(th(u))), u, &mut v);
        vseg(cy - lo(thz.max(th(d))), h, d, &mut v);
        return Some(v);
    }
    let r = |x: f32, y: f32, ww: f32, hh: f32| Prim::Rect(x, y, ww, hh, 1.0);
    let (hw, hh) = (cx, cy);
    match c as u32 {
        0x2580 => v.push(r(0.0, 0.0, w, hh)),
        0x2581..=0x2587 => { let f = (c as u32 - 0x2580) as f32 / 8.0; let y = (h * (1.0 - f)).round(); v.push(r(0.0, y, w, h - y)); }
        0x2588 => v.push(r(0.0, 0.0, w, h)),
        0x2589..=0x258F => { let f = (0x2590 - c as u32) as f32 / 8.0; v.push(r(0.0, 0.0, (w * f).round(), h)); }
        0x2590 => v.push(r(hw, 0.0, w - hw, h)),
        0x2591 => v.push(Prim::Rect(0.0, 0.0, w, h, 0.25)),
        0x2592 => v.push(Prim::Rect(0.0, 0.0, w, h, 0.5)),
        0x2593 => v.push(Prim::Rect(0.0, 0.0, w, h, 0.75)),
        0x2594 => v.push(r(0.0, 0.0, w, (h / 8.0).round().max(1.0))),
        0x2595 => { let ww = (w / 8.0).round().max(1.0); v.push(r(w - ww, 0.0, ww, h)); }
        0x2596..=0x259F => {
            // quadrants: UL, UR, LL, LR
            let q: [bool; 4] = match c as u32 {
                0x2596 => [false, false, true, false], 0x2597 => [false, false, false, true], 0x2598 => [true, false, false, false],
                0x2599 => [true, false, true, true], 0x259A => [true, false, false, true], 0x259B => [true, true, true, false],
                0x259C => [true, true, false, true], 0x259D => [false, true, false, false], 0x259E => [false, true, true, false],
                _ => [false, true, true, true],
            };
            if q[0] { v.push(r(0.0, 0.0, hw, hh)); }
            if q[1] { v.push(r(hw, 0.0, w - hw, hh)); }
            if q[2] { v.push(r(0.0, hh, hw, h - hh)); }
            if q[3] { v.push(r(hw, hh, w - hw, h - hh)); }
        }
        0xE0B0 => v.push(Prim::Tri([(0.0, 0.0), (w, h / 2.0), (0.0, h)])),
        0xE0B2 => v.push(Prim::Tri([(w, 0.0), (0.0, h / 2.0), (w, h)])),
        0xE0B1 => v.push(Prim::Stroke(vec![(0.0, 0.0), (w, h / 2.0), (0.0, h)], t1)),
        0xE0B3 => v.push(Prim::Stroke(vec![(w, 0.0), (0.0, h / 2.0), (w, h)], t1)),
        0xE0B8 => v.push(Prim::Tri([(0.0, 0.0), (0.0, h), (w, h)])),
        0xE0BA => v.push(Prim::Tri([(w, 0.0), (w, h), (0.0, h)])),
        0xE0BC => v.push(Prim::Tri([(0.0, 0.0), (w, 0.0), (0.0, h)])),
        0xE0BE => v.push(Prim::Tri([(0.0, 0.0), (w, 0.0), (w, h)])),
        0xE0B9 => v.push(Prim::Stroke(vec![(0.0, 0.0), (w, h)], t1)),
        0xE0BB | 0xE0BD => v.push(Prim::Stroke(vec![(w, 0.0), (0.0, h)], t1)),
        0xE0BF => v.push(Prim::Stroke(vec![(0.0, 0.0), (w, h)], t1)),
        _ => return None,
    }
    Some(v)
}

/// Rasterize primitives into axis-aligned rects (x, y, w, h, alpha). Triangles and strokes are
/// scan-converted per pixel row, which renders crisply on every backend without path support.
pub fn to_rects(prims: &[Prim], h: f32) -> Vec<(f32, f32, f32, f32, f32)> {
    let mut out = Vec::new();
    let rows = h.round() as i32;
    for p in prims {
        match p {
            Prim::Rect(x, y, w, hh, a) => out.push((*x, *y, *w, *hh, *a)),
            Prim::Tri(pts) => for row in 0..rows {
                let yc = row as f32 + 0.5;
                let mut xs: Vec<f32> = Vec::new();
                for i in 0..3 {
                    let (a, b) = (pts[i], pts[(i + 1) % 3]);
                    if (a.1 - b.1).abs() < f32::EPSILON { continue; }
                    let (lo, hi) = if a.1 < b.1 { (a, b) } else { (b, a) };
                    if yc >= lo.1 && yc <= hi.1 { xs.push(lo.0 + (yc - lo.1) / (hi.1 - lo.1) * (hi.0 - lo.0)); }
                }
                if xs.len() >= 2 {
                    let (x0, x1) = xs.iter().fold((f32::MAX, f32::MIN), |(m, n), &x| (m.min(x), n.max(x)));
                    if x1 > x0 { out.push((x0, row as f32, x1 - x0, 1.0, 1.0)); }
                }
            },
            Prim::Stroke(pts, t) => for seg in pts.windows(2) {
                let (a, b) = (seg[0], seg[1]);
                let (lo, hi) = if a.1 < b.1 { (a, b) } else { (b, a) };
                for row in lo.1.floor() as i32..hi.1.ceil() as i32 {
                    let yc = (row as f32 + 0.5).clamp(lo.1, hi.1);
                    let x = if hi.1 > lo.1 { lo.0 + (yc - lo.1) / (hi.1 - lo.1) * (hi.0 - lo.0) } else { lo.0 };
                    let slope = if hi.1 > lo.1 { ((hi.0 - lo.0) / (hi.1 - lo.1)).abs() } else { 0.0 };
                    let wpx = (t * (1.0 + slope * slope).sqrt()).max(*t);
                    out.push((x - wpx / 2.0, row as f32, wpx, 1.0, 1.0));
                }
            },
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn horizontal_line_spans_cell() {
        let p = prims('─', 8.0, 18.0).unwrap();
        let (x0, x1) = p.iter().fold((f32::MAX, f32::MIN), |(a, b), p| match p { Prim::Rect(x, _, w, _, _) => (a.min(*x), b.max(x + w)), _ => (a, b) });
        assert_eq!((x0, x1), (0.0, 8.0));
    }
    #[test]
    fn corner_and_blocks() {
        assert_eq!(prims('┌', 8.0, 18.0).unwrap().len(), 2);
        assert_eq!(prims('█', 8.0, 18.0).unwrap(), vec![Prim::Rect(0.0, 0.0, 8.0, 18.0, 1.0)]);
        assert!(matches!(prims('\u{E0B0}', 8.0, 18.0).unwrap()[0], Prim::Tri(_)));
        assert!(prims('╱', 8.0, 18.0).is_none());
        assert!(prims('a', 8.0, 18.0).is_none());
    }
    #[test]
    fn corner_joins_exactly() {
        // ┌ : right segment starts where the down segment's left edge is; no stub above the line
        let p = prims('┌', 8.0, 18.0).unwrap();
        let ys: Vec<f32> = p.iter().map(|p| match p { Prim::Rect(_, y, ..) => *y, _ => 0.0 }).collect();
        assert!(ys.iter().all(|&y| y >= 9.0 - 0.01), "{p:?}");
    }
    #[test]
    fn triangle_rasterizes_per_row() {
        let r = to_rects(&prims('\u{E0B0}', 8.0, 18.0).unwrap(), 18.0);
        assert_eq!(r.len(), 18);
        let widest = r.iter().map(|r| r.2).fold(0.0, f32::max);
        assert!(widest > 7.0 && widest <= 8.0);
    }
}
