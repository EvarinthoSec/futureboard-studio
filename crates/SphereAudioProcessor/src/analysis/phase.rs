//! Stereo correlation and goniometer samples. Worker / UI thread only.

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct PhaseMeasurement {
    /// Pearson-style correlation in `[-1, 1]`.
    pub correlation: f32,
    /// Latest goniometer point, L/R or M/S depending on the caller.
    pub gonio_x: f32,
    pub gonio_y: f32,
}

/// Correlation over a stereo window plus the last frame as a vectorscope point.
pub fn measure_phase(left: &[f32], right: &[f32], mid_side: bool) -> PhaseMeasurement {
    let n = left.len().min(right.len());
    if n == 0 {
        return PhaseMeasurement::default();
    }
    let mut sum_l = 0.0_f64;
    let mut sum_r = 0.0_f64;
    let mut sum_lr = 0.0_f64;
    let mut sum_l2 = 0.0_f64;
    let mut sum_r2 = 0.0_f64;
    for i in 0..n {
        let l = left[i] as f64;
        let r = right[i] as f64;
        sum_l += l;
        sum_r += r;
        sum_lr += l * r;
        sum_l2 += l * l;
        sum_r2 += r * r;
    }
    let n = n as f64;
    let cov = sum_lr - sum_l * sum_r / n;
    let var_l = (sum_l2 - sum_l * sum_l / n).max(0.0);
    let var_r = (sum_r2 - sum_r * sum_r / n).max(0.0);
    let den = (var_l * var_r).sqrt();
    let correlation = if den <= f64::EPSILON {
        0.0
    } else {
        (cov / den).clamp(-1.0, 1.0) as f32
    };
    let last_l = left[left.len() - 1];
    let last_r = right[right.len() - 1];
    let (x, y) = if mid_side {
        ((last_l + last_r) * 0.5, (last_l - last_r) * 0.5)
    } else {
        (last_l, last_r)
    };
    PhaseMeasurement {
        correlation,
        gonio_x: x.clamp(-1.0, 1.0),
        gonio_y: y.clamp(-1.0, 1.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_channels_correlate() {
        let l: Vec<f32> = (0..256).map(|i| (i as f32 / 32.0).sin()).collect();
        let m = measure_phase(&l, &l, false);
        assert!(m.correlation > 0.99);
    }

    #[test]
    fn inverted_channels_anti_correlate() {
        let l: Vec<f32> = (0..256).map(|i| (i as f32 / 32.0).sin()).collect();
        let r: Vec<f32> = l.iter().map(|s| -s).collect();
        let m = measure_phase(&l, &r, false);
        assert!(m.correlation < -0.99);
    }
}
