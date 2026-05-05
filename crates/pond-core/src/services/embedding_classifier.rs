//! Embedding-based domain classifier — maps user messages to tool domains via
//! cosine similarity against pre-computed domain description embeddings.
//!
//! Replaces both the keyword classifier (fast but brittle) and the LLM classifier
//! (accurate but slow ~2s) with a single ~10ms embedding lookup.
//!
//! ## How it works
//! 1. At startup, each [`ToolDomain`] gets a representative description string
//!    embedded via the [`EmbeddingProvider`] port.
//! 2. At query time, the user message is embedded and compared against all domain
//!    vectors using cosine similarity.
//! 3. The domain with the highest similarity above the threshold wins; otherwise
//!    [`ToolDomain::General`] is returned.
//!
//! ## Performance
//! - Startup: ~500ms (8 domain descriptions embedded once)
//! - Per-query: ~5-10ms (single embed + 8 cosine similarities)
//! - Memory: 8 × 384-dim vectors ≈ 12 KB

use crate::ports::embedding::EmbeddingProvider;
use crate::services::domain_classifier::ToolDomain;
use anyhow::Result;

/// Minimum cosine similarity required to assign a domain.
/// Below this threshold the query is classified as [`ToolDomain::General`].
const SIMILARITY_THRESHOLD: f32 = 0.35;

/// Pre-computed domain embeddings for fast cosine-similarity classification.
pub struct EmbeddingClassifier {
    domains: Vec<(ToolDomain, Vec<f32>)>,
}

impl EmbeddingClassifier {
    /// Build the classifier by embedding description strings for each domain.
    ///
    /// This should be called once at startup. The `provider` is only used during
    /// construction — the resulting struct holds the pre-computed vectors.
    pub async fn new(provider: &dyn EmbeddingProvider) -> Result<Self> {
        let domain_descriptions: Vec<(ToolDomain, &str)> = vec![
            (
                ToolDomain::Music,
                "play music, search for songs, pause playback, skip to next track, \
                 control volume, what song is playing right now, add to playlist, \
                 queue up music, shuffle, spotify, listen to artist, put on album",
            ),
            (
                ToolDomain::Weather,
                "weather forecast today, current temperature outside, is it going \
                 to rain, humidity and wind conditions, sunny or cloudy, what's the \
                 weather like",
            ),
            (
                ToolDomain::Knowledge,
                "who is this person, what is this thing, explain a concept, tell me \
                 about a topic, look up information, define a term, history and \
                 facts, search wikipedia",
            ),
            (
                ToolDomain::Home,
                "turn on the lights, turn off devices, switch on appliance, lock the \
                 front door, unlock door, list connected devices, thermostat \
                 temperature, smart home control",
            ),
            (
                ToolDomain::Schedule,
                "schedule a task, set a reminder for later, create an alarm, set a \
                 timer, cron job automation, what's scheduled today, remind me at a \
                 specific time",
            ),
            (
                ToolDomain::Memory,
                "remember this fact about me, recall what you know, forget something \
                 I told you, my name is, I prefer, do you remember what I said, save \
                 to memory",
            ),
            (
                ToolDomain::System,
                "system information, disk space usage, computer uptime, send a \
                 desktop notification, run a shell command, clipboard contents, \
                 hostname",
            ),
            (
                ToolDomain::FileSystem,
                "read a file from disk, write content to a file, list files in a \
                 directory, create a new folder, search for files, delete a file, \
                 file system operations",
            ),
        ];

        let mut domains = Vec::with_capacity(domain_descriptions.len());
        for (domain, desc) in domain_descriptions {
            let embedding = provider.embed(desc).await?;
            domains.push((domain, embedding));
        }

        tracing::info!(
            count = domains.len(),
            "Embedding classifier initialized with domain vectors"
        );
        Ok(Self { domains })
    }

    /// Classify a user query by finding the most similar domain.
    ///
    /// Returns [`ToolDomain::General`] if no domain exceeds the similarity
    /// threshold or if embedding the query fails.
    pub async fn classify(&self, query: &str, provider: &dyn EmbeddingProvider) -> ToolDomain {
        let query_embedding = match provider.embed(query).await {
            Ok(e) => e,
            Err(e) => {
                tracing::debug!(error = %e, "Embedding failed for classification");
                return ToolDomain::General;
            }
        };

        let mut best_domain = ToolDomain::General;
        let mut best_similarity: f32 = SIMILARITY_THRESHOLD;

        for (domain, domain_emb) in &self.domains {
            let sim = cosine_similarity(&query_embedding, domain_emb);
            if sim > best_similarity {
                best_similarity = sim;
                best_domain = *domain;
            }
        }

        tracing::info!(
            domain = ?best_domain,
            similarity = format!("{:.3}", best_similarity),
            query = &query[..query.len().min(60)],
            "Embedding classification"
        );

        best_domain
    }
}

/// Compute the cosine similarity between two vectors.
///
/// Returns 0.0 for empty or mismatched-length vectors. Values range from -1.0
/// (opposite) to 1.0 (identical direction).
fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }

    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();

    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }

    dot / (norm_a * norm_b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_vectors_have_similarity_one() {
        let v = vec![1.0, 2.0, 3.0, 4.0];
        let sim = cosine_similarity(&v, &v);
        assert!((sim - 1.0).abs() < 1e-6, "Expected ~1.0, got {sim}");
    }

    #[test]
    fn orthogonal_vectors_have_similarity_zero() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![0.0, 1.0, 0.0];
        let sim = cosine_similarity(&a, &b);
        assert!(sim.abs() < 1e-6, "Expected ~0.0, got {sim}");
    }

    #[test]
    fn empty_vectors_return_zero() {
        let sim = cosine_similarity(&[], &[]);
        assert_eq!(sim, 0.0);
    }

    #[test]
    fn mismatched_lengths_return_zero() {
        let a = vec![1.0, 2.0];
        let b = vec![1.0, 2.0, 3.0];
        let sim = cosine_similarity(&a, &b);
        assert_eq!(sim, 0.0);
    }

    #[test]
    fn zero_vector_returns_zero() {
        let a = vec![0.0, 0.0, 0.0];
        let b = vec![1.0, 2.0, 3.0];
        let sim = cosine_similarity(&a, &b);
        assert_eq!(sim, 0.0);
    }

    #[test]
    fn opposite_vectors_have_negative_similarity() {
        let a = vec![1.0, 0.0];
        let b = vec![-1.0, 0.0];
        let sim = cosine_similarity(&a, &b);
        assert!((sim - (-1.0)).abs() < 1e-6, "Expected ~-1.0, got {sim}");
    }

    #[test]
    fn scaled_vectors_have_similarity_one() {
        // Cosine similarity is scale-invariant
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![2.0, 4.0, 6.0];
        let sim = cosine_similarity(&a, &b);
        assert!((sim - 1.0).abs() < 1e-6, "Expected ~1.0, got {sim}");
    }
}
