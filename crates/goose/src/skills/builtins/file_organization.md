---
name: file-organization
description: Suggest folder structures for screenshots, downloads, projects, and personal files. Activates when the user asks how to organize files, where to put something, how to clean up Downloads or Desktop, or asks for a filing convention.
metadata:
  triggers:
    - organize my files
    - clean up downloads
    - file structure
    - where should I put
    - folder structure
    - naming convention
---

# File organization

Most file chaos comes from inconsistency, not lack of effort. Your job is to propose a small, durable structure the user can actually maintain — not a perfect taxonomy they'll abandon in a week.

## Principles

**Few categories, many files.** A handful of top-level folders that each hold a lot beats a deep tree the user has to navigate.

**Time beats topic for recall.** People remember when more reliably than what. Year-based folders age out well; topic-based folders need maintenance.

**Filename does the work.** A good filename means the folder doesn't have to. If the file is named well, you can find it with search even if it's in the wrong folder.

**Mirror existing structure.** If the user has folders named "Projects" or "Inbox", use those words. Don't invent parallel vocabulary.

## Default structures by location

### Screenshots
```
~/Pictures/Screenshots/
  2026/
    01/
    02/
    …
```
Or, if the user takes many screenshots for specific projects:
```
~/Pictures/Screenshots/
  inbox/         # default landing zone
  projects/
    {{project-name}}/
  archive/
    2025/
```
Auto-saving to `~/Pictures/Screenshots/inbox/` and triaging weekly is more sustainable than trying to file each one in the moment.

### Downloads
Don't try to organize Downloads. It's a transient inbox. Establish:

- Anything still in Downloads after 30 days gets reviewed — delete or move
- "Move to its real home or delete" — never "find it a folder in Downloads"
- For files the user knows they want to keep, move on receipt to the right home

### Documents
```
~/Documents/
  Projects/             # active work, one folder per project
    {{project-name}}/
  Reference/            # things you'll re-read but won't edit
  Archive/              # done projects, by year
    2025/
    2024/
  Personal/             # taxes, leases, IDs — anything not work
    2026/
```
"Active" projects move to Archive at year-end or on project completion, whichever is sooner. The archive is by year, not by topic — searchability does the rest.

### Source code
Defer to the user's existing convention. Common ones:
```
~/code/{{org}}/{{repo}}/
~/work/{{repo}}/
~/dev/{{repo}}/
```
Don't propose a new pattern if `~/code` or similar already exists.

## Filename conventions

Default to:
```
YYYY-MM-DD-short-kebab-name.ext
```
Examples:
- `2026-05-18-q2-board-deck.pdf`
- `2026-01-15-lease-renewal-signed.pdf`
- `2026-05-18-airbnb-receipt-tokyo.pdf`

Why:
- Lexicographic sort = chronological sort
- Kebab-case survives every shell, URL, and filesystem
- The date prefix means even the wrong folder is findable

Exceptions:
- Don't rename files you didn't author unless you need to. Original filenames carry provenance.
- Generated content (recordings, downloads with timestamps) is already named — leave it.

## What to ask before suggesting

A few questions sharpen any proposal:

1. How do you mostly find files — by searching, or by clicking through folders?
2. Roughly how many files in the messy folder? (10 vs 10,000 changes the strategy)
3. What's actively used vs. archive?
4. Are there any folders you already have that we should keep?

Don't ask all four unless the user is asking for a top-to-bottom rework. For "where should I put this PDF", just suggest.

## When the user wants automation

If they ask "how do I keep this clean", propose:

- A weekly 5-minute review of Downloads and Desktop
- A `~/Inbox` folder that anything new lands in by default, triaged weekly
- For screenshots: auto-save to a dated subfolder via the OS (macOS: `defaults write com.apple.screencapture location`)
- Avoid heavy auto-sorters (Hazel, automator rules) unless they already use them; they add maintenance debt
