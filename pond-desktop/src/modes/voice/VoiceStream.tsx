// ────────────────────────────────────────────────────────────
// VoiceStream — the conversation, flowing either side of the orb.
//
// A single scroll container laid out as three grid columns: what the pond
// said and did on the left, a reserved channel down the middle that the orb
// occupies, and what you said on the right. Placement is the only thing that
// says who spoke — no name chips, no avatars — because in a conversation with
// exactly two participants, the side IS the attribution.
//
// Not TranscriptFeed: that component builds its own flex column and owns its
// own scrolling, neither of which can put a message in a specific grid column.
// The two coexist — TranscriptFeed still serves the browser voice path and the
// chat panels, where a single stacked column is right.
// ────────────────────────────────────────────────────────────

import { useEffect, useRef } from "react";
import type { ContextCard, TranscriptMessage } from "../../state/reducer";
import { ContextCard as ContextCardView } from "../../components/ContextCard";
import { ThinkingPlaceholder } from "../../components/ThinkingPlaceholder";

interface Props {
  messages: TranscriptMessage[];
  contextCards?: ContextCard[];
}

export function VoiceStream({ messages, contextCards = [] }: Props) {
  const scrollerRef = useRef<HTMLDivElement>(null);

  // Follow the conversation. This element IS the scroller (unlike the old
  // drawer, where the feed inside never overflowed and scrollTo was a no-op),
  // so it can scroll itself directly.
  useEffect(() => {
    const el = scrollerRef.current;
    if (!el) return;
    el.scrollTo({ top: el.scrollHeight, behavior: "smooth" });
  }, [messages, contextCards]);

  if (messages.length === 0) {
    return (
      <div className="vm-stream vm-stream--empty" ref={scrollerRef}>
        {/* An empty screen is an invitation, so it names the next move rather
            than reporting that a list is empty. */}
        <p className="vm-stream__empty">Say the wake word to begin.</p>
      </div>
    );
  }

  return (
    <div
      className="vm-stream"
      ref={scrollerRef}
      role="log"
      aria-live="polite"
      aria-label="Conversation"
    >
      {messages.map((msg, idx) => {
        // Cards timestamped after this message and before the next belong to
        // this turn — same rule TranscriptFeed uses.
        const nextMsg = messages[idx + 1];
        const inlineCards = msg.role === "agent"
          ? contextCards.filter((c) => {
              const afterThis = c.timestamp_ms >= msg.timestamp;
              const beforeNext = !nextMsg || c.timestamp_ms < nextMsg.timestamp;
              return afterThis && beforeNext;
            })
          : [];

        return (
          // Explicit row per message. Without it, grid auto-placement fills
          // whichever cell is free — so a reply and the question that FOLLOWED
          // it land on the same row, and the conversation reads as though the
          // two of you talked over each other. One message, one row, in order.
          <div
            key={msg.id}
            className={`vm-said vm-said--${msg.role}`}
            style={{ gridRow: idx + 1 }}
          >
            <p className="vm-said__text">
              {msg.text || (msg.role === "agent" ? <ThinkingPlaceholder compact /> : "")}
            </p>

            {inlineCards.length > 0 && (
              <div className="vm-said__cards">
                {inlineCards.map((card) => (
                  <ContextCardView key={card.id} card={card} />
                ))}
              </div>
            )}
          </div>
        );
      })}
    </div>
  );
}
