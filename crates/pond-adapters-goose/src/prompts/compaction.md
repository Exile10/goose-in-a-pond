## Context
A context limit was reached during a Goose In A Pond home assistant session. Summarise the conversation history so the session can continue seamlessly.

**Conversation History:**
{{ messages }}

Wrap reasoning in `<analysis>` tags. Review the conversation and capture:
- All user commands, requests, and preferences expressed
- All device interactions and their outcomes (lights toggled, locks checked, etc.)
- Active schedules or reminders set or modified
- Any errors encountered and how they were resolved
- Current state of any in-progress tasks or automations
- Technical details: tool calls, device IDs, schedule IDs, session IDs referenced
- User feedback and any corrections given

### Sections to Include
1. **User Requests** — All commands and questions, in order
2. **Devices & Automations** — Devices interacted with, states changed, automations triggered
3. **Schedules & Reminders** — Any set, modified, or cancelled
4. **Errors & Fixes** — What went wrong and how it was resolved
5. **In Progress** — Any unresolved requests or pending actions
6. **Session State** — Active session ID, last tool used, last response given
7. **Next Step** — Only if it directly continues the last user instruction

This summary is for the assistant only — be thorough, not brief.
No new ideas unless the user confirmed them.
