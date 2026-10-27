//! The pond's face recognition, which may arrive after startup.
//!
//! The face models are large (the embedder alone is 261 MB), so the server binds its listeners
//! first and fetches them in the background. Every consumer (the REST routes, the camera
//! pipeline) holds the same [`FaceRecognitionSlot`], so recognition installed once the download
//! lands reaches all of them without a restart.

use std::sync::{Arc, PoisonError, RwLock};

use crate::user_data::ports::face_recognition::FaceRecognition;

/// How far the face-model download has got, for a client to show while it waits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FaceModelsProgress {
    /// The file being fetched, as the download names it.
    pub file: String,
    /// Bytes of that file received so far.
    pub downloaded_bytes: u64,
    /// That file's size, or the download's estimate of it.
    pub total_bytes: u64,
}

/// Why there is no face recognition to use yet.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FaceRecognitionNotReady {
    /// The models are still downloading; recognition switches on when they land.
    #[error("the face recognition models are still downloading")]
    Downloading {
        /// The latest progress report; `None` before the first one arrives.
        progress: Option<FaceModelsProgress>,
    },
    /// The download ended without a usable model, or the model on disk would not load.
    #[error("face recognition is unavailable: {reason}")]
    Unavailable {
        /// What went wrong, as a sentence a client can show as it is.
        reason: String,
    },
}

enum State {
    NotReady(FaceRecognitionNotReady),
    Ready(Arc<dyn FaceRecognition>),
}

/// The face recognition every consumer shares: empty while the models download, filled once.
pub struct FaceRecognitionSlot {
    state: RwLock<State>,
}

impl FaceRecognitionSlot {
    /// A slot whose recognition is already loaded.
    pub fn ready(face: Arc<dyn FaceRecognition>) -> Self {
        Self::with_state(State::Ready(face))
    }

    /// A slot waiting on the model download.
    pub fn downloading() -> Self {
        Self::with_state(State::NotReady(FaceRecognitionNotReady::Downloading {
            progress: None,
        }))
    }

    /// A slot that cannot be filled until something changes on disk, said why.
    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self::with_state(State::NotReady(FaceRecognitionNotReady::Unavailable {
            reason: reason.into(),
        }))
    }

    fn with_state(state: State) -> Self {
        Self {
            state: RwLock::new(state),
        }
    }

    /// The loaded recognition, or why there is none yet.
    pub fn current(&self) -> Result<Arc<dyn FaceRecognition>, FaceRecognitionNotReady> {
        match &*self.state.read().unwrap_or_else(PoisonError::into_inner) {
            State::Ready(face) => Ok(face.clone()),
            State::NotReady(why) => Err(why.clone()),
        }
    }

    /// Switch recognition on for every holder of this slot.
    pub fn install(&self, face: Arc<dyn FaceRecognition>) {
        *self.state.write().unwrap_or_else(PoisonError::into_inner) = State::Ready(face);
    }

    /// Record that recognition cannot be loaded; a slot already filled keeps what it has.
    pub fn mark_unavailable(&self, reason: impl Into<String>) {
        let mut state = self.state.write().unwrap_or_else(PoisonError::into_inner);
        if let State::NotReady(_) = &*state {
            *state = State::NotReady(FaceRecognitionNotReady::Unavailable {
                reason: reason.into(),
            });
        }
    }

    /// Record download progress; ignored unless the slot is still waiting on the download.
    pub fn report_progress(&self, progress: FaceModelsProgress) {
        let mut state = self.state.write().unwrap_or_else(PoisonError::into_inner);
        if let State::NotReady(FaceRecognitionNotReady::Downloading { progress: latest }) =
            &mut *state
        {
            *latest = Some(progress);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::user_data::domain::face_recognition::{
        BoundingBox, FaceEmbedding, FaceIdentification,
    };
    use crate::user_data::ports::face_recognition::PairwiseSimilarity;
    use anyhow::Result;
    use async_trait::async_trait;

    struct Threshold(f32);

    #[async_trait]
    impl FaceRecognition for Threshold {
        async fn register_face(
            &self,
            _profile_id: &str,
            _image_bytes: &[u8],
            _bbox: Option<BoundingBox>,
        ) -> Result<FaceEmbedding> {
            unimplemented!("not exercised")
        }
        async fn identify_face(
            &self,
            _image_bytes: &[u8],
            _bbox: Option<BoundingBox>,
        ) -> Result<FaceIdentification> {
            unimplemented!("not exercised")
        }
        async fn list_embeddings(&self, _profile_id: &str) -> Result<Vec<FaceEmbedding>> {
            Ok(Vec::new())
        }
        async fn delete_embeddings(&self, _profile_id: &str) -> Result<u64> {
            Ok(0)
        }
        fn match_threshold(&self) -> f32 {
            self.0
        }
        async fn pairwise_similarities(&self) -> Result<Vec<PairwiseSimilarity>> {
            Ok(Vec::new())
        }
    }

    fn progress(downloaded_bytes: u64) -> FaceModelsProgress {
        FaceModelsProgress {
            file: "adaface_ir101.onnx".into(),
            downloaded_bytes,
            total_bytes: 1_000,
        }
    }

    #[test]
    fn a_downloading_slot_says_so_and_carries_the_latest_progress() {
        let slot = FaceRecognitionSlot::downloading();
        assert_eq!(
            slot.current().err(),
            Some(FaceRecognitionNotReady::Downloading { progress: None })
        );
        slot.report_progress(progress(10));
        slot.report_progress(progress(400));
        assert_eq!(
            slot.current().err(),
            Some(FaceRecognitionNotReady::Downloading {
                progress: Some(progress(400))
            })
        );
    }

    #[test]
    fn installing_switches_every_holder_on_without_a_new_slot() {
        let slot = Arc::new(FaceRecognitionSlot::downloading());
        let held_by_the_camera = slot.clone();
        slot.install(Arc::new(Threshold(0.42)));
        let face = held_by_the_camera.current().expect("installed");
        assert_eq!(face.match_threshold(), 0.42);
    }

    #[test]
    fn a_failed_download_is_reported_with_its_reason() {
        let slot = FaceRecognitionSlot::downloading();
        slot.mark_unavailable("no embedder could be downloaded");
        assert_eq!(
            slot.current().err(),
            Some(FaceRecognitionNotReady::Unavailable {
                reason: "no embedder could be downloaded".into()
            })
        );
        // Progress arriving after the verdict does not turn it back into a download.
        slot.report_progress(progress(1));
        assert!(matches!(
            slot.current(),
            Err(FaceRecognitionNotReady::Unavailable { .. })
        ));
    }

    #[test]
    fn a_loaded_slot_is_never_marked_unavailable() {
        let slot = FaceRecognitionSlot::ready(Arc::new(Threshold(0.5)));
        slot.mark_unavailable("late failure");
        slot.report_progress(progress(1));
        assert_eq!(slot.current().expect("still loaded").match_threshold(), 0.5);
    }

    #[test]
    fn an_unavailable_slot_can_still_be_filled() {
        let slot = FaceRecognitionSlot::unavailable("model would not load");
        slot.install(Arc::new(Threshold(0.48)));
        assert!(slot.current().is_ok());
    }
}
