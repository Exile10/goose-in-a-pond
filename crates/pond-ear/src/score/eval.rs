//! Verification metrics over labelled trials: FAR/FRR curve, EER, minDCF and Wilson intervals,
//! in the shape of the pond's `/faces/debug/eval`.

#[derive(Clone, Copy, Debug)]
pub struct Point {
    pub threshold: f32,
    pub far: f32,
    pub frr: f32,
}

#[derive(Clone, Debug, Default)]
pub struct Eval {
    pub n_genuine: usize,
    pub n_impostor: usize,
    pub curve: Vec<Point>,
    pub eer: Option<f32>,
    /// Where FAR and FRR cross (the EER threshold).
    pub crossover: Option<f32>,
    /// Lowest threshold with FAR <= 1%: the operating point Voice Match calibrates to.
    pub recommended: Option<f32>,
    /// NIST SRE-style minimum detection cost, P_target 0.01, C_miss = C_fa = 1, normalised by the
    /// cost of always rejecting.
    pub min_dcf: Option<f32>,
}

/// `genuine` and `impostor` are raw scores.
pub fn evaluate(genuine: &[f32], impostor: &[f32]) -> Eval {
    let mut e = Eval {
        n_genuine: genuine.len(),
        n_impostor: impostor.len(),
        ..Default::default()
    };
    let steps = 200;
    let (lo, hi) = (-0.2f32, 1.0f32);
    for i in 0..=steps {
        let t = lo + (hi - lo) * i as f32 / steps as f32;
        let far = rate(impostor, |s| s >= t);
        let frr = rate(genuine, |s| s < t);
        e.curve.push(Point {
            threshold: t,
            far,
            frr,
        });
    }
    if genuine.is_empty() || impostor.is_empty() {
        return e;
    }
    let Some(best) = e
        .curve
        .iter()
        .min_by(|a, b| (a.far - a.frr).abs().total_cmp(&(b.far - b.frr).abs()))
    else {
        return e;
    };
    e.eer = Some((best.far + best.frr) / 2.0);
    e.crossover = Some(best.threshold);
    e.recommended = e.curve.iter().find(|p| p.far <= 0.01).map(|p| p.threshold);
    let (pt, cm, cf) = (0.01f32, 1.0f32, 1.0f32);
    let norm = (cm * pt).min(cf * (1.0 - pt));
    e.min_dcf = e
        .curve
        .iter()
        .map(|p| (cm * pt * p.frr + cf * (1.0 - pt) * p.far) / norm)
        .fold(None, |m: Option<f32>, v| Some(m.map_or(v, |m| m.min(v))));
    e
}

fn rate(v: &[f32], f: impl Fn(f32) -> bool) -> f32 {
    if v.is_empty() {
        return 0.0;
    }
    v.iter().filter(|&&s| f(s)).count() as f32 / v.len() as f32
}

/// Wilson score interval for k successes in n trials; z = 1.645 gives 90%.
pub fn wilson(k: usize, n: usize, z: f32) -> (f32, f32) {
    if n == 0 {
        return (0.0, 1.0);
    }
    let (k, n) = (k as f32, n as f32);
    let p = k / n;
    let d = 1.0 + z * z / n;
    let c = (p + z * z / (2.0 * n)) / d;
    let h = z * ((p * (1.0 - p) + z * z / (4.0 * n)) / n).sqrt() / d;
    ((c - h).max(0.0), (c + h).min(1.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn separable_scores_have_zero_eer() {
        let e = evaluate(&[0.8, 0.9, 0.85], &[0.1, 0.2, 0.15]);
        assert_eq!(e.eer, Some(0.0));
        let t = e.recommended.unwrap();
        assert!(t > 0.2 && t <= 0.8);
    }

    #[test]
    fn wilson_bounds_zero_of_three_hundred() {
        let (lo, hi) = wilson(0, 300, 1.645);
        assert_eq!(lo, 0.0);
        assert!(
            hi < 0.01,
            "0/300 bounds the rate under 1% (rule of 3): {hi}"
        );
    }
}
