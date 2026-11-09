//! Sets of characters (code units or code points) as sorted, disjoint, non-adjacent inclusive ranges.

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CharSet {
    ranges: Vec<(u32, u32)>,
}

impl CharSet {
    pub fn new() -> CharSet {
        CharSet { ranges: Vec::new() }
    }

    pub fn from_ranges(mut ranges: Vec<(u32, u32)>) -> CharSet {
        ranges.sort_unstable();
        let mut out: Vec<(u32, u32)> = Vec::with_capacity(ranges.len());
        for (a, b) in ranges {
            if let Some(last) = out.last_mut()
                && a <= last.1.saturating_add(1)
            {
                last.1 = last.1.max(b);
                continue;
            }
            out.push((a, b));
        }
        CharSet { ranges: out }
    }

    pub fn single(c: u32) -> CharSet {
        CharSet { ranges: vec![(c, c)] }
    }

    pub fn range(a: u32, b: u32) -> CharSet {
        CharSet { ranges: vec![(a, b)] }
    }

    pub fn ranges(&self) -> &[(u32, u32)] {
        &self.ranges
    }

    pub fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }

    pub fn contains(&self, c: u32) -> bool {
        let ranges = &self.ranges;
        // Binary search for the last range starting at or before c.
        let mut lo = 0;
        let mut hi = ranges.len();
        while lo < hi {
            let mid = (lo + hi) / 2;
            if ranges[mid].0 <= c {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        lo > 0 && c <= ranges[lo - 1].1
    }

    pub fn add(&mut self, c: u32) {
        self.add_range(c, c);
    }

    pub fn add_range(&mut self, a: u32, b: u32) {
        if let Some(last) = self.ranges.last_mut()
            && a > last.1.saturating_add(1)
        {
            self.ranges.push((a, b));
            return;
        }
        if self.ranges.is_empty() {
            self.ranges.push((a, b));
            return;
        }
        let mut ranges = std::mem::take(&mut self.ranges);
        ranges.push((a, b));
        *self = CharSet::from_ranges(ranges);
    }

    pub fn add_set(&mut self, other: &CharSet) {
        if other.ranges.is_empty() {
            return;
        }
        if self.ranges.is_empty() {
            self.ranges = other.ranges.clone();
            return;
        }
        let mut ranges = std::mem::take(&mut self.ranges);
        ranges.extend_from_slice(&other.ranges);
        *self = CharSet::from_ranges(ranges);
    }

    /// The complement within 0..=max.
    pub fn complement(&self, max: u32) -> CharSet {
        let mut out = Vec::with_capacity(self.ranges.len() + 1);
        let mut next = 0u32;
        for &(a, b) in &self.ranges {
            if a > max {
                break;
            }
            if a > next {
                out.push((next, a - 1));
            }
            next = b.saturating_add(1);
            if b >= max {
                next = max + 1;
                break;
            }
        }
        if next <= max {
            out.push((next, max));
        }
        CharSet { ranges: out }
    }

    pub fn intersect(&self, other: &CharSet) -> CharSet {
        let mut out = Vec::new();
        let (mut i, mut j) = (0, 0);
        while i < self.ranges.len() && j < other.ranges.len() {
            let (a1, b1) = self.ranges[i];
            let (a2, b2) = other.ranges[j];
            let a = a1.max(a2);
            let b = b1.min(b2);
            if a <= b {
                out.push((a, b));
            }
            if b1 < b2 {
                i += 1;
            } else {
                j += 1;
            }
        }
        CharSet { ranges: out }
    }

    pub fn subtract(&self, other: &CharSet) -> CharSet {
        if other.ranges.is_empty() || self.ranges.is_empty() {
            return self.clone();
        }
        self.intersect(&other.complement(u32::MAX - 1))
    }
}
