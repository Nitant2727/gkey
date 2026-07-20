//! Hint model and label generation.
//!
//! A "hint session" is a list of on-screen targets, each given a short typed
//! label. Targets come either from a UIA element scan (`f`, real controls) or
//! from a uniform screen grid (`g`, the fallback that works even where the
//! accessibility tree is empty — games, legacy Win32, poorly-instrumented
//! Electron). Both feed the same overlay and the same selection logic.

/// Home-row-biased alphabet, same spirit as Vimium/hunt-and-peck. The engine
/// uses the configured `hint.chars`; this is the default used by tests.
#[allow(dead_code)]
pub const ALPHABET: &[char] = &[
    's', 'a', 'd', 'f', 'j', 'k', 'l', 'e', 'w', 'c', 'm', 'p', 'g', 'h',
];

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

/// Build a uniform grid of target points across the given screen rectangle.
/// `cell` is the approximate desired cell size in pixels.
pub fn grid_targets(left: i32, top: i32, width: i32, height: i32, cell: i32) -> Vec<(i32, i32)> {
    let cell = cell.max(40);
    let cols = (width / cell).max(1);
    let rows = (height / cell).max(1);
    let cw = width / cols;
    let ch = height / rows;
    let mut out = Vec::with_capacity((cols * rows) as usize);
    for r in 0..rows {
        for c in 0..cols {
            let cx = left + c * cw + cw / 2;
            let cy = top + r * ch + ch / 2;
            out.push((cx, cy));
        }
    }
    out
}

/// Attach labels to targets given by their click points, using the default
/// alphabet. The draw box is placed slightly up-left of the click point so the
/// label doesn't hide the target.
#[cfg(test)]
pub fn build(targets: &[(i32, i32)]) -> Vec<Hint> {
    build_with(targets, ALPHABET)
}

/// Like [`build`], but with a caller-supplied alphabet.
pub fn build_with(targets: &[(i32, i32)], chars: &[char]) -> Vec<Hint> {
    let labels = labels(targets.len(), chars);
    targets
        .iter()
        .zip(labels)
        .map(|(&(cx, cy), label)| Hint {
            label,
            x: cx - 8,
            y: cy - 10,
            cx,
            cy,
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
            // unique
            let mut sorted = ls.clone();
            sorted.dedup();
            assert_eq!(sorted.len(), n, "labels not unique for {n}");
            // prefix-free: no label is a prefix of another
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
    fn grid_covers_screen() {
        let t = grid_targets(0, 0, 1920, 1080, 200);
        assert!(!t.is_empty());
        assert!(t
            .iter()
            .all(|&(x, y)| x >= 0 && x < 1920 && y >= 0 && y < 1080));
    }
}
