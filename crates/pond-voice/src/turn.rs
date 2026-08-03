//! The voice turn as an explicit state machine.
//!
//! `ChatService::run_loop` keeps the state of a conversation in two locals —
//! `first_turn: bool` and `pending_input: Option<String>` — plus the shape of
//! the control flow itself. That works, but it means every interesting
//! transition (barge-in mid-speech, a dismissal phrase, an empty transcript,
//! an error surfacing to the UI) can only be exercised by driving the whole
//! loop with real audio hardware. In practice none of them were tested at all.
//!
//! This module is the decision half of that loop, lifted out: pure, total, and
//! `std`-only. Feed it what happened; it returns the next state and the actions
//! to take. `run_loop` keeps the I/O — and keeps the exactly-once persistence
//! invariant, which is deliberately NOT modelled here.
//!
//! ## Why the loop does not move with it
//!
//! `run_loop` is welded to persistence, the Q2-26 speculative gate, and
//! `WorkflowEvent` emission that the NDJSON contract depends on. Moving it
//! would mean making `stream_response_inner` and `persist_confirmed_turn`
//! public — trading a real invariant for a nominal crate boundary. So the
//! machine advises and `run_loop` still decides when to commit.

/// Where the conversation is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoiceState {
    /// Idle, waiting for the wake word. The state a session opens in, and the
    /// one it returns to after a dismissal or a turn that produced nothing.
    Wait,
    /// Microphone open, capturing an utterance.
    Listen,
    /// Model is working. Speech may already be playing — TTS is pipelined —
    /// which is why barge-in has to be handled here and not only in Speak.
    Think,
    /// Speaking the reply.
    Speak,
    /// Terminal. The loop exits.
    Closed,
}

/// What just happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VoiceEvent {
    /// Wake word fired, optionally carrying audio captured with it — the
    /// wake-word detector keeps recording past the trigger so the user can say
    /// "hey goose, what's the weather" without pausing.
    Activated { has_captured_audio: bool },
    /// The user asked to start a turn directly: a talk control, or the global
    /// hotkey. Skips the wake word entirely.
    PushToTalk,
    /// An utterance transcribed to something usable.
    Transcript(String),
    /// Capture ended with nothing — silence, or a transcript that was only
    /// whisper artifacts.
    Empty,
    /// A dismissal phrase ("goodbye", "never mind"). Ends the exchange but not
    /// the session.
    Dismissed,
    /// A hard exit phrase ("quit"), or stdin EOF.
    Exit,
    /// The reply finished normally.
    Replied,
    /// The user spoke over the assistant.
    BargedIn,
    /// Something failed. Non-fatal: the session continues.
    Failed(String),
}

/// What the caller should do about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VoiceAction {
    /// Arm the wake-word detector.
    ArmWakeWord,
    /// Open the microphone.
    StartCapture,
    /// Feed audio captured alongside the wake word straight into transcription
    /// rather than re-recording it.
    UsePrimedAudio,
    /// Send the transcript to the model.
    Infer(String),
    /// Stop TTS now.
    StopSpeaking,
    /// Clear the turn's interrupt state before any speech.
    BeginUtterance,
    /// Surface a message to the user. Never silence — a failed turn the user
    /// cannot see is the failure mode this whole rebuild exists to end.
    Report(String),
    /// Persist and finalise the turn. Emitted exactly once per completed turn,
    /// and never after an interruption.
    Finalize,
    /// Release the microphone and any audio device.
    ReleaseAudio,
}

/// One transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transition {
    pub state: VoiceState,
    pub actions: Vec<VoiceAction>,
}

impl Transition {
    fn to(state: VoiceState, actions: Vec<VoiceAction>) -> Self {
        Self { state, actions }
    }
}

/// Advance the machine.
///
/// Total by construction: every (state, event) pair has a defined result, so a
/// surprising event can never wedge the loop. Unexpected pairs stay in place
/// and emit nothing rather than panicking or silently resetting.
pub fn next(state: VoiceState, event: VoiceEvent) -> Transition {
    use VoiceAction as A;
    use VoiceEvent as E;
    use VoiceState as S;

    match (state, event) {
        // Exit wins from anywhere, and always releases the device.
        (_, E::Exit) => Transition::to(S::Closed, vec![A::StopSpeaking, A::ReleaseAudio]),

        // ── Wait ──────────────────────────────────────────────────────────
        (S::Wait, E::Activated { has_captured_audio }) => Transition::to(
            S::Listen,
            if has_captured_audio {
                // The detector already has the user's speech; re-recording
                // would make them say it twice.
                vec![A::UsePrimedAudio]
            } else {
                vec![A::StartCapture]
            },
        ),
        (S::Wait, E::PushToTalk) => Transition::to(S::Listen, vec![A::StartCapture]),

        // ── Listen ────────────────────────────────────────────────────────
        (S::Listen, E::Transcript(text)) => {
            Transition::to(S::Think, vec![A::BeginUtterance, A::Infer(text)])
        }
        // Nothing said: back to the wake word rather than inferring on silence.
        (S::Listen, E::Empty) => Transition::to(S::Wait, vec![A::ArmWakeWord]),
        (S::Listen, E::Dismissed) => Transition::to(S::Wait, vec![A::ArmWakeWord]),

        // ── Think / Speak ─────────────────────────────────────────────────
        // TTS is pipelined, so speech can already be playing while the model
        // is still generating. Both states therefore accept BargedIn.
        (S::Think, E::Replied) | (S::Speak, E::Replied) => {
            // Conversational turn-taking: after a reply, listen again without
            // making the user repeat the wake word.
            Transition::to(S::Listen, vec![A::Finalize, A::StartCapture])
        }
        (S::Think, E::BargedIn) | (S::Speak, E::BargedIn) => {
            // No Finalize. An interrupted turn persists nothing — the
            // invariant run_loop enforces and three of its tests cover.
            Transition::to(S::Listen, vec![A::StopSpeaking, A::StartCapture])
        }
        (S::Think, E::Dismissed) | (S::Speak, E::Dismissed) => {
            Transition::to(S::Wait, vec![A::StopSpeaking, A::ArmWakeWord])
        }

        // ── Failure ───────────────────────────────────────────────────────
        // Always reported, never silent, and never fatal. The session drops
        // back to the wake word so the user can simply try again.
        (_, E::Failed(why)) => Transition::to(
            S::Wait,
            vec![A::StopSpeaking, A::Report(why), A::ArmWakeWord],
        ),

        // Push-to-talk mid-reply: treat as a barge-in, which is what the user
        // means by reaching for the talk control while the assistant is going.
        (S::Think, E::PushToTalk) | (S::Speak, E::PushToTalk) => {
            Transition::to(S::Listen, vec![A::StopSpeaking, A::StartCapture])
        }

        // Terminal.
        (S::Closed, _) => Transition::to(S::Closed, vec![]),

        // Anything else is out of order — hold position rather than guessing.
        (s, _) => Transition::to(s, vec![]),
    }
}

#[cfg(test)]
mod tests {
    use super::VoiceAction::*;
    use super::VoiceEvent as E;
    use super::VoiceState as S;
    use super::*;

    fn step(s: VoiceState, e: VoiceEvent) -> Transition {
        next(s, e)
    }

    // ── the happy path ───────────────────────────────────────────────────

    #[test]
    fn a_full_turn_walks_wait_listen_think_and_back_to_listen() {
        let t = step(
            S::Wait,
            E::Activated {
                has_captured_audio: false,
            },
        );
        assert_eq!(t.state, S::Listen);
        assert_eq!(t.actions, vec![StartCapture]);

        let t = step(t.state, E::Transcript("what is the weather".into()));
        assert_eq!(t.state, S::Think);
        assert_eq!(
            t.actions,
            vec![BeginUtterance, Infer("what is the weather".into())],
            "interrupt state must be cleared before any speech"
        );

        let t = step(t.state, E::Replied);
        assert_eq!(t.state, S::Listen, "conversational turn-taking");
        assert!(t.actions.contains(&Finalize));
    }

    /// The detector keeps recording past the trigger, so "hey goose, what's
    /// the weather" must not make the user repeat themselves.
    #[test]
    fn captured_wake_audio_is_reused_rather_than_recording_again() {
        let t = step(
            S::Wait,
            E::Activated {
                has_captured_audio: true,
            },
        );
        assert_eq!(t.actions, vec![UsePrimedAudio]);
        assert!(!t.actions.contains(&StartCapture));
    }

    // ── push-to-talk ─────────────────────────────────────────────────────

    #[test]
    fn push_to_talk_starts_a_turn_without_the_wake_word() {
        let t = step(S::Wait, E::PushToTalk);
        assert_eq!(t.state, S::Listen);
        assert_eq!(t.actions, vec![StartCapture]);
    }

    /// Reaching for the talk control while the assistant is speaking means
    /// "stop and listen to me".
    #[test]
    fn push_to_talk_during_a_reply_interrupts_it() {
        for from in [S::Think, S::Speak] {
            let t = step(from, E::PushToTalk);
            assert_eq!(t.state, S::Listen, "from {from:?}");
            assert!(t.actions.contains(&StopSpeaking), "from {from:?}");
            assert!(
                !t.actions.contains(&Finalize),
                "must not persist, from {from:?}"
            );
        }
    }

    // ── barge-in ─────────────────────────────────────────────────────────

    /// The invariant run_loop enforces and three of its tests cover: an
    /// interrupted turn persists nothing.
    #[test]
    fn a_barge_in_never_finalizes_the_turn() {
        for from in [S::Think, S::Speak] {
            let t = step(from, E::BargedIn);
            assert_eq!(t.state, S::Listen, "from {from:?}");
            assert!(t.actions.contains(&StopSpeaking));
            assert!(
                !t.actions.contains(&Finalize),
                "an interrupted turn must persist nothing (from {from:?})"
            );
        }
    }

    /// TTS is pipelined, so the first sentence can be playing while the model
    /// still generates. Barge-in must work in Think, not only in Speak.
    #[test]
    fn barge_in_is_accepted_while_still_thinking() {
        let t = step(S::Think, E::BargedIn);
        assert_eq!(t.state, S::Listen);
        assert!(t.actions.contains(&StopSpeaking));
    }

    // ── nothing said ─────────────────────────────────────────────────────

    #[test]
    fn an_empty_capture_returns_to_the_wake_word_without_inferring() {
        let t = step(S::Listen, E::Empty);
        assert_eq!(t.state, S::Wait);
        assert_eq!(t.actions, vec![ArmWakeWord]);
        assert!(
            !t.actions.iter().any(|a| matches!(a, Infer(_))),
            "must never infer on silence"
        );
    }

    // ── dismissal and exit ───────────────────────────────────────────────

    #[test]
    fn dismissal_ends_the_exchange_but_not_the_session() {
        for from in [S::Listen, S::Think, S::Speak] {
            let t = step(from, E::Dismissed);
            assert_eq!(t.state, S::Wait, "from {from:?}");
            assert!(t.actions.contains(&ArmWakeWord));
            assert_ne!(t.state, S::Closed, "dismissal is not exit");
        }
    }

    #[test]
    fn exit_closes_from_any_state_and_releases_the_device() {
        for from in [S::Wait, S::Listen, S::Think, S::Speak] {
            let t = step(from, E::Exit);
            assert_eq!(t.state, S::Closed, "from {from:?}");
            assert!(
                t.actions.contains(&ReleaseAudio),
                "must not leave the mic open (from {from:?})"
            );
        }
    }

    #[test]
    fn closed_is_terminal() {
        for e in [
            E::Activated {
                has_captured_audio: true,
            },
            E::PushToTalk,
            E::Replied,
            E::BargedIn,
        ] {
            let t = step(S::Closed, e.clone());
            assert_eq!(t.state, S::Closed, "{e:?} must not revive a closed session");
            assert!(t.actions.is_empty());
        }
    }

    // ── failure is always visible ────────────────────────────────────────

    /// The whole point of the rebuild: a failure the user cannot see is worse
    /// than a crash. Every state must report and recover.
    #[test]
    fn a_failure_is_always_reported_and_never_fatal() {
        for from in [S::Wait, S::Listen, S::Think, S::Speak] {
            let t = step(from, E::Failed("piper voice not installed".into()));
            assert_eq!(t.state, S::Wait, "from {from:?}");
            assert!(
                t.actions
                    .iter()
                    .any(|a| matches!(a, Report(m) if m.contains("piper"))),
                "the reason must reach the user (from {from:?})"
            );
            assert!(
                t.actions.contains(&ArmWakeWord),
                "recoverable, from {from:?}"
            );
            assert_ne!(t.state, S::Closed, "non-fatal, from {from:?}");
        }
    }

    // ── totality ─────────────────────────────────────────────────────────

    /// A surprising event must never wedge the loop or panic. Exhaustively
    /// drives every state/event pair.
    #[test]
    fn every_state_event_pair_is_defined() {
        let states = [S::Wait, S::Listen, S::Think, S::Speak, S::Closed];
        let events = [
            E::Activated {
                has_captured_audio: false,
            },
            E::Activated {
                has_captured_audio: true,
            },
            E::PushToTalk,
            E::Transcript("x".into()),
            E::Empty,
            E::Dismissed,
            E::Exit,
            E::Replied,
            E::BargedIn,
            E::Failed("e".into()),
        ];
        for s in states {
            for e in &events {
                let t = next(s, e.clone());
                // Finalize is only ever legitimate on a completed reply.
                if t.actions.contains(&Finalize) {
                    assert!(
                        matches!(e, E::Replied),
                        "Finalize emitted for {e:?} from {s:?} — would persist an \
                         unfinished or interrupted turn"
                    );
                }
            }
        }
    }

    /// An out-of-order event holds position instead of resetting the session.
    #[test]
    fn an_out_of_order_event_is_ignored_rather_than_resetting() {
        let t = step(S::Wait, E::Replied);
        assert_eq!(t.state, S::Wait);
        assert!(t.actions.is_empty());

        let t = step(
            S::Listen,
            E::Activated {
                has_captured_audio: false,
            },
        );
        assert_eq!(t.state, S::Listen, "already listening");
        assert!(t.actions.is_empty());
    }

    /// Finalize must be reachable exactly one way.
    #[test]
    fn finalize_comes_only_from_a_completed_reply() {
        let mut finalizing = Vec::new();
        for s in [S::Wait, S::Listen, S::Think, S::Speak, S::Closed] {
            for e in [
                E::Replied,
                E::BargedIn,
                E::Dismissed,
                E::Exit,
                E::Empty,
                E::PushToTalk,
                E::Failed("x".into()),
            ] {
                if next(s, e.clone()).actions.contains(&Finalize) {
                    finalizing.push((s, e));
                }
            }
        }
        assert_eq!(
            finalizing.len(),
            2,
            "expected Think+Replied and Speak+Replied only, got {finalizing:?}"
        );
    }
}
