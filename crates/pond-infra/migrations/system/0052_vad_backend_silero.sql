-- vad_backend: 'rms' -> 'silero'.
--
-- `rms` is a level threshold. It calls anything louder than 0.005 speech, which
-- a fan, a fridge or a laptop under load comfortably clears — so the endpoint
-- never fires, the microphone stays open to the hard cap, and the assistant
-- looks like it is ignoring you. Moving the threshold does not fix it: lower
-- clips quiet speech, higher deafens the pond in a quiet room. Silero is a 2 MB
-- ONNX model that scores that same noise at 0.08 and speech at 0.945.
--
-- It shipped opt-in for one commit and is now the default, so this adopts it on
-- installs that saved a settings snapshot in between and therefore have the key
-- pinned to 'rms'. Guarded on the stored value and on `is_user_set`, per
-- migration 0035 — a user who chose 'rms' deliberately keeps it, and re-running
-- this file is a no-op. The registry entry is `DEFAULT_ADOPTIONS` in
-- `pond-core/src/user_data/domain/settings.rs`.
--
-- The pond degrades to the energy gate at run time whenever the model cannot be
-- fetched or the ONNX Runtime will not load, so adopting this on a machine with
-- no network changes nothing except what it tries first.
UPDATE settings
   SET value = 'silero', updated_at = datetime('now')
 WHERE key = 'vad_backend'
   AND value = 'rms'
   AND is_user_set = 0;
