// ────────────────────────────────────────────────────────────
// Onboarding Wizard — Constants & Static Data
// ────────────────────────────────────────────────────────────

import type {
  StepMeta,
  OnboardingDraft,
  LangOption,
  PromptStyle,
  TtsVoice,
  WakePreset,
  ProviderOption,
} from "./onboarding.types";

// ── Steps ──────────────────────────────────────────────────

export const STEPS: StepMeta[] = [
  { id: "welcome",     label: "Welcome",              caption: "Say hi to Goose",     required: false },
  { id: "about-you",   label: "About you",            caption: "Name and profile",    required: true  },
  { id: "locale",      label: "Language & location",   caption: "Locale, weather",     required: true  },
  { id: "personality", label: "Personality & identity", caption: "How Goose talks",     required: false },
  { id: "wake-word",   label: "Wake word",             caption: "How to summon",       required: false },
  { id: "model",       label: "AI model",              caption: "The brain",           required: true  },
  { id: "complete",    label: "All set",               caption: "Hello, world",        required: false },
];

// ── Avatars ────────────────────────────────────────────────

export const AVATARS = ["\u{1F986}", "\u{1F427}", "\u{1F985}", "\u{1F99C}", "\u{1F438}", "\u{1F989}", "\u{1F43B}", "\u{1F98A}", "\u{1F431}", "\u{1F436}"];

// ── Languages ──────────────────────────────────────────────

export const LANGUAGES: LangOption[] = [
  { key: "en", label: "English" },
  { key: "fr", label: "Fran\u00e7ais" },
  { key: "es", label: "Espa\u00f1ol" },
  { key: "de", label: "Deutsch" },
  { key: "sw", label: "Kiswahili" },
  { key: "pt", label: "Portugu\u00eas" },
  { key: "ja", label: "\u65e5\u672c\u8a9e" },
  { key: "zh", label: "\u4e2d\u6587" },
];

// ── Timezones ──────────────────────────────────────────────

export const TIMEZONES = [
  "Africa/Nairobi", "Europe/London", "Europe/Berlin", "Europe/Paris",
  "America/New_York", "America/Los_Angeles", "America/Chicago",
  "Asia/Tokyo", "Asia/Singapore", "Asia/Dubai",
  "Australia/Sydney", "Pacific/Auckland", "UTC",
];

// ── Prompt styles ──────────────────────────────────────────

/** Prompt style metadata — icons are rendered via lucide-react in StepPersonality. */
export const PROMPT_STYLES: PromptStyle[] = [
  { value: "balanced",  label: "Balanced",  icon: "", desc: "Warm and practical. Just enough detail." },
  { value: "concise",   label: "Concise",   icon: "", desc: "Short, action-first. Skips the small talk." },
  { value: "technical", label: "Technical",  icon: "", desc: "Step-by-step. Detailed narration for tinkerers." },
  { value: "warm",      label: "Warm",       icon: "", desc: "Conversational, like a helpful neighbour." },
];

// ── TTS voices ─────────────────────────────────────────────

export const TTS_VOICES: TtsVoice[] = [
  { value: "amy",      label: "Amy",      accent: "British \u00b7 Female",  file: "piper-amy.onnx" },
  { value: "ryan",     label: "Ryan",     accent: "American \u00b7 Male",   file: "piper-ryan.onnx" },
  { value: "kathleen", label: "Kathleen", accent: "Irish \u00b7 Female",    file: "piper-kathleen.onnx" },
  { value: "libritts", label: "LibriTTS", accent: "Neutral \u00b7 Mixed",   file: "libritts-r.onnx" },
];

// ── Wake word presets ──────────────────────────────────────

export const WAKE_PRESETS: WakePreset[] = [
  { value: "goose",       label: '"Goose"',       desc: "Short and memorable." },
  { value: "hey goose",   label: '"Hey Goose"',   desc: "Natural call-and-response." },
  { value: "ok computer", label: '"OK Computer"', desc: "Classic command style." },
  { value: "custom",      label: "Custom phrase",  desc: "Say anything you like." },
];

// ── AI providers ───────────────────────────────────────────

/** Provider metadata — icons are rendered via lucide-react in StepModel. */
export const PROVIDERS: ProviderOption[] = [
  { key: "llamafile", label: "Llamafile",  icon: "", desc: "Self-contained. Starts automatically.", recommended: true },
  { key: "ollama",    label: "Ollama",     icon: "", desc: "Use models you've already set up." },
  { key: "local",     label: "GGUF file",  icon: "", desc: "Load a GGUF directly from disk." },
];

// ── Default draft ──────────────────────────────────────────

export const DEFAULT_DRAFT: OnboardingDraft = {
  userName: "",
  preferredName: "",
  birthday: "",
  avatar: "\ud83e\udd86",
  atypicalSpeech: false,
  slowSpeech: false,
  highContrast: false,
  reduceMotion: false,
  language: "en",
  timezone: Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC",
  locationName: "",
  enableWeather: false,
  promptStyle: "balanced",
  personality: "friendly and helpful",
  assistantName: "Goose",
  ttsVoice: "amy",
  ttsRate: 50,
  wakeWord: "goose",
  wakeWordCustom: "",
  llmProvider: "llamafile",
  llmModel: "",
  asrModel: "",
  ttsModel: "",
  enableMcpMemory: true,
  enableHomeAssistant: false,
  enableCalendar: false,
};
