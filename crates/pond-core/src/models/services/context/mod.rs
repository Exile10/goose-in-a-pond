//! Context-window management: [`context_governor`] resolves the active window,
//! [`context_budget`] splits it, [`context_monitor`] tracks live utilisation, and
//! [`model_class`] decides which compaction mechanisms the model can afford.
//! [`resume_compaction`] and [`resummarisation`] are PAI-4's time and model axes.
pub mod answer_contract;
pub mod context_budget;
pub mod context_governor;
pub mod context_monitor;
pub mod image_history;
pub mod model_class;
pub mod prefix_cache;
pub mod resume_compaction;
pub mod resummarisation;
pub mod token_counting;
pub mod turn_budget;
pub mod turn_trimmer;
