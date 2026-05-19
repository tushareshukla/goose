---
name: calendar-triage
description: Classify upcoming calendar events by importance and surface what needs prep. Activates when the user asks about their day, their week, what's coming up, what they should prepare for, or pastes a list of calendar events.
metadata:
  triggers:
    - what's on my calendar
    - what's coming up
    - triage my day
    - what should I prep for
    - upcoming meetings
    - busy today
---

# Calendar triage

Goal: give the user a 30-second read on their day or week so they walk in prepared and can decline what doesn't need them.

## Classify every event

For each event, pick exactly one tier:

**Critical** — the user is the decision-maker or primary speaker. Cancelling causes harm.
- 1:1s the user owns
- Reviews where they have the final call
- External meetings (customers, candidates, investors)
- Events where they are presenting or facilitating

**Important** — the user contributes meaningfully but isn't load-bearing.
- Team syncs where they have a topic
- Working sessions
- Project check-ins

**Optional** — the user could skip without anyone noticing, or asynchronously catch up.
- Large all-hands or town halls
- Recurring syncs they joined for context
- "FYI" invites where they're one of many

**Skip-candidate** — actively flag for declining.
- Conflicts with a Critical event
- They've attended the same recurring meeting twice this week without speaking
- The agenda is empty or marked "as needed"
- Duplicates content from another meeting on the calendar

## Output format

```markdown
## {{Today | This week}}

**Critical**
- {{HH:MM}} {{title}} — {{1-line note about why or what to prep}}

**Important**
- {{HH:MM}} {{title}} — {{1-line note if useful}}

**Optional**
- {{HH:MM}} {{title}}

**Could decline**
- {{HH:MM}} {{title}} — {{reason}}
```

If a category is empty, omit it entirely. Don't pad with "(none)".

## What to surface for prep

For Critical and Important events, add a short prep note when one of these is true:

- The event has an agenda doc — link it and call out unresolved items
- The user has an action item from the last instance of this meeting
- The attendees include someone the user hasn't met (worth a 30-second look at their profile)
- There's pre-read material — flag it
- It's externally-facing and the user is the host — confirm the room/link works ahead of time

Skip prep notes for routine events (standups, 1:1s with frequent collaborators) unless something specific is up.

## Patterns to watch for

- **Back-to-back blocks** longer than 3 hours — call out and suggest a buffer
- **No-lunch days** — if there's no gap between noon and 2pm
- **Meetings that should be email** — flag explicitly, propose a written alternative
- **Recurring meetings the user keeps declining** — suggest dropping
- **Travel time between physical locations** — surface if it's tight

## Tone

Be direct. "You can skip the All-Hands; the recording is fine." Not "You may want to consider whether attending the All-Hands is necessary."

The user is asking because they want a decision, not a list of options.

## When details are missing

If the user hasn't given you enough to classify (just titles, no descriptions or attendees), say so once and triage on title alone — with lower confidence. Don't refuse to triage.
