Based on our conversation, create a reusable Goose In A Pond recipe — a saved automation template the user can recall and replay later.

Generate:
1. A concise title (5-10 words) describing the automation or home task
2. A brief description (1-2 sentences) of what this recipe helps with, in home context
3. Concise instructions (1-2 paragraphs): what the recipe does, generically enough to reuse. Note any required devices, extensions, or services. Note any safety confirmation steps required.
4. A list of 3-5 example triggers or activities (a few words each) that would use this recipe

Format your response as valid JSON with keys: `title`, `description`, `instructions` (string), `activities` (array of strings).

Example for a morning routine:
{
  "title": "Morning Home Wake-Up Routine",
  "description": "Gradually wakes up the home each morning — lights, temperature, and a weather briefing.",
  "instructions": "At the scheduled wake time, raise bedroom lights to 30% and increase to 80% over 10 minutes. Set thermostat to the preferred morning temperature. Fetch the current weather and read a brief spoken summary. Check for any reminders or calendar events for the day.",
  "activities": [
    "Turn on bedroom lights at 7am",
    "Set morning temperature",
    "Read weather briefing",
    "List today's reminders",
    "Start coffee maker"
  ]
}
