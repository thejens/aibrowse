//! Order-free accumulators: what a recording keeps instead of the samples themselves.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Bins grow by 2% each, so one histogram covers fractions and seconds alike at ~1%
/// resolution, which is far finer than the persona parameters need.
const BIN_RATIO: f64 = 1.02;
/// Values are positive quantities; a zero (a press in the same frame as the last move)
/// lands in the lowest bin instead of at −∞.
const FLOOR: f64 = 1e-3;

/// A log-binned histogram of positive values. Merging two is adding counts, so
/// recordings from many handoffs pool without keeping any one of them.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Histogram(BTreeMap<i32, u64>);

impl Histogram {
    pub fn add(&mut self, value: f64) {
        if !value.is_finite() {
            return;
        }
        let bin = (value.max(FLOOR).ln() / BIN_RATIO.ln()).round() as i32;
        *self.0.entry(bin).or_default() += 1;
    }

    pub fn merge(&mut self, other: &Histogram) {
        for (bin, n) in &other.0 {
            *self.0.entry(*bin).or_default() += n;
        }
    }

    pub fn count(&self) -> u64 {
        self.0.values().sum()
    }

    /// The `q` quantile, to bin resolution. `None` when empty.
    pub fn quantile(&self, q: f64) -> Option<f64> {
        let total = self.count();
        if total == 0 {
            return None;
        }
        let rank = ((total - 1) as f64 * q.clamp(0.0, 1.0)).round() as u64;
        let mut seen = 0;
        for (bin, n) in &self.0 {
            seen += n;
            if seen > rank {
                return Some(center(*bin));
            }
        }
        self.0.keys().next_back().map(|bin| center(*bin))
    }

    pub fn median(&self) -> Option<f64> {
        self.quantile(0.5)
    }

    /// Standard deviation of ln(value): the σ of a log-normal fitted to the data, which
    /// is how the planners model right-skewed human timing. `None` below two values.
    pub fn log_sigma(&self) -> Option<f64> {
        let total = self.count();
        if total < 2 {
            return None;
        }
        let ln = |bin: i32| f64::from(bin) * BIN_RATIO.ln();
        let mean = self.0.iter().map(|(b, n)| ln(*b) * *n as f64).sum::<f64>() / total as f64;
        let var = self
            .0
            .iter()
            .map(|(b, n)| (ln(*b) - mean).powi(2) * *n as f64)
            .sum::<f64>()
            / (total - 1) as f64;
        Some(var.sqrt())
    }
}

fn center(bin: i32) -> f64 {
    BIN_RATIO.powi(bin)
}

/// Sufficient statistics for a least-squares line y = a + b·x. They merge by addition
/// and say nothing about any single observation.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Regression {
    pub n: u64,
    pub sx: f64,
    pub sy: f64,
    pub sxx: f64,
    pub sxy: f64,
}

impl Regression {
    pub fn add(&mut self, x: f64, y: f64) {
        self.n += 1;
        self.sx += x;
        self.sy += y;
        self.sxx += x * x;
        self.sxy += x * y;
    }

    pub fn merge(&mut self, other: &Regression) {
        self.n += other.n;
        self.sx += other.sx;
        self.sy += other.sy;
        self.sxx += other.sxx;
        self.sxy += other.sxy;
    }

    pub fn mean_x(&self) -> f64 {
        self.sx / self.n.max(1) as f64
    }

    pub fn mean_y(&self) -> f64 {
        self.sy / self.n.max(1) as f64
    }

    /// The (population) variance of x: how much the observations' difficulty varied.
    pub fn var_x(&self) -> f64 {
        let n = self.n.max(1) as f64;
        (self.sxx / n - self.mean_x().powi(2)).max(0.0)
    }

    /// Intercept and slope, when x varies at all.
    pub fn line(&self) -> Option<(f64, f64)> {
        if self.n < 3 || self.var_x() < 1e-9 {
            return None;
        }
        let n = self.n as f64;
        let b = (self.sxy / n - self.mean_x() * self.mean_y()) / self.var_x();
        Some((self.mean_y() - b * self.mean_x(), b))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn histogram_quantiles_are_close_to_the_data() {
        let mut h = Histogram::default();
        for v in [90.0, 100.0, 110.0, 120.0, 500.0] {
            h.add(v);
        }
        let median = h.median().unwrap();
        assert!((median - 110.0).abs() < 1.5, "{median}");
        assert_eq!(h.count(), 5);
        assert!(Histogram::default().median().is_none());
    }

    #[test]
    fn log_sigma_matches_the_spread() {
        let mut h = Histogram::default();
        for v in [100.0, 100.0 * 1.5f64, 100.0 / 1.5] {
            h.add(v);
        }
        // ln(1.5) ≈ 0.405 is the sample standard deviation of {−0.405, 0, 0.405}.
        assert!((h.log_sigma().unwrap() - 0.405).abs() < 0.02);
    }

    #[test]
    fn regression_recovers_a_line() {
        let mut r = Regression::default();
        for x in [1.0, 2.0, 3.0, 4.0] {
            r.add(x, 100.0 + 150.0 * x);
        }
        let (a, b) = r.line().unwrap();
        assert!((a - 100.0).abs() < 1e-6 && (b - 150.0).abs() < 1e-6);
        let mut flat = Regression::default();
        for _ in 0..5 {
            flat.add(2.0, 300.0);
        }
        assert!(flat.line().is_none());
    }
}
