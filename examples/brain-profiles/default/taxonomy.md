# Knowledge Taxonomy

## Zones

### concepts/

Short, atomic reference cards. One concept per note. Self-contained —
readable with no context. These are the things I look up.

What goes here:

- Things I keep re-learning ("what is the Price equation again?")
- Techniques that span projects (MCMC, FTS5, SPI protocol)
- Definitions, patterns, gotchas, mental models
- Anything I'd want to find when working on something unrelated

Signals: cross-project utility. "I always forget this."
A technique, pattern, or definition — not tied to one codebase or project.

### notes/

Longer, notebook-style explorations of a subject. Working through
ideas, connecting threads, building understanding over time. These
are the journal entries — thinking on paper.

What goes here:

- Extended treatments of a topic (derivations, worked examples)
- Connections between multiple concepts
- Chat summaries and conversation distillations
- Learning notes when picking up a new domain
- Anything too expansive for a concept card

Signals: multiple sections, evolving understanding, "I'm working
through how X connects to Y." Longer than a concept card.

### projects/

Active work. Each project gets a subfolder. Project notes
reference decisions, status, and implementation details that
only matter in context.

Active projects (update as needed):

- starsim/ — agent-based disease simulation framework
- arcana/ — vault indexer and knowledge tools
- clasp/ — personal agent/security system
- polio/ — polio transmission modeling
- typhoid/ — typhoid modeling and calibration
- calabaria/ — Calabaria disease modeling

Signals: references a specific codebase, dataset, or deadline.
Implementation details, not general knowledge.

### work-projects/

Work-related projects. Same structure as projects/ but for
professional/employer work that should stay separate from
personal projects.

Signals: tied to work codebase, work team, or employer context.

### ideas/

Raw capture. Brainstorms, essay seeds, half-formed connections,
"what if..." notes. Allowed to be messy and incomplete.
Fragments are fine. Promoted to concepts/ or notes/ when mature.

Signals: speculative. "What if..." No clear category yet.

### references/

External sources — papers, codebases, talks, books.
The note is my REACTION to the source, not a summary.
What did I agree with? Disagree? Want to explore?

Signals: responding to a specific external source.

### writing/

Essay drafts, blog posts, longer-form pieces intended for
an audience beyond myself.

### inbox/

The dump zone. Everything lands here when capturing fast.
Zero structure required. `arcana tidy inbox/` processes it.

## Routing Rules

1. Is there a GENERAL concept that can stand alone as a reference card?
   Extract to concepts/. Keep it short and atomic.

2. Is there a longer exploration — connecting ideas, working through
   a derivation, summarizing a conversation? Route to notes/.

3. Is there project-specific context? Route to projects/ for personal
   work, work-projects/ for professional work. Link BACK to concept
   and notebook notes.

4. Is it a reaction to something external? Route to references/.

5. Is it too raw to classify? Put it in ideas/ with a descriptive
   title. Tag #unsorted.

6. Always cross-link. If a concept relates to an existing note,
   add a [[wikilink]]. Connections > completeness.

7. One idea per note. If a messy dump has three distinct thoughts,
   make three notes.

8. Stubs are valid. "I need to understand X better #todo" is a
   perfectly good note.

## Tags

### Type tags (pick one)

- #concept — short reference card
- #note — longer exploration / notebook entry
- #project — tied to specific active work
- #idea — speculative, half-formed
- #reference — reaction to external source
- #writing — essay or blog draft

### Status tags

- #unsorted — captured but not classified
- #stub — intentionally minimal, expand later
- #draft — longer but unfinished
- #solid — I trust this note
- #todo — needs more work (can combine with others)

### Domain tags (pick 1-3)

- #epi, #immunology, #typhoid, #polio — disease modeling
- #abm, #calibration, #ode, #stochastic — modeling techniques
- #math, #stats, #probability, #numerics — foundations
- #evolution, #population-genetics, #phylogenetics — evo bio
- #rust, #python, #data, #sqlite — programming
- #electronics, #esp32, #sensors — hardware
- #metascience, #epistemology, #incentives — science of science
- #meta — notes about note-taking, tools, process

### Rules

- Every note gets at least one type tag + one domain tag
- #todo can appear inline in the body to mark gaps
- 2-4 tags per note. More means the note needs splitting.
