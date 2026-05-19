---
name: meeting-notes
description: Capture decisions, action items, and open questions from a meeting in a consistent format. Activates when the user mentions a meeting, asks for notes, summarizes a sync, or pastes a transcript/recording.
metadata:
  triggers:
    - meeting notes
    - summarize this meeting
    - what did we decide
    - capture decisions
    - action items from
    - transcript
---

# Meeting notes

Use this template every time. Consistency is the point — the user scans these later and needs to find the same fields in the same place.

## Output template

```markdown
# {{Meeting title}}

**Date:** {{YYYY-MM-DD}}
**Attendees:** {{name, name, name}}
**Type:** {{1:1 | team sync | review | decision meeting | other}}

## Decisions
- {{Decision in one sentence. What was decided, by whom if non-obvious.}}

## Action items
- [ ] **{{Owner}}** — {{Task in one sentence}} — due {{date or "next sync"}}

## Open questions
- {{Question that didn't get resolved, with context}}

## Notes
{{Optional. Background, links, or context worth preserving but not actionable. Skip this section if it would be empty.}}
```

## Capture rules

**Decisions** are things that were resolved. "We will use Postgres" is a decision. "We talked about Postgres" is not — that goes in Notes if it matters at all.

**Action items** must have an owner and ideally a date. If no owner was assigned in the meeting, write `**Unassigned**` rather than guessing — the user will assign it after.

**Open questions** are anything that came up and didn't get resolved. They're the most valuable section because they're easy to lose. If someone said "let's table that" or "we'll come back to it", that's an open question.

## What to leave out

- Greetings, small talk, sign-offs
- Tangents that weren't decided or actioned
- Quotes — paraphrase instead, unless the exact words matter (a commitment, a price, a date)
- Anything sensitive that doesn't need to be in writing

## When working from a transcript

- Read the entire transcript before writing anything. Decisions often get revisited; capture the final version.
- If two speakers disagreed, note the resolution, not the disagreement (unless it stayed unresolved — then it's an open question)
- Strip filler — "um", "you know", "kind of", "I think maybe"

## When the user types up notes themselves

If the user gives you rough notes to format, don't add information they didn't include. If you're unsure whether something was a decision or just a discussion, put it in Notes and ask in a follow-up message — don't invent commitments.

## Length

Most meetings produce ~5-15 lines of notes. If you find yourself writing more than half a page, you're probably keeping things that should be cut. The test: would the user re-read this in a week and find it useful, or would they skim past?
