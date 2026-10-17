//! Who is speaking: voiceprints and the open-set decision, term for term the pond's face matcher
//! (`sqlite_face_recognition.rs`), so a threshold calibrated here ports there unchanged.

pub mod eval;

pub const TOP_K: usize = 3;
pub const CENTROID_WEIGHT: f32 = 0.6;
pub const SNORM_WEIGHT: f32 = 0.5;
pub const RUNNER_UP_MARGIN: f32 = 0.10;
pub const OPEN_SET_GAP: f32 = 0.08;
pub const SINGLE_PROFILE_MARGIN: f32 = 0.10;
/// Cohort scores used per side in adaptive S-norm.
pub const ASNORM_TOP_N: usize = 50;

pub fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

pub fn normalized(v: &[f32]) -> Vec<f32> {
    let n = dot(v, v).sqrt().max(1e-12);
    v.iter().map(|x| x / n).collect()
}

/// One enrolled person: unit-norm embeddings of their accepted clips.
#[derive(Clone, Debug)]
pub struct Print {
    pub id: String,
    pub name: String,
    pub embs: Vec<Vec<f32>>,
}

impl Print {
    pub fn centroid(&self) -> Option<Vec<f32>> {
        let first = self.embs.first()?;
        let mut c = vec![0f32; first.len()];
        for e in &self.embs {
            for (a, b) in c.iter_mut().zip(e) {
                *a += b;
            }
        }
        Some(normalized(&c))
    }

    /// The pond's per-profile score.
    pub fn raw(&self, q: &[f32]) -> f32 {
        let mut s: Vec<f32> = self.embs.iter().map(|e| dot(q, e)).collect();
        if s.is_empty() {
            return -1.0;
        }
        s.sort_by(|a, b| b.total_cmp(a));
        let k = s.len().min(TOP_K);
        let topk = s[..k].iter().sum::<f32>() / k as f32;
        let cent = self.centroid().map(|c| dot(q, &c)).unwrap_or(topk);
        CENTROID_WEIGHT * cent + (1.0 - CENTROID_WEIGHT) * topk
    }

    /// Pairwise cosine across the enrolment clips, `(min, mean)`: a low minimum means one clip is not
    /// like the others (noise, a different person, a cough).
    pub fn self_consistency(&self) -> Option<(f32, f32)> {
        let n = self.embs.len();
        if n < 2 {
            return None;
        }
        let (mut lo, mut sum, mut k) = (1f32, 0f32, 0f32);
        for i in 0..n {
            for j in i + 1..n {
                let c = dot(&self.embs[i], &self.embs[j]);
                lo = lo.min(c);
                sum += c;
                k += 1.0;
            }
        }
        Some((lo, sum / k))
    }
}

/// External impostor cohort for adaptive S-norm and threshold calibration.
#[derive(Clone, Debug, Default)]
pub struct Cohort {
    pub embs: Vec<Vec<f32>>,
}

impl Cohort {
    /// Mean and std of the top-N cohort cosines to `e` (adaptive S-norm).
    pub fn stats(&self, e: &[f32]) -> Option<(f32, f32)> {
        if self.embs.len() < 2 {
            return None;
        }
        let mut s: Vec<f32> = self.embs.iter().map(|c| dot(e, c)).collect();
        s.sort_by(|a, b| b.total_cmp(a));
        s.truncate(ASNORM_TOP_N);
        let m = s.iter().sum::<f32>() / s.len() as f32;
        let v = s.iter().map(|x| (x - m) * (x - m)).sum::<f32>() / (s.len() - 1) as f32;
        Some((m, v.sqrt().max(1e-6)))
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Policy {
    pub threshold: f32,
    /// Set by a person or calibrated: suppresses the single-profile margin, as the pond's
    /// `threshold_is_user_set` does.
    pub threshold_is_set: bool,
    pub runner_up_margin: f32,
    pub open_set_gap: f32,
}

impl Default for Policy {
    fn default() -> Self {
        Policy {
            threshold: 0.5,
            threshold_is_set: false,
            runner_up_margin: RUNNER_UP_MARGIN,
            open_set_gap: OPEN_SET_GAP,
        }
    }
}

#[derive(Clone, Debug)]
pub struct MemberScore {
    pub id: String,
    pub name: String,
    pub raw: f32,
    pub norm: f32,
    /// Adaptive S-norm against the external cohort (z units), if a cohort exists.
    pub asnorm: Option<f32>,
}

#[derive(Clone, Debug)]
pub struct Decision {
    /// Sorted by `norm`, best first.
    pub scores: Vec<MemberScore>,
    pub accepted: Option<String>,
    pub threshold: f32,
    pub margin: f32,
    pub gap: f32,
    pub passes_threshold: bool,
    pub passes_margin: bool,
    pub passes_gap: bool,
}

pub fn identify(q: &[f32], prints: &[Print], cohort: &Cohort, policy: &Policy) -> Decision {
    let q = normalized(q);
    let qstats = cohort.stats(&q);
    let raws: Vec<f32> = prints.iter().map(|p| p.raw(&q)).collect();
    let total: f32 = raws.iter().sum();
    let n = prints.len() as f32;
    let mut scores: Vec<MemberScore> = prints
        .iter()
        .zip(&raws)
        .map(|(p, &raw)| {
            let mean_other = if n > 1.0 {
                (total - raw) / (n - 1.0)
            } else {
                0.0
            };
            let asnorm = match (qstats, p.centroid().and_then(|c| cohort.stats(&c))) {
                (Some((mq, sq)), Some((mp, sp))) => Some(0.5 * ((raw - mq) / sq + (raw - mp) / sp)),
                _ => None,
            };
            MemberScore {
                id: p.id.clone(),
                name: p.name.clone(),
                raw,
                norm: raw - SNORM_WEIGHT * mean_other,
                asnorm,
            }
        })
        .collect();
    scores.sort_by(|a, b| b.norm.total_cmp(&a.norm));

    let threshold = if scores.len() <= 1 && !policy.threshold_is_set {
        (policy.threshold + SINGLE_PROFILE_MARGIN).min(0.99)
    } else {
        policy.threshold
    };
    let Some(best) = scores.first() else {
        return Decision {
            scores,
            accepted: None,
            threshold,
            margin: 0.0,
            gap: 0.0,
            passes_threshold: false,
            passes_margin: false,
            passes_gap: false,
        };
    };
    let runner = scores.get(1).map(|s| s.norm).unwrap_or(-1.0);
    let margin = best.norm - runner;
    let gap = if scores.len() > 1 {
        best.norm - scores[1..].iter().map(|s| s.norm).sum::<f32>() / (scores.len() - 1) as f32
    } else {
        policy.open_set_gap
    };
    let (pt, pm, pg) = (
        best.raw >= threshold,
        margin >= policy.runner_up_margin,
        gap >= policy.open_set_gap,
    );
    let accepted = (pt && pm && pg).then(|| best.id.clone());
    Decision {
        scores,
        accepted,
        threshold,
        margin,
        gap,
        passes_threshold: pt,
        passes_margin: pm,
        passes_gap: pg,
    }
}

/// The threshold at which at most `far` of cohort clips would be accepted by any enrolled print.
pub fn calibrate(prints: &[Print], cohort: &Cohort, far: f32) -> Option<f32> {
    if prints.is_empty() || cohort.embs.is_empty() {
        return None;
    }
    let mut s: Vec<f32> = cohort
        .embs
        .iter()
        .map(|c| prints.iter().map(|p| p.raw(c)).fold(f32::MIN, f32::max))
        .collect();
    s.sort_by(f32::total_cmp);
    let i = ((1.0 - far) * s.len() as f32).ceil() as usize;
    Some(s[i.min(s.len() - 1)] + 1e-4)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(seed: u32, dim: usize) -> Vec<f32> {
        let mut s = seed;
        let v: Vec<f32> = (0..dim)
            .map(|_| {
                s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (s >> 8) as f32 / 16_777_216.0 - 0.5
            })
            .collect();
        normalized(&v)
    }

    fn near(base: &[f32], seed: u32, spread: f32) -> Vec<f32> {
        let n = unit(seed, base.len());
        normalized(
            &base
                .iter()
                .zip(&n)
                .map(|(a, b)| a + spread * b)
                .collect::<Vec<_>>(),
        )
    }

    fn print(id: &str, base: &[f32], seed: u32) -> Print {
        Print {
            id: id.into(),
            name: id.to_uppercase(),
            embs: (0..5).map(|i| near(base, seed + i, 0.5)).collect(),
        }
    }

    #[test]
    fn the_enrolled_voice_is_named_and_a_stranger_is_not() {
        let (a, b) = (unit(1, 256), unit(2, 256));
        let prints = [print("a", &a, 10), print("b", &b, 20)];
        let p = Policy::default();
        let d = identify(&near(&a, 99, 0.4), &prints, &Cohort::default(), &p);
        assert_eq!(d.accepted.as_deref(), Some("a"), "{d:?}");
        assert_eq!(d.scores[0].name, "A");
        let stranger = identify(&unit(3, 256), &prints, &Cohort::default(), &p);
        assert_eq!(stranger.accepted, None, "{stranger:?}");
    }

    #[test]
    fn one_enrolled_profile_needs_the_extra_margin_unless_the_threshold_was_set() {
        let a = unit(1, 64);
        let prints = [print("a", &a, 10)];
        let d = identify(&a, &prints, &Cohort::default(), &Policy::default());
        assert!((d.threshold - 0.6).abs() < 1e-6);
        let set = Policy {
            threshold_is_set: true,
            ..Policy::default()
        };
        assert!((identify(&a, &prints, &Cohort::default(), &set).threshold - 0.5).abs() < 1e-6);
    }

    #[test]
    fn nobody_enrolled_and_a_nan_query_decide_nothing_and_never_panic() {
        let d = identify(&unit(1, 32), &[], &Cohort::default(), &Policy::default());
        assert!(d.accepted.is_none() && d.scores.is_empty());
        let prints = [print("a", &unit(1, 32), 10), print("b", &unit(2, 32), 20)];
        let d = identify(
            &[f32::NAN; 32],
            &prints,
            &Cohort::default(),
            &Policy::default(),
        );
        assert!(d.accepted.is_none());
    }

    #[test]
    fn calibration_admits_at_most_the_asked_share_of_the_cohort() {
        let a = unit(1, 128);
        let prints = [print("a", &a, 10)];
        let cohort = Cohort {
            embs: (100..300).map(|s| unit(s, 128)).collect(),
        };
        let t = calibrate(&prints, &cohort, 0.01).unwrap();
        let accepted = cohort.embs.iter().filter(|e| prints[0].raw(e) >= t).count();
        assert!(accepted <= 2, "{accepted} of 200 cohort clips pass {t}");
        assert!(prints[0].raw(&near(&a, 7, 0.3)) > t);
    }
}
