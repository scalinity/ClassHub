You are writing multiple-choice options for flashcards in Daniel's master's
program (AI in Biomedical & Health Sciences). Each card below was written from
his own course material by an earlier synthesis pass: a question on the front,
the full answer on the back.

Your job, per card: one short TRUE option, and three short FALSE ones.

# The cards

{cards}

# What a good option set looks like

- **The true option is a one-line form of the back**, not a copy of it. Most of
  these answers run several sentences; compress to the one claim that makes it
  right, in at most about twenty-five words. If the back lists things, the true
  option names them.
- **A false option must answer the same question, wrongly.** It has to be about
  the same topic, in the same register, and the same rough length as the true
  one. The commonest failure is writing distractors that answer a *different*
  question — those are spotted without knowing anything, and the card teaches
  nothing.
- **Draw the false options from real confusions**, the ones this material
  invites: the neighbouring term, the inverted direction of an effect, the
  right idea attached to the wrong stage, a plausible-sounding definition that
  belongs to a sibling concept, the common misreading a lecturer would correct.
- **Never** make a false option absurd, a joke, or obviously off-topic; never
  use "all of the above", "none of the above", "both A and B"; never write a
  false option that is arguably true; never give the true one away by making it
  the longest, the most hedged, or the only specific one.
- Write in the material's own vocabulary. Plain sentences, no leading letters
  or numbers, no trailing full stops on fragments.

# Output

Print **only** a JSON array on stdout, one object per card you were given, and
nothing else — no prose, no code fence:

```
[{"id": 12, "true": "…", "false": ["…", "…", "…"]}]
```

Every `id` must be one of the ids above. `false` must hold exactly three
strings. A card you cannot write a fair set for is left out of the array
entirely rather than filled with weak options; it stays as it is and is asked
again another time.
