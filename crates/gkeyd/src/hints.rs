//! Hint model and label generation.
//!
//! A "hint session" is a list of on-screen targets, each given a short typed
//! label. Targets come either from a UIA element scan (`f`, real controls) or
//! from a screen grid (`g`, the fallback that works even where the
//! accessibility tree is empty — games, legacy Win32, poorly-instrumented
//! Electron). Grid targets carry their cell size so a coarse grid can be
//! refined into a finer one instead of covering the screen in labels.

/// Home-row-biased alphabet, same spirit as Vimium/hunt-and-peck. The engine
/// uses the configured `hint.chars`; this is the default used by tests.
#[allow(dead_code)]
pub const ALPHABET: &[char] = &[
    's', 'a', 'd', 'f', 'j', 'k', 'l', 'e', 'w', 'c', 'm', 'p', 'g', 'h',
];

/// A candidate location. `w`/`h` are the size of the region it represents
/// (a grid cell); zero for point targets like UIA elements, which are never
/// refined.
#[derive(Debug, Clone, Copy)]
pub struct Target {
    pub cx: i32,
    pub cy: i32,
    pub w: i32,
    pub h: i32,
}

/// A single labelled target.
#[derive(Debug, Clone)]
pub struct Hint {
    pub label: String,
    /// Box to draw the label near (physical screen pixels).
    pub x: i32,
    pub y: i32,
    /// Point to move the cursor to / click (physical screen pixels).
    pub cx: i32,
    pub cy: i32,
    /// Region size, for grid refinement (0 = not refinable).
    pub w: i32,
    pub h: i32,
}

/// Generate `count` prefix-free labels using Vimium's algorithm: build strings
/// by prepending alphabet chars breadth-first, then reverse each so the first
/// typed character varies across labels.
pub fn labels(count: usize, chars: &[char]) -> Vec<String> {
    if count == 0 {
        return Vec::new();
    }
    let mut hints: Vec<String> = vec![String::new()];
    let mut offset = 0usize;
    while hints.len() - offset < count || hints.len() == 1 {
        let prefix = hints[offset].clone();
        offset += 1;
        for &c in chars {
            let mut s = String::with_capacity(prefix.len() + 1);
            s.push(c);
            s.push_str(&prefix);
            hints.push(s);
        }
    }
    let mut out: Vec<String> = hints[offset..offset + count]
        .iter()
        .map(|h| h.chars().rev().collect::<String>())
        .collect();
    out.sort();
    out
}

/// Divide a rectangle into `cols` x `rows` cells, returning their centres and
/// sizes (row-major, so labels read left-to-right, top-to-bottom).
pub fn grid_cells(left: i32, top: i32, width: i32, height: i32, cols: i32, rows: i32) -> Vec<Target> {
    let cols = cols.max(1);
    let rows = rows.max(1);
    let mut out = Vec::with_capacity((cols * rows) as usize);
    for r in 0..rows {
        for c in 0..cols {
            let x0 = left + (width * c) / cols;
            let x1 = left + (width * (c + 1)) / cols;
            let y0 = top + (height * r) / rows;
            let y1 = top + (height * (r + 1)) / rows;
            out.push(Target {
                cx: (x0 + x1) / 2,
                cy: (y0 + y1) / 2,
                w: x1 - x0,
                h: y1 - y0,
            });
        }
    }
    out
}

/// Wrap bare click points (UIA elements) as non-refinable targets.
pub fn point_targets(points: &[(i32, i32)]) -> Vec<Target> {
    points
        .iter()
        .map(|&(cx, cy)| Target { cx, cy, w: 0, h: 0 })
        .collect()
}

/// Attach labels to targets. The draw box sits slightly up-left of the click
/// point so the label doesn't hide the target.
pub fn build(targets: &[Target], chars: &[char]) -> Vec<Hint> {
    let labels = labels(targets.len(), chars);
    targets
        .iter()
        .zip(labels)
        .map(|(t, label)| Hint {
            label,
            x: t.cx - 8,
            y: t.cy - 10,
            cx: t.cx,
            cy: t.cy,
            w: t.w,
            h: t.h,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_are_prefix_free_and_unique() {
        for &n in &[1usize, 5, 14, 15, 100, 700] {
            let ls = labels(n, ALPHABET);
            assert_eq!(ls.len(), n, "count {n}");
            let mut sorted = ls.clone();
            sorted.dedup();
            assert_eq!(sorted.len(), n, "labels not unique for {n}");
            for a in &ls {
                for b in &ls {
                    if a != b {
                        assert!(!b.starts_with(a.as_str()), "{a} is a prefix of {b}");
                    }
                }
            }
        }
    }

    #[test]
    fn grid_tiles_the_area_without_gaps() {
        let cells = grid_cells(0, 0, 1920, 1080, 8, 5);
        assert_eq!(cells.len(), 40);
        assert!(cells.iter().all(|c| c.w > 0 && c.h > 0));
        assert!(cells
            .iter()
            .all(|c| c.cx >= 0 && c.cx < 1920 && c.cy >= 0 && c.cy < 1080));
    }

    #[test]
    fn refining_a_cell_stays_inside_it() {
        let cell = grid_cells(0, 0, 800, 600, 4, 4)[5];
        let sub = grid_cells(cell.cx - cell.w / 2, cell.cy - cell.h / 2, cell.w, cell.h, 3, 3);
        assert_eq!(sub.len(), 9);
        for s in sub {
            assert!(s.cx >= cell.cx - cell.w / 2 && s.cx <= cell.cx + cell.w / 2);
            assert!(s.cy >= cell.cy - cell.h / 2 && s.cy <= cell.cy + cell.h / 2);
        }
    }
}
