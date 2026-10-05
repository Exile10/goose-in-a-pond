{# giap: bounded compaction prompt. The pond installs it on the budgeted device and removes it
   elsewhere; a compaction.md without this first line is never touched. Measured on a Jetson Orin
   with gemma-4-E4B: the summary call 79 s against 126 s for goose's own prompt, and 53% of the 8k
   window left in use after a compaction against 58-63%. #}
## Task Context
- An llm context limit was reached when a user was in a session with an assistant (you)
- Distill the conversation below into a short structured summary
- The summary will be read by you on the next exchange to continue the session

**Conversation History:**
{{ messages }}

Output exactly one ```json code block and nothing else, matching this schema. At most 8 items in a list, each one short line:

```json
{
  "user_intent": ["each request the user made, in their own words, oldest first"],
  "pending_tasks": ["anything asked for and not yet done; leave empty if none"],
  "current_work": "what the conversation was about most recently",
  "next_step": "what you would do next if the user asked nothing new"
}
```
