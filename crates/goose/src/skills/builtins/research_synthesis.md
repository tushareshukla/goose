---
name: research-synthesis
description: Pull from multiple sources on a topic, cite each, and produce a short, decision-grade brief. Activates when the user asks to research something, compare options, gather context, or write a brief on a topic.
metadata:
  triggers:
    - research this
    - find me sources on
    - compare these options
    - write me a brief on
    - synthesize
    - what do we know about
---

# Research synthesis

The user is going to make a decision off your brief. Your job is to compress what's actually known into a short, sourced document — not to write an essay.

## The pipeline

1. **Restate the question** in one sentence. If it's ambiguous (e.g. "research vector databases" — for what use case, what scale?), ask one clarifying question before searching.
2. **Search broadly first**, narrowly second. Cast a wide net to find the shape of the topic; then dig into the highest-signal sources.
3. **Pull from at least 3 independent sources.** A single source is not synthesis. Aim for diversity — vendor docs + a critic + a practitioner write-up beats three blog posts with the same talking points.
4. **Read enough of each source to know what it actually says.** Skimming titles produces wrong syntheses. If you only got the abstract, say so.
5. **Synthesize, don't summarize each source.** Group by claim, not by source. When two sources disagree, surface the disagreement — that's the most useful part of the brief.

## Output structure

```markdown
# {{Topic, framed as the question}}

## TL;DR
{{2-3 sentence answer to the user's actual question. Make a call where you can. Hedge only where the evidence is genuinely thin.}}

## Key findings
- **{{Claim in one sentence.}}** {{1-2 line explanation.}} [^1] [^3]
- **{{Claim.}}** {{Explanation.}} [^2]

## Disagreements / open questions
- {{Where sources contradict, or where evidence is missing.}}

## Sources
[^1]: {{Author or org, year}} — {{Title}}. {{URL}}. {{1-line note: "Vendor docs, biased toward X" or "Independent benchmark from Y"}}
[^2]: …
```

## Citation rules

- **Every non-trivial claim is cited.** If you can't tie a sentence back to a source, either remove it or mark `(my inference — verify)`.
- **One source = one footnote.** Don't pile-on cite the same source for three claims; once is enough.
- **Cite the strongest source first.** Footnote order should reflect importance.
- **Note the source's stance.** Vendor docs are useful but biased. Hacker News threads are signal-rich but anecdotal. Peer-reviewed beats blog. Note this in the source line — the user uses it to weight claims.

## What to leave out

- Definitions and history the user already knows
- "Some say X, others say Y" filler when both sides are well-known
- Long quotes — paraphrase and footnote
- Caveats so heavy the brief becomes useless ("it depends" is rarely a finding)

## When evidence is thin

Say so plainly in the TL;DR. "Most sources don't address this directly; the best signal is from [^2], who suggests…" beats inventing confidence.

## When the user wants depth, not brevity

The user can ask for a longer version. Default to the short brief — they can pull on threads. If they explicitly ask for "deep research" or a long-form report, expand each section with more detail, but keep the structure.

## Bias check

Before sending, ask yourself:
- Did I pick sources that all lean one way?
- Did I find at least one source that disagrees with the leading narrative? If none exist, say so — it's a real finding.
- Am I confident enough in the conclusion to put my name on it?
