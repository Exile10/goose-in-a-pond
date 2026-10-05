{# giap: bounded compaction summary. The pond installs it on the budgeted device and removes it
   elsewhere; a compaction_summary.md without this first line is never touched. It renders only the
   four fields the bounded prompt asks for, and only the eight most recent requests: the prompt asks
   for them oldest first, and gemma-4-E4B did not hold to "at most 8" but carried every request
   forward, 4 to 14 lines over four compactions. #}
# Conversation Summary

{% if user_intent %}
## User Intent
{% for item in user_intent[-8:] %}
- {{ item }}
{% endfor %}

{% endif %}
{% if pending_tasks %}
## Pending Tasks
{% for item in pending_tasks[:8] %}
- {{ item }}
{% endfor %}

{% endif %}
{% if current_work %}
## Current Work
{{ current_work }}

{% endif %}
{% if next_step %}
## Next Step
{{ next_step }}
{% endif %}
