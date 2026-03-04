# Note Style

## Voice

A diligent scientific colleague — clear, thorough, technically precise.
Writes like a good lab notebook: detailed enough that future-me can
pick up the thread months later. First person when expressing opinions
or uncertainty. No hedging, no filler, no fluff. Quantitative by
default: if there's a number, rate, or scale, include it.

This vault is expansive — it captures everything from formal concepts
to long exploratory notes to chat summaries. Notes should be written
for retrieval: strong titles, clear openers, good tags. The body can
be as long as the subject demands.

## Formatting

- Bold one-liner at the top: what this note IS, one sentence
- Short paragraphs (3-4 sentences). Bullet lists for structured info.
- [[Wikilinks]] to related vault notes — connections > completeness
- Code blocks for anything runnable (Python, Rust, shell)
- LaTeX in code blocks for math: ```latex \frac{dS}{dt} = -\beta SI/N ```
- Tables for parameter mappings, comparisons, specs
- Headers for notes with 3+ sections; skip for short notes
- Tags in frontmatter only. Exception: #todo inline to mark gaps.
- "See Also" section at the bottom for 3+ related notes

## Note Templates

### Concept Note (short, atomic, reference-card)

**[One sentence: what is this and why it matters]**

[Core idea — intuition, not textbook. Why does this exist?]

[Details — formulas, code, specs. What I need to USE this.]

See Also: [[related]], [[related]]

### Notebook Note (longer, exploratory, working-through-a-subject)

**[Topic and what angle I'm exploring]**

[Extended treatment. Multiple sections, derivations, worked examples,
connections to other ideas. This is the journal entry — thinking
on paper. Can be long. Should still have clear section headers
so future-me can skim.]

### Project Note

**[What this project IS and current status]**

[Architecture / approach]
[Current state: working / broken / blocked / next steps]
[Links to concepts and notebook notes for techniques used]

### Idea Note

**[The seed of the idea in one line]**

[Stream of consciousness. Don't polish. Capture the thought
before it evaporates. Fragments and incomplete sentences are fine.]

### Reference Note

**[Source + one-sentence summary of why I care]**

Source: [link/citation]
[Key claims in my own words — not a summary, a reaction]
[What I agree with, disagree with, want to explore]

## Exemplars

### Example: Concept Note

---
title: interior mutability
tags: [concept, rust, patterns]
---

**Rust's escape hatch from borrow rules — runtime checking instead of compile-time.**

Interior mutability lets you mutate data through a shared reference.
The compiler can't verify safety statically, so the check moves to runtime.

Core types:

- `Cell<T>` — copy semantics, no overhead, single-threaded
- `RefCell<T>` — borrow tracking at runtime, panics on violation
- `Mutex<T>` / `RwLock<T>` — thread-safe versions

When to use: [[shared references]] that need mutation.
Common in [[observer pattern]], [[graph structures]], anything circular.

The insight: this isn't "unsafe." It's moving the borrow check later.
You still get the check. See [[unsafe rust]] for the actual escape hatch.

### Example: Notebook Note

---
title: Price equation and the levels of selection
tags: [note, math, evolution, population-genetics]
---

**Working through how the Price equation unifies different levels of selection — individual, group, gene.**

The Price equation partitions evolutionary change into two terms:

```latex
\Delta \bar{z} = \frac{1}{\bar{w}} \left[ \text{Cov}(w_i, z_i) + E(w_i \Delta z_i) \right]
```

The first term is selection (covariance between fitness and trait).
The second is transmission bias (how offspring deviate from parents).

What makes this powerful is that it's *recursive*. You can nest it.
Apply Price at the group level, and the "selection" term decomposes
into between-group selection and within-group selection. This is
exactly the multilevel selection debate in one equation.

The connection to [[Fisher's fundamental theorem]] is that Fisher's
theorem is the special case where you drop the transmission term
and assume additive genetic variance. Price is more general.

I keep coming back to this because it shows up everywhere:
- [[cultural evolution]] (memes as replicators)
- [[epidemiological modeling]] (strain competition)
- Even [[market selection]] (firms as units of selection)

Open question: can you apply Price to [[agent-based models]] where
fitness isn't well-defined? The agents don't reproduce in the
biological sense, but some strategies persist and spread. #todo

### Example: Idea Note

---
title: calibration as conversation
tags: [idea, calibration, modeling]
---

**What if model calibration was interactive instead of batch?**

Right now calibration is: define objective, run optimizer, wait, look
at results, tweak, repeat. What if it was a conversation? Show the
model fit so far, the human says "the peak is too early" or "the tail
is wrong," and the system translates that into parameter constraints
or loss function adjustments.

This would need: natural language to parameter space mapping, some
kind of [[Bayesian optimization]] with human priors, and a way to
visualize partial fits in real time.

Related to [[human-in-the-loop ML]] and maybe [[active learning]]. #todo
